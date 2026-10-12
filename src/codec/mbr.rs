//! MBR partition table layout: four primary partition records and the
//! `55 AA` signature.

use layline::Layout;

/// On-disk 16-byte primary partition table entry. The CHS addresses are
/// carried but unused (LBA only).
#[derive(Layout, Debug, Clone, Copy, PartialEq, Eq)]
#[layout(bytes = 16, endian = le)]
pub(crate) struct MbrPartitionRecord {
    pub(crate) status: u8,
    pub(crate) _chs_first: [u8; 3],
    pub(crate) type_byte: u8,
    pub(crate) _chs_last: [u8; 3],
    pub(crate) start_lba: u32,
    pub(crate) sector_count: u32,
}

/// Byte offset of [`MbrPartitionTable`] in sector 0; the boot code
/// before it is opaque and never decoded.
pub(crate) const PARTITION_TABLE_OFFSET: usize = 446;

/// The tail of sector 0 of an MBR-partitioned disk: four primary
/// partition records, then the boot signature. Decode fails unless the
/// signature is `55 AA` (little-endian `0xAA55`).
#[derive(Layout, Debug, Clone, PartialEq, Eq)]
#[layout(bytes = 66, endian = le)]
pub(crate) struct MbrPartitionTable {
    #[bytes(16)]
    pub(crate) partitions: [MbrPartitionRecord; 4],
    #[magic(0xAA55u16)]
    pub(crate) signature: u16,
}
