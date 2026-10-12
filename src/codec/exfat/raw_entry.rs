//! On-disk ExFAT directory-entry layouts.
//!
//! Every entry is exactly [`EXFAT_ENTRY_SIZE`] (32) bytes. File entries
//! are decoded as a whole [`super::entry_set::ExfatEntrySet`]; the
//! secondaries it nests live here.

use layline::Layout;

use super::consts::{
    ENTRY_TYPE_FILE_NAME, ENTRY_TYPE_STREAM_EXTENSION, EXFAT_ENTRY_SIZE,
    STREAM_FLAG_ALLOCATION_POSSIBLE, STREAM_FLAG_NO_FAT_CHAIN,
};
use crate::codec::window;

/// The Stream Extension of an on-disk entry set, read without verifying
/// the set (refresh paths only need its length/allocation fields).
pub(crate) fn stream_of(set: &[u8]) -> Option<ExfatStreamExtension> {
    ExfatStreamExtension::decode(window(set.get(EXFAT_ENTRY_SIZE..)?)?).ok()
}

/// Stream Extension secondary (`0xC0`) — allocation + name metadata.
#[derive(Layout, Debug, Clone, Copy, PartialEq, Eq)]
#[layout(bytes = 32, endian = le)]
pub(crate) struct ExfatStreamExtension {
    #[magic(0xC0u8)]
    pub entry_type: u8,
    pub general_secondary_flags: u8,
    pub _reserved1: u8,
    pub name_length: u8,
    pub name_hash: u16,
    pub _reserved2: u16,
    pub valid_data_length: u64,
    pub _reserved3: u32,
    pub first_cluster: u32,
    pub data_length: u64,
}

impl ExfatStreamExtension {
    pub(crate) fn new(
        name_length: u8,
        first_cluster: u32,
        valid_data_length: u64,
        data_length: u64,
        no_fat_chain: bool,
    ) -> Self {
        let mut flags = STREAM_FLAG_ALLOCATION_POSSIBLE;
        if no_fat_chain {
            flags |= STREAM_FLAG_NO_FAT_CHAIN;
        }
        Self {
            entry_type: ENTRY_TYPE_STREAM_EXTENSION,
            general_secondary_flags: flags,
            _reserved1: 0,
            name_length,
            name_hash: 0,
            _reserved2: 0,
            valid_data_length,
            _reserved3: 0,
            first_cluster,
            data_length,
        }
    }

    #[inline]
    pub(crate) fn no_fat_chain(self) -> bool {
        self.general_secondary_flags & STREAM_FLAG_NO_FAT_CHAIN != 0
    }

    pub(crate) fn set_no_fat_chain(&mut self, on: bool) {
        if on {
            self.general_secondary_flags |= STREAM_FLAG_NO_FAT_CHAIN;
        } else {
            self.general_secondary_flags &= !STREAM_FLAG_NO_FAT_CHAIN;
        }
    }
}

/// File Name secondary (`0xC1`) — up to 15 UCS-2 units.
#[derive(Layout, Debug, Clone, Copy, PartialEq, Eq)]
#[layout(bytes = 32, endian = le)]
pub(crate) struct ExfatFileNameEntry {
    #[magic(0xC1u8)]
    pub entry_type: u8,
    pub general_secondary_flags: u8,
    pub name: [u16; 15],
}

impl ExfatFileNameEntry {
    pub(crate) fn from_units(units: &[u16]) -> Self {
        let mut name = [0u16; 15];
        let n = units.len().min(15);
        name[..n].copy_from_slice(&units[..n]);
        Self {
            entry_type: ENTRY_TYPE_FILE_NAME,
            general_secondary_flags: 0,
            name,
        }
    }
}

/// Allocation Bitmap (`0x81`) / Up-case Table (`0x82`) primary. The
/// checksum field at offset 4 is meaningful only for the Up-case Table
/// (`TableChecksum`); it is reserved on the bitmap entry.
#[derive(Layout, Debug, Clone, Copy, PartialEq, Eq)]
#[layout(bytes = 32, endian = le)]
pub(crate) struct ExfatClusteredMetaEntry {
    pub entry_type: u8,
    pub _reserved1: [u8; 3],
    /// Up-case Table `TableChecksum` (offset 4); reserved on the bitmap.
    pub table_checksum: u32,
    pub _reserved2: [u8; 12],
    pub first_cluster: u32,
    pub data_length: u64,
}

/// One little-endian FAT entry word (4 bytes) on an ExFAT volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub(crate) struct ExfatFatEntryWord(pub u32);

impl ExfatFatEntryWord {
    pub(crate) fn parse(bytes: &[u8; 4]) -> Self {
        Self(u32::from_le_bytes(*bytes))
    }

    pub(crate) fn encode(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_entry_round_trips() {
        let e = ExfatStreamExtension::new(5, 7, 123, 123, true);
        let b = e.encode();
        assert_eq!(b[0], ENTRY_TYPE_STREAM_EXTENSION);
        assert!(e.no_fat_chain());
        let parsed = ExfatStreamExtension::decode(&b).unwrap();
        assert_eq!(parsed, e);
        assert_eq!(parsed.first_cluster, 7);
        assert_eq!(parsed.valid_data_length, 123);
    }

    #[test]
    fn file_name_entry_packs_ucs2_le() {
        let e = ExfatFileNameEntry::from_units(&[0x0041, 0x0042]);
        let b = e.encode();
        assert_eq!(&b[2..6], &[0x41, 0x00, 0x42, 0x00]);
        assert_eq!(ExfatFileNameEntry::decode(&b), Ok(e));
    }

    #[test]
    fn fat_entry_word_round_trips() {
        let w = ExfatFatEntryWord(0x0ABC_DEF0);
        assert_eq!(ExfatFatEntryWord::parse(&w.encode()).0, 0x0ABC_DEF0);
    }
}
