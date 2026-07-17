//! On-disk ExFAT directory-entry layouts (hand-coded fixed offsets).
//!
//! Every entry is exactly [`EXFAT_ENTRY_SIZE`] (32) bytes. Variable-length
//! names are a chain of [`ExfatFileNameEntry`] secondaries; the entry-set
//! The set checksum still lives outside these types (it spans the whole set).

use super::consts::{
    ENTRY_TYPE_FILE, ENTRY_TYPE_FILE_NAME, ENTRY_TYPE_STREAM_EXTENSION, EXFAT_ENTRY_SIZE,
    STREAM_FLAG_ALLOCATION_POSSIBLE, STREAM_FLAG_NO_FAT_CHAIN,
};
use crate::codec::{FixedCodec, bytes, le_u16, le_u32, le_u64, put_u16, put_u32, put_u64};

/// Primary File entry (`0x85`) — attributes and timestamps for a file or directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExfatFileEntry {
    pub entry_type: u8,
    pub secondary_count: u8,
    pub set_checksum: u16,
    pub attributes: u16,
    pub _reserved1: u16,
    pub create_timestamp: u32,
    pub modified_timestamp: u32,
    pub accessed_timestamp: u32,
    pub create_10ms: u8,
    pub modified_10ms: u8,
    pub create_utc_offset: u8,
    pub modified_utc_offset: u8,
    pub accessed_utc_offset: u8,
    pub _reserved2: [u8; 7],
}

impl ExfatFileEntry {
    pub(crate) const fn new_zeroed(secondary_count: u8, attributes: u16) -> Self {
        Self {
            entry_type: ENTRY_TYPE_FILE,
            secondary_count,
            set_checksum: 0,
            attributes,
            _reserved1: 0,
            create_timestamp: 0,
            modified_timestamp: 0,
            accessed_timestamp: 0,
            create_10ms: 0,
            modified_10ms: 0,
            create_utc_offset: 0,
            modified_utc_offset: 0,
            accessed_utc_offset: 0,
            _reserved2: [0; 7],
        }
    }

    /// Decode from a 32-byte slice, or `None` if the type/size is wrong.
    pub(crate) fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < EXFAT_ENTRY_SIZE || b[0] != ENTRY_TYPE_FILE {
            return None;
        }
        Some(Self {
            entry_type: b[0],
            secondary_count: b[1],
            set_checksum: le_u16(b, 2),
            attributes: le_u16(b, 4),
            _reserved1: le_u16(b, 6),
            create_timestamp: le_u32(b, 8),
            modified_timestamp: le_u32(b, 12),
            accessed_timestamp: le_u32(b, 16),
            create_10ms: b[20],
            modified_10ms: b[21],
            create_utc_offset: b[22],
            modified_utc_offset: b[23],
            accessed_utc_offset: b[24],
            _reserved2: bytes(b, 25),
        })
    }

    pub(crate) fn write_into(self, dst: &mut [u8]) {
        debug_assert!(dst.len() >= EXFAT_ENTRY_SIZE);
        let b = &mut dst[..EXFAT_ENTRY_SIZE];
        b[0] = self.entry_type;
        b[1] = self.secondary_count;
        put_u16(b, 2, self.set_checksum);
        put_u16(b, 4, self.attributes);
        put_u16(b, 6, self._reserved1);
        put_u32(b, 8, self.create_timestamp);
        put_u32(b, 12, self.modified_timestamp);
        put_u32(b, 16, self.accessed_timestamp);
        b[20] = self.create_10ms;
        b[21] = self.modified_10ms;
        b[22] = self.create_utc_offset;
        b[23] = self.modified_utc_offset;
        b[24] = self.accessed_utc_offset;
        b[25..32].copy_from_slice(&self._reserved2);
    }
}

