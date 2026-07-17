//! Fuzzing hooks (feature `__fuzz`, `#[doc(hidden)]`, unstable).
//!
//! Exposes byte-in round-trip harnesses for the pure codec layer so
//! `cargo fuzz` can reach the on-disk parsers directly, without leaking
//! any codec types into the public API. Each function takes raw bytes,
//! decodes, re-encodes, and asserts the codec's own invariants — a
//! panic is a finding. These are NOT part of the stable surface.

use crate::codec::FixedCodec;
use crate::codec::exfat::boot::{EXFAT_BOOT_RECORD_SIZE, ExfatBootRecord};
use crate::codec::exfat::consts::EXFAT_ENTRY_SIZE;
use crate::codec::exfat::raw_entry::{
    ExfatClusteredMetaEntry, ExfatFileEntry, ExfatFileNameEntry, ExfatStreamExtension,
};
use crate::codec::fat::DIRENTRY_SIZE;
use crate::codec::fat::bpb::{FAT_BPB_SIZE, FatBpb};
use crate::codec::fat::dir_entry::RawDirEntry;
use crate::codec::fat::lfn::LfnEntry;

/// Decode → re-encode → decode a fixed-size record. Invariant: a value
/// decoded from `SIZE` bytes re-encodes, and a second decode/encode
/// yields byte-identical output (the codec reaches a fixed point).
/// Comparing bytes needs no `PartialEq` on the on-disk types. A panic
/// is a finding.
fn roundtrip<T: FixedCodec, const SIZE: usize>(data: &[u8]) {
    const { assert!(SIZE > 0) };
    if data.len() < SIZE {
        return;
    }
    let Some(first) = T::parse(&data[..SIZE]) else {
        return;
    };
    let mut once = [0u8; SIZE];
    first.write_into(&mut once);
    let again = T::parse(&once[..]).expect("re-encoded bytes must decode");
    let mut twice = [0u8; SIZE];
    again.write_into(&mut twice);
    assert_eq!(
        once, twice,
        "codec is not a fixed point after one round-trip"
    );
}

/// Round-trip the FAT BIOS Parameter Block.
pub fn fat_bpb(data: &[u8]) {
    roundtrip::<FatBpb, FAT_BPB_SIZE>(data);
}

/// Round-trip a FAT 8.3 directory entry.
pub fn fat_dir_entry(data: &[u8]) {
    roundtrip::<RawDirEntry, DIRENTRY_SIZE>(data);
}

/// Round-trip a FAT long-filename entry.
pub fn fat_lfn(data: &[u8]) {
    roundtrip::<LfnEntry, DIRENTRY_SIZE>(data);
}

/// Round-trip the ExFAT volume boot record head.
pub fn exfat_boot(data: &[u8]) {
    roundtrip::<ExfatBootRecord, EXFAT_BOOT_RECORD_SIZE>(data);
}

/// Round-trip every ExFAT 32-byte entry codec through the fixed-point
/// check. The clustered-meta entry has no writer, so it is parse-only
/// (a panic — not `None` — is the failure signal).
pub fn exfat_entries(data: &[u8]) {
    roundtrip::<ExfatFileEntry, EXFAT_ENTRY_SIZE>(data);
    roundtrip::<ExfatStreamExtension, EXFAT_ENTRY_SIZE>(data);
    roundtrip::<ExfatFileNameEntry, EXFAT_ENTRY_SIZE>(data);
    if data.len() >= EXFAT_ENTRY_SIZE {
        let _ = ExfatClusteredMetaEntry::parse(&data[..EXFAT_ENTRY_SIZE]);
    }
}
