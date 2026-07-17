//! MBR partition support: parsing the table and mounting a filesystem
//! that lives in a partition (as on a real SD card).
//!
//! `fat16-mbr.img` is an MBR whose first partition (type 0x0E, FAT16
//! LBA) starts at LBA 2048 and contains the same filesystem as
//! `fat16-3m.img`. `fat16-3m.img` (a bare VBR at byte 0) doubles as the
//! "not an MBR" case.

#[path = "common/mod.rs"]
mod common;

use common::{medium, read_all};
use unifat::io::{Seek, SeekFrom, Write};
use unifat::{MemBlockDevice, Partition, PartitionError, PartitionKind, PartitionTable, Volume};

const FAT16_MBR: &[u8] = include_bytes!("fixtures/fat16-mbr.img");
const FAT16_BARE: &[u8] = include_bytes!("fixtures/fat16-3m.img");
// "ndev-rs" is baked into the committed binary fixtures (predates the rename).
const HELLO_TXT: &[u8] = b"Hello from FAT12 root, via ndev-rs!\n";

// ── Tests ───────────────────────────────────────────────────────────────

#[test]
fn partition_table_parses() {
    let mut dev = medium(FAT16_MBR);
    let table = PartitionTable::read(&mut dev).expect("read table");

    let entry = table.partitions[0].expect("partition 0 present");
    assert_eq!(entry.kind, PartitionKind::Fat16);
    assert!(entry.bootable);
    assert_eq!(entry.start_lba, 2048);
    assert_eq!(entry.sector_count, 6144);
    assert!(entry.kind.is_filesystem());

    // Slots 1..4 are empty.
    for slot in &table.partitions[1..] {
        assert!(slot.is_none());
    }
    assert_eq!(table.first_filesystem(), Some(0));
}

#[test]
fn mount_first_partition_reads() {
    let vol = Volume::mount_first_partition(medium(FAT16_MBR)).expect("mount");

    let names: Vec<String> = vol
        .read_dir("/")
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_ascii_uppercase())
        .collect();
    assert!(names.iter().any(|n| n == "HELLO.TXT"), "saw {names:?}");
    assert!(names.iter().any(|n| n == "SUBDIR"), "saw {names:?}");

    let mut f = vol.open("/HELLO.TXT").expect("open");
    assert_eq!(
        read_all(&mut f),
        HELLO_TXT,
        "content read through partition"
    );
}

#[test]
fn mount_partition_by_index() {
    // Index 0 is the FAT16 partition.
    assert!(Volume::mount_partition(medium(FAT16_MBR), 0).is_ok());
    // Index 1 is an empty slot.
    assert!(
        Volume::mount_partition(medium(FAT16_MBR), 1).is_err(),
        "empty partition slot should not mount",
    );
}

#[test]
fn write_through_partition_roundtrips() {
    let dev = medium(FAT16_MBR);
    let vol = Volume::mount_first_partition(dev).expect("mount");

    vol.write("/PART.BIN", b"written into a partitioned volume")
        .expect("write");
    assert_eq!(
        vol.read("/PART.BIN").expect("read"),
        b"written into a partitioned volume",
        "partition write/read mismatch",
    );
}

#[test]
fn partition_write_past_end_errors() {
    // 4 sectors = 2048 bytes of partition inside a larger device.
    let dev = MemBlockDevice::new(vec![0u8; 4096]);
    let mut part = Partition::new(dev, 0, 4);

    // At the end: a non-empty write must error, never return Ok(0)
    // (embedded-io's write_all panics on zero-length writes).
    part.seek(SeekFrom::Start(2048)).expect("seek to end");
    let err = part.write(&[1u8; 4]).expect_err("write past end");
    assert!(matches!(err, PartitionError::OutOfBounds), "got {err:?}");

    // Straddling the boundary: clamped partial write, then the error.
    part.seek(SeekFrom::Start(2046)).expect("seek near end");
    assert_eq!(part.write(&[1u8; 4]).expect("clamped write"), 2);
    let err = part.write(&[1u8; 2]).expect_err("now at end");
    assert!(matches!(err, PartitionError::OutOfBounds), "got {err:?}");
}

#[test]
fn bare_vbr_is_not_an_mbr() {
    // A filesystem VBR at byte 0 is not a partition table.
    let mut dev = medium(FAT16_BARE);
    assert!(
        PartitionTable::read(&mut dev).is_err(),
        "a bare VBR must not parse as an MBR",
    );
    assert!(Volume::mount_first_partition(medium(FAT16_BARE)).is_err());
}
