//! Directory-extent I/O: entry-set append/locate/unlink/remove.
//!
//! A directory is treated as one logical byte stream (its clusters
//! concatenated in walk order). Entry sets may straddle cluster
//! boundaries — Windows writes them that way — so every read/write goes
//! through [`SetLoc`]-based helpers that scatter across the boundary
//! instead of assuming a contiguous on-disk range.

use alloc::vec;
use alloc::vec::Vec;

use embedded_io::{Read, Seek, Write};

use super::direntry::{DirEntryDecoder, DirExtent, SetLoc};
use super::{
    END_OF_DIRECTORY, ENTRY_IN_USE, ENTRY_TYPE_FILE, EXFAT_ENTRY_SIZE, ExfatVfs, FAT_EOC,
    cluster_to_byte_offset, entry_set_checksum,
};
use crate::codec::exfat::raw_entry::ExfatStreamExtension;
use crate::error::{CorruptKind, FsError, FsResult};

impl<S> ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Collect a directory's clusters into a `Vec` so write helpers can
    /// iterate without double-borrowing `self`. When FAT-chained, EOC
    /// ends the walk and `max_clusters` (possibly stale after growth) is
    /// ignored — but a walk longer than the volume's cluster count can
    /// only be a corrupt FAT cycle, and errors instead of ballooning the
    /// directory buffer (once ~4 GiB on a cyclic 4 KiB-cluster chain).
    /// When contiguous, the FAT is meaningless past the extent and
    /// `max_clusters` is authoritative.
    fn collect_dir_clusters_of(
        &self,
        first_cluster: u32,
        no_fat_chain: bool,
        max_clusters: u32,
    ) -> FsResult<Vec<u32>, S::Error> {
        let mut out = Vec::new();
        let mut cur = Some(first_cluster);
        let mut walked = 0u32;
        let chained_cap = self.boot.cluster_count;
        while let Some(c) = cur {
            if no_fat_chain {
                if walked >= max_clusters {
                    break;
                }
            } else if walked >= chained_cap {
                return Err(FsError::Corrupt(CorruptKind::ClusterChain));
            }
            out.push(c);
            walked += 1;
            cur = self.next_cluster(c, no_fat_chain)?;
        }
        Ok(out)
    }

    /// Read a whole directory into one buffer: its clusters, concatenated
    /// in walk order. Logical offsets into this buffer are what [`SetLoc`]
    /// records.
    pub(super) fn read_dir_stream(
        &self,
        first_cluster: u32,
        no_fat_chain: bool,
        max_clusters: u32,
    ) -> FsResult<(Vec<u32>, Vec<u8>), S::Error> {
        let clusters = self.collect_dir_clusters_of(first_cluster, no_fat_chain, max_clusters)?;
        let cluster_bytes = self.boot.bytes_per_cluster() as usize;
        let mut buf = vec![0u8; clusters.len() * cluster_bytes];
        for (i, &c) in clusters.iter().enumerate() {
            self.read_cluster_into(c, &mut buf[i * cluster_bytes..(i + 1) * cluster_bytes])?;
        }
        Ok((clusters, buf))
    }

    /// Locate logical byte `pos` of the directory stream described by
    /// `clusters`: the device byte offset and how many bytes remain in
    /// that cluster. Positions past the collected run are corruption.
    fn locate_in_dir_stream(&self, clusters: &[u32], pos: u64) -> FsResult<(u64, usize), S::Error> {
        let cluster_bytes = u64::from(self.boot.bytes_per_cluster());
        let idx = usize::try_from(pos / cluster_bytes).unwrap_or(usize::MAX);
        let in_cluster = pos % cluster_bytes;
        let cluster = *clusters
            .get(idx)
            .ok_or(FsError::Corrupt(CorruptKind::DirEntry))?;
        let offset = cluster_to_byte_offset(
            self.boot.cluster_heap_offset,
            cluster,
            self.boot.bytes_per_sector(),
            self.boot.bytes_per_cluster(),
        ) + in_cluster;
        let span = usize::try_from(cluster_bytes - in_cluster).unwrap_or(usize::MAX);
        Ok((offset, span))
    }

    /// Write `bytes` at logical offset `logical` of the directory stream
    /// described by `clusters`, scattering across cluster boundaries.
    fn write_dir_stream_at(
        &self,
        clusters: &[u32],
        logical: u64,
        bytes: &[u8],
    ) -> FsResult<(), S::Error> {
        let mut written = 0usize;
        while written < bytes.len() {
            let (offset, span) = self.locate_in_dir_stream(clusters, logical + written as u64)?;
            let chunk = span.min(bytes.len() - written);
            self.write_at(offset, &bytes[written..written + chunk])?;
            written += chunk;
        }
        Ok(())
    }

    /// Read the entry set at `loc` (possibly spanning clusters).
    pub(super) fn read_set(&self, loc: &SetLoc) -> FsResult<Vec<u8>, S::Error> {
        let clusters = self.collect_dir_clusters_of(
            loc.dir_first_cluster,
            loc.dir_no_fat_chain,
            loc.dir_max_clusters,
        )?;
        let mut out = vec![0u8; loc.len as usize];
        let mut done = 0usize;
        while done < out.len() {
            let (offset, span) = self.locate_in_dir_stream(&clusters, loc.logical + done as u64)?;
            let chunk = span.min(out.len() - done);
            self.read_at(offset, &mut out[done..done + chunk])?;
            done += chunk;
        }
        Ok(out)
    }

    /// Overwrite the entry set at `loc` with `bytes` (`bytes.len()` must
    /// equal `loc.len`).
    pub(super) fn write_set(&self, loc: &SetLoc, bytes: &[u8]) -> FsResult<(), S::Error> {
        debug_assert_eq!(bytes.len(), loc.len as usize);
        let clusters = self.collect_dir_clusters_of(
            loc.dir_first_cluster,
            loc.dir_no_fat_chain,
            loc.dir_max_clusters,
        )?;
        self.write_dir_stream_at(&clusters, loc.logical, bytes)
    }

    /// Append an entry set at the first end-of-directory slot, allocating
    /// and FAT-linking a fresh cluster (and patching the parent entry)
    /// when the remaining space can't hold it. The set may straddle the
    /// boundary into the new cluster.
    pub(super) fn append_entry_set_in(
        &self,
        ext: &DirExtent,
        entry_set: &[u8],
    ) -> FsResult<(), S::Error> {
        // A held extent goes stale after growth; re-derive from disk.
        let ext = &self.refreshed_extent(ext);
        let cluster_bytes = self.boot.bytes_per_cluster() as usize;
        let (mut clusters, buf) =
            self.read_dir_stream(ext.first_cluster, ext.no_fat_chain, ext.max_clusters)?;

        // First terminator (or end of allocated space) is the insert point.
        let mut insert = buf.len();
        let mut pos = 0usize;
        while pos + EXFAT_ENTRY_SIZE <= buf.len() {
            if buf[pos] == END_OF_DIRECTORY {
                insert = pos;
                break;
            }
            pos += EXFAT_ENTRY_SIZE;
        }

        if insert + entry_set.len() <= buf.len() {
            return self.write_dir_stream_at(&clusters, insert as u64, entry_set);
        }

        // Grow the directory by as many clusters as the shortfall needs —
        // the insert point can sit just below the old end, so a set may
        // span several fresh clusters. The set itself may straddle the
        // old/new boundary.
        let shortfall = insert + entry_set.len() - buf.len();
        let extra = shortfall.div_ceil(cluster_bytes);
        let mut tail = *clusters
            .last()
            .ok_or(FsError::Corrupt(CorruptKind::ClusterChain))?;
        // A previously contiguous (NoFatChain) directory needs FAT
        // linkage now so the iterator can reach the new clusters.
        if ext.no_fat_chain {
            for i in 0..clusters.len().saturating_sub(1) {
                self.write_fat_entry(clusters[i], clusters[i + 1])?;
            }
        }
        let zeros = vec![0u8; cluster_bytes];
        for _ in 0..extra {
            let new_cluster = self.find_free_cluster()?.ok_or(FsError::StorageFull)?;
            self.mark_cluster_allocated(new_cluster)?;
            self.write_fat_entry(tail, new_cluster)?;
            self.write_fat_entry(new_cluster, FAT_EOC)?;
            // Zero the new cluster so the slot after the set terminates
            // the directory (and stale data can't decode as entries).
            self.write_cluster(new_cluster, &zeros)?;
            clusters.push(new_cluster);
            tail = new_cluster;
        }
        if extra > 0 {
            // Allocate-before-link: the extended, zeroed chain must be
            // durable before the set written into it becomes visible.
            self.sync()?;
        }
        self.write_dir_stream_at(&clusters, insert as u64, entry_set)?;

        // The root has no parent entry; its growth is implicit in the FAT.
        if let Some(parent_loc) = ext.parent_entry {
            self.patch_dir_entry_after_growth(&parent_loc, extra * cluster_bytes)?;
        }
        Ok(())
    }

    /// After growing a sub-directory, update its entry in the parent:
    /// bump `DataLength` by `added_bytes`, clear `NoFatChain` (growth
    /// always FAT-chains), and recompute the entry-set checksum.
    fn patch_dir_entry_after_growth(
        &self,
        loc: &SetLoc,
        added_bytes: usize,
    ) -> FsResult<(), S::Error> {
        let mut set = self.read_set(loc)?;
        // Stream Extension is the first secondary, at offset 32.
        let stream_off = EXFAT_ENTRY_SIZE;
        let mut stream =
            ExfatStreamExtension::parse(&set[stream_off..stream_off + EXFAT_ENTRY_SIZE])
                .ok_or(FsError::Corrupt(CorruptKind::DirEntry))?;
        stream.set_no_fat_chain(false);
        // Growth always FAT-chains; bump both length fields.
        let new_dl = stream.data_length + added_bytes as u64;
        stream.data_length = new_dl;
        stream.valid_data_length = new_dl;
        stream.write_into(&mut set[stream_off..stream_off + EXFAT_ENTRY_SIZE]);
        set[0x02] = 0;
        set[0x03] = 0;
        let sum = entry_set_checksum(&set);
        set[0x02..0x04].copy_from_slice(&sum.to_le_bytes());
        self.write_set(loc, &set)
    }

    /// Locate `name`'s entry set in the given directory (case-insensitive
    /// via the upcase table). Straddling sets are found like any other —
    /// the scan runs over the concatenated directory stream.
    pub(super) fn locate_entry_set_in(
        &self,
        ext: &DirExtent,
        name: &str,
    ) -> FsResult<Option<SetLoc>, S::Error> {
        // A held extent goes stale after growth; re-derive from disk.
        let ext = &self.refreshed_extent(ext);
        let (_, buf) =
            self.read_dir_stream(ext.first_cluster, ext.no_fat_chain, ext.max_clusters)?;
        let mut pos = 0usize;
        while pos + EXFAT_ENTRY_SIZE <= buf.len() {
            let ty = buf[pos];
            if ty == END_OF_DIRECTORY {
                return Ok(None);
            }
            if ty == ENTRY_TYPE_FILE {
                let secondary_count = buf[pos + 1] as usize;
                let set_bytes = (1 + secondary_count) * EXFAT_ENTRY_SIZE;
                if pos + set_bytes > buf.len() {
                    // Truncated by the end of the allocation: malformed.
                    pos += EXFAT_ENTRY_SIZE;
                    continue;
                }
                let slice_buf = buf[pos..pos + set_bytes].to_vec();
                let mut decoder = DirEntryDecoder::new(slice_buf);
                match decoder.next() {
                    Some(entry) if self.names_equal(&entry.name, name) => {
                        return Ok(Some(SetLoc {
                            dir_first_cluster: ext.first_cluster,
                            dir_no_fat_chain: ext.no_fat_chain,
                            dir_max_clusters: ext.max_clusters,
                            logical: pos as u64,
                            len: u32::try_from(set_bytes)
                                .expect("entry set is at most 256 entries (8192 bytes)"),
                        }));
                    }
                    // Valid set, different name: skip the whole set.
                    Some(_) => pos += set_bytes,
                    // Corrupt set (bad checksum / malformed): advance one entry,
                    // matching DirEntryDecoder's skip width — trusting the
                    // claimed secondary_count here would overshoot a valid
                    // set the listing path yields (visible-but-unopenable).
                    None => pos += EXFAT_ENTRY_SIZE,
                }
                continue;
            }
            pos += EXFAT_ENTRY_SIZE;
        }
        Ok(None)
    }

    /// Free every cluster referenced by an entry's stream (`data_length`
    /// bytes from `first`). Saturating count: an out-of-range on-disk
    /// `data_length` must not truncate to a bogus (possibly zero) count.
    pub(super) fn free_entry_clusters(
        &self,
        first: u32,
        data_length: u64,
        no_fat_chain: bool,
    ) -> FsResult<(), S::Error> {
        if data_length == 0 || first < 2 {
            return Ok(());
        }
        let cluster_bytes = u64::from(self.boot.bytes_per_cluster());
        let cluster_count = u32::try_from(data_length.div_ceil(cluster_bytes)).unwrap_or(u32::MAX);
        self.free_cluster_chain(first, cluster_count, no_fat_chain)
    }

    /// Clear the `InUse` bit on every entry of `name`'s set, marking it
    /// deleted. Does **not** free clusters — `rename` relies on that to
    /// relocate an entry while its data stays in place.
    pub(super) fn unlink_entry_set_in(
        &self,
        parent: &DirExtent,
        name: &str,
    ) -> FsResult<(), S::Error> {
        let loc = self
            .locate_entry_set_in(parent, name)?
            .ok_or(FsError::NotFound)?;
        let mut entry_bytes = self.read_set(&loc)?;
        for i in 0..entry_bytes.len() / EXFAT_ENTRY_SIZE {
            entry_bytes[i * EXFAT_ENTRY_SIZE] &= !ENTRY_IN_USE;
        }
        self.write_set(&loc, &entry_bytes)
    }
}
