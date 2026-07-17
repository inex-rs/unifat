//! Shared parameterized integration suite: one set of assertions runs on
//! every writable fixture, so create / rename / drop-flush / truncate /
//! lock behaviour is exercised uniformly across FAT16/FAT32/ExFAT.

#[path = "common/mod.rs"]
mod common;

use common::{gunzip, medium, read_all};
use embedded_io::{Read, Seek, SeekFrom, Write};
use unifat::{Format, FsError, MemBlockDevice, Volume};

const FAT16: &[u8] = include_bytes!("fixtures/fat16-3m.img");
const FAT16_MSC: &[u8] = include_bytes!("fixtures/fat16-msc-8m.img");
const FAT32_GZ: &[u8] = include_bytes!("fixtures/fat32-34m.img.gz");
const EXFAT: &[u8] = include_bytes!("fixtures/exfat-1m.img");
const EXFAT_MC: &[u8] = include_bytes!("fixtures/exfat-mc-3m.img");
// 4096-byte-sector geometries (gzipped; mostly zeros).
const FAT16_4KS_GZ: &[u8] = include_bytes!("fixtures/fat16-4ks-32m.img.gz");
const EXFAT_4KS_GZ: &[u8] = include_bytes!("fixtures/exfat-4ks-8m.img.gz");

/// Writable golden images used by the suite.
struct Fixture {
    name: &'static str,
    image: &'static [u8],
    /// The FAT32 image is committed gzipped (34 MiB of mostly zeros).
    gzipped: bool,
    format: Format,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "fat16-3m",
        image: FAT16,
        gzipped: false,
        format: Format::Fat16,
    },
    Fixture {
        name: "fat16-msc-8m",
        image: FAT16_MSC,
        gzipped: false,
        format: Format::Fat16,
    },
    Fixture {
        name: "fat32-34m",
        image: FAT32_GZ,
        gzipped: true,
        format: Format::Fat32,
    },
    Fixture {
        name: "exfat-1m",
        image: EXFAT,
        gzipped: false,
        format: Format::ExFat,
    },
    Fixture {
        name: "exfat-mc-3m",
        image: EXFAT_MC,
        gzipped: false,
        format: Format::ExFat,
    },
    Fixture {
        name: "fat16-4ks-32m",
        image: FAT16_4KS_GZ,
        gzipped: true,
        format: Format::Fat16,
    },
    Fixture {
        name: "exfat-4ks-8m",
        image: EXFAT_4KS_GZ,
        gzipped: true,
        format: Format::ExFat,
    },
];

fn mount(fix: &Fixture) -> Volume<MemBlockDevice> {
    let dev = if fix.gzipped {
        MemBlockDevice::fixed(gunzip(fix.image))
    } else {
        medium(fix.image)
    };
    let vol = Volume::mount(dev).unwrap_or_else(|e| {
        panic!("mount {} failed: {e:?}", fix.name);
    });
    assert_eq!(
        vol.format(),
        fix.format,
        "fixture {} format mismatch",
        fix.name
    );
    vol
}

/// Unique absolute root path per fixture so pre-populated FAT roots do not collide.
fn root_name(fix: &Fixture, leaf: &str) -> String {
    // Keep names short-ish for FAT16 root slot pressure.
    let tag: String = fix
        .name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(6)
        .collect();
    format!("\\S_{tag}_{leaf}")
}

// ── Shared assertions ───────────────────────────────────────────────────

fn assert_create_write_readback(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "rw.bin");
    let payload = b"suite-create-write-readback";
    {
        let mut f = vol.create(&path).expect("create");
        f.write_all(payload).expect("write");
        f.flush().expect("flush");
    }
    let mut f = vol.open(&path).expect("reopen");
    assert_eq!(read_all(&mut f), payload, "{}: content mismatch", fix.name);
}

fn assert_drop_without_flush(fix: &Fixture) {
    let path = root_name(fix, "drop.bin");
    let payload = b"suite-drop-flush";
    let image = {
        let vol = mount(fix);
        {
            let mut f = vol.create(&path).expect("create");
            f.write_all(payload).expect("write");
            // no flush — StreamFile Drop must persist entry + bytes
        }
        vol.flush().expect("vol flush");
        vol.into_storage().into_inner()
    };
    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let mut f = vol.open(&path).expect("open after remount");
    assert_eq!(
        f.len(),
        payload.len() as u64,
        "{}: length lost on drop",
        fix.name
    );
    assert_eq!(
        read_all(&mut f),
        payload,
        "{}: content lost on drop",
        fix.name
    );
}

