//! Host-runnable coverage for the unified [`Volume`] API over FAT16
//! fixtures (FAT12 is intentionally unsupported).
//!
//! Ships its own small, committed fixtures (built with `mkfs.fat` +
//! `mtools`) under `tests/fixtures/`, exercising the read path — boot
//! sector / BPB parse, format detection, FAT chain walk, root and
//! sub-directory traversal, and multi-cluster file read — plus the FAT
//! write path (entry create / remove) through `Volume`. A second FAT16
//! fixture with 1 KiB (2-sector) clusters exercises the multi-sector
//! directory-cluster paths. FAT32's read+write path is covered
//! elsewhere against a full-size image.
//!
//! Shared layout of the main fixture:
//!
//! ```text
//!   /HELLO.TXT          36 bytes, single cluster
//!   /BIG.DAT          5000 bytes, multi-cluster (exercises chain walk)
//!   /SUBDIR/
//!       NESTED.BIN      29 bytes
//! ```

#[path = "common/mod.rs"]
mod common;

use common::{FixedClock, medium, read_all};
use embedded_io::{Read, Seek, SeekFrom, Write};
use time::macros::datetime;
use unifat::{CorruptKind, Format, FsError, FsOptions, MemBlockDevice, Volume};

const FAT16: &[u8] = include_bytes!("fixtures/fat16-3m.img");
// FAT16 with 1024 B/cluster (2 sectors/cluster, 32 dir entries per
// cluster). Exercises the multi-sector-cluster directory paths.
const FAT16_MSC: &[u8] = include_bytes!("fixtures/fat16-msc-8m.img");

// ── Expected fixture contents (see module docs / mkfs script) ───────────

// "ndev-rs" is baked into the committed binary fixtures (predates the rename).
const HELLO_TXT: &[u8] = b"Hello from FAT12 root, via ndev-rs!\n";
const NESTED_BIN: &[u8] = b"nested payload inside SUBDIR\n";
const BIG_LEN: usize = 5000;

/// Regenerate BIG.DAT's expected bytes: `(i * 7 + 3) & 0xFF`.
fn big_dat_expected() -> Vec<u8> {
    (0..BIG_LEN).map(|i| ((i * 7 + 3) & 0xFF) as u8).collect()
}

/// Mount a fixture and run the full read-path battery against the known
/// on-disk layout. `expected` pins that format detection landed right.
fn exercise_fixture(image: &[u8], expected: Format) {
    let vol = Volume::mount(medium(image)).expect("mount");

    assert_eq!(vol.format(), expected, "format detection landed wrong");

    // 1. List root — must contain HELLO.TXT, BIG.DAT and the SUBDIR dir.
    let mut saw_hello = false;
    let mut saw_big = false;
    let mut saw_subdir = false;
    for entry in vol.read_dir("/").expect("read_dir root") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name().to_ascii_uppercase();
        match name.as_str() {
            "HELLO.TXT" => {
                saw_hello = true;
                assert!(entry.is_file());
                assert_eq!(entry.metadata().len() as usize, HELLO_TXT.len());
            }
            "BIG.DAT" => {
                saw_big = true;
                assert!(entry.is_file());
                assert_eq!(entry.metadata().len() as usize, BIG_LEN);
            }
            "SUBDIR" => {
                saw_subdir = true;
                assert!(entry.is_dir());
            }
            other => panic!("unexpected root entry: {other:?}"),
        }
    }
    assert!(saw_hello, "HELLO.TXT missing from root listing");
    assert!(saw_big, "BIG.DAT missing from root listing");
    assert!(saw_subdir, "SUBDIR missing from root listing");

    // 2. Read a known root file's bytes (single cluster).
    {
        let mut f = vol.open("/HELLO.TXT").expect("open HELLO.TXT");
        assert_eq!(read_all(&mut f), HELLO_TXT, "HELLO.TXT content mismatch");
    }

    // 3. Read a multi-cluster file — exercises the FAT chain walk.
    {
        let mut f = vol.open("/BIG.DAT").expect("open BIG.DAT");
        let got = read_all(&mut f);
        assert_eq!(got.len(), BIG_LEN, "BIG.DAT short/over read");
        assert_eq!(got, big_dat_expected(), "BIG.DAT content mismatch");
    }

    // 4. Traverse one sub-directory and read the file inside it.
    {
        let names: Vec<String> = vol
            .read_dir("/SUBDIR")
            .expect("read_dir SUBDIR")
            .map(|e| e.expect("subdir entry").file_name().to_ascii_uppercase())
            .collect();
        assert!(
            names.iter().any(|n| n == "NESTED.BIN"),
            "NESTED.BIN missing from SUBDIR; saw {names:?}",
        );

        let mut f = vol
            .open("/SUBDIR/NESTED.BIN")
            .expect("open SUBDIR/NESTED.BIN");
        assert_eq!(read_all(&mut f), NESTED_BIN, "NESTED.BIN content mismatch");
    }
}

