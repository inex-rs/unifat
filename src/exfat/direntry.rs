use alloc::string::String;
use alloc::vec::Vec;

use time::{Date, PrimitiveDateTime};

use crate::attrs::AttrBits;

use super::timestamp::{decode_date, decode_datetime};
use super::{END_OF_DIRECTORY, ENTRY_IN_USE, ENTRY_TYPE_FILE, EXFAT_ENTRY_SIZE, FAT_BAD, FAT_EOC};
use crate::codec::exfat::raw_entry::{ExfatFileEntry, ExfatFileNameEntry, ExfatStreamExtension};

/// File attribute bits at offset 0x04 of the primary File entry;
/// same low-byte layout as classic FAT ([`AttrBits`]).
pub(crate) type ExfatAttributes = AttrBits;

/// Location of one entry set: the containing directory's walk parameters
/// plus the set's logical byte range within that directory's concatenated
/// cluster stream. Entry sets may straddle cluster boundaries (Windows
/// writes them that way), so consumers resolve this to physical byte
/// segments via `ExfatVfs::read_set` / `write_set` rather than assuming
/// one contiguous on-disk range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SetLoc {
    pub(crate) dir_first_cluster: u32,
    pub(crate) dir_no_fat_chain: bool,
    /// Cluster cap for the directory walk (`u32::MAX` = unbounded root).
    pub(crate) dir_max_clusters: u32,
    /// Logical byte offset of the primary File entry in the dir stream.
    pub(crate) logical: u64,
    /// Whole entry-set length in bytes.
    pub(crate) len: u32,
}

/// One resolved directory entry from an ExFAT volume, produced by the
/// directory iterators.
#[derive(Debug, Clone)]
pub(crate) struct ExfatDirEntry {
    pub name: String,
    pub attributes: ExfatAttributes,
    pub first_cluster: u32,
    /// Bytes actually containing data (used portion of the file).
    pub valid_data_length: u64,
    /// File size in bytes (`DataLength`); the on-disk allocation is
    /// this rounded up to whole clusters.
    pub data_length: u64,
    /// When true, the clusters are contiguous — no FAT chain walk.
    pub no_fat_chain: bool,
    /// Where the entry set lives on disk; `None` when hand-built (tests)
    /// or decoded from a detached buffer.
    pub loc: Option<SetLoc>,
    /// Entry-set size in bytes, so write-side code can re-read the set.
    pub entry_set_len: u32,
    /// Creation timestamp, if present (`0` on disk decodes to `None`).
    pub created: Option<PrimitiveDateTime>,
    /// Last-modification timestamp, if present.
    pub modified: Option<PrimitiveDateTime>,
    /// Last-access date, if present.
    pub accessed: Option<Date>,
}

impl ExfatDirEntry {
    /// `true` if this entry is a directory.
    pub fn is_dir(&self) -> bool {
        self.attributes.is_dir()
    }
}

/// ExFAT entry-set checksum (`SetChecksum`): rotate-right + add per
/// byte, skipping bytes
/// 2-3 of the first (File) entry, which hold the checksum itself.
pub(crate) fn entry_set_checksum(data: &[u8]) -> u16 {
    let mut sum: u16 = 0;
    for (i, &b) in data.iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        sum = (sum >> 1) | ((sum & 1) << 15);
        sum = sum.wrapping_add(u16::from(b));
    }
    sum
}

/// Decoder assembling File + Stream Extension + File Name entry sets
/// from an in-memory directory-cluster buffer into [`ExfatDirEntry`]s.
pub(crate) struct DirEntryDecoder {
    pub(super) buf: Vec<u8>,
    pub(super) pos: usize,
}

impl DirEntryDecoder {
    pub(super) fn new(buf: Vec<u8>) -> Self {
        Self { buf, pos: 0 }
    }
}

impl Iterator for DirEntryDecoder {
    type Item = ExfatDirEntry;

    fn next(&mut self) -> Option<Self::Item> {
        while self.pos + EXFAT_ENTRY_SIZE <= self.buf.len() {
            let ty = self.buf[self.pos];
            if ty == END_OF_DIRECTORY {
                return None;
            }
            // InUse (bit 7) cleared = deleted entry.
            if ty & ENTRY_IN_USE == 0 {
                self.pos += EXFAT_ENTRY_SIZE;
                continue;
            }
            if ty == ENTRY_TYPE_FILE {
                if let Some(entry) = self.try_decode_file_set() {
                    return Some(entry);
                }
                // Malformed set: skip just the primary — safer than
                // trusting its secondary count.
                self.pos += EXFAT_ENTRY_SIZE;
                continue;
            }
            // Benign metadata entry (bitmap, upcase, label, …) — skip.
            self.pos += EXFAT_ENTRY_SIZE;
        }
        None
    }
}