fn assert_mkdir_nested_file(fix: &Fixture) {
    let vol = mount(fix);
    let dir = root_name(fix, "d");
    let nested = format!("{dir}\\nested.dat");
    vol.create_dir(&dir).expect("mkdir");
    {
        let mut f = vol.create(&nested).expect("create nested");
        f.write_all(b"nested").expect("write");
    }
    assert!(vol.metadata(&dir).expect("stat dir").is_dir());
    assert_eq!(vol.read(&nested).expect("read nested"), b"nested");
}

fn assert_create_dir_all(fix: &Fixture) {
    let vol = mount(fix);
    let base = root_name(fix, "tree");
    let deep = format!("{base}\\a\\b\\c");
    vol.create_dir_all(&deep).expect("create_dir_all");
    assert!(vol.metadata(format!("{base}\\a")).unwrap().is_dir());
    assert!(vol.metadata(format!("{base}\\a\\b")).unwrap().is_dir());
    assert!(vol.metadata(&deep).unwrap().is_dir());
}

fn assert_rename_roundtrip(fix: &Fixture) {
    let vol = mount(fix);
    let a = root_name(fix, "a.txt");
    let b = root_name(fix, "b.txt");
    {
        let mut f = vol.create(&a).expect("create");
        f.write_all(b"rename-me").expect("write");
    }
    vol.rename(&a, &b).expect("rename");
    assert!(
        vol.open(&a).is_err(),
        "{}: old name still present",
        fix.name
    );
    assert_eq!(vol.read(&b).expect("read renamed"), b"rename-me");
}

fn assert_rename_across_dirs(fix: &Fixture) {
    let vol = mount(fix);
    let src = root_name(fix, "src");
    let dst = root_name(fix, "dst");
    vol.create_dir_all(format!("{src}/inner"))
        .expect("src/inner");
    // Unique dst when fixture already has content; avoid colliding with
    // pre-existing names on fat16-msc if any.
    if vol.metadata(&dst).is_ok() {
        // Should not happen with unique root_name tags.
        panic!("{}: dst {} already exists", fix.name, dst);
    }
    vol.create_dir(&dst).expect("dst");
    let file = format!("{src}\\inner\\payload.bin");
    {
        let mut f = vol.create(&file).expect("create");
        f.write_all(b"moved").expect("write");
    }
    vol.rename(format!("{src}\\inner"), format!("{dst}\\inner"))
        .expect("rename dir");
    assert_eq!(
        vol.read(format!("{dst}\\inner\\payload.bin"))
            .expect("read"),
        b"moved",
        "{}: nested content lost",
        fix.name
    );
}

fn assert_truncate_shrinks(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "trunc.bin");
    let data = vec![0xABu8; 2000];
    {
        let mut f = vol.create(&path).expect("create");
        f.write_all(&data).expect("write");
        f.flush().expect("flush");
    }
    {
        let mut f = vol.open_rw(&path).expect("open_rw");
        f.set_len(100).expect("set_len");
    }
    let got = vol.read(&path).expect("read");
    assert_eq!(got.len(), 100, "{}: truncate length", fix.name);
    assert_eq!(&got[..], &data[..100], "{}: truncate content", fix.name);
}

fn assert_error_variant_taxonomy(fix: &Fixture) {
    let vol = mount(fix);
    let d = root_name(fix, "taxdir");
    let f = root_name(fix, "taxfile");
    vol.create_dir(&d).expect("mkdir");
    vol.write(&f, b"x").expect("file");
    vol.write(format!("{d}\\inner"), b"i").expect("inner");

    // remove_dir: non-empty / on-file / missing / then Ok when empty.
    assert!(
        matches!(vol.remove_dir(&d), Err(FsError::DirectoryNotEmpty)),
        "{}: remove_dir(non-empty)",
        fix.name
    );
    assert!(
        matches!(vol.remove_dir(&f), Err(FsError::NotADirectory)),
        "{}: remove_dir(file)",
        fix.name
    );
    assert!(
        matches!(
            vol.remove_dir(root_name(fix, "absent")),
            Err(FsError::NotFound)
        ),
        "{}: remove_dir(missing)",
        fix.name
    );

    // Type-confusion variants.
    assert!(
        matches!(vol.read_dir(&f), Err(FsError::NotADirectory)),
        "{}: read_dir(file)",
        fix.name
    );
    assert!(
        matches!(vol.open(&d), Err(FsError::IsADirectory)),
        "{}: open(dir)",
        fix.name
    );
    assert!(
        matches!(vol.write(&d, b"no"), Err(FsError::IsADirectory)),
        "{}: write(dir)",
        fix.name
    );
    assert!(
        matches!(vol.remove_file(&d), Err(FsError::IsADirectory)),
        "{}: remove_file(dir)",
        fix.name
    );

    // rename onto a DIFFERENT existing entry: AlreadyExists, both intact.
    let g = root_name(fix, "taxfile2");
    vol.write(&g, b"y").expect("second file");
    assert!(
        matches!(vol.rename(&f, &g), Err(FsError::AlreadyExists)),
        "{}: rename onto existing",
        fix.name
    );
    assert_eq!(vol.read(&f).expect("from intact"), b"x");
    assert_eq!(vol.read(&g).expect("to intact"), b"y");

    // create() while the same path is open read-only: FileLocked.
    {
        let _ro = vol.open(&f).expect("ro open");
        assert!(
            matches!(vol.create(&f), Err(FsError::FileLocked)),
            "{}: create while open",
            fix.name
        );
    }

    // Cleanup path: remove_dir succeeds once emptied.
    vol.remove_file(format!("{d}\\inner")).expect("clear");
    vol.remove_dir(&d).expect("remove_dir(empty) must succeed");
}