#[test]
fn fat16_read_path() {
    exercise_fixture(FAT16, Format::Fat16);
}

#[test]
fn fat16_seek_then_read_midfile() {
    // Seek into the middle of BIG.DAT and confirm the FAT chain walk
    // resumes at the correct cluster/offset.
    let vol = Volume::mount(medium(FAT16)).expect("mount");

    let mut f = vol.open("/BIG.DAT").expect("open BIG.DAT");
    let mid = 3000usize;
    f.seek(SeekFrom::Start(mid as u64)).expect("seek");

    let mut got = vec![0u8; 64];
    let mut total = 0;
    while total < got.len() {
        let n = f.read(&mut got[total..]).expect("read");
        if n == 0 {
            break;
        }
        total += n;
    }
    assert_eq!(total, 64, "short read after seek");

    let expected = big_dat_expected();
    assert_eq!(
        got,
        &expected[mid..mid + 64],
        "post-seek chain-walk read mismatch",
    );
}

/// Regression: boot-record signature validation must *reject* a volume
/// whose boot-sector signature (the `55 AA` at bytes 510..512) is wrong.
#[test]
fn bad_boot_signature_is_rejected() {
    // Sanity: the unmodified fixture has a valid signature and mounts.
    assert!(
        Volume::mount(medium(FAT16)).is_ok(),
        "pristine fixture should mount",
    );

    // Zero the last two bytes of the boot sector (offsets 510, 511).
    let mut bad = FAT16.to_vec();
    bad[510] = 0x00;
    bad[511] = 0x00;

    match Volume::mount(medium(&bad)) {
        Err(FsError::Corrupt(CorruptKind::BootSector)) => {}
        Err(other) => {
            panic!("expected Corrupt(BootSector) for zeroed boot signature, got {other:?}")
        }
        Ok(_) => panic!("zeroed boot signature was wrongly accepted"),
    }
}

/// Regression: removing a *high-index* entry from a multi-sector-cluster
/// directory must target the correct sector and leave every other entry
/// intact — driven end-to-end through the unified `Volume` write path.
#[test]
fn free_entry_targets_correct_sector_on_multisector_cluster() {
    let vol = Volume::mount(medium(FAT16_MSC)).expect("mount FAT16_MSC");
    assert_eq!(vol.format(), Format::Fat16, "fixture should be FAT16");

    // Create 20 files in /SUBDIR — enough to spill past the cluster's
    // first sector (16 dir slots each) into the second.
    let names: Vec<String> = (0..20).map(|i| format!("F{i:02}.TXT")).collect();
    for name in &names {
        vol.create(format!("/SUBDIR/{name}"))
            .expect("create file in SUBDIR");
    }

    let list_subdir = |vol: &Volume<MemBlockDevice>| -> Vec<String> {
        vol.read_dir("/SUBDIR")
            .expect("read_dir SUBDIR")
            .map(|e| e.expect("subdir entry").file_name().to_ascii_uppercase())
            .collect()
    };

    let before = list_subdir(&vol);
    for name in &names {
        assert!(
            before.iter().any(|n| n == name),
            "{name} missing before removal; saw {before:?}",
        );
    }

    // Remove a high-index entry (lives in the cluster's second sector).
    let removed = "F18.TXT";
    vol.remove_file(format!("/SUBDIR/{removed}"))
        .expect("remove high-index entry");

    let after = list_subdir(&vol);
    assert!(
        !after.iter().any(|n| n == removed),
        "{removed} should be gone; saw {after:?}",
    );
    for name in &names {
        if name == removed {
            continue;
        }
        assert!(
            after.iter().any(|n| n == name),
            "{name} was wrongly removed/corrupted; saw {after:?}",
        );
    }
    assert_eq!(after.len(), names.len() - 1, "expected exactly one removed");
}

/// Timestamps stamped on create via an injected clock, then read back
/// through `Metadata` on FAT.
#[test]
fn create_stamps_timestamps_fat() {
    let when = datetime!(2022-02-22 22:22:20);
    let opts = FsOptions::new().with_clock(Box::new(FixedClock(when)));
    let vol = Volume::mount_with(medium(FAT16), opts).expect("mount");

    vol.write("/STAMP.BIN", b"hi").expect("write");
    let meta = vol.metadata("/STAMP.BIN").expect("meta");
    assert_eq!(meta.modified(), Some(when), "modified timestamp");
    assert_eq!(meta.created(), Some(when), "create timestamp");
}