impl DirEntryDecoder {
    /// Decode the entry set whose File primary starts at `self.pos`.
    /// Advances `pos` past the set on success; leaves it untouched on
    /// failure so the caller decides how far to skip.
    fn try_decode_file_set(&mut self) -> Option<ExfatDirEntry> {
        let start = self.pos;
        let first = self.buf.get(start..start + EXFAT_ENTRY_SIZE)?;
        let file = ExfatFileEntry::parse(first)?;
        let secondary_count = usize::from(file.secondary_count);
        // A set needs at least the Stream Extension secondary.
        if secondary_count < 1 {
            return None;
        }
        let set_bytes = (1 + secondary_count) * EXFAT_ENTRY_SIZE;
        let set = self.buf.get(start..start + set_bytes)?;

        if entry_set_checksum(set) != file.set_checksum {
            return None;
        }

        let attributes = AttrBits::from_u16(file.attributes);
        let created = decode_datetime(file.create_timestamp, file.create_10ms);
        let modified = decode_datetime(file.modified_timestamp, file.modified_10ms);
        let accessed = decode_date(file.accessed_timestamp);

        // The Stream Extension is always the first secondary.
        let stream = ExfatStreamExtension::parse(&set[EXFAT_ENTRY_SIZE..EXFAT_ENTRY_SIZE * 2])?;
        let no_fat_chain = stream.no_fat_chain();
        let name_length = usize::from(stream.name_length);
        let valid_data_length = stream.valid_data_length;
        let first_cluster = stream.first_cluster;
        let data_length = stream.data_length;

        // File Name secondaries hold 15 UCS-2 units each.
        let mut name_u16: Vec<u16> = Vec::with_capacity(name_length);
        let name_entries = secondary_count - 1;
        for i in 0..name_entries {
            let off = EXFAT_ENTRY_SIZE * (2 + i);
            let fname = ExfatFileNameEntry::parse(&set[off..off + EXFAT_ENTRY_SIZE])?;
            for &unit in &fname.name {
                if name_u16.len() >= name_length {
                    break;
                }
                name_u16.push(unit);
            }
        }
        // A malformed set may promise more name chars than are present.
        name_u16.truncate(name_length);
        let name = String::from_utf16(&name_u16).ok()?;

        self.pos = start + set_bytes;

        Some(ExfatDirEntry {
            name,
            attributes,
            first_cluster,
            valid_data_length,
            data_length,
            no_fat_chain,
            // The decoder can't know the buffer's on-disk location;
            // `ExfatReadDir` fills in the real location before yielding.
            loc: None,
            entry_set_len: u32::try_from(set_bytes)
                .expect("entry set is at most 256 entries (8192 bytes)"),
            created,
            modified,
            accessed,
        })
    }
}

/// Classified FAT entry value for one cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FatEntry {
    /// Cluster in use; points to the next cluster in the chain.
    Next(u32),
    /// Cluster is free.
    Free,
    /// Cluster is marked bad.
    Bad,
    /// End-of-chain marker for the current file / directory.
    Eoc,
    /// Value reserved or out of the valid range.
    Invalid,
}

pub(super) fn classify_fat_entry(raw: u32, cluster_count: u32) -> FatEntry {
    match raw {
        0 => FatEntry::Free,
        FAT_BAD => FatEntry::Bad,
        FAT_EOC => FatEntry::Eoc,
        // Valid clusters are 2..=cluster_count+1; `n - 1 <=` avoids
        // overflow when `cluster_count == u32::MAX`.
        n if n >= 2 && n - 1 <= cluster_count => FatEntry::Next(n),
        _ => FatEntry::Invalid,
    }
}