fn assert_api_semantics_batch(fix: &Fixture) {
    let vol = mount(fix);

    // R6: directories report len 0 / is-empty parity on BOTH backends.
    let d = root_name(fix, "mdir");
    vol.create_dir(&d).expect("mkdir");
    vol.write(format!("{d}\\child"), b"c").expect("child");
    let meta = vol.metadata(&d).expect("dir meta");
    assert_eq!(meta.len(), 0, "{}: directory len must be 0", fix.name);

    // R7: create_dir_all whose FINAL component is an existing file.
    let f = root_name(fix, "plainfile");
    vol.write(&f, b"data").expect("file");
    let err = vol.create_dir_all(&f).expect_err("must fail on a file");
    assert!(
        matches!(err, FsError::NotADirectory),
        "{}: expected NotADirectory, got {err:?}",
        fix.name
    );
    assert!(!vol.metadata(&f).expect("still there").is_dir());

    // R8: self-rename is a no-op success; case-only rename respells.
    let a = root_name(fix, "Case.bin");
    vol.write(&a, b"case-data").expect("create");
    vol.rename(&a, &a).expect("self-rename is Ok");
    let upper = a.to_uppercase();
    vol.rename(&a, &upper).expect("case-only rename");
    assert_eq!(vol.read(&upper).expect("read"), b"case-data");
    let leaf = upper.rsplit('\\').next().unwrap().to_string();
    let names: Vec<String> = vol
        .read_dir("/")
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string())
        .collect();
    assert!(
        names.contains(&leaf),
        "{}: respelled name not listed: {names:?}",
        fix.name
    );

    // R21: seeking before byte 0 errors (embedded-io contract).
    let mut h = vol.open_rw(&upper).expect("open");
    h.seek(SeekFrom::Start(2)).expect("seek");
    assert!(
        h.seek(SeekFrom::Current(-100)).is_err(),
        "{}: seek before 0 must error",
        fix.name
    );
    // The failed seek must not have moved the cursor.
    assert_eq!(h.stream_position().expect("pos"), 2);
}

fn assert_remove_dir_all_root_rejected_without_damage(fix: &Fixture) {
    let vol = mount(fix);
    let keep = root_name(fix, "keep.bin");
    vol.write(&keep, b"precious").expect("create");
    let before = vol.read_dir("/").expect("read_dir").count();
    for root in ["\\", "/", ""] {
        let err = vol
            .remove_dir_all(root)
            .expect_err("root removal must fail");
        assert!(
            matches!(err, FsError::InvalidInput),
            "{}: expected InvalidInput, got {err:?}",
            fix.name
        );
    }
    // The refusal must come BEFORE any deletion.
    let after = vol.read_dir("/").expect("read_dir").count();
    assert_eq!(before, after, "{}: root children were deleted", fix.name);
    assert_eq!(vol.read(&keep).expect("keep intact"), b"precious");
}

fn assert_file_metadata_matches_and_is_live(fix: &Fixture) {
    use unifat::Attributes;

    let vol = mount(fix);
    let path = root_name(fix, "fmeta.bin");
    vol.write(&path, b"0123456789").expect("create");
    vol.set_attributes(
        &path,
        Attributes {
            hidden: true,
            ..Attributes::default()
        },
    )
    .expect("set attrs");

    // An open handle's metadata matches the path-level metadata.
    let path_meta = vol.metadata(&path).expect("path metadata");
    let mut f = vol.open_rw(&path).expect("open_rw");
    let fm = f.metadata();
    assert_eq!(fm.len(), 10, "{}: len", fix.name);
    assert!(!fm.is_dir(), "{}: file is not a dir", fix.name);
    assert!(fm.is_hidden(), "{}: attrs surfaced", fix.name);
    assert_eq!(fm.len(), path_meta.len(), "{}: len parity", fix.name);
    assert_eq!(
        fm.attributes(),
        path_meta.attributes(),
        "{}: attrs parity",
        fix.name
    );

    // Length is live: growing through the handle updates its metadata.
    f.seek(SeekFrom::End(0)).expect("seek end");
    f.write_all(b"more").expect("append");
    assert_eq!(
        f.metadata().len(),
        14,
        "{}: live len after append",
        fix.name
    );
}

