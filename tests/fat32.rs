//! FAT32-specific behaviour against the gzipped 34 MiB fixture (the
//! generic read/write suite also runs on it — see `suite.rs`).
//!
//! Fixture geometry (asserted below so a regenerated fixture that no
//! longer matches fails loudly): 512 B sectors, 1 sector/cluster,
//! 32 reserved sectors, 2 FATs of 536 sectors, root at cluster 2.

#[path = "common/mod.rs"]
mod common;

use common::gunzip;
use unifat::{CorruptKind, Format, FsError, MemBlockDevice, Volume};

const FAT32_GZ: &[u8] = include_bytes!("fixtures/fat32-34m.img.gz");

const BPS: usize = 512;
const RSVD: usize = 32;
const FATSZ: usize = 536; // sectors per FAT copy
const NFATS: usize = 2;
const EXTFLAGS_OFF: usize = 40;
const FSINFO_SECTOR: usize = 1;

fn image() -> Vec<u8> {
    let img = gunzip(FAT32_GZ);
    // Guard the constants above against fixture regeneration drift.
    assert_eq!(u16::from_le_bytes([img[11], img[12]]) as usize, BPS);
    assert_eq!(img[13], 1, "sectors per cluster");
    assert_eq!(u16::from_le_bytes([img[14], img[15]]) as usize, RSVD);
    assert_eq!(img[16] as usize, NFATS);
    assert_eq!(
        u32::from_le_bytes([img[36], img[37], img[38], img[39]]) as usize,
        FATSZ
    );
    img
}

/// Byte offset of cluster `n`'s cell in FAT copy `copy`.
fn cell_off(copy: usize, n: usize) -> usize {
    (RSVD + copy * FATSZ) * BPS + n * 4
}

#[test]
fn mounts_as_fat32_and_roundtrips() {
    let vol = Volume::mount(MemBlockDevice::new(image())).expect("mount");
    assert_eq!(vol.format(), Format::Fat32);
    vol.write("/HELLO32.TXT", b"fat32").expect("write");
    let img = vol.into_storage().into_inner();
    let vol = Volume::mount(MemBlockDevice::new(img)).expect("remount");
    assert_eq!(vol.read("/HELLO32.TXT").expect("read"), b"fat32");
}

/// With mirroring disabled, `BPB_ExtFlags.ActiveFat` selects the one
/// live FAT copy — reads and writes must target it, not copy 0
/// (repair tools legitimately switch the active copy).
#[test]
fn active_fat_copy_honored_when_mirroring_disabled() {
    let mut img = image();
    // mirroring_disabled (bit 7) + ActiveFat = 1.
    img[EXTFLAGS_OFF] = 0x81;
    img[EXTFLAGS_OFF + 1] = 0;

    let vol = Volume::mount(MemBlockDevice::new(img)).expect("mount");
    vol.write("/ACTIVE.BIN", b"copy1").expect("write");
    vol.flush().expect("flush");
    let img = vol.into_storage().into_inner();

    // The copies started identical (mkfs mirrors them); the allocation
    // must have landed in copy 1 only.
    let fat0 = &img[RSVD * BPS..(RSVD + FATSZ) * BPS];
    let fat1 = &img[(RSVD + FATSZ) * BPS..(RSVD + 2 * FATSZ) * BPS];
    assert_ne!(fat0, fat1, "write must go to the ACTIVE copy only");
    // Cluster 3 (first free) is EOC in copy 1, still free in copy 0.
    let c3_0 = u32::from_le_bytes(img[cell_off(0, 3)..cell_off(0, 3) + 4].try_into().unwrap());
    let c3_1 = u32::from_le_bytes(img[cell_off(1, 3)..cell_off(1, 3) + 4].try_into().unwrap());
    assert_eq!(c3_0 & 0x0FFF_FFFF, 0, "copy 0 untouched");
    assert_eq!(c3_1 & 0x0FFF_FFFF, 0x0FFF_FFFF, "copy 1 holds the chain");

    // And the chain must resolve on remount (reads go through copy 1).
    let vol = Volume::mount(MemBlockDevice::new(img)).expect("remount");
    assert_eq!(vol.read("/ACTIVE.BIN").expect("read"), b"copy1");
}

/// An ActiveFat index past the FAT count (with mirroring disabled) is an
/// inconsistent volume, not a mountable one.
#[test]
fn out_of_range_active_fat_rejected() {
    let mut img = image();
    img[EXTFLAGS_OFF] = 0x85; // mirroring_disabled + ActiveFat = 5 (> 2 FATs)
    img[EXTFLAGS_OFF + 1] = 0;
    let err = Volume::mount(MemBlockDevice::new(img)).expect_err("must reject");
    assert!(
        matches!(err, FsError::Corrupt(CorruptKind::BootSector)),
        "got {err:?}"
    );
}

/// FSInfo is advisory: a scribbled sector must not fail the mount, and
/// the next sync writes back a valid (repaired) copy.
#[test]
fn fsinfo_corruption_is_advisory_and_repaired() {
    let mut img = image();
    let off = FSINFO_SECTOR * BPS;
    img[off..off + 4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);

    let vol = Volume::mount(MemBlockDevice::new(img)).expect("mount tolerates bad FSInfo");
    vol.write("/FSI.BIN", b"x").expect("write");
    let img = vol.into_storage().into_inner();

    let off = FSINFO_SECTOR * BPS;
    assert_eq!(
        &img[off..off + 4],
        &0x41615252u32.to_le_bytes(),
        "lead signature repaired on sync"
    );
    assert_eq!(
        &img[off + 484..off + 488],
        &0x61417272u32.to_le_bytes(),
        "mid signature repaired on sync"
    );
    let vol = Volume::mount(MemBlockDevice::new(img)).expect("remount");
    assert_eq!(vol.read("/FSI.BIN").expect("read"), b"x");
}

/// The reserved top nibble of a FAT32 cell must be preserved by writes,
/// not zeroed (fatgen103).
#[test]
fn reserved_high_nibble_preserved_on_write() {
    let mut img = image();
    // Poison the reserved nibble of (free) cluster 3 in both copies.
    for copy in 0..NFATS {
        img[cell_off(copy, 3) + 3] |= 0xF0;
    }

    let vol = Volume::mount(MemBlockDevice::new(img)).expect("mount");
    // The first allocation on the fresh volume takes cluster 3.
    vol.write("/NIB.BIN", b"n").expect("write");
    vol.flush().expect("flush");
    let img = vol.into_storage().into_inner();

    for copy in 0..NFATS {
        let off = cell_off(copy, 3);
        let cell = u32::from_le_bytes(img[off..off + 4].try_into().unwrap());
        assert_eq!(
            cell & 0x0FFF_FFFF,
            0x0FFF_FFFF,
            "cluster 3 must be the file's EOC (copy {copy})"
        );
        assert_eq!(
            cell >> 28,
            0xF,
            "reserved nibble must survive the write (copy {copy})"
        );
    }
    let vol = Volume::mount(MemBlockDevice::new(img)).expect("remount");
    assert_eq!(vol.read("/NIB.BIN").expect("read"), b"n");
}
