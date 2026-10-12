//! The exFAT File entry set: the File primary (`0x85`), its Stream
//! Extension, then a chain of File Name secondaries. The `SetChecksum`
//! covers the whole set, so the set (not each entry) is the decoding unit.

use alloc::vec::Vec;

use layline::{Checksum, Message};

use super::consts::ENTRY_TYPE_FILE;
use super::raw_entry::{ExfatFileNameEntry, ExfatStreamExtension};

/// The exFAT `SetChecksum`: rotate right by one, then add each byte.
#[derive(Debug)]
pub(crate) struct SetChecksum;

impl Checksum for SetChecksum {
    type Output = u16;
    type State = u16;

    fn init() -> u16 {
        0
    }

    fn update(sum: u16, run: &[u8]) -> u16 {
        run.iter().fold(sum, |sum, &b| {
            sum.rotate_right(1).wrapping_add(u16::from(b))
        })
    }

    fn finish(sum: u16) -> u16 {
        sum
    }
}

/// A File entry set (`0x85` primary + Stream Extension + File Names).
///
/// Decode checks the entry types, that `SecondaryCount` covers at least
/// the Stream Extension, and the `SetChecksum`; encode restamps
/// `SecondaryCount` from `names` and recomputes the checksum, so a
/// patched set is always written back consistent.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
#[message(endian = le)]
pub(crate) struct ExfatEntrySet {
    #[magic(0x85u8)]
    pub entry_type: u8,
    pub secondary_count: u8,
    #[checksum(SetChecksum, over = ..=names)]
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
    /// The Stream Extension is always the first secondary.
    #[bytes(32)]
    pub stream: ExfatStreamExtension,
    /// File Name secondaries hold 15 UCS-2 units each.
    #[count(secondary_count, offset = -1)]
    #[bytes(32)]
    pub names: Vec<ExfatFileNameEntry>,
}

impl ExfatEntrySet {
    /// A set with zeroed timestamps carrying `name` (at most 255 UCS-2
    /// units, split across as many File Name entries as it needs; an
    /// empty name still gets one).
    pub(crate) fn new(name: &[u16], attributes: u16, stream: ExfatStreamExtension) -> Self {
        let mut names: Vec<ExfatFileNameEntry> = name
            .chunks(15)
            .map(ExfatFileNameEntry::from_units)
            .collect();
        if names.is_empty() {
            names.push(ExfatFileNameEntry::from_units(&[]));
        }
        Self {
            entry_type: ENTRY_TYPE_FILE,
            secondary_count: 0, // restamped on encode
            set_checksum: 0,    // computed on encode
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
            stream,
            names,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::exfat::consts::{ENTRY_TYPE_FILE_NAME, EXFAT_ENTRY_SIZE};

    /// The spec's `EntrySetChecksum` loop (exFAT §6.3.3), written out
    /// independently of [`SetChecksum`]: every byte except 2 and 3.
    fn spec_set_checksum(set: &[u8]) -> u16 {
        let mut sum: u16 = 0;
        for (i, &b) in set.iter().enumerate() {
            if i == 2 || i == 3 {
                continue;
            }
            sum = (if sum & 1 != 0 { 0x8000 } else { 0 }) + (sum >> 1) + u16::from(b);
        }
        sum
    }

    fn sample_set(name: &str) -> ExfatEntrySet {
        let units: Vec<u16> = name.encode_utf16().collect();
        let stream =
            ExfatStreamExtension::new(u8::try_from(units.len()).unwrap(), 7, 123, 123, true);
        ExfatEntrySet::new(&units, 0x20, stream)
    }

    #[test]
    fn entry_set_encode_stamps_count_and_spec_checksum() {
        let set = sample_set("a-very-long-filename.bin"); // 24 units → 2 names
        let wire = set.encode();
        assert_eq!(wire.len(), 4 * EXFAT_ENTRY_SIZE);
        assert_eq!(wire[1], 3, "SecondaryCount = Stream + 2 File Names");
        assert_eq!(
            u16::from_le_bytes([wire[2], wire[3]]),
            spec_set_checksum(&wire)
        );
        let (back, used) = ExfatEntrySet::decode(&wire).unwrap();
        assert_eq!(used, wire.len());
        assert_eq!(back.encode(), wire);
    }

    #[test]
    fn entry_set_rejects_bad_checksum_type_and_count() {
        let wire = sample_set("x").encode();
        let mut bad = wire.clone();
        bad[4] ^= 0xFF;
        assert!(ExfatEntrySet::decode(&bad).is_err(), "checksum");
        let mut bad = wire.clone();
        bad[EXFAT_ENTRY_SIZE] = ENTRY_TYPE_FILE_NAME;
        assert!(ExfatEntrySet::decode(&bad).is_err(), "stream type");
        let mut bad = wire.clone();
        bad[1] = 0;
        assert!(ExfatEntrySet::decode(&bad).is_err(), "no stream secondary");
        assert!(
            ExfatEntrySet::decode(&wire[..wire.len() - 1]).is_err(),
            "short"
        );
    }
}