/// `Volume::read` / `Volume::write` whole-file convenience on FAT.
#[test]
fn read_write_helpers_fat() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.write("/RW.TXT", b"whole-file helper").expect("write");
    assert_eq!(
        vol.read("/RW.TXT").expect("read"),
        b"whole-file helper",
        "read did not return what write stored",
    );
}

/// Truncate a multi-cluster file to a sub-cluster length on FAT.
#[test]
fn truncate_shrinks_file_fat() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    let data = vec![0xABu8; 4000]; // spans several 512 B clusters
    {
        let mut f = vol.create("/T.BIN").expect("create");
        f.write_all(&data).expect("write");
        f.flush().expect("flush");
    }
    {
        let mut f = vol.open_rw("/T.BIN").expect("open_rw");
        f.set_len(100).expect("set_len");
    }
    let mut f = vol.open("/T.BIN").expect("reopen");
    let got = read_all(&mut f);
    assert_eq!(got.len(), 100, "truncated length wrong");
    assert_eq!(got, &data[..100], "truncated content mismatch");
}

/// Rename through the unified write path on FAT.
#[test]
fn rename_roundtrip_fat() {
    let vol = Volume::mount(medium(FAT16_MSC)).expect("mount");
    {
        let mut f = vol.create("/A.TXT").expect("create");
        f.write_all(b"rename me").expect("write");
    }
    vol.rename("/A.TXT", "/B.TXT").expect("rename");
    assert!(vol.open("/A.TXT").is_err(), "old name should be gone");
    let mut f = vol.open("/B.TXT").expect("renamed file");
    assert_eq!(read_all(&mut f), b"rename me", "content lost on rename");
}

/// Round-trip through the unified write path: create a file, write bytes,
/// flush, then read them back through a fresh handle.
#[test]
fn create_write_readback_roundtrip() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");

    let payload = b"round-trip through Volume::create";
    {
        let mut f = vol.create("/RT.BIN").expect("create");
        f.write_all(payload).expect("write");
        f.flush().expect("flush");
    }

    let mut f = vol.open("/RT.BIN").expect("reopen");
    assert_eq!(read_all(&mut f), payload, "written content mismatch");
}

/// FAT drop without explicit flush persists size + bytes (parity with ExFAT).
#[test]
fn drop_without_flush_persists_fat() {
    let payload = b"drop-flush-fat-payload";
    let image = {
        let vol = Volume::mount(medium(FAT16)).expect("mount");
        {
            let mut f = vol.create("/DROP.BIN").expect("create");
            f.write_all(payload).expect("write");
            // no flush — rely on StreamFile Drop + DirSlotWriter
        }
        vol.flush().expect("volume flush");
        vol.into_storage().into_inner()
    };
    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let mut f = vol.open("/DROP.BIN").expect("open");
    assert_eq!(
        f.len(),
        payload.len() as u64,
        "length not persisted on drop"
    );
    assert_eq!(read_all(&mut f), payload, "content not persisted on drop");
}

/// PR4/PR5: dirty FAT + data, flush, remount from the same image bytes, read back.
#[test]
fn flush_remount_persists_writes() {
    let payload = b"persist-across-remount";
    let image = {
        let vol = Volume::mount(medium(FAT16)).expect("mount");
        {
            let mut f = vol.create("/REMOUNT.BIN").expect("create");
            f.write_all(payload).expect("write");
            f.flush().expect("file flush");
        }
        vol.flush().expect("volume flush");
        vol.into_storage().into_inner()
    };

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let mut f = vol.open("/REMOUNT.BIN").expect("open after remount");
    assert_eq!(read_all(&mut f), payload, "content lost across remount");
}

// ── Path-pure FAT (no ambient cwd) regression tests ─────────────────────

