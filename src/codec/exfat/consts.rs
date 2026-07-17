//! ExFAT on-disk constants (pure; no I/O).

/// Size of every ExFAT directory entry; an entry set is a variable
/// number of consecutive 32-byte entries.
pub(crate) const EXFAT_ENTRY_SIZE: usize = 32;

/// Entry-type discriminator for the Allocation Bitmap primary entry.
pub(crate) const ENTRY_TYPE_ALLOCATION_BITMAP: u8 = 0x81;

/// Entry-type discriminator for the Up-case Table primary entry.
pub(crate) const ENTRY_TYPE_UPCASE_TABLE: u8 = 0x82;

/// Entry-type discriminator for the File primary entry.
pub(crate) const ENTRY_TYPE_FILE: u8 = 0x85;

/// Entry-type discriminator for the Stream Extension secondary entry.
pub(crate) const ENTRY_TYPE_STREAM_EXTENSION: u8 = 0xC0;

/// Entry-type discriminator for the File Name secondary entry.
pub(crate) const ENTRY_TYPE_FILE_NAME: u8 = 0xC1;

/// Directory terminator: first byte of an unused end marker.
pub(crate) const END_OF_DIRECTORY: u8 = 0x00;

/// In-use bit on entry type bytes (cleared when unlinking).
pub(crate) const ENTRY_IN_USE: u8 = 0x80;

/// End-of-chain marker in the ExFAT FAT.
pub(crate) const FAT_EOC: u32 = 0xFFFF_FFFF;

/// Bad-cluster marker in the ExFAT FAT.
pub(crate) const FAT_BAD: u32 = 0xFFFF_FFF7;

/// Stream Extension: allocation is possible / clusters allocated.
pub(crate) const STREAM_FLAG_ALLOCATION_POSSIBLE: u8 = 0x01;

/// Stream Extension: contiguous cluster run (no FAT walk needed).
pub(crate) const STREAM_FLAG_NO_FAT_CHAIN: u8 = 0x02;
