//! Fuzzing hooks (feature `__fuzz`, `#[doc(hidden)]`, unstable).
//!
//! Exposes byte-in round-trip harnesses for the pure codec layer so
//! `cargo fuzz` can reach the on-disk parsers directly, without leaking
//! any codec types into the public API. Each function takes raw bytes,
//! decodes, re-encodes, and asserts the codec's own invariants — a
//! panic is a finding. These are NOT part of the stable surface.

use alloc::vec::Vec;

use layline::{Layout, Message};

use crate::codec::exfat::boot::ExfatBootRecord;
use crate::codec::exfat::entry_set::ExfatEntrySet;
use crate::codec::exfat::raw_entry::{
    ExfatClusteredMetaEntry, ExfatFileNameEntry, ExfatStreamExtension,
};
use crate::codec::fat::bpb::{
    BootSectorSignature, Fat16Ebr, Fat32Ebr, Fat32FsInfo, Fat32FsInfoLead, FatBpb,
};
use crate::codec::fat::dir_entry::RawDirEntry;
use crate::codec::fat::lfn::LfnEntry;
use crate::codec::mbr::MbrPartitionTable;

/// Decode → re-encode a fixed-size record. Invariant: every record
/// stores its raw on-disk fields, so anything that decodes re-encodes to
/// exactly the input bytes. A panic is a finding.
fn roundtrip<T: Layout>(data: &[u8]) {
    let Some(wire) = data.get(..T::WIRE_BYTES) else {
        return;
    };
    let Ok(value) = T::decode_slice(wire) else {
        return;
    };
    let mut out = Vec::with_capacity(T::WIRE_BYTES);
    value
        .encode_into(&mut out)
        .expect("a Vec buffer never overflows");
    assert_eq!(out, wire, "{} does not round-trip byte-for-byte", T::NAME);
}

/// Round-trip the FAT BIOS Parameter Block, both EBR shapes, and the
/// boot-sector / FSInfo records.
pub fn fat_bpb(data: &[u8]) {
    roundtrip::<FatBpb>(data);
    if let Some(ebr) = data.get(<FatBpb as Layout>::WIRE_BYTES..) {
        roundtrip::<Fat16Ebr>(ebr);
        roundtrip::<Fat32Ebr>(ebr);
    }
    roundtrip::<BootSectorSignature>(data);
    roundtrip::<Fat32FsInfoLead>(data);
    roundtrip::<Fat32FsInfo>(data);
}

/// Round-trip a FAT 8.3 directory entry.
pub fn fat_dir_entry(data: &[u8]) {
    roundtrip::<RawDirEntry>(data);
}

/// Round-trip a FAT long-filename entry.
pub fn fat_lfn(data: &[u8]) {
    roundtrip::<LfnEntry>(data);
}

/// Round-trip the ExFAT volume boot record head.
pub fn exfat_boot(data: &[u8]) {
    roundtrip::<ExfatBootRecord>(data);
}

/// Round-trip every ExFAT 32-byte entry layout, then the whole File
/// entry set: a set that decodes (types, count and `SetChecksum` all
/// valid) must re-encode to exactly the bytes it consumed.
pub fn exfat_entries(data: &[u8]) {
    roundtrip::<ExfatStreamExtension>(data);
    roundtrip::<ExfatFileNameEntry>(data);
    roundtrip::<ExfatClusteredMetaEntry>(data);
    if let Ok((set, used)) = ExfatEntrySet::decode(data) {
        assert_eq!(set.encode(), &data[..used], "entry set does not round-trip");
    }
}

/// Round-trip the MBR partition table (four records + signature).
pub fn mbr(data: &[u8]) {
    roundtrip::<MbrPartitionTable>(data);
}