fn assert_set_attributes_roundtrip(fix: &Fixture) {
    use unifat::Attributes;

    let vol = mount(fix);
    let path = root_name(fix, "attr.bin");
    vol.write(&path, b"attrs").expect("create");

    let ro_hidden = Attributes {
        read_only: true,
        hidden: true,
        ..Attributes::default()
    };
    vol.set_attributes(&path, ro_hidden).expect("set attrs");
    let meta = vol.metadata(&path).expect("meta");
    assert_eq!(meta.attributes(), ro_hidden, "{}", fix.name);
    assert!(meta.is_read_only() && meta.is_hidden(), "{}", fix.name);

    // The read-only bit must now bite.
    assert!(
        matches!(vol.open_rw(&path), Err(FsError::ReadOnlyFile)),
        "{}: open_rw on read-only",
        fix.name
    );
    assert!(
        matches!(vol.remove_file(&path), Err(FsError::ReadOnlyFile)),
        "{}: remove on read-only",
        fix.name
    );
    // Contents unaffected, and clearing the bit restores writability.
    assert_eq!(vol.read(&path).expect("read"), b"attrs", "{}", fix.name);
    vol.set_attributes(&path, Attributes::default())
        .expect("clear attrs");
    assert_eq!(
        vol.metadata(&path).expect("meta").attributes(),
        Attributes::default(),
        "{}",
        fix.name
    );
    vol.remove_file(&path).expect("remove after clearing");

    // Directories carry attributes too (and stay directories).
    let dir = root_name(fix, "attrdir");
    vol.create_dir(&dir).expect("mkdir");
    vol.set_attributes(
        &dir,
        Attributes {
            hidden: true,
            ..Attributes::default()
        },
    )
    .expect("set dir attrs");
    let meta = vol.metadata(&dir).expect("dir meta");
    assert!(meta.is_dir() && meta.is_hidden(), "{}", fix.name);

    // A held handle blocks attribute changes, like the time setters.
    let file2 = root_name(fix, "attr2.bin");
    vol.write(&file2, b"x").expect("create 2");
    let handle = vol.open(&file2).expect("ro handle");
    assert!(
        matches!(
            vol.set_attributes(&file2, ro_hidden),
            Err(FsError::FileLocked)
        ),
        "{}: set_attributes while open",
        fix.name
    );
    drop(handle);
}

fn assert_set_len_grows_with_zeros(fix: &Fixture) {
    let path = root_name(fix, "grow.bin");
    let body = vec![0xCDu8; 300];
    let image = {
        let vol = mount(fix);
        vol.write(&path, &body).expect("create");
        {
            let mut f = vol.open_rw(&path).expect("open_rw");
            f.set_len(5000).expect("grow");
            assert_eq!(f.len(), 5000, "{}: len after grow", fix.name);
            // Shrink-then-grow keeps working on the same handle.
            f.set_len(200).expect("shrink");
            f.set_len(4000).expect("re-grow");
        }
        vol.flush().expect("flush");
        vol.into_storage().into_inner()
    };
    // The extension must read as zeros, in-session and after remount
    // (FAT zero-fills the clusters; ExFAT tracks ValidDataLength).
    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    let got = vol.read(&path).expect("read");
    assert_eq!(got.len(), 4000, "{}: grown length", fix.name);
    assert_eq!(&got[..200], &body[..200], "{}: prefix intact", fix.name);
    assert!(
        got[200..].iter().all(|&b| b == 0),
        "{}: extension must read as zeros",
        fix.name
    );
}

fn assert_case_insensitive_open(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "Case.TXT");
    {
        let mut f = vol.create(&path).expect("create");
        f.write_all(b"ci").expect("write");
    }
    // Fold only the leaf; parent path keeps fixture-specific casing.
    let upper = path.to_ascii_uppercase();
    let lower = path.to_ascii_lowercase();
    assert_eq!(vol.read(&upper).expect("upper"), b"ci");
    assert_eq!(vol.read(&lower).expect("lower"), b"ci");
}

fn assert_ro_shared_rw_exclusive(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "lock.bin");
    {
        let mut f = vol.create(&path).expect("create");
        f.write_all(b"lock").expect("write");
    }
    let _ro1 = vol.open(&path).expect("ro1");
    let _ro2 = vol.open(&path).expect("ro2 shared");
    let err = vol.open_rw(&path).expect_err("rw while ro open");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: expected FileLocked, got {err:?}",
        fix.name
    );
}

