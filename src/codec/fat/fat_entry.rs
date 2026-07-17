//! FAT table slot decoding — a raw cell value to its meaning.

use crate::codec::fat::types::{ClusterCount, ClusterIndex, FatType};

/// The meaning of one File-Allocation-Table cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FatEntry {
    /// Unallocated and available.
    Free,
    /// In use; the chain continues at the given cluster.
    Next(ClusterIndex),
    /// In use and terminal — the last cluster of its chain.
    End,
    /// Marked defective.
    Bad,
    /// Reserved or out of range; traversal treats it as a chain end.
    Reserved,
}

impl FatType {
    /// Low bits of a cell that carry the cluster value (the top nibble of a
    /// FAT32 cell is reserved and ignored).
    #[inline]
    const fn value_mask(self) -> u32 {
        match self {
            FatType::FAT16 => 0x0000_FFFF,
            FatType::FAT32 => 0x0FFF_FFFF,
        }
    }

    /// The defective-cluster marker for this width.
    #[inline]
    const fn bad_marker(self) -> u32 {
        self.value_mask() - 8
    }

    /// Smallest value that denotes end-of-chain (`…F8` … `…FF`).
    #[inline]
    const fn end_floor(self) -> u32 {
        self.value_mask() - 7
    }

    /// The end-of-chain value written for a chain's final cluster.
    #[inline]
    pub(crate) const fn end_value(self) -> u32 {
        self.value_mask()
    }
}

/// Decode a raw FAT cell. `max_cluster` is the highest valid data-cluster
/// index (`CountofClusters + 1`); references are fenced to `2..=max_cluster`.
#[inline]
pub(crate) fn decode_fat_cell(raw: u32, max_cluster: ClusterCount, fat_type: FatType) -> FatEntry {
    let cell = raw & fat_type.value_mask();
    if cell == 0 {
        FatEntry::Free
    } else if cell == fat_type.bad_marker() {
        FatEntry::Bad
    } else if cell >= fat_type.end_floor() {
        FatEntry::End
    } else if (2..=max_cluster).contains(&cell) {
        FatEntry::Next(cell)
    } else {
        FatEntry::Reserved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat16_cells_decode() {
        let n = 100;
        assert_eq!(decode_fat_cell(0x0000, n, FatType::FAT16), FatEntry::Free);
        assert_eq!(
            decode_fat_cell(0x0002, n, FatType::FAT16),
            FatEntry::Next(2)
        );
        assert_eq!(
            decode_fat_cell(0x0064, n, FatType::FAT16),
            FatEntry::Next(100)
        );
        assert_eq!(decode_fat_cell(0xFFF7, n, FatType::FAT16), FatEntry::Bad);
        assert_eq!(decode_fat_cell(0xFFF8, n, FatType::FAT16), FatEntry::End);
        assert_eq!(decode_fat_cell(0xFFFF, n, FatType::FAT16), FatEntry::End);
        // Past the heap → reserved, not a follow.
        assert_eq!(
            decode_fat_cell(0x0065, n, FatType::FAT16),
            FatEntry::Reserved
        );
    }

    #[test]
    fn fat32_ignores_reserved_top_nibble() {
        let n = 1_000_000;
        // Top nibble set must not change the classification.
        assert_eq!(
            decode_fat_cell(0xF000_0002, n, FatType::FAT32),
            FatEntry::Next(2)
        );
        assert_eq!(
            decode_fat_cell(0x0FFF_FFFF, n, FatType::FAT32),
            FatEntry::End
        );
        assert_eq!(
            decode_fat_cell(0xFFFF_FFF8, n, FatType::FAT32),
            FatEntry::End
        );
        assert_eq!(
            decode_fat_cell(0x0FFF_FFF7, n, FatType::FAT32),
            FatEntry::Bad
        );
        assert_eq!(
            decode_fat_cell(0x0000_0000, n, FatType::FAT32),
            FatEntry::Free
        );
    }

    #[test]
    fn end_value_round_trips_to_end() {
        for ty in [FatType::FAT16, FatType::FAT32] {
            assert_eq!(decode_fat_cell(ty.end_value(), 50, ty), FatEntry::End);
        }
    }
}