/// Create a directory after listing a *sibling* path — must not desync
/// the intended parent (ambient-cwd bug that re-pinning used to paper over).
#[test]
fn create_dir_after_sibling_read_dir() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.create_dir("/left").expect("mkdir left");
    vol.create_dir("/right").expect("mkdir right");

    // List a foreign sibling directory first.
    let names: Vec<_> = vol
        .read_dir("/left")
        .expect("read_dir left")
        .map(|e| e.expect("entry").file_name().to_string())
        .collect();
    assert!(names.is_empty(), "fresh left dir should be empty");

    // Create under a different parent; gen_sfn / insert must use /right only.
    vol.create_dir("/right/child")
        .expect("mkdir right/child after sibling list");
    assert!(
        vol.metadata("/right/child").expect("stat child").is_dir(),
        "child should exist under right"
    );
    assert!(
        vol.metadata("/left/child").is_err(),
        "child must not appear under left"
    );

    // SFN uniqueness after path switches: two long names in root after listing subdir.
    let _ = vol.read_dir("/right/child").expect("list nested");
    {
        let mut f = vol
            .create("/VeryLongFileNameOne.txt")
            .expect("create long name 1");
        f.write_all(b"one").expect("write");
    }
    {
        let mut f = vol
            .create("/VeryLongFileNameTwo.txt")
            .expect("create long name 2 (gen_sfn uniqueness)");
        f.write_all(b"two").expect("write");
    }
    assert_eq!(
        read_all(&mut vol.open("/VeryLongFileNameOne.txt").expect("open1")),
        b"one"
    );
    assert_eq!(
        read_all(&mut vol.open("/VeryLongFileNameTwo.txt").expect("open2")),
        b"two"
    );
}

/// Nested `create_dir_all` after a foreign listing still creates each level
/// under the correct parents (path-pure intermediate creates).
#[test]
fn create_dir_all_after_foreign_listing() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.create_dir("/other").expect("mkdir other");
    let _ = vol.read_dir("/other").expect("list other");

    vol.create_dir_all("/a/b/c").expect("create_dir_all");
    assert!(vol.metadata("/a").expect("a").is_dir());
    assert!(vol.metadata("/a/b").expect("b").is_dir());
    assert!(vol.metadata("/a/b/c").expect("c").is_dir());

    {
        let mut f = vol.create("/a/b/c/nested.bin").expect("create nested");
        f.write_all(b"deep").expect("write");
    }
    assert_eq!(
        read_all(&mut vol.open("/a/b/c/nested.bin").expect("open")),
        b"deep"
    );
}

/// Multi-slot LFN create + case-insensitive lookup.
#[test]
fn long_lfn_create_and_lookup() {
    let vol = Volume::mount(medium(FAT16_MSC)).expect("mount");
    // 40+ chars → multiple LFN directory slots + 1 SFN.
    let name = "/ThisIsAVeryLongFileNameThatNeedsMultipleLfnSlots.dat";
    {
        let mut f = vol.create(name).expect("create LFN");
        f.write_all(b"lfn-payload").expect("write");
        f.flush().expect("flush");
    }
    let meta = vol.metadata(name).expect("metadata by long name");
    assert!(!meta.is_dir());
    assert_eq!(meta.len(), 11);

    // Case-insensitive open of the long name.
    let mut f = vol
        .open("/thisisaverylongfilenamethatneedsmultiplelfnslots.DAT")
        .expect("open LFN ci");
    assert_eq!(read_all(&mut f), b"lfn-payload");
}

/// Cross-directory rename of a directory keeps nested content reachable
/// (implies a consistent parent link after the move).
#[test]
fn rename_directory_across_parents() {
    let vol = Volume::mount(medium(FAT16_MSC)).expect("mount");
    vol.create_dir_all("/src/inner").expect("src/inner");
    vol.create_dir("/dst").expect("dst");
    {
        let mut f = vol
            .create("/src/inner/payload.bin")
            .expect("create payload");
        f.write_all(b"moved-payload").expect("write");
    }

    vol.rename("/src/inner", "/dst/inner")
        .expect("rename dir across parents");

    assert!(vol.metadata("/src/inner").is_err(), "old path gone");
    assert!(vol.metadata("/dst/inner").expect("new path").is_dir());
    assert_eq!(
        read_all(&mut vol.open("/dst/inner/payload.bin").expect("open nested")),
        b"moved-payload",
        "contents must survive cross-dir rename"
    );

    // Rename onto root parent: `..` cluster should become 0 (checked in unit test);
    // here verify the public path still works.
    vol.rename("/dst/inner", "/inner_at_root")
        .expect("rename to root parent");
    assert_eq!(
        read_all(
            &mut vol
                .open("/inner_at_root/payload.bin")
                .expect("open at root")
        ),
        b"moved-payload"
    );
}