fn assert_lock_is_case_insensitive(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "clock.bin");
    {
        let mut f = vol.create(&path).expect("create");
        f.write_all(b"cl").expect("write");
    }
    // Names resolve case-insensitively, so locks must too — a case
    // variant must not obtain a second "exclusive" writable handle.
    let _rw = vol.open_rw(path.to_ascii_uppercase()).expect("rw upper");
    let err = vol
        .open_rw(path.to_ascii_lowercase())
        .expect_err("case-variant rw while rw open");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: expected FileLocked, got {err:?}",
        fix.name
    );
    let err = vol.open(&path).expect_err("case-variant ro while rw open");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: expected FileLocked for ro, got {err:?}",
        fix.name
    );
}

fn assert_write_past_eof_zero_fills(fix: &Fixture) {
    let vol = mount(fix);
    // Dirty some free clusters with a sentinel, then free them so the
    // hole below lands on recycled clusters.
    let dirty = root_name(fix, "dirty.bin");
    vol.write(&dirty, &[0xAA; 8192]).expect("dirty");
    vol.remove_file(&dirty).expect("rm dirty");
    // Seek past EOF and write: the gap must read back as zeros, never
    // as the deleted file's contents.
    let path = root_name(fix, "hole.bin");
    let mut f = vol.create(&path).expect("create");
    f.seek(SeekFrom::Start(4096)).expect("seek");
    f.write_all(b"END").expect("write");
    drop(f);
    let data = vol.read(&path).expect("read");
    assert_eq!(data.len(), 4099, "{}: hole file length", fix.name);
    assert!(
        data[..4096].iter().all(|&b| b == 0),
        "{}: gap leaks stale cluster data",
        fix.name
    );
    assert_eq!(&data[4096..], b"END", "{}: tail payload", fix.name);
}

fn assert_multi_cluster_span_roundtrip(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "span.bin");
    // 64 KiB deterministic pattern: spans many clusters on every fixture
    // (cluster sizes range 512 B – 4 KiB), exercising the coalesced
    // contiguous-run I/O path in StreamFile.
    let data: Vec<u8> = (0..65536u32)
        .map(|i| (i.wrapping_mul(31) >> 3) as u8)
        .collect();
    vol.write(&path, &data).expect("write span");
    assert_eq!(
        vol.read(&path).expect("read span"),
        data,
        "{}: bulk",
        fix.name
    );
    // Odd-offset window crossing several cluster boundaries.
    let mut f = vol.open(&path).expect("open");
    f.seek(SeekFrom::Start(1234)).expect("seek");
    let mut window = vec![0u8; 10_000];
    let mut done = 0;
    while done < window.len() {
        let n = f.read(&mut window[done..]).expect("read window");
        assert!(n > 0, "{}: unexpected EOF at {done}", fix.name);
        done += n;
    }
    assert_eq!(
        window[..],
        data[1234..1234 + 10_000],
        "{}: odd-offset window",
        fix.name
    );
}

fn assert_two_growth_writes_same_handle(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "grow2.bin");
    // 64 KiB spans many clusters on every fixture; the second write forces a
    // second `extend`, which must link from the chain TAIL, not `first`.
    let body = vec![0x5Au8; 64 * 1024];
    {
        let mut f = vol.create(&path).expect("create");
        f.write_all(&body).expect("first write");
        f.write_all(b"MORE").expect("second growth write");
        f.flush().expect("flush");
    }
    let got = vol.read(&path).expect("read back");
    assert_eq!(got.len(), body.len() + 4, "{}: grown length", fix.name);
    assert!(
        got[..body.len()].iter().all(|&b| b == 0x5A),
        "{}: body corrupted by growth",
        fix.name
    );
    assert_eq!(&got[body.len()..], b"MORE", "{}: appended tail", fix.name);
}

fn assert_append_after_reopen(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "appnd.bin");
    let body: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
    vol.write(&path, &body).expect("initial write");
    {
        let mut f = vol.open_rw(&path).expect("open_rw");
        f.seek(SeekFrom::End(0)).expect("seek end");
        f.write_all(b"TAIL").expect("append");
        f.flush().expect("flush");
    }
    let got = vol.read(&path).expect("read back");
    assert_eq!(got.len(), body.len() + 4, "{}: appended length", fix.name);
    assert_eq!(
        &got[..body.len()],
        &body[..],
        "{}: body corrupted by append",
        fix.name
    );
    assert_eq!(&got[body.len()..], b"TAIL", "{}: tail", fix.name);
}

fn assert_create_empty_then_write_later(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "empty.bin");
    drop(vol.create(&path).expect("create"));
    assert_eq!(
        vol.metadata(&path).expect("meta").len(),
        0,
        "{}: fresh file not empty",
        fix.name
    );
    // Recreate over the existing file (used to leak a cluster per call on
    // FAT), then write through a plain rw handle.
    drop(vol.create(&path).expect("recreate over existing"));
    {
        let mut f = vol.open_rw(&path).expect("open_rw");
        f.write_all(b"late").expect("write");
    }
    assert_eq!(vol.read(&path).expect("read"), b"late", "{}", fix.name);
}

