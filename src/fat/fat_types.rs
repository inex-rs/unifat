//! FAT geometry conversions and cluster-map value types for [`FatVfs`].

use super::*;
use embedded_io::{Read, Seek, Write};

pub(crate) use crate::codec::fat::fat_entry::FatEntry;

/// Cells 0 and 1 of the FAT are reserved; data clusters count from 2.
pub(crate) const RESERVED_FAT_ENTRIES: ClusterIndex = 2;

/// A value to store into a FAT cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FatWrite {
    /// Release the cell.
    Free,
    /// Terminate the chain at this cell.
    End,
    /// Continue the chain at `cluster`.
    Next(ClusterIndex),
}

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    #[inline]
    pub(crate) fn cluster_size(&self) -> u32 {
        self.props.cluster_size
    }

    #[inline]
    pub(crate) fn sectors_per_cluster(&self) -> u8 {
        self.props.sec_per_clus
    }

    /// Byte offset of a partition sector from the start of the medium.
    #[inline]
    pub(crate) fn sector_byte_offset(&self, sector: SectorIndex) -> u64 {
        u64::from(sector) * u64::from(self.props.sector_size)
    }

    /// The partition sector where data cluster `cluster` begins.
    ///
    /// Saturating throughout: an out-of-range cluster from a corrupt entry
    /// (or a FAT32 root cluster of 0–1) resolves to an in-bounds sector
    /// rather than overflowing — the subsequent read is EOF-fenced.
    #[inline]
    pub(crate) fn cluster_first_sector(&self, cluster: ClusterIndex) -> SectorIndex {
        cluster
            .saturating_sub(RESERVED_FAT_ENTRIES)
            .saturating_mul(ClusterIndex::from(self.props.sec_per_clus))
            .saturating_add(self.props.first_data_sector)
    }
}
