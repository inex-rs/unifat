//! Index/count type aliases for on-disk layouts.

pub(crate) type ClusterIndex = u32;
pub(crate) type ClusterCount = ClusterIndex;

pub(crate) type SectorIndex = u32;
pub(crate) type SectorCount = SectorIndex;

pub(crate) type EntryIndex = u16;
pub(crate) type EntryCount = EntryIndex;

pub(crate) type FileSize = u32;

/// FAT width of a volume; FAT12 is rejected at mount.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FatType {
    /// 16-bit entries. Volume size ~8 MB – 16 GB.
    FAT16,
    /// 32-bit entries. Volume size ~256 MB – 16 TB.
    FAT32,
}

impl FatType {
    #[inline]
    /// How many bytes each FAT entry occupies on disk.
    pub(crate) fn entry_size(&self) -> u8 {
        match self {
            FatType::FAT16 => 2,
            FatType::FAT32 => 4,
        }
    }
}