/// Stream Extension secondary (`0xC0`) — allocation + name metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExfatStreamExtension {
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

    pub(crate) fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < EXFAT_ENTRY_SIZE || b[0] != ENTRY_TYPE_STREAM_EXTENSION {
            return None;
        }
        Some(Self {
            entry_type: b[0],
            general_secondary_flags: b[1],
            _reserved1: b[2],
            name_length: b[3],
            name_hash: le_u16(b, 4),
            _reserved2: le_u16(b, 6),
            valid_data_length: le_u64(b, 8),
            _reserved3: le_u32(b, 16),
            first_cluster: le_u32(b, 20),
            data_length: le_u64(b, 24),
        })
    }

    pub(crate) fn write_into(self, dst: &mut [u8]) {
        debug_assert!(dst.len() >= EXFAT_ENTRY_SIZE);
        let b = &mut dst[..EXFAT_ENTRY_SIZE];
        b[0] = self.entry_type;
        b[1] = self.general_secondary_flags;
        b[2] = self._reserved1;
        b[3] = self.name_length;
        put_u16(b, 4, self.name_hash);
        put_u16(b, 6, self._reserved2);
        put_u64(b, 8, self.valid_data_length);
        put_u32(b, 16, self._reserved3);
        put_u32(b, 20, self.first_cluster);
        put_u64(b, 24, self.data_length);
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExfatFileNameEntry {
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

    pub(crate) fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < EXFAT_ENTRY_SIZE || b[0] != ENTRY_TYPE_FILE_NAME {
            return None;
        }
        let mut name = [0u16; 15];
        for (i, unit) in name.iter_mut().enumerate() {
            *unit = le_u16(b, 2 + i * 2);
        }
        Some(Self {
            entry_type: b[0],
            general_secondary_flags: b[1],
            name,
        })
    }

    pub(crate) fn write_into(self, dst: &mut [u8]) {
        debug_assert!(dst.len() >= EXFAT_ENTRY_SIZE);
        let b = &mut dst[..EXFAT_ENTRY_SIZE];
        b[0] = self.entry_type;
        b[1] = self.general_secondary_flags;
        for (i, unit) in self.name.iter().enumerate() {
            put_u16(b, 2 + i * 2, *unit);
        }
    }
}

/// Allocation Bitmap (`0x81`) / Up-case Table (`0x82`) primary. The
/// checksum field at offset 4 is meaningful only for the Up-case Table
/// (`TableChecksum`); it is reserved on the bitmap entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExfatClusteredMetaEntry {
    pub entry_type: u8,
    pub _reserved1: [u8; 3],
    /// Up-case Table `TableChecksum` (offset 4); reserved on the bitmap.
    pub table_checksum: u32,
    pub _reserved2: [u8; 12],
    pub first_cluster: u32,
    pub data_length: u64,
}

impl ExfatClusteredMetaEntry {
    pub(crate) fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < EXFAT_ENTRY_SIZE {
            return None;
        }
        Some(Self {
            entry_type: b[0],
            _reserved1: bytes(b, 1),
            table_checksum: le_u32(b, 4),
            _reserved2: bytes(b, 8),
            first_cluster: le_u32(b, 20),
            data_length: le_u64(b, 24),
        })
    }
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

/// [`FixedCodec`] shims so the fuzz round-trip harness
/// ([`crate::fuzz::exfat_entries`]) can drive these layouts through the
/// generic fixed-point check.
macro_rules! fixed_codec_via_parse {
    ($ty:ty) => {
        impl FixedCodec for $ty {
            const SIZE: usize = EXFAT_ENTRY_SIZE;
            fn parse(bytes: &[u8]) -> Option<Self> {
                <$ty>::parse(bytes)
            }
            fn write_into(&self, out: &mut [u8]) {
                <$ty>::write_into(*self, out);
            }
        }
    };
}
fixed_codec_via_parse!(ExfatFileEntry);
fixed_codec_via_parse!(ExfatStreamExtension);
fixed_codec_via_parse!(ExfatFileNameEntry);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_entry_round_trips() {
        let e = ExfatFileEntry::new_zeroed(2, 0x20);
        let mut b = [0u8; EXFAT_ENTRY_SIZE];
        e.write_into(&mut b);
        assert_eq!(ExfatFileEntry::parse(&b), Some(e));
    }

    #[test]
    fn stream_entry_round_trips() {
        let e = ExfatStreamExtension::new(5, 7, 123, 123, true);
        let mut b = [0u8; EXFAT_ENTRY_SIZE];
        e.write_into(&mut b);
        assert_eq!(b[0], ENTRY_TYPE_STREAM_EXTENSION);
        assert!(e.no_fat_chain());
        let parsed = ExfatStreamExtension::parse(&b).unwrap();
        assert_eq!(parsed, e);
        assert_eq!(parsed.first_cluster, 7);
        assert_eq!(parsed.valid_data_length, 123);
    }

    #[test]
    fn file_name_entry_packs_ucs2_le() {
        let e = ExfatFileNameEntry::from_units(&[0x0041, 0x0042]);
        let mut b = [0u8; EXFAT_ENTRY_SIZE];
        e.write_into(&mut b);
        assert_eq!(&b[2..6], &[0x41, 0x00, 0x42, 0x00]);
        assert_eq!(ExfatFileNameEntry::parse(&b), Some(e));
    }

    #[test]
    fn fat_entry_word_round_trips() {
        let w = ExfatFatEntryWord(0x0ABC_DEF0);
        assert_eq!(ExfatFatEntryWord::parse(&w.encode()).0, 0x0ABC_DEF0);
    }
}