/// Entries whose `DIR_WrtTime`/`DIR_WrtDate` are zero (cameras and MCU
/// firmware routinely write these) must stay visible and readable, with
/// the FAT epoch as their modification stamp — not silently vanish.
#[test]
fn zero_write_stamp_entry_stays_visible() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.write("/ZSTAMP.BIN", b"still here").expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    // Locate the SFN slot by its 8.3 name field and zero DIR_WrtTime /
    // DIR_WrtDate (bytes 22..26 of the 32-byte entry).
    let sfn = b"ZSTAMP  BIN";
    let pos = image
        .windows(sfn.len())
        .position(|w| w == sfn)
        .expect("SFN entry present in image");
    image[pos + 22..pos + 26].fill(0);

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let meta = vol
        .metadata("/ZSTAMP.BIN")
        .expect("zero-stamp entry must stay visible");
    assert_eq!(
        meta.modified(),
        Some(unifat::EPOCH),
        "zero write stamp reads back as the FAT epoch"
    );
    assert_eq!(vol.read("/ZSTAMP.BIN").expect("read"), b"still here");
}

/// The read-only attribute must block writes and deletes: open_rw,
/// remove_file, and remove_dir_all all refuse; reads still work.
#[test]
fn read_only_attribute_enforced() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.create_dir("/RODIR").expect("mkdir");
    vol.write("/RODIR/ROLOCK.BIN", b"guarded").expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    // Set the read-only bit (attr byte at offset 11 of the SFN entry).
    let sfn = b"ROLOCK  BIN";
    let pos = image
        .windows(sfn.len())
        .position(|w| w == sfn)
        .expect("SFN entry present in image");
    image[pos + 11] |= 0x01;

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    assert!(
        matches!(vol.open_rw("/RODIR/ROLOCK.BIN"), Err(FsError::ReadOnlyFile)),
        "open_rw must refuse a read-only file"
    );
    assert!(
        matches!(
            vol.remove_file("/RODIR/ROLOCK.BIN"),
            Err(FsError::ReadOnlyFile)
        ),
        "remove_file must refuse a read-only file"
    );
    assert!(
        matches!(vol.remove_dir_all("/RODIR"), Err(FsError::ReadOnlyFile)),
        "remove_dir_all must refuse a tree containing a read-only file"
    );
    assert_eq!(vol.read("/RODIR/ROLOCK.BIN").expect("read"), b"guarded");
    assert!(
        vol.metadata("/RODIR/ROLOCK.BIN")
            .expect("meta")
            .is_read_only()
    );
}

/// Multi-dot names must be stored as an LFN plus a clean 8.3 alias —
/// never as an SFN with a raw `.` inside the 11-byte name field (which
/// other implementations treat as corruption).
#[test]
fn multi_dot_names_use_lfn_with_clean_alias() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.write("/A.B.C", b"multidot").expect("create");
    vol.flush().expect("flush");
    let image = vol.into_storage().into_inner();

    let broken_sfn = b"A       B.C";
    assert!(
        !image.windows(broken_sfn.len()).any(|w| w == broken_sfn),
        "SFN field must not contain a raw dot"
    );

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    assert_eq!(vol.read("/A.B.C").expect("read"), b"multidot");
    let listed = vol
        .read_dir("/")
        .expect("read_dir")
        .any(|e| e.expect("entry").file_name() == "A.B.C");
    assert!(listed, "long name must round-trip through the LFN");
}

/// LFN slots must pad after the NUL terminator with 0xFFFF, not zeros
/// (strict readers and chkdsk flag zero padding).
#[test]
fn lfn_padding_is_ffff_after_terminator() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    // 7 units -> one LFN slot: terminator at unit 7, 0xFFFF pad after.
    vol.write("/Pad.bin", b"p").expect("create");
    vol.flush().expect("flush");
    let image = vol.into_storage().into_inner();

    // Locate the (single, last) LFN slot: order byte 0x41, then the
    // first five name units "Pad.b" in UTF-16LE.
    let mut pat = vec![0x41u8];
    for u in "Pad.b".encode_utf16() {
        pat.extend_from_slice(&u.to_le_bytes());
    }
    let pos = image
        .windows(pat.len())
        .position(|w| w == pat)
        .expect("LFN slot present in image");
    let slot = &image[pos..pos + 32];
    // mid_chars (units 5..11) hold "in", NUL, then three 0xFFFF pads.
    assert_eq!(&slot[14..20], &[b'i', 0, b'n', 0, 0, 0]);
    assert!(
        slot[20..26].iter().all(|&b| b == 0xFF),
        "mid_chars padding must be 0xFF"
    );
    // last_chars (units 11..13) are all padding.
    assert!(
        slot[28..32].iter().all(|&b| b == 0xFF),
        "last_chars padding must be 0xFF"
    );
}

