//! Create file/directory entry sets under a parent [`DirExtent`].

use alloc::vec;
use alloc::vec::Vec;

use embedded_io::{Read, Seek, Write};

use super::compose::{compose_file_entry_set, compose_file_entry_set_full};
use super::direntry::DirExtent;
use super::timestamp::EntryTimes;
use super::{ExfatVfs, FAT_EOC, cluster_to_byte_offset};
use crate::attrs::AttrBits;
use crate::error::{FsError, FsResult};
use crate::vfs::NewEntry;

impl<S> ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Write `data` into `cluster`, zero-padding the rest of the cluster.
    pub(super) fn write_cluster(&self, cluster: u32, data: &[u8]) -> FsResult<(), S::Error> {
        let cluster_bytes = self.boot.bytes_per_cluster() as usize;
        if data.len() > cluster_bytes {
            return Err(FsError::InvalidInput);
        }
        let offset = cluster_to_byte_offset(
            self.boot.cluster_heap_offset,
            cluster,
            self.boot.bytes_per_sector(),
            self.boot.bytes_per_cluster(),
        );
        self.write_at(offset, data)?;
        // Zero-pad so stale data from prior occupants doesn't leak.
        if data.len() < cluster_bytes {
            let pad = vec![0u8; cluster_bytes - data.len()];
            self.write_at(offset + data.len() as u64, &pad)?;
        }
        Ok(())
    }

    /// Insert a [`NewEntry`] under `parent`, honouring pre-allocated cluster,
    /// size, `no_fat_chain`, and `allocated_size` when provided.
    ///
    /// - **Dir, no cluster** → allocate one zeroed cluster.
    /// - **Dir/file with `first_cluster >= 2`** → stamp the entry set only
    ///   (caller already owns the allocation).
    /// - **File, zero cluster, empty size** → empty file entry (no clusters).
    /// - **File, zero cluster, non-zero size** → preallocate clusters with
    ///   `ValidDataLength = 0` (the range reads as zeros).
    pub(super) fn insert_new_entry_in_extent(
        &self,
        parent: DirExtent,
        entry: NewEntry,
    ) -> FsResult<(), S::Error> {
        // 255 UCS-2 units, not bytes — non-ASCII names are longer in UTF-8.
        if entry.name.is_empty() || entry.name.encode_utf16().count() > 255 {
            return Err(FsError::InvalidInput);
        }
        let entries = self.list_entries_in(&parent)?;
        if entries
            .iter()
            .any(|e| self.names_equal(&e.name, &entry.name))
        {
            return Err(FsError::AlreadyExists);
        }

        // Prefer caller stamps; fall back to the clock if all absent.
        let t = entry.times;
        let times = if t.created.is_none() && t.modified.is_none() && t.accessed.is_none() {
            EntryTimes::now(&*self.options.clock)
        } else {
            t
        };

        let mut attrs = AttrBits::from_attributes(entry.attrs, entry.is_dir);
        if entry.is_dir {
            attrs.set(AttrBits::DIRECTORY, true);
        } else if !attrs.contains(AttrBits::ARCHIVE) {
            // New files get the archive bit (Windows convention).
            attrs.set(AttrBits::ARCHIVE, true);
        }

        if entry.is_dir {
            let cluster_bytes = u64::from(self.boot.bytes_per_cluster());
            let (cluster, data_len, no_fat) = if entry.first_cluster >= 2 {
                let data_len = entry
                    .allocated_size
                    .unwrap_or_else(|| entry.size.max(cluster_bytes));
                (
                    entry.first_cluster,
                    data_len,
                    entry.no_fat_chain.unwrap_or(true),
                )
            } else {
                let cluster = self.find_free_cluster()?.ok_or(FsError::StorageFull)?;
                let zeros = vec![
                    0u8;
                    usize::try_from(cluster_bytes)
                        .expect("cluster size ≤ 32 MiB (shift-validated at mount)")
                ];
                self.write_cluster(cluster, &zeros)?;
                self.mark_cluster_allocated(cluster)?;
                // Allocate-before-link: cluster + bitmap durable before
                // the entry set referencing them becomes visible.
                self.sync()?;
                (cluster, cluster_bytes, entry.no_fat_chain.unwrap_or(true))
            };
            let entry_set = compose_file_entry_set(
                &entry.name,
                self.name_hash(&entry.name),
                attrs,
                cluster,
                data_len,
                no_fat,
                times,
            );
            self.append_entry_set_in(&parent, &entry_set)?;
            self.sync()?;
            return Ok(());
        }

        if entry.first_cluster >= 2 {
            let valid = entry.size;
            let allocated = entry.allocated_size.unwrap_or(valid);
            let no_fat = entry.no_fat_chain.unwrap_or(false);
            let entry_set = compose_file_entry_set_full(
                &entry.name,
                self.name_hash(&entry.name),
                attrs,
                entry.first_cluster,
                valid,
                allocated,
                no_fat,
                times,
            );
            self.append_entry_set_in(&parent, &entry_set)?;
            self.sync()?;
            return Ok(());
        }

        // No pre-allocated cluster: empty stamp or allocate for `size`.
        if entry.size == 0 {
            let entry_set = compose_file_entry_set(
                &entry.name,
                self.name_hash(&entry.name),
                attrs,
                0,
                0,
                entry.no_fat_chain.unwrap_or(false),
                times,
            );
            self.append_entry_set_in(&parent, &entry_set)?;
            self.sync()?;
            return Ok(());
        }

        // Non-zero size without a cluster: preallocate the extent and set
        // ValidDataLength = 0 — the whole range then reads as zeros with
        // no payload materialization.
        let cluster_bytes = u64::from(self.boot.bytes_per_cluster());
        let count =
            u32::try_from(entry.size.div_ceil(cluster_bytes)).map_err(|_| FsError::StorageFull)?;
        let (first, no_fat) = self.allocate_extent(count)?;
        let entry_set = compose_file_entry_set_full(
            &entry.name,
            self.name_hash(&entry.name),
            attrs,
            first,
            0,
            entry.size,
            no_fat,
            times,
        );
        self.append_entry_set_in(&parent, &entry_set)?;
        self.sync()?;
        Ok(())
    }

    /// Allocate `count` clusters: a contiguous run when available
    /// (`NoFatChain`), else scattered clusters FAT-chained together.
    /// Returns `(first_cluster, no_fat_chain)`.
    fn allocate_extent(&self, count: u32) -> FsResult<(u32, bool), S::Error> {
        if count == 0 {
            return Ok((0, false));
        }
        if let Some(start) = self.find_free_contiguous_run(count)? {
            self.mark_cluster_range_allocated(start, count)?;
            return Ok((start, true));
        }
        let mut chain: Vec<u32> = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let c = self.find_free_cluster()?.ok_or(FsError::StorageFull)?;
            self.mark_cluster_allocated(c)?;
            chain.push(c);
        }
        for i in 0..chain.len() - 1 {
            self.write_fat_entry(chain[i], chain[i + 1])?;
        }
        self.write_fat_entry(*chain.last().expect("count >= 1"), FAT_EOC)?;
        Ok((chain[0], false))
    }
}