/// Cluster-layout descriptor for a directory: the root is FAT-chained
/// and unbounded, while a sub-directory may be contiguous (NoFatChain)
/// and bounded by its Stream Extension's `data_length`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DirExtent {
    pub(crate) first_cluster: u32,
    pub(crate) no_fat_chain: bool,
    /// Maximum clusters to walk — `u32::MAX` = unbounded (the root).
    pub(crate) max_clusters: u32,
    /// Location of this directory's own entry set in its parent, so
    /// growth can patch `DataLength`/`NoFatChain` and the checksum. `None`
    /// for the root, which has no parent entry.
    pub(crate) parent_entry: Option<SetLoc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[test]
    fn checksum_skips_bytes_2_and_3() {
        // Differing only in checksum bytes 2-3 → identical sums.
        let a = [0x85u8, 0x01, 0xAA, 0xBB, 0x20, 0x00];
        let b = [0x85u8, 0x01, 0x11, 0x22, 0x20, 0x00];
        assert_eq!(entry_set_checksum(&a), entry_set_checksum(&b));
    }

    #[test]
    fn checksum_reacts_to_other_bytes() {
        let base = [0x85u8, 0x01, 0x00, 0x00, 0x20, 0x00];
        let mut bumped = base;
        bumped[5] = 0x21;
        assert_ne!(entry_set_checksum(&base), entry_set_checksum(&bumped));
    }

    /// Build a well-formed File + Stream Extension + File Name entry
    /// set for the given name, with a correct `SetChecksum`.
    fn synth_entry_set(name: &str, is_dir: bool, first_cluster: u32, size: u64) -> Vec<u8> {
        use crate::codec::exfat::raw_entry::{
            ExfatFileEntry, ExfatFileNameEntry, ExfatStreamExtension,
        };

        let name_u16: Vec<u16> = name.encode_utf16().collect();
        let name_entries_needed = name_u16.len().div_ceil(15);
        let secondary_count = 1 + name_entries_needed; // Stream + N FileName
        let total_entries = 1 + secondary_count;
        let mut set = vec![0u8; total_entries * EXFAT_ENTRY_SIZE];

        let attrs = if is_dir {
            AttrBits::DIRECTORY.bits()
        } else {
            AttrBits::ARCHIVE.bits()
        };
        let file = ExfatFileEntry::new_zeroed(
            u8::try_from(secondary_count).expect("test entry sets stay under 255 entries"),
            attrs,
        );
        file.write_into(&mut set[..EXFAT_ENTRY_SIZE]);

        let stream = ExfatStreamExtension::new(
            u8::try_from(name_u16.len()).expect("test names fit in 255 UCS-2 units"),
            first_cluster,
            size,
            size,
            false,
        );
        stream.write_into(&mut set[EXFAT_ENTRY_SIZE..EXFAT_ENTRY_SIZE * 2]);

        for (i, chunk) in name_u16.chunks(15).enumerate() {
            let off = EXFAT_ENTRY_SIZE * (2 + i);
            ExfatFileNameEntry::from_units(chunk).write_into(&mut set[off..off + EXFAT_ENTRY_SIZE]);
        }

        set[0x02] = 0;
        set[0x03] = 0;
        let sum = entry_set_checksum(&set);
        set[0x02..0x04].copy_from_slice(&sum.to_le_bytes());
        set
    }

    #[test]
    fn decode_short_file_entry_set() {
        let set = synth_entry_set("hello.txt", false, 7, 123);
        let mut rd = DirEntryDecoder::new(set);
        let entry = rd.next().expect("decoded entry");
        assert_eq!(entry.name, "hello.txt");
        assert!(!entry.is_dir());
        assert!(entry.attributes.contains(AttrBits::ARCHIVE));
        assert_eq!(entry.first_cluster, 7);
        assert_eq!(entry.valid_data_length, 123);
        assert_eq!(entry.data_length, 123);
        assert!(!entry.no_fat_chain);
        assert!(rd.next().is_none());
    }

    #[test]
    fn decode_directory_entry_set() {
        let set = synth_entry_set("SUBDIR", true, 42, 4096);
        let mut rd = DirEntryDecoder::new(set);
        let entry = rd.next().unwrap();
        assert_eq!(entry.name, "SUBDIR");
        assert!(entry.is_dir());
        assert_eq!(entry.first_cluster, 42);
    }

    #[test]
    fn decode_long_filename_spanning_two_filename_entries() {
        let name = "a-very-long-filename.bin";
        let set = synth_entry_set(name, false, 9, 8);
        let mut rd = DirEntryDecoder::new(set);
        let entry = rd.next().unwrap();
        assert_eq!(entry.name, name);
    }

    #[test]
    fn bad_checksum_rejects_entry_set() {
        let mut set = synth_entry_set("corrupt.bin", false, 2, 0);
        // Flip a byte outside the checksum field to invalidate it.
        set[0x04] ^= 0xFF;
        let mut rd = DirEntryDecoder::new(set);
        assert!(rd.next().is_none());
    }

    #[test]
    fn deleted_entries_skipped() {
        let mut set = synth_entry_set("deleted.txt", false, 2, 0);
        // Clear the InUse bit on the primary to mark the set deleted.
        set[0x00] &= 0x7F;
        let mut rd = DirEntryDecoder::new(set);
        assert!(rd.next().is_none());
    }

    #[test]
    fn terminator_stops_iteration() {
        let mut buf = synth_entry_set("real.txt", false, 2, 0);
        buf.extend(core::iter::repeat_n(0u8, EXFAT_ENTRY_SIZE));
        // An entry set after the terminator must not be yielded.
        buf.extend_from_slice(&synth_entry_set("after.txt", false, 3, 0));
        let mut rd = DirEntryDecoder::new(buf);
        assert_eq!(rd.next().unwrap().name, "real.txt");
        assert!(rd.next().is_none());
    }

    #[test]
    fn fat_entry_classification() {
        assert_eq!(classify_fat_entry(0, 1000), FatEntry::Free);
        assert_eq!(classify_fat_entry(0xFFFF_FFFF, 1000), FatEntry::Eoc);
        assert_eq!(classify_fat_entry(0xFFFF_FFF7, 1000), FatEntry::Bad);
        assert_eq!(classify_fat_entry(5, 1000), FatEntry::Next(5));
        // Cluster 1 is reserved, so not a valid Next.
        assert_eq!(classify_fat_entry(1, 1000), FatEntry::Invalid);
        // Out-of-range (past cluster_count + 1) → invalid.
        assert_eq!(classify_fat_entry(2000, 1000), FatEntry::Invalid);
    }
}