fn assert_rename_into_own_subtree_rejected(fix: &Fixture) {
    let vol = mount(fix);
    let base = root_name(fix, "cyc");
    vol.create_dir_all(format!("{base}\\in")).expect("mkdirs");
    vol.write(format!("{base}\\f.bin"), b"keep").expect("file");

    let err = vol
        .rename(&base, format!("{base}\\in\\loop"))
        .expect_err("rename into own subtree must be rejected");
    assert!(
        matches!(err, FsError::InvalidInput),
        "{}: expected InvalidInput, got {err:?}",
        fix.name
    );
    // Case-variant descendant must be caught by the fold-aware check too.
    let err = vol
        .rename(&base, format!("{}\\in\\loop", base.to_ascii_uppercase()))
        .expect_err("case-variant subtree rename must be rejected");
    assert!(
        matches!(err, FsError::InvalidInput),
        "{}: expected InvalidInput (case variant), got {err:?}",
        fix.name
    );
    // Tree must be fully intact.
    assert!(vol.metadata(&base).expect("base").is_dir(), "{}", fix.name);
    assert!(
        vol.metadata(format!("{base}\\in")).expect("in").is_dir(),
        "{}",
        fix.name
    );
    assert_eq!(
        vol.read(format!("{base}\\f.bin")).expect("read"),
        b"keep",
        "{}",
        fix.name
    );
}

fn assert_rename_remove_blocked_while_open(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "held.bin");
    let dst = root_name(fix, "held2.bin");
    vol.write(&path, b"held").expect("create");

    let handle = vol.open_rw(&path).expect("open_rw");
    let err = vol.rename(&path, &dst).expect_err("rename while open");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: rename expected FileLocked, got {err:?}",
        fix.name
    );
    let err = vol.remove_file(&path).expect_err("remove while open");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: remove expected FileLocked, got {err:?}",
        fix.name
    );
    drop(handle);

    // A read-only handle must also block both (its clusters/slot would go stale).
    let handle = vol.open(&path).expect("open ro");
    assert!(
        matches!(vol.rename(&path, &dst), Err(FsError::FileLocked)),
        "{}: rename with ro handle",
        fix.name
    );
    assert!(
        matches!(vol.remove_file(&path), Err(FsError::FileLocked)),
        "{}: remove with ro handle",
        fix.name
    );
    drop(handle);

    vol.rename(&path, &dst).expect("rename after close");
    assert_eq!(vol.read(&dst).expect("read"), b"held", "{}", fix.name);
    vol.remove_file(&dst).expect("remove after close");
}

fn assert_dir_ops_blocked_while_child_open(fix: &Fixture) {
    let vol = mount(fix);
    let dir = root_name(fix, "hdir");
    let dst = root_name(fix, "hdir2");
    vol.create_dir(&dir).expect("mkdir");
    let file = format!("{dir}\\f.bin");
    vol.write(&file, b"x").expect("file");

    let handle = vol.open(&file).expect("ro handle on child");
    let err = vol
        .rename(&dir, &dst)
        .expect_err("dir rename with open child");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: dir rename expected FileLocked, got {err:?}",
        fix.name
    );
    let err = vol
        .remove_dir_all(&dir)
        .expect_err("remove_dir_all with open child");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: remove_dir_all expected FileLocked, got {err:?}",
        fix.name
    );
    // The failed attempts must not have deleted anything.
    assert_eq!(vol.read(&file).expect("still there"), b"x", "{}", fix.name);
    drop(handle);

    vol.rename(&dir, &dst).expect("dir rename after close");
    assert_eq!(
        vol.read(format!("{dst}\\f.bin")).expect("read moved"),
        b"x",
        "{}",
        fix.name
    );
    vol.remove_dir_all(&dst).expect("remove after close");
    assert!(vol.metadata(&dst).is_err(), "{}: dir gone", fix.name);
}

fn assert_relative_paths_resolve_from_root(fix: &Fixture) {
    let vol = mount(fix);
    let abs = root_name(fix, "rel.bin");
    let rel = abs.trim_start_matches('\\').to_string();

    // One-component relative path — regression: FAT mapped the missing
    // parent to IsADirectory while ExFAT accepted it.
    vol.write(&rel, b"rel").expect("create via relative path");
    assert_eq!(
        vol.read(&abs).expect("read via absolute path"),
        b"rel",
        "{}",
        fix.name
    );

    // Both spellings must share one handle-lock key.
    let handle = vol.open_rw(&rel).expect("open_rw via relative");
    let err = vol.open(&abs).expect_err("ro via absolute while rw open");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: expected FileLocked, got {err:?}",
        fix.name
    );
    drop(handle);
    vol.remove_file(&rel).expect("remove via relative");
    assert!(vol.metadata(&abs).is_err(), "{}", fix.name);
}

