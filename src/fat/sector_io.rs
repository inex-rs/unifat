//! Byte-level cached I/O and FAT-cell access for [`FatVfs`].

use super::*;
use crate::codec::fat::fat_entry::decode_fat_cell;
use crate::error::*;
use embedded_io::*;

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Reject accesses outside the volume (a crafted chain/offset must
    /// not read or clobber neighbouring disk content).
    fn check_bounds(&self, offset: u64, len: usize) -> FsResult<(), S::Error> {
        let end = offset.saturating_add(len as u64);
        let volume_end = u64::from(self.props.total_sectors) * u64::from(self.props.sector_size);
        if end > volume_end {
            return Err(FsError::Corrupt(CorruptKind::ClusterChain));
        }
        Ok(())
    }

    /// Bounds-checked cached read of an arbitrary byte range.
    pub(crate) fn read_bytes(&self, offset: u64, buf: &mut [u8]) -> FsResult<(), S::Error> {
        self.check_bounds(offset, buf.len())?;
        self.sector_cache().read_at(offset, buf)
    }

    /// Bounds-checked cached write-back write of an arbitrary byte range.
    pub(crate) fn write_bytes(&self, offset: u64, buf: &[u8]) -> FsResult<(), S::Error> {
        self.check_bounds(offset, buf.len())?;
        self.sector_cache().write_at(offset, buf)
    }

    /// Bounds-checked uncached bulk read (file payload).
    pub(crate) fn read_bytes_through(&self, offset: u64, buf: &mut [u8]) -> FsResult<(), S::Error> {
        self.check_bounds(offset, buf.len())?;
        self.sector_cache().read_through(offset, buf)
    }

    /// Bounds-checked uncached bulk write (file payload).
    pub(crate) fn write_bytes_through(&self, offset: u64, buf: &[u8]) -> FsResult<(), S::Error> {
        self.check_bounds(offset, buf.len())?;
        self.sector_cache().write_through(offset, buf)
    }

    /// Byte offset of FAT copy `copy`'s first sector.
    fn fat_copy_base(&self, copy: u8) -> u64 {
        let sector = SectorIndex::from(self.props.first_fat_sector)
            .saturating_add(SectorCount::from(copy).saturating_mul(self.props.fat_sector_size));
        self.sector_byte_offset(sector)
    }

    /// The FAT copy reads target: copy 0 while mirroring is active, else
    /// the FAT32 `BPB_ExtFlags` ActiveFat copy (repair tools legitimately
    /// switch it — using copy 0 regardless reads stale chains).
    fn active_fat_copy(&self) -> u8 {
        match &self.boot_record.borrow().ebr {
            Ebr::Fat32(ebr, _) if ebr.extended_flags.mirroring_disabled => {
                ebr.extended_flags.active_fat
            }
            _ => 0,
        }
    }

    /// Whether FAT writes mirror to every copy (all volumes except FAT32
    /// with mirroring disabled, which writes the active copy only).
    fn fat_mirroring(&self) -> bool {
        !matches!(
            &self.boot_record.borrow().ebr,
            Ebr::Fat32(ebr, _) if ebr.extended_flags.mirroring_disabled
        )
    }

    /// Reject cluster indices outside the data-cluster range. Without this
    /// fence a corrupt on-disk `data_cluster` (e.g. an OS/2 EA handle in
    /// FstClusHI) would compute a cell offset past the FAT table — still
    /// inside the volume — and read/WRITE stray bytes in the data area.
    fn check_fat_index(&self, cluster: ClusterIndex) -> FsResult<(), S::Error> {
        if cluster < 2 || cluster > self.props.max_cluster {
            return Err(FsError::Corrupt(CorruptKind::ClusterChain));
        }
        Ok(())
    }

    /// Decode the FAT cell for `cluster` (from the active copy).
    pub(crate) fn read_fat(&self, cluster: ClusterIndex) -> FsResult<FatEntry, S::Error> {
        self.check_fat_index(cluster)?;
        let width = usize::from(self.fat_type.entry_size());
        let cell_off = u64::from(cluster) * width as u64;
        let mut raw = [0u8; 4];
        self.read_bytes(
            self.fat_copy_base(self.active_fat_copy()) + cell_off,
            &mut raw[..width],
        )?;
        Ok(decode_fat_cell(
            u32::from_le_bytes(raw),
            self.props.max_cluster,
            self.fat_type,
        ))
    }

    /// The next cluster in a chain, or `None` at end-of-chain (or on any
    /// non-continuation cell — chain validity is checked when a file opens).
    pub(crate) fn next_cluster(
        &self,
        cluster: ClusterIndex,
    ) -> FsResult<Option<ClusterIndex>, S::Error> {
        Ok(match self.read_fat(cluster)? {
            FatEntry::Next(next) => Some(next),
            _ => None,
        })
    }

    /// Store `value` into cluster `cluster`'s cell — in every FAT copy
    /// while mirroring is active, else in the active copy only.
    pub(crate) fn write_fat(
        &self,
        cluster: ClusterIndex,
        value: FatWrite,
    ) -> FsResult<(), S::Error> {
        self.check_fat_index(cluster)?;
        let mut cell = match value {
            FatWrite::Free => 0,
            FatWrite::End => self.fat_type.end_value(),
            FatWrite::Next(next) => next & self.fat_type.end_value(), // mask reserved FAT32 bits
        };
        let width = usize::from(self.fat_type.entry_size());
        let cell_off = u64::from(cluster) * width as u64;
        let active_base = self.fat_copy_base(self.active_fat_copy());
        if self.fat_type == FatType::FAT32 {
            // The top nibble of a FAT32 cell is reserved: preserve
            // whatever is there rather than zeroing it (fatgen103).
            // `end_value` doubles as the value mask (all payload bits).
            let mut raw = [0u8; 4];
            self.read_bytes(active_base + cell_off, &mut raw)?;
            cell |= u32::from_le_bytes(raw) & !self.fat_type.end_value();
        }
        let bytes = cell.to_le_bytes();
        if self.fat_mirroring() {
            for copy in 0..self.props.fat_table_count {
                self.write_bytes(self.fat_copy_base(copy) + cell_off, &bytes[..width])?;
            }
        } else {
            self.write_bytes(active_base + cell_off, &bytes[..width])?;
        }

        // Keep the allocator's search hint tight; freeing a low cluster may
        // expose a new minimum. The FAT32 free-cluster *count* is advisory,
        // so once the on-disk FAT diverges from it we mark it unknown rather
        // than maintain a running (and easily-wrong) tally.
        if matches!(value, FatWrite::Free) && cluster < *self.first_free_cluster.borrow() {
            *self.first_free_cluster.borrow_mut() = cluster;
        }
        if let Ebr::Fat32(_, fsinfo) = &mut self.boot_record.borrow_mut().ebr {
            fsinfo.free_cluster_count = ClusterIndex::MAX;
            if matches!(value, FatWrite::Free) && cluster < fsinfo.first_free_cluster {
                fsinfo.first_free_cluster = cluster;
            }
            self.fsinfo_modified.set(true);
        }
        Ok(())
    }

    /// Lowest free cluster at or after the search hint, or `None` when full.
    pub(crate) fn next_free_cluster(&self) -> FsResult<Option<ClusterIndex>, S::Error> {
        let mut cluster = {
            let hint = *self.first_free_cluster.borrow();
            match &self.boot_record.borrow().ebr {
                Ebr::Fat32(_, fsinfo)
                    if fsinfo.first_free_cluster != ClusterIndex::MAX
                        && fsinfo.first_free_cluster <= self.props.max_cluster =>
                {
                    hint.min(fsinfo.first_free_cluster)
                }
                _ => hint,
            }
        }
        .max(RESERVED_FAT_ENTRIES);

        while cluster <= self.props.max_cluster {
            if self.read_fat(cluster)? == FatEntry::Free {
                *self.first_free_cluster.borrow_mut() = cluster;
                if let Ebr::Fat32(_, fsinfo) = &mut self.boot_record.borrow_mut().ebr {
                    fsinfo.first_free_cluster = cluster;
                    self.fsinfo_modified.set(true);
                }
                return Ok(Some(cluster));
            }
            cluster += 1;
        }

        // Full: park the hint past the end so later scans start there.
        *self.first_free_cluster.borrow_mut() = self.props.max_cluster;
        Ok(None)
    }
}