/// Writes through a handle refresh the modification stamp by default
/// (std::fs-flavoured API); opt out with `with_auto_timestamps(false)`.
#[test]
fn writes_update_mtime_by_default() {
    let t1 = datetime!(2024-06-01 10:00:00);
    let opts = FsOptions::new().with_clock(Box::new(FixedClock(t1)));
    let vol = Volume::mount_with(medium(FAT16), opts).expect("mount");
    vol.write("/MTIME.BIN", b"x").expect("create");
    let image = vol.into_storage().into_inner();

    // Rewrite through a handle under a later clock: mtime must follow.
    let t2 = datetime!(2025-01-02 12:00:00);
    let opts = FsOptions::new().with_clock(Box::new(FixedClock(t2)));
    let vol = Volume::mount_with(MemBlockDevice::new(image), opts).expect("remount");
    {
        let mut f = vol.open_rw("/MTIME.BIN").expect("open_rw");
        f.write_all(b"y").expect("write");
    }
    assert_eq!(
        vol.metadata("/MTIME.BIN").expect("meta").modified(),
        Some(t2),
        "a write must refresh the modification stamp by default"
    );
    let image = vol.into_storage().into_inner();

    // And the opt-out really opts out.
    let t3 = datetime!(2026-03-04 08:00:00);
    let opts = FsOptions::new()
        .with_clock(Box::new(FixedClock(t3)))
        .with_auto_timestamps(false);
    let vol = Volume::mount_with(MemBlockDevice::new(image), opts).expect("remount 2");
    {
        let mut f = vol.open_rw("/MTIME.BIN").expect("open_rw");
        f.write_all(b"z").expect("write");
    }
    assert_eq!(
        vol.metadata("/MTIME.BIN").expect("meta").modified(),
        Some(t2),
        "auto_timestamps(false) must leave the stamp alone"
    );
}

/// Filling the volume must surface a clean StorageFull, leave the
/// partial file readable at its recorded length, and removing it must
/// give the space back.
#[test]
fn volume_full_fails_cleanly_and_recovers() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    let mut f = vol.create("/FULL.BIN").expect("create");
    let chunk = vec![0x77u8; 64 * 1024];
    let err = loop {
        match f.write_all(&chunk) {
            Ok(()) => {}
            Err(e) => break e,
        }
    };
    assert!(
        matches!(err, unifat::FileError::StorageFull),
        "expected StorageFull, got {err:?}"
    );
    f.flush().expect("flush after full still works");
    let len = f.len();
    drop(f);

    // The volume stays consistent: recorded length readable, space
    // reclaimable, and new writes succeed after the remove.
    assert!(len > 0, "some data must have fit");
    assert_eq!(vol.read("/FULL.BIN").expect("read").len() as u64, len);
    vol.remove_file("/FULL.BIN").expect("remove");
    vol.write("/AGAIN.BIN", b"space back")
        .expect("write after free");
    assert_eq!(vol.read("/AGAIN.BIN").expect("read"), b"space back");
}

/// R1 regression: directory growth across a fragmented FAT must zero the
/// clusters it actually allocated (chained, not necessarily adjacent) —
/// never `first + i`, which lands on a foreign in-use cluster and zeroes
/// another file's data while leaving a directory cluster unzeroed.
#[test]
fn fragmented_dir_growth_does_not_zero_foreign_clusters() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");

    // One cluster (512 B / 16 slots) for the new dir; `.` + `..` + 14
    // single-slot 8.3 names fill it exactly, so the next insert grows it.
    vol.create_dir("/FRAG").expect("mkdir");
    for i in 0..14 {
        vol.write(format!("/FRAG/F{i:02}"), b"").expect("filler");
    }

    // Fragment the free space: HOLE takes the lowest free cluster, VICTIM
    // the next (adjacent); deleting HOLE leaves free = {k, k+2, ...}.
    let sentinel = vec![0xC7u8; 400];
    vol.write("/HOLE.BIN", b"x").expect("hole");
    vol.write("/VICTIM.BIN", &sentinel).expect("victim");
    vol.remove_file("/HOLE.BIN").expect("free the hole");

    // A 200-char name needs 17 slots -> 2-cluster growth: k and k+2.
    // The buggy zero loop would wipe k+1 = VICTIM's cluster.
    let long = "L".repeat(200);
    vol.write(format!("/FRAG/{long}"), b"payload")
        .expect("long-name create");

    assert_eq!(
        vol.read("/VICTIM.BIN").expect("victim readable"),
        sentinel,
        "growth zeroed a foreign in-use cluster"
    );
    // The second new cluster must be zeroed: exactly 15 real files, no
    // phantom entries decoded from stale bytes.
    let names: Vec<String> = vol
        .read_dir("/FRAG")
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string())
        .collect();
    assert_eq!(names.len(), 15, "phantom entries: {names:?}");
    assert!(names.iter().any(|n| n == &long));
    assert_eq!(
        vol.read(format!("/FRAG/{long}")).expect("read long"),
        b"payload"
    );
}