fn assert_name_length_boundaries(fix: &Fixture) {
    let vol = mount(fix);
    let dir = root_name(fix, "nlen");
    vol.create_dir(&dir).expect("mkdir");

    // Exactly 255 UTF-16 units: the format limit on both FAT and ExFAT.
    let max_name = format!("{}.txt", "a".repeat(251));
    let path = format!("{dir}\\{max_name}");
    vol.write(&path, b"max")
        .expect("255-unit name must be accepted");
    assert_eq!(vol.read(&path).expect("read"), b"max", "{}", fix.name);
    let listed = vol
        .read_dir(&dir)
        .expect("read_dir")
        .any(|e| e.expect("entry").file_name() == max_name);
    assert!(listed, "{}: 255-unit name missing from listing", fix.name);

    // 256 units: rejected cleanly at the API boundary.
    let over = format!("{dir}\\{}.txt", "a".repeat(252));
    let err = vol.create(&over).expect_err("256-unit name");
    assert!(
        matches!(err, FsError::InvalidInput),
        "{}: expected InvalidInput, got {err:?}",
        fix.name
    );

    // The limit is UTF-16 units, not bytes: 200 Cyrillic chars are 400
    // UTF-8 bytes but only 200 units — legal on both formats.
    let cyr = format!("{dir}\\{}", "\u{431}".repeat(200));
    vol.write(&cyr, b"cyr")
        .expect("200-unit non-ASCII name must be accepted");
    assert_eq!(vol.read(&cyr).expect("read cyr"), b"cyr", "{}", fix.name);
}

fn assert_huge_seek_write_errors_not_panics(fix: &Fixture) {
    let vol = mount(fix);
    let path = root_name(fix, "huge.bin");
    let mut f = vol.create(&path).expect("create");

    // Position arithmetic must never overflow into a panic; the write
    // must surface a clean error instead.
    f.seek(SeekFrom::Start(u64::MAX)).expect("seek u64::MAX");
    f.write(b"abc")
        .expect_err("write at u64::MAX must error, not panic");

    // FAT files cap at 4 GiB - 1; a cursor at the cap must report
    // FileTooLarge, never Ok(0) (write_all panics on zero-length writes).
    if fix.format != Format::ExFat {
        f.seek(SeekFrom::Start(u64::from(u32::MAX)))
            .expect("seek to cap");
        let err = f.write(b"abc").expect_err("write at the 4 GiB cap");
        assert!(
            matches!(err, unifat::FileError::FileTooLarge),
            "{}: expected FileTooLarge, got {err:?}",
            fix.name
        );
    }
}

fn assert_rename_preserves_timestamps(fix: &Fixture) {
    use time::macros::datetime;

    let vol = mount(fix);
    let a = root_name(fix, "stamp.bin");
    let b = root_name(fix, "stamp2.bin");
    vol.write(&a, b"s").expect("create");
    let c = datetime!(2019-03-04 05:06:07);
    let m = datetime!(2020-07-08 09:10:12);
    vol.set_created(&a, c).expect("set_created");
    vol.set_modified(&a, m).expect("set_modified");

    vol.rename(&a, &b).expect("rename");
    let meta = vol.metadata(&b).expect("meta");
    assert_eq!(meta.created(), Some(c), "{}: created lost", fix.name);
    assert_eq!(meta.modified(), Some(m), "{}: modified lost", fix.name);
}

fn assert_path_level_time_setters(fix: &Fixture) {
    use time::macros::{date, datetime};

    let vol = mount(fix);
    let path = root_name(fix, "times.bin");
    vol.write(&path, b"t").expect("create");

    // Values chosen inside on-disk resolution: modified = 2 s steps,
    // created = 10 ms steps, accessed = date only.
    let c = datetime!(2019-01-02 03:04:05.50);
    let m = datetime!(2020-05-05 10:20:30);
    let a = date!(2021 - 12 - 24);
    vol.set_created(&path, c).expect("set_created");
    vol.set_modified(&path, m).expect("set_modified");
    vol.set_accessed(&path, a).expect("set_accessed");

    let meta = vol.metadata(&path).expect("metadata");
    assert_eq!(meta.created(), Some(c), "{}: created", fix.name);
    assert_eq!(meta.modified(), Some(m), "{}: modified", fix.name);
    assert_eq!(meta.accessed(), Some(a), "{}: accessed", fix.name);

    // Directories carry stamps too.
    let dir = root_name(fix, "tdir");
    vol.create_dir(&dir).expect("mkdir");
    vol.set_modified(&dir, m).expect("set_modified dir");
    assert_eq!(
        vol.metadata(&dir).expect("dir meta").modified(),
        Some(m),
        "{}: dir modified",
        fix.name
    );

    // An open handle owns the entry — path-level setters must refuse.
    let handle = vol.open_rw(&path).expect("open_rw");
    let err = vol.set_modified(&path, m).expect_err("set while open");
    assert!(
        matches!(err, FsError::FileLocked),
        "{}: expected FileLocked, got {err:?}",
        fix.name
    );
    drop(handle);
    vol.set_modified(&path, m).expect("set after close");

    // The root has no directory entry to stamp.
    let err = vol.set_modified("\\", m).expect_err("root");
    assert!(
        matches!(err, FsError::PermissionDenied),
        "{}: expected PermissionDenied for root, got {err:?}",
        fix.name
    );
}

