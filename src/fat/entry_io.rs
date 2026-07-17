//! FAT cluster allocation and directory entry-set mutation for [`FatVfs`].

use alloc::boxed::Box;

use super::*;
use crate::codec::FixedCodec;
use crate::error::*;
use crate::vfs::EntryPatch;
use core::num::NonZero;
use embedded_io::*;
use time::PrimitiveDateTime;

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Allocate `n` clusters, returning them in chain order. When `link_from`
    /// is given, its cell is pointed at the new run so an existing chain is
    /// extended. The run is chained but **not necessarily contiguous** —
    /// callers must address the returned clusters individually, never
    /// `first + i`. On any mid-run failure every claimed cluster is freed
    /// and `link_from` is restored to end-of-chain, so nothing leaks and no
    /// half-linked garbage stays reachable.
    pub(crate) fn allocate_clusters(
        &self,
        n: NonZero<ClusterCount>,
        link_from: Option<ClusterIndex>,
    ) -> FsResult<alloc::vec::Vec<ClusterIndex>, S::Error> {
        let mut claimed: alloc::vec::Vec<ClusterIndex> = alloc::vec::Vec::new();
        let mut prev = link_from;

        let result = (|| {
            for _ in 0..n.get() {
                let cluster = self.next_free_cluster()?.ok_or(FsError::StorageFull)?;
                // Claim it immediately (End) so the next scan can't hand it
                // out again.
                self.write_fat(cluster, FatWrite::End)?;
                if let Some(prev) = prev {
                    self.write_fat(prev, FatWrite::Next(cluster))?;
                }
                claimed.push(cluster);
                prev = Some(cluster);
            }
            Ok(())
        })();

        if let Err(e) = result {
            // Unwind: free the partial run and unhook it from the chain.
            for &cluster in &claimed {
                let _ = self.write_fat(cluster, FatWrite::Free);
            }
            if let Some(link_from) = link_from
                && !claimed.is_empty()
            {
                let _ = self.write_fat(link_from, FatWrite::End);
            }
            return Err(e);
        }

        Ok(claimed)
    }

    /// Free every cluster of the chain starting at `first_cluster`.
    pub(crate) fn free_cluster_chain(&self, first_cluster: ClusterIndex) -> FsResult<(), S::Error> {
        let mut cluster = first_cluster;
        loop {
            // Read the link before freeing overwrites it.
            let next = self.next_cluster(cluster)?;
            self.write_fat(cluster, FatWrite::Free)?;
            match next {
                Some(next) => cluster = next,
                None => break,
            }
        }
        Ok(())
    }

    /// Zero every sector of `cluster` (required for a fresh directory cluster).
    fn zero_cluster(&self, cluster: ClusterIndex) -> FsResult<(), S::Error> {
        let zeros = alloc::vec![0u8; usize::from(self.props.sector_size)];
        let first = self.cluster_first_sector(cluster);
        for sector in first..first + SectorCount::from(self.sectors_per_cluster()) {
            self.write_bytes(self.sector_byte_offset(sector), &zeros)?;
        }
        Ok(())
    }

    /// Slots per cluster (directories are cluster-granular).
    fn slots_per_cluster(&self) -> EntryCount {
        let slot_size = u32::try_from(DIRENTRY_SIZE).expect("32 fits u32");
        EntryCount::try_from(self.props.cluster_size / slot_size).unwrap_or(EntryCount::MAX)
    }

    /// Locate a run of `needed` consecutive free slots in `dir`, growing a
    /// cluster-backed directory (and zeroing the new clusters) when the tail
    /// run is too short. A full fixed root yields [`FsError::RootDirectoryFull`].
    fn reserve_slots(
        &self,
        dir: FatDir,
        needed: NonZero<EntryCount>,
    ) -> FsResult<SlotPos, S::Error> {
        let needed = needed.get();
        let mut pos = self.dir_first_slot(dir);
        let mut run_start: Option<SlotPos> = None;
        let mut run_len: EntryCount = 0;
        let mut examined = 0u32;

        loop {
            // A directory can't hold more than u16::MAX slots; bail out well
            // before that (also fences a crafted cyclic chain).
            if examined >= u32::from(EntryCount::MAX) {
                return Err(FsError::StorageFull);
            }
            examined += 1;

            let free = matches!(self.read_slot(pos)?[0], SLOT_END | SLOT_DELETED);
            if free {
                run_start.get_or_insert(pos);
                run_len = run_len.saturating_add(1);
                if run_len >= needed {
                    return Ok(run_start.expect("set on the first free slot"));
                }
            } else {
                run_start = None;
                run_len = 0;
            }

            match self.next_slot(pos)? {
                Some(next) => pos = next,
                None => break,
            }
        }

        // Ran off the end without a long-enough run.
        match dir {
            FatDir::FixedRoot => Err(FsError::RootDirectoryFull),
            FatDir::Clusters(_) => {
                let last_cluster = pos
                    .cluster()
                    .ok_or(FsError::Corrupt(CorruptKind::DirEntry))?;
                let missing = needed - run_len;
                let per_cluster = self.slots_per_cluster().max(1);
                // `missing >= 1`, so `extra >= 1`.
                let extra = missing.div_ceil(per_cluster);
                let extra =
                    NonZero::new(ClusterCount::from(extra)).unwrap_or(NonZero::<ClusterCount>::MIN);
                let new = self.allocate_clusters(extra, Some(last_cluster))?;
                // Zero the clusters actually allocated — the run is chained,
                // not contiguous, so `first + i` could hit a foreign cluster.
                for &cluster in &new {
                    self.zero_cluster(cluster)?;
                }
                // Allocate-before-link: the extended, zeroed chain must be
                // durable before entries written into it become visible.
                self.metadata_barrier()?;
                // The run continues into the new clusters; if there was no tail
                // run, it starts at the first new cluster's first slot.
                Ok(run_start.unwrap_or_else(|| self.dir_first_slot(FatDir::Clusters(new[0]))))
            }
        }
    }

    /// Write `slots` sequentially starting at `start`, following the directory
    /// across sector/cluster boundaries.
    fn write_slot_run(
        &self,
        start: SlotPos,
        slots: &[[u8; DIRENTRY_SIZE]],
    ) -> FsResult<(), S::Error> {
        let mut pos = start;
        for (i, bytes) in slots.iter().enumerate() {
            self.write_slot(pos, bytes)?;
            if i + 1 < slots.len() {
                pos = self
                    .next_slot(pos)?
                    .ok_or(CorruptKind::DirEntry)
                    .map_err(FsError::Corrupt)?;
            }
        }
        Ok(())
    }

    /// Insert one entry into `dir`, returning the slot set it occupies.
    pub(crate) fn insert_entry(
        &self,
        props: &MinProperties,
        dir: FatDir,
    ) -> FsResult<SlotChain, S::Error> {
        let slots = compose_entry(props);
        let len = EntryCount::try_from(slots.len()).unwrap_or(EntryCount::MAX);
        let needed = NonZero::new(len).ok_or(FsError::Corrupt(CorruptKind::DirEntry))?;
        let first = self.reserve_slots(dir, needed)?;
        self.write_slot_run(first, &slots)?;
        Ok(SlotChain { first, len })
    }

    /// Mark every slot of `chain` free (does not touch data clusters).
    pub(crate) fn remove_entry_set(&self, chain: &SlotChain) -> FsResult<(), S::Error> {
        let mut pos = chain.first;
        for i in 0..chain.len {
            self.free_slot(pos, false)?;
            if i + 1 < chain.len {
                match self.next_slot(pos)? {
                    Some(next) => pos = next,
                    None => break,
                }
            }
        }
        Ok(())
    }

    /// Cluster value for a `..` entry pointing at `parent`. Per spec it
    /// is 0 whenever the parent is the root — including FAT32, whose
    /// root is an ordinary cluster chain.
    pub(crate) fn parent_link_cluster(&self, parent: FatDir) -> ClusterIndex {
        match parent {
            FatDir::FixedRoot => 0,
            FatDir::Clusters(c) => match &self.boot_record.borrow().ebr {
                Ebr::Fat32(ebr, _) if ebr.root_cluster == c => 0,
                _ => c,
            },
        }
    }

    /// Create a sub-directory cluster seeded with `.` and `..`. Per spec the
    /// two entries share the parent's timestamps; `parent` supplies `..`'s
    /// cluster (0 when the parent is the root, on FAT16 and FAT32 alike).
    pub(crate) fn create_dir_cluster(
        &self,
        parent: FatDir,
        datetime: PrimitiveDateTime,
    ) -> FsResult<ClusterIndex, S::Error> {
        let dir_cluster = self.allocate_clusters(NonZero::new(1).expect("1"), None)?[0];
        self.zero_cluster(dir_cluster)?;

        let parent_cluster = self.parent_link_cluster(parent);

        let dot = self.dot_entry(CURRENT_DIR_SFN, dir_cluster, datetime);
        let dotdot = self.dot_entry(PARENT_DIR_SFN, parent_cluster, datetime);

        let start = self.dir_first_slot(FatDir::Clusters(dir_cluster));
        self.write_slot_run(start, &[dot, dotdot])?;
        Ok(dir_cluster)
    }

    /// Encode a `.`/`..` SFN slot (no LFN — these always fit 8.3).
    fn dot_entry(
        &self,
        sfn: Sfn,
        cluster: ClusterIndex,
        datetime: PrimitiveDateTime,
    ) -> [u8; DIRENTRY_SIZE] {
        let fat32 = self.fat_type == crate::fat::FatType::FAT32;
        let props = MinProperties {
            name: Box::from(""),
            sfn,
            attributes: RawAttributes::DIRECTORY,
            created: Some(datetime),
            modified: datetime,
            accessed: Some(datetime.date()),
            file_size: 0,
            data_cluster: cluster,
            nt_res: 0,
            ea_handle: (!fat32).then_some(0),
        };
        let mut bytes = [0u8; DIRENTRY_SIZE];
        RawDirEntry::from(props).write_into(&mut bytes);
        bytes
    }

    /// Sync the [`Fat32FsInfo`] back to the medium (FAT32 only).
    pub(crate) fn sync_fsinfo(&self) -> FsResult<(), S::Error> {
        if !self.fsinfo_modified.get() {
            return Ok(());
        }
        if let Ebr::Fat32(ebr_fat32, fsinfo) = &self.boot_record.borrow().ebr {
            let mut bytes = [0u8; FSINFO_SIZE];
            fsinfo.write_into(&mut bytes);
            self.write_bytes(
                self.sector_byte_offset(SectorIndex::from(ebr_fat32.fat_info)),
                &bytes,
            )?;
        }
        self.fsinfo_modified.set(false);
        Ok(())
    }

    /// Apply `patch` to `props` and rewrite the set's SFN record (the
    /// last slot of the LFN+SFN chain). FAT has no `ValidDataLength` or
    /// `NoFatChain`; those exFAT-only patch fields are ignored.
    pub(crate) fn patch_sfn_record(
        &self,
        chain: SlotChain,
        props: &mut MinProperties,
        is_dir: bool,
        patch: EntryPatch,
    ) -> FsResult<(), S::Error> {
        let EntryPatch {
            size,
            first_cluster,
            times,
            attrs,
            valid_size: _,
            no_fat_chain: _,
        } = patch;
        if let Some(size) = size {
            props.file_size = u32::try_from(size).unwrap_or(u32::MAX);
        }
        if let Some(cluster) = first_cluster {
            props.data_cluster = cluster;
        }
        if let Some(times) = times {
            if let Some(c) = times.created {
                props.created = Some(c);
            }
            if let Some(m) = times.modified {
                props.modified = m;
            }
            if let Some(a) = times.accessed {
                props.accessed = Some(a);
            }
        }
        if let Some(attrs) = attrs {
            props.attributes = RawAttributes::from_attributes(attrs, is_dir);
        }

        let mut bytes = [0u8; DIRENTRY_SIZE];
        RawDirEntry::from(props.clone()).write_into(&mut bytes);

        // The SFN is the last slot of the LFN+SFN set.
        let sfn_pos = self
            .nth_slot(chain.first, chain.len.saturating_sub(1))?
            .ok_or(FsError::Corrupt(CorruptKind::DirEntry))?;
        self.write_slot(sfn_pos, &bytes)
    }
}