/// R15 regression: a mid-run allocation failure (ENOSPC) must unwind —
/// no claimed-but-unreachable clusters, and the directory chain intact.
#[test]
fn enospc_mid_growth_leaks_nothing() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.create_dir("/TIGHT").expect("mkdir");
    for i in 0..14 {
        vol.write(format!("/TIGHT/G{i:02}"), b"").expect("filler");
    }
    // Reserve exactly one cluster, then fill everything else.
    vol.write("/ONE.BIN", b"x").expect("reserve one cluster");
    {
        let mut f = vol.create("/BULK.BIN").expect("bulk");
        let chunk = [0u8; 512];
        loop {
            if f.write_all(&chunk).is_err() {
                break;
            }
        }
        let _ = f.flush();
    }
    vol.remove_file("/ONE.BIN")
        .expect("free exactly one cluster");

    // 200-char name needs 2 fresh dir clusters; only 1 is free.
    let long = "M".repeat(200);
    let err = vol
        .write(format!("/TIGHT/{long}"), b"p")
        .expect_err("must fail");
    assert!(matches!(err, FsError::StorageFull), "got {err:?}");

    // The single free cluster must still be allocatable (not leaked), and
    // the directory must have no garbage tail.
    vol.write("/AFTER.BIN", &[7u8; 100])
        .expect("cluster not leaked");
    let count = vol.read_dir("/TIGHT").expect("read_dir").count();
    assert_eq!(count, 14, "directory gained phantom entries");
}

/// Locate the root-directory SFN record for `name8.3` (space-padded 11
/// bytes) in a raw FAT16 image and return its byte offset.
fn sfn_offset(image: &[u8], padded: &[u8; 11]) -> usize {
    image
        .windows(11)
        .position(|w| w == padded)
        .expect("SFN present in image")
}

/// R4 regression: a corrupt on-disk start cluster (way past the FAT) must
/// surface `Corrupt` on remove — never compute a FAT-cell offset landing in
/// the data area and write stray bytes there.
#[test]
fn out_of_range_start_cluster_fails_clean_not_stray_write() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.write("/EVIL.BIN", b"payload").expect("create");
    vol.write("/VICT.BIN", &[0xEEu8; 300]).expect("victim");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    // Forge EVIL.BIN's cluster_low (offset 26) far past max_cluster.
    let off = sfn_offset(&image, b"EVIL    BIN");
    image[off + 26..off + 28].copy_from_slice(&0xFFF0u16.to_le_bytes());

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let err = vol.remove_file("/EVIL.BIN").expect_err("must reject");
    assert!(
        matches!(err, FsError::Corrupt(_)),
        "expected Corrupt, got {err:?}"
    );
    assert_eq!(
        vol.read("/VICT.BIN").expect("victim readable"),
        vec![0xEEu8; 300],
        "stray write hit the data area"
    );
}

/// R5 regression: a cyclic FAT chain must error on append, not hang.
#[test]
fn cyclic_chain_append_errors_instead_of_hanging() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    // Two clusters (512 B each) so the chain has a link to corrupt.
    vol.write("/CYC.BIN", &[0xABu8; 700]).expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    let off = sfn_offset(&image, b"CYC     BIN");
    let first = u16::from_le_bytes([image[off + 26], image[off + 27]]);
    // FAT16 fixture: FAT0 at byte 512, cells are 2 bytes.
    let cell = |c: u16| 512 + 2 * usize::from(c);
    let second = u16::from_le_bytes([image[cell(first)], image[cell(first) + 1]]);
    assert!(second >= 2, "expected a chained second cluster");
    // Point the second cluster back at the first (cycle) in both copies.
    let fat_len = 24 * 512; // 24 sectors per FAT on this fixture
    for base in [cell(second), cell(second) + fat_len] {
        image[base..base + 2].copy_from_slice(&first.to_le_bytes());
    }

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let mut f = vol.open_rw("/CYC.BIN").expect("open_rw");
    f.seek(SeekFrom::End(0)).expect("seek end");
    // Must terminate with an error — pre-fix this looped forever.
    let err = f.write_all(&[1u8; 2000]).expect_err("append must fail");
    let _ = err; // any structured error is fine; the point is termination
}