// ── Generated tests per fixture ─────────────────────────────────────────

macro_rules! suite_tests {
    ($($fix:ident => $idx:expr),* $(,)?) => {
        $(
            mod $fix {
                use super::*;
                const FIX: &Fixture = &FIXTURES[$idx];

                #[test]
                fn create_write_readback() {
                    assert_create_write_readback(FIX);
                }
                #[test]
                fn drop_without_flush() {
                    assert_drop_without_flush(FIX);
                }
                #[test]
                fn mkdir_nested_file() {
                    assert_mkdir_nested_file(FIX);
                }
                #[test]
                fn create_dir_all() {
                    assert_create_dir_all(FIX);
                }
                #[test]
                fn rename_roundtrip() {
                    assert_rename_roundtrip(FIX);
                }
                #[test]
                fn rename_across_dirs() {
                    assert_rename_across_dirs(FIX);
                }
                #[test]
                fn truncate_shrinks() {
                    assert_truncate_shrinks(FIX);
                }
                #[test]
                fn case_insensitive_open() {
                    assert_case_insensitive_open(FIX);
                }
                #[test]
                fn ro_shared_rw_exclusive() {
                    assert_ro_shared_rw_exclusive(FIX);
                }
                #[test]
                fn lock_is_case_insensitive() {
                    assert_lock_is_case_insensitive(FIX);
                }
                #[test]
                fn write_past_eof_zero_fills() {
                    assert_write_past_eof_zero_fills(FIX);
                }
                #[test]
                fn multi_cluster_span_roundtrip() {
                    assert_multi_cluster_span_roundtrip(FIX);
                }
                #[test]
                fn path_level_time_setters() {
                    assert_path_level_time_setters(FIX);
                }
                #[test]
                fn two_growth_writes_same_handle() {
                    assert_two_growth_writes_same_handle(FIX);
                }
                #[test]
                fn append_after_reopen() {
                    assert_append_after_reopen(FIX);
                }
                #[test]
                fn create_empty_then_write_later() {
                    assert_create_empty_then_write_later(FIX);
                }
                #[test]
                fn rename_into_own_subtree_rejected() {
                    assert_rename_into_own_subtree_rejected(FIX);
                }
                #[test]
                fn rename_remove_blocked_while_open() {
                    assert_rename_remove_blocked_while_open(FIX);
                }
                #[test]
                fn dir_ops_blocked_while_child_open() {
                    assert_dir_ops_blocked_while_child_open(FIX);
                }
                #[test]
                fn rename_preserves_timestamps() {
                    assert_rename_preserves_timestamps(FIX);
                }
                #[test]
                fn huge_seek_write_errors_not_panics() {
                    assert_huge_seek_write_errors_not_panics(FIX);
                }
                #[test]
                fn name_length_boundaries() {
                    assert_name_length_boundaries(FIX);
                }
                #[test]
                fn relative_paths_resolve_from_root() {
                    assert_relative_paths_resolve_from_root(FIX);
                }
                #[test]
                fn set_len_grows_with_zeros() {
                    assert_set_len_grows_with_zeros(FIX);
                }
                #[test]
                fn set_attributes_roundtrip() {
                    assert_set_attributes_roundtrip(FIX);
                }
                #[test]
                fn file_metadata_matches_and_is_live() {
                    assert_file_metadata_matches_and_is_live(FIX);
                }
                #[test]
                fn remove_dir_all_root_rejected_without_damage() {
                    assert_remove_dir_all_root_rejected_without_damage(FIX);
                }
                #[test]
                fn api_semantics_batch() {
                    assert_api_semantics_batch(FIX);
                }
                #[test]
                fn error_variant_taxonomy() {
                    assert_error_variant_taxonomy(FIX);
                }
            }
        )*
    };
}

suite_tests! {
    fat16_3m => 0,
    fat16_msc => 1,
    fat32 => 2,
    exfat_1m => 3,
    exfat_mc => 4,
    fat16_4ks => 5,
    exfat_4ks => 6,
}