/// R12 regression: on FAT12/16, SFN bytes 20-21 are the OS/2/NT EA handle,
/// not a cluster-high word. They must not be OR'd into the start cluster,
/// and a metadata rewrite must preserve them.
#[test]
fn fat16_ea_handle_not_treated_as_cluster_high() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.write("/EAFILE.BIN", b"ea-payload").expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    // Forge a nonzero EA handle (bytes 20-21 of the SFN record).
    let off = sfn_offset(&image, b"EAFILE  BIN");
    image[off + 20..off + 22].copy_from_slice(&0x0007u16.to_le_bytes());

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    // Pre-fix: data_cluster = 0x0007_0000 | low -> unreadable garbage.
    assert_eq!(
        vol.read("/EAFILE.BIN").expect("file readable"),
        b"ea-payload",
        "EA handle was OR'd into the start cluster"
    );
    // A metadata rewrite must keep the EA handle verbatim.
    vol.set_modified("/EAFILE.BIN", time::macros::datetime!(2024-06-01 12:00))
        .expect("set_modified");
    vol.flush().expect("flush");
    let image = vol.into_storage().into_inner();
    let off = sfn_offset(&image, b"EAFILE  BIN");
    assert_eq!(
        u16::from_le_bytes([image[off + 20], image[off + 21]]),
        0x0007,
        "EA handle destroyed by rewrite"
    );
}

/// R14 regression: DIR_NTRes (byte 12) carries Windows' lowercase-name
/// bits for LFN-less 8.3 aliases; every rewrite previously zeroed it,
/// permanently changing `readme.txt` to `README.TXT` on Windows.
#[test]
fn nt_res_lowercase_bits_survive_rewrites() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.write("/CASED.TXT", b"c").expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    let off = sfn_offset(&image, b"CASED   TXT");
    image[off + 12] = 0x18; // lowercase base + lowercase extension

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    vol.set_modified("/CASED.TXT", time::macros::datetime!(2024-06-01 12:00))
        .expect("set_modified");
    vol.flush().expect("flush");
    let image = vol.into_storage().into_inner();
    let off = sfn_offset(&image, b"CASED   TXT");
    assert_eq!(image[off + 12], 0x18, "NTRes zeroed by rewrite");
}

/// R13 regression: an LFN run that violates the spec's set structure
/// (here: lost last-entry flag) must fall back to the 8.3 alias like
/// Windows — not surface a garbled partial long name.
#[test]
fn broken_lfn_run_falls_back_to_short_name() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    // 20 chars -> 2 LFN slots + SFN (alias LONGNA~1.TXT).
    vol.write("/LongName Interop.txt", b"lfn").expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    let sfn = sfn_offset(&image, b"LONGNA~1TXT");
    // First LFN slot sits two slots before the SFN; clear its 0x40 flag.
    let lfn_first = sfn - 64;
    assert_eq!(image[lfn_first] & 0x40, 0x40, "expected last-flagged LFN");
    image[lfn_first] &= !0x40;

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let names: Vec<String> = vol
        .read_dir("/")
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string())
        .collect();
    assert!(
        names.iter().any(|n| n == "LONGNA~1.TXT"),
        "SFN fallback missing: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("Interop")),
        "garbled partial LFN surfaced: {names:?}"
    );
    assert_eq!(vol.read("/LONGNA~1.TXT").expect("open via alias"), b"lfn");
}

/// R20 regression: a valid date with time word 0x0000 is a legitimate
/// midnight creation stamp — it must decode as present and survive a
/// rewrite (previously read as absent, then encoded back as (0,0,0)).
#[test]
fn midnight_creation_stamp_survives_rewrite() {
    let vol = Volume::mount(medium(FAT16)).expect("mount");
    vol.write("/MIDNIGHT.BIN", b"m").expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    let off = sfn_offset(&image, b"MIDNIGHTBIN");
    // CrtTimeTenth=0, CrtTime=0 (midnight), keep the valid CrtDate.
    image[off + 13] = 0;
    image[off + 14..off + 16].copy_from_slice(&0u16.to_le_bytes());
    let date_word = u16::from_le_bytes([image[off + 16], image[off + 17]]);
    assert_ne!(date_word, 0, "fixture entry must carry a creation date");

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let created = vol.metadata("/MIDNIGHT.BIN").expect("meta").created();
    assert!(created.is_some(), "midnight creation stamp read as absent");
    assert_eq!(created.unwrap().time(), time::macros::time!(0:00));

    vol.set_modified("/MIDNIGHT.BIN", time::macros::datetime!(2024-06-01 12:00))
        .expect("set_modified");
    vol.flush().expect("flush");
    let image = vol.into_storage().into_inner();
    let off = sfn_offset(&image, b"MIDNIGHTBIN");
    assert_eq!(
        u16::from_le_bytes([image[off + 16], image[off + 17]]),
        date_word,
        "creation date erased by rewrite"
    );
}
