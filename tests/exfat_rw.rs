//! ExFAT round-trip coverage through the unified [`Volume`] API.
//!
//! The fixture is a freshly-`mkfs.exfat`'d empty 1 MiB volume (512 B
//! sectors, 4 KiB clusters). Every test drives create / write / read /
//! remove through `Volume`, which exercises the ExFAT backend's
//! interior-mutability borrow model, allocation bitmap, FAT-chain
//! growth, directory-entry compose/append, and — crucially — the
//! flush-on-drop path that persists a file's length and entry CRC when
//! a writable handle is dropped without an explicit `flush()`.

#[path = "common/mod.rs"]
mod common;

use common::{FixedClock, medium, read_all};
use embedded_io::{Seek, SeekFrom, Write};
use time::macros::datetime;
use unifat::{Format, FsOptions, MemBlockDevice, Volume};

const EXFAT: &[u8] = include_bytes!("fixtures/exfat-1m.img");
// 3 MiB, 512 B clusters → 6072 clusters, a 759-byte allocation bitmap
// spanning 2 clusters / 2 scan windows.
const EXFAT_MC: &[u8] = include_bytes!("fixtures/exfat-mc-3m.img");

/// Independent (test-local) implementation of the exFAT NameHash from the
/// spec, so the driver's hash is cross-checked rather than self-compared.
fn spec_name_hash(upcased: &str) -> u16 {
    let mut hash: u16 = 0;
    for unit in upcased.encode_utf16() {
        for b in unit.to_le_bytes() {
            hash = ((hash & 1) << 15) | (hash >> 1);
            hash = hash.wrapping_add(u16::from(b));
        }
    }
    hash
}

/// Locate the entry set whose File Name entry carries exactly `name`
/// (≤ 15 UTF-16 units — a single FileName entry) and return the byte
/// offset of its primary File entry in `image`.
fn set_offset_in_image(image: &[u8], name: &str) -> usize {
    assert!(name.encode_utf16().count() <= 15, "single-entry names only");
    let mut pat = vec![0xC1u8, 0x00]; // FileName entry type + flags
    for u in name.encode_utf16() {
        pat.extend_from_slice(&u.to_le_bytes());
    }
    let pos = image
        .windows(pat.len())
        .position(|w| w == pat)
        .expect("file-name entry present in image");
    let set = pos - 64; // File, Stream Extension, then the name entry
    assert_eq!(image[set], 0x85, "primary file entry type");
    assert_eq!(image[set + 32], 0xC0, "stream extension entry type");
    set
}

/// The NameHash field of `name`'s Stream Extension.
fn name_hash_in_image(image: &[u8], name: &str) -> u16 {
    let stream_off = set_offset_in_image(image, name) + 32;
    u16::from_le_bytes([image[stream_off + 4], image[stream_off + 5]])
}

/// Recompute and store the entry-set CRC16 at `set` (independent,
/// test-local implementation of the spec algorithm).
fn fix_set_checksum(image: &mut [u8], set: usize) {
    let sec = image[set + 1] as usize;
    let set_len = (1 + sec) * 32;
    image[set + 2] = 0;
    image[set + 3] = 0;
    let mut sum: u16 = 0;
    for (i, &b) in image[set..set + set_len].iter().enumerate() {
        if i == 2 || i == 3 {
            continue;
        }
        sum = ((sum & 1) << 15) | (sum >> 1);
        sum = sum.wrapping_add(u16::from(b));
    }
    image[set + 2..set + 4].copy_from_slice(&sum.to_le_bytes());
}

/// A file's on-disk size fields must follow the spec: DataLength is the
/// logical byte size (what Windows shows), ValidDataLength the
/// initialized prefix. Previously DL was written as the cluster-rounded
/// allocation (a 10-byte file showed as 4096 bytes on Windows).
#[test]
fn data_length_is_logical_size_on_disk() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    {
        let mut f = vol.create("DlProbe.bin").expect("create");
        f.write_all(&[7u8; 10]).expect("write");
        f.flush().expect("flush");
    }
    vol.flush().expect("vol flush");
    let image = vol.into_storage().into_inner();

    let stream = set_offset_in_image(&image, "DlProbe.bin") + 32;
    let vdl = u64::from_le_bytes(image[stream + 8..stream + 16].try_into().unwrap());
    let dl = u64::from_le_bytes(image[stream + 24..stream + 32].try_into().unwrap());
    assert_eq!(dl, 10, "DataLength must be the logical size");
    assert_eq!(vdl, 10, "fully-written file has VDL == DL");
}

/// A Windows-preallocated file (VDL < DL) must report DL as its size and
/// read the uninitialized VDL..DL range as zeros — not EOF at VDL, and
/// never stale cluster contents.
#[test]
fn preallocated_file_reads_zeros_beyond_valid_length() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    vol.write("Prealloc.bin", &[0xAAu8; 1000]).expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    // Forge the preallocation: shrink VDL to 100, re-CRC the set.
    let set = set_offset_in_image(&image, "Prealloc.bin");
    let stream = set + 32;
    image[stream + 8..stream + 16].copy_from_slice(&100u64.to_le_bytes());
    fix_set_checksum(&mut image, set);

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    assert_eq!(
        vol.metadata("Prealloc.bin").expect("meta").len(),
        1000,
        "size is DataLength, not VDL"
    );
    let data = vol.read("Prealloc.bin").expect("read");
    assert_eq!(data.len(), 1000, "read to EOF covers DL");
    assert!(
        data[..100].iter().all(|&b| b == 0xAA),
        "valid prefix intact"
    );
    assert!(
        data[100..].iter().all(|&b| b == 0),
        "VDL..DL must read as zeros"
    );
}

#[test]
fn created_and_renamed_entry_sets_carry_name_hash() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    vol.write("HashProbe.bin", b"nh").expect("create");
    vol.write("KeepMe.txt", b"k").expect("create 2");
    vol.rename("HashProbe.bin", "Renamed.dat").expect("rename");
    vol.flush().expect("flush");
    let image = vol.into_storage().into_inner();

    // Create path (compose) and rename path (compose_full) both hash.
    assert_eq!(
        name_hash_in_image(&image, "KeepMe.txt"),
        spec_name_hash("KEEPME.TXT"),
        "NameHash must be the spec hash of the up-cased name (create)"
    );
    assert_eq!(
        name_hash_in_image(&image, "Renamed.dat"),
        spec_name_hash("RENAMED.DAT"),
        "NameHash must be the spec hash of the up-cased name (rename)"
    );
}

#[test]
fn mounts_as_exfat() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount exFAT");
    assert_eq!(vol.format(), Format::ExFat);
    // A freshly-formatted volume has an empty root.
    let count = vol.read_dir("/").expect("read_dir root").count();
    assert_eq!(count, 0, "fresh exFAT root should be empty");
}

#[test]
fn create_write_readback_small() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    let payload = b"hello from exFAT via the unified Volume API";

    // Write through a handle that is dropped WITHOUT an explicit flush,
    // to prove the drop-flush path persists length + entry CRC (the C1
    // regression: previously a no-op, silently losing the write).
    {
        let mut f = vol.create("game.sav").expect("create");
        f.write_all(payload).expect("write");
        // no explicit flush — rely on Drop.
    }

    let mut f = vol.open("game.sav").expect("reopen");
    assert_eq!(
        f.len(),
        payload.len() as u64,
        "length not persisted on drop"
    );
    assert_eq!(read_all(&mut f), payload, "content mismatch");
}

#[test]
fn create_write_readback_multicluster() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    // > 2 clusters (cluster size is 4 KiB) to exercise chain growth.
    let payload: Vec<u8> = (0..10_000u32)
        .map(|i| (i.wrapping_mul(31) & 0xFF) as u8)
        .collect();

    {
        let mut f = vol.create("big.bin").expect("create");
        f.write_all(&payload).expect("write");
        f.flush().expect("flush");
    }

    let mut f = vol.open("big.bin").expect("reopen");
    assert_eq!(f.len(), payload.len() as u64);
    assert_eq!(read_all(&mut f), payload, "multi-cluster content mismatch");
}

#[test]
fn directories_and_listing() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");

    vol.create_dir_all("saves/slot1").expect("create_dir_all");
    {
        let mut f = vol.create("saves/slot1/a.dat").expect("create a");
        f.write_all(b"aaaa").expect("write a");
    }
    {
        let mut f = vol.create("saves/slot1/b.dat").expect("create b");
        f.write_all(b"bbbbbb").expect("write b");
    }

    let mut names: Vec<String> = vol
        .read_dir("saves/slot1")
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["a.dat".to_string(), "b.dat".to_string()]);

    // Metadata reflects the written sizes.
    assert_eq!(vol.metadata("saves/slot1/a.dat").expect("meta a").len(), 4);
    assert_eq!(vol.metadata("saves/slot1/b.dat").expect("meta b").len(), 6);
}

#[test]
fn rename_within_and_across_dirs() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    {
        let mut f = vol.create("old.txt").expect("create");
        f.write_all(b"payload-1234").expect("write");
    }

    // Rename within the root — content must be preserved.
    vol.rename("old.txt", "new.txt").expect("rename");
    assert!(vol.open("old.txt").is_err(), "old name should be gone");
    let mut f = vol.open("new.txt").expect("new name exists");
    assert_eq!(read_all(&mut f), b"payload-1234", "content lost on rename");
    // Renaming requires no outstanding handles (open files lock their path).
    drop(f);

    // Move across directories.
    vol.create_dir("sub").expect("mkdir");
    vol.rename("new.txt", "sub/moved.txt").expect("move");
    assert!(vol.open("new.txt").is_err(), "source gone after move");
    let mut f = vol.open("sub/moved.txt").expect("moved");
    assert_eq!(read_all(&mut f), b"payload-1234", "content lost on move");

    // Renaming onto an existing name is rejected.
    {
        let mut g = vol.create("occupied.txt").expect("create");
        g.write_all(b"x").expect("write");
    }
    assert!(
        vol.rename("sub/moved.txt", "occupied.txt").is_err(),
        "rename onto existing target should fail",
    );
}

#[test]
fn rename_directory_keeps_contents() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    vol.create_dir_all("a/b").expect("mkdirs");
    {
        let mut f = vol.create("a/b/file.dat").expect("create");
        f.write_all(b"hello").expect("write");
    }

    vol.rename("a/b", "a/c").expect("rename dir");
    assert!(vol.read_dir("a/b").is_err(), "old dir path should be gone");
    let mut f = vol.open("a/c/file.dat").expect("file under renamed dir");
    assert_eq!(read_all(&mut f), b"hello", "contents survived dir rename");
}

#[test]
fn read_write_helpers_exfat() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    let payload: Vec<u8> = (0..5000u32).map(|i| (i & 0xFF) as u8).collect();
    vol.write("rw.bin", &payload).expect("write");
    assert_eq!(
        vol.read("rw.bin").expect("read"),
        payload,
        "read/write mismatch"
    );
}

#[test]
fn truncate_shrinks_file_exfat() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    // ~3 clusters at 4 KiB.
    let data: Vec<u8> = (0..9000u32).map(|i| (i & 0xFF) as u8).collect();
    {
        let mut f = vol.create("t.bin").expect("create");
        f.write_all(&data).expect("write");
        f.flush().expect("flush");
    }
    {
        let mut f = vol.open_rw("t.bin").expect("open_rw");
        f.set_len(1000).expect("set_len");
    }
    let mut f = vol.open("t.bin").expect("reopen");
    let got = read_all(&mut f);
    assert_eq!(got.len(), 1000, "exFAT truncated length wrong");
    assert_eq!(got, &data[..1000], "exFAT truncated content mismatch");
}

#[test]
fn create_stamps_timestamps() {
    let when = datetime!(2021-06-15 09:30:00);
    let opts = FsOptions::new().with_clock(Box::new(FixedClock(when)));
    let vol = Volume::mount_with(medium(EXFAT), opts).expect("mount");

    vol.write("stamped.bin", b"hi").expect("write");
    let meta = vol.metadata("stamped.bin").expect("meta");
    assert_eq!(meta.created(), Some(when), "create timestamp");
    assert_eq!(meta.modified(), Some(when), "modified timestamp");
    assert_eq!(meta.accessed(), Some(when.date()), "accessed date");
}

#[test]
fn set_modified_persists() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    {
        let mut f = vol.create("t.bin").expect("create");
        f.write_all(b"x").expect("write");
    }
    let stamp = datetime!(2019-12-25 12:00:00);
    {
        let mut f = vol.open_rw("t.bin").expect("open_rw");
        f.set_modified(stamp).expect("set_modified");
    }
    assert_eq!(
        vol.metadata("t.bin").expect("meta").modified(),
        Some(stamp),
        "set_modified did not persist",
    );
}

#[test]
fn rename_preserves_timestamps() {
    let when = datetime!(2020-01-02 03:04:00);
    let opts = FsOptions::new().with_clock(Box::new(FixedClock(when)));
    let vol = Volume::mount_with(medium(EXFAT), opts).expect("mount");

    vol.write("orig.bin", b"data").expect("write");
    vol.rename("orig.bin", "moved.bin").expect("rename");
    let meta = vol.metadata("moved.bin").expect("meta");
    assert_eq!(meta.created(), Some(when), "created lost on rename");
    assert_eq!(meta.modified(), Some(when), "modified lost on rename");
}

#[test]
fn multi_cluster_bitmap_large_write() {
    // The bitmap spans 2 clusters / 2 scan windows (>512 bytes). Writing
    // a file large enough to allocate clusters past #4098 lands their
    // bits in the *second* bitmap window, exercising the multi-cluster
    // addressing and the chunked scan end to end.
    let vol = Volume::mount(medium(EXFAT_MC)).expect("mount");

    // ~2.5 MiB (≈5120 × 512 B clusters) crosses the 2 MiB / cluster-4098
    // boundary into the second bitmap window.
    let payload: Vec<u8> = (0..2_621_440u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 24) as u8)
        .collect();
    {
        let mut f = vol.create("big.bin").expect("create");
        f.write_all(&payload).expect("write");
        f.flush().expect("flush");
    }

    let mut f = vol.open("big.bin").expect("reopen");
    assert_eq!(f.len(), payload.len() as u64, "length wrong");
    assert_eq!(
        read_all(&mut f),
        payload,
        "multi-cluster-bitmap content mismatch"
    );

    // A second file must still allocate (from the free region above the
    // first file), proving the scan finds free clusters past the used run.
    vol.write("second.bin", b"after the big one")
        .expect("second write");
    assert_eq!(
        vol.read("second.bin").expect("read second"),
        b"after the big one"
    );
}

#[test]
fn seek_back_after_full_read_rereads() {
    // Read a multi-cluster file to EOF, then seek back and re-read — the
    // forward-walk cache must re-derive from the start, not stick at EOF.
    let vol = Volume::mount(medium(EXFAT_MC)).expect("mount");
    let payload: Vec<u8> = (0..3000u32).map(|i| i as u8).collect(); // ~6 × 512 B clusters
    vol.write("s.bin", &payload).expect("write");

    let mut f = vol.open("s.bin").expect("open");
    assert_eq!(read_all(&mut f), payload, "initial read");

    f.seek(SeekFrom::Start(0)).expect("seek back");
    let reread = read_all(&mut f);
    assert_eq!(reread, payload, "re-read after seeking back past EOF");
}

#[test]
fn multi_cluster_bitmap_free_reuses_space() {
    // Fill, delete, and refill to confirm mark_cluster_free clears bits
    // in the second window and the space is reused.
    let vol = Volume::mount(medium(EXFAT_MC)).expect("mount");
    let payload = vec![0x5Au8; 2_400_000];
    vol.write("a.bin", &payload).expect("write a");
    vol.remove_file("a.bin").expect("remove a");
    // Should succeed by reusing the just-freed clusters.
    vol.write("b.bin", &payload).expect("write b after free");
    assert_eq!(vol.read("b.bin").expect("read b").len(), payload.len());
}

#[test]
fn remove_file_roundtrip() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    {
        let mut f = vol.create("temp.bin").expect("create");
        f.write_all(b"scratch").expect("write");
    }
    assert!(vol.open("temp.bin").is_ok(), "file should exist");
    vol.remove_file("temp.bin").expect("remove");
    assert!(vol.open("temp.bin").is_err(), "file should be gone");
}

/// Entry sets that straddle cluster boundaries (Windows writes these in
/// multi-cluster directories) must survive every operation. The MC
/// fixture's 512 B clusters force it: each 240-char name yields a
/// 576-byte entry set, so every set spans a boundary or triggers growth.
#[test]
fn entry_sets_straddling_cluster_boundaries() {
    let vol = Volume::mount(medium(EXFAT_MC)).expect("mount");
    vol.create_dir("straddle").expect("mkdir");
    let names: Vec<String> = (0..6)
        .map(|i| format!("{}-{i}.bin", "n".repeat(240)))
        .collect();
    for (i, n) in names.iter().enumerate() {
        vol.write(format!("straddle/{n}"), &vec![i as u8; 700])
            .unwrap_or_else(|e| panic!("create long-named file {i}: {e:?}"));
    }

    // Every set must be listable and its payload readable.
    let mut listed: Vec<String> = vol
        .read_dir("straddle")
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_owned())
        .collect();
    listed.sort();
    let mut want = names.clone();
    want.sort();
    assert_eq!(listed, want, "all straddling sets visible");
    for (i, n) in names.iter().enumerate() {
        assert_eq!(
            vol.read(format!("straddle/{n}")).expect("read"),
            vec![i as u8; 700],
            "payload of {n}"
        );
    }

    // Mutations through straddling sets: rename, remove, timestamp patch.
    let renamed = format!("{}-r.bin", "m".repeat(240));
    vol.rename(
        format!("straddle/{}", names[0]),
        format!("straddle/{renamed}"),
    )
    .expect("rename straddling set");
    assert_eq!(
        vol.read(format!("straddle/{renamed}")).expect("renamed"),
        vec![0u8; 700]
    );
    vol.remove_file(format!("straddle/{}", names[1]))
        .expect("remove straddling set");
    assert!(vol.metadata(format!("straddle/{}", names[1])).is_err());
    vol.set_modified(format!("straddle/{renamed}"), unifat::EPOCH)
        .expect("patch straddling set");

    // And it all persists across a remount.
    vol.flush().expect("flush");
    let image = vol.into_storage().into_inner();
    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    assert_eq!(
        vol.read(format!("straddle/{renamed}"))
            .expect("after remount"),
        vec![0u8; 700]
    );
    assert_eq!(
        vol.read_dir("straddle").expect("read_dir").count(),
        names.len() - 1,
        "renamed kept, removed gone"
    );
}

/// The read-only attribute must block writes and deletes (matching FAT
/// and Windows): open_rw, remove_file, and remove_dir_all all refuse;
/// read-only opens still work.
#[test]
fn read_only_attribute_enforced() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    vol.create_dir("rodir").expect("mkdir");
    vol.write("rodir/RoLock.bin", b"guarded").expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    // Forge the read-only bit (attributes u16 at offset 4 of the File
    // entry) and re-CRC the set.
    let set = set_offset_in_image(&image, "RoLock.bin");
    image[set + 4] |= 0x01;
    fix_set_checksum(&mut image, set);

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    assert!(
        matches!(
            vol.open_rw("rodir/RoLock.bin"),
            Err(unifat::FsError::ReadOnlyFile)
        ),
        "open_rw must refuse a read-only file"
    );
    assert!(
        matches!(
            vol.remove_file("rodir/RoLock.bin"),
            Err(unifat::FsError::ReadOnlyFile)
        ),
        "remove_file must refuse a read-only file"
    );
    assert!(
        matches!(
            vol.remove_dir_all("rodir"),
            Err(unifat::FsError::ReadOnlyFile)
        ),
        "remove_dir_all must refuse a tree containing a read-only file"
    );
    // Reading still works, and nothing was deleted.
    assert_eq!(vol.read("rodir/RoLock.bin").expect("read"), b"guarded");
    assert!(
        vol.metadata("rodir/RoLock.bin")
            .expect("meta")
            .is_read_only()
    );
}

/// Byte offset of the VBR `VolumeFlags` field and its `VolumeDirty` bit.
const VOLUME_FLAGS_OFFSET: usize = 106;
const VOLUME_DIRTY: u16 = 0x0002;

fn volume_dirty_bit(image: &[u8]) -> bool {
    let flags = u16::from_le_bytes([image[VOLUME_FLAGS_OFFSET], image[VOLUME_FLAGS_OFFSET + 1]]);
    flags & VOLUME_DIRTY != 0
}

/// A device wrapper recording whether the VolumeDirty bit was ever set
/// on the medium mid-session (it is cleared again by a clean flush, so
/// the final image alone can't show it).
struct DirtySpy {
    inner: MemBlockDevice,
    pos: u64,
    saw_dirty_set: std::rc::Rc<std::cell::Cell<bool>>,
}

impl embedded_io::ErrorType for DirtySpy {
    type Error = unifat::MemError;
}
impl embedded_io::Read for DirtySpy {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        use embedded_io::Seek as _;
        self.inner.seek(embedded_io::SeekFrom::Start(self.pos))?;
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        Ok(n)
    }
}
impl embedded_io::Write for DirtySpy {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        use embedded_io::Seek as _;
        self.inner.seek(embedded_io::SeekFrom::Start(self.pos))?;
        let n = self.inner.write(buf)?;
        self.pos += n as u64;
        if volume_dirty_bit(self.inner.as_slice()) {
            self.saw_dirty_set.set(true);
        }
        Ok(n)
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        embedded_io::Write::flush(&mut self.inner)
    }
}
impl embedded_io::Seek for DirtySpy {
    fn seek(&mut self, pos: embedded_io::SeekFrom) -> Result<u64, Self::Error> {
        self.pos = Seek::seek(&mut self.inner, pos)?;
        Ok(self.pos)
    }
}

/// VolumeDirty lifecycle: raised before the first metadata write of a
/// session, lowered by a clean flush/unmount — and a flag inherited from
/// an unclean previous session is never cleared (we didn't verify).
#[test]
fn volume_dirty_flag_lifecycle() {
    // Clean volume: the bit goes up mid-session and comes down on unmount.
    let saw = std::rc::Rc::new(std::cell::Cell::new(false));
    let spy = DirtySpy {
        inner: medium(EXFAT),
        pos: 0,
        saw_dirty_set: saw.clone(),
    };
    let vol = Volume::mount(spy).expect("mount");
    vol.write("dirty.bin", b"d").expect("write");
    assert!(
        saw.get(),
        "VolumeDirty must be raised before metadata writes"
    );
    let image = vol.into_storage().inner.into_inner();
    assert!(
        !volume_dirty_bit(&image),
        "clean unmount must lower VolumeDirty"
    );

    // Remount the clean image and verify the file survived.
    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    assert_eq!(vol.read("dirty.bin").expect("read"), b"d");

    // A volume that mounted dirty must STAY dirty across our session.
    let mut unclean = medium(EXFAT).into_inner();
    let flags = u16::from_le_bytes([
        unclean[VOLUME_FLAGS_OFFSET],
        unclean[VOLUME_FLAGS_OFFSET + 1],
    ]) | VOLUME_DIRTY;
    unclean[VOLUME_FLAGS_OFFSET..VOLUME_FLAGS_OFFSET + 2].copy_from_slice(&flags.to_le_bytes());
    let vol = Volume::mount(MemBlockDevice::new(unclean)).expect("mount unclean");
    vol.write("still.bin", b"s").expect("write");
    let image = vol.into_storage().into_inner();
    assert!(
        volume_dirty_bit(&image),
        "a pre-existing dirty flag must survive (volume was never verified)"
    );
}

/// Filling the volume must surface a clean StorageFull, leave the
/// partial file readable at its recorded length, and removing it must
/// give the space back (bitmap bits actually freed).
#[test]
fn volume_full_fails_cleanly_and_recovers() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    let mut f = vol.create("full.bin").expect("create");
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

    assert!(len > 0, "some data must have fit");
    assert_eq!(vol.read("full.bin").expect("read").len() as u64, len);
    vol.remove_file("full.bin").expect("remove");
    vol.write("again.bin", b"space back")
        .expect("write after free");
    assert_eq!(vol.read("again.bin").expect("read"), b"space back");
}

/// A contiguous (NoFatChain=1) file keeps its layout until it actually
/// grows: reads never touch the FAT, and the first growth materializes
/// the chain lazily (previously open_rw eagerly rewrote the FAT for the
/// whole file, even for read-modify workloads).
#[test]
fn contiguous_file_grows_via_lazy_chain_conversion() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    // Two 4 KiB clusters; fresh-volume allocations are adjacent.
    let body: Vec<u8> = (0..8192u32).map(|i| (i % 250) as u8).collect();
    vol.write("Contig.bin", &body).expect("create");
    vol.flush().expect("flush");
    let mut image = vol.into_storage().into_inner();

    // Forge Windows-style contiguous layout: set NoFatChain, re-CRC.
    let set = set_offset_in_image(&image, "Contig.bin");
    image[set + 32 + 1] |= 0x02; // GeneralSecondaryFlags.NoFatChain
    fix_set_checksum(&mut image, set);

    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount");
    assert_eq!(vol.read("Contig.bin").expect("read"), body);
    {
        let mut f = vol.open_rw("Contig.bin").expect("open_rw");
        f.seek(SeekFrom::End(0)).expect("seek");
        f.write_all(b"TAIL").expect("append");
    }
    let got = vol.read("Contig.bin").expect("read after growth");
    assert_eq!(got.len(), body.len() + 4, "grown length");
    assert_eq!(&got[..body.len()], &body[..], "body intact");
    assert_eq!(&got[body.len()..], b"TAIL", "tail appended");

    // The converted (now FAT-chained) file survives a remount.
    vol.flush().expect("flush");
    let image = vol.into_storage().into_inner();
    let vol = Volume::mount(MemBlockDevice::new(image)).expect("remount 2");
    assert_eq!(
        vol.read("Contig.bin").expect("read final").len(),
        body.len() + 4
    );
}

/// Byte offset of cluster `n` in `img`, from the raw boot-record fields.
fn exfat_cluster_offset(img: &[u8], n: u32) -> usize {
    let sector_bytes = 1usize << img[108];
    let cluster_bytes = 1usize << (img[108] + img[109]);
    let heap_off_sectors = u32::from_le_bytes(img[88..92].try_into().unwrap()) as usize;
    heap_off_sectors * sector_bytes + (n as usize - 2) * cluster_bytes
}

/// A corrupt Main Boot Checksum must be rejected at mount, not trusted.
#[test]
fn bad_boot_checksum_rejected() {
    let mut img = medium(EXFAT).into_inner();
    // Flip a non-excluded byte in the boot region (boot code, sector 0).
    img[200] ^= 0xFF;
    let err = Volume::mount(MemBlockDevice::new(img)).expect_err("must reject");
    assert!(
        matches!(
            err,
            unifat::FsError::Corrupt(unifat::CorruptKind::BootSector)
        ),
        "expected Corrupt(BootSector), got {err:?}"
    );
}

/// A corrupt up-case `TableChecksum` must be rejected at mount.
#[test]
fn bad_upcase_checksum_rejected() {
    let mut img = medium(EXFAT).into_inner();
    let root_clu = u32::from_le_bytes(img[96..100].try_into().unwrap());
    let root_off = exfat_cluster_offset(&img, root_clu);
    let cluster_bytes = 1usize << (img[108] + img[109]);
    // Find the up-case table entry (0x82) in the root cluster and corrupt
    // its stored TableChecksum (offset 4) — the table data is untouched,
    // so the recomputed checksum no longer matches.
    let mut pos = root_off;
    let mut corrupted = false;
    while pos < root_off + cluster_bytes {
        if img[pos] == 0x00 {
            break;
        }
        if img[pos] == 0x82 {
            img[pos + 4] ^= 0xFF;
            corrupted = true;
            break;
        }
        pos += 32;
    }
    assert!(corrupted, "no up-case table entry found in root");
    let err = Volume::mount(MemBlockDevice::new(img)).expect_err("must reject");
    assert!(
        matches!(err, unifat::FsError::Corrupt(_)),
        "expected Corrupt, got {err:?}"
    );
}

/// A mutate + remount round-trip must still pass both integrity checks:
/// the boot region is immutable except the checksum-excluded VolumeFlags
/// bytes, and the up-case table is never rewritten.
#[test]
fn checksums_survive_mutation_roundtrip() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    vol.write("integrity.bin", b"data").expect("write");
    vol.create_dir("d").expect("mkdir");
    vol.flush().expect("flush");
    let img = vol.into_storage().into_inner();
    // The whole point: this must still mount (checksums valid after writes).
    let vol = Volume::mount(MemBlockDevice::new(img)).expect("remount after writes");
    assert_eq!(vol.read("integrity.bin").expect("read"), b"data");
}

/// R10 regression: the allocation bitmap's location comes from an
/// un-checksummed root entry — corrupt geometry must fail the mount, not
/// silently redirect every allocator write into file data.
#[test]
fn corrupt_bitmap_geometry_rejected_at_mount() {
    let mut img = medium(EXFAT).into_inner();
    let root_clu = u32::from_le_bytes(img[96..100].try_into().unwrap());
    let root_off = exfat_cluster_offset(&img, root_clu);
    // The 0x81 bitmap entry is in the root; forge FirstCluster out of range.
    let cluster_bytes = 1usize << (img[108] + img[109]);
    let mut pos = root_off;
    let mut forged = false;
    while pos < root_off + cluster_bytes {
        if img[pos] == 0x81 {
            img[pos + 20..pos + 24].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes());
            forged = true;
            break;
        }
        pos += 32;
    }
    assert!(forged, "bitmap entry not found");
    let err = Volume::mount(MemBlockDevice::new(img)).expect_err("must reject");
    assert!(matches!(err, unifat::FsError::Corrupt(_)), "got {err:?}");
}

/// R11 regression: a corrupt FAT cycle in a (chained) directory must
/// surface Corrupt from read_dir — not balloon a multi-GiB buffer.
#[test]
fn cyclic_directory_chain_errors_not_oom() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    vol.create_dir("big").expect("mkdir");
    // Grow the directory past one cluster so it converts to FAT-chained.
    for i in 0..80 {
        vol.write(format!("big\\some longer file name {i:03}.txt"), b"x")
            .expect("fill");
    }
    vol.flush().expect("flush");
    let mut img = vol.into_storage().into_inner();

    // First cluster of `big` from its stream extension in the root.
    let set = set_offset_in_image(&img, "big");
    let first = u32::from_le_bytes(img[set + 32 + 20..set + 32 + 24].try_into().unwrap());
    // exFAT FAT: fat_offset sectors from volume start, 4-byte cells.
    let fat_off = u32::from_le_bytes(img[80..84].try_into().unwrap()) as usize;
    let sector = 1usize << img[108];
    let cell = |c: u32| fat_off * sector + 4 * c as usize;
    let second = u32::from_le_bytes(img[cell(first)..cell(first) + 4].try_into().unwrap());
    assert!((2..0xFFFF_FFF0).contains(&second), "expected a chained dir");
    // Point the second cluster back at the first: a two-cluster cycle.
    img[cell(second)..cell(second) + 4].copy_from_slice(&first.to_le_bytes());

    let vol = Volume::mount(MemBlockDevice::new(img)).expect("remount");
    let err = vol.read_dir("big").expect_err("cycle must error");
    assert!(matches!(err, unifat::FsError::Corrupt(_)), "got {err:?}");
}

/// R17 regression: truncating a contiguous (NoFatChain) file to zero must
/// clear the persisted NoFatChain flag — NFC=1 with FirstCluster=0 is out
/// of spec.
#[test]
fn truncate_to_zero_clears_no_fat_chain() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    vol.write("nfc.bin", &[7u8; 100]).expect("create");
    vol.flush().expect("flush");
    let mut img = vol.into_storage().into_inner();

    // Forge the file as NoFatChain (single-cluster: trivially contiguous).
    let set = set_offset_in_image(&img, "nfc.bin");
    img[set + 32 + 1] |= 0x02; // Stream Extension GeneralSecondaryFlags: NoFatChain
    fix_set_checksum(&mut img, set);

    let vol = Volume::mount(MemBlockDevice::new(img)).expect("remount");
    {
        let mut f = vol.open_rw("nfc.bin").expect("open_rw");
        f.set_len(0).expect("truncate");
        f.flush().expect("flush");
    }
    vol.flush().expect("vol flush");
    let img = vol.into_storage().into_inner();
    let set = set_offset_in_image(&img, "nfc.bin");
    assert_eq!(
        img[set + 32 + 1] & 0x02,
        0,
        "NoFatChain persisted on a zero-length file"
    );
}

/// R25 regression: a corrupt set with an inflated secondary_count must not
/// make locate overshoot the NEXT valid set — files were visible in
/// read_dir but unopenable (list skipped 32 bytes; locate skipped the
/// claimed length).
#[test]
fn corrupt_set_does_not_hide_the_next_file() {
    let vol = Volume::mount(medium(EXFAT)).expect("mount");
    vol.write("aa.bin", b"a").expect("aa");
    vol.write("bb.bin", b"b").expect("bb");
    vol.flush().expect("flush");
    let mut img = vol.into_storage().into_inner();

    // Corrupt aa's set: inflated secondary_count (also invalidates CRC).
    let set = set_offset_in_image(&img, "aa.bin");
    img[set + 1] = 0xF0;

    let vol = Volume::mount(MemBlockDevice::new(img)).expect("remount");
    let listed: Vec<String> = vol
        .read_dir("/")
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string())
        .collect();
    assert!(listed.iter().any(|n| n == "bb.bin"), "bb.bin listed");
    assert_eq!(
        vol.read("bb.bin").expect("bb.bin must be openable"),
        b"b",
        "list and locate disagreed"
    );
}

/// R16 regression: dropping a Volume without flush must not strand
/// `VolumeDirty` — the driver never clears an inherited dirty flag, so
/// Windows would demand a scan forever.
#[test]
fn drop_without_flush_clears_volume_dirty() {
    // MemBlockDevice is moved into the Volume, so observe through a
    // shared mirror of the final image via into-scope drop + re-mount:
    // write, drop WITHOUT flush/into_storage, then check the bit by
    // remounting the same backing storage captured beforehand.
    use core::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug, Clone, Copy)]
    struct NoErr;
    impl embedded_io::Error for NoErr {
        fn kind(&self) -> embedded_io::ErrorKind {
            embedded_io::ErrorKind::Other
        }
    }
    impl core::fmt::Display for NoErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("noerr")
        }
    }
    impl core::error::Error for NoErr {}

    /// Shared-buffer device so the bytes survive the Volume's drop.
    struct SharedDev {
        buf: Rc<RefCell<Vec<u8>>>,
        pos: u64,
    }
    impl embedded_io::ErrorType for SharedDev {
        type Error = NoErr;
    }
    impl embedded_io::Read for SharedDev {
        fn read(&mut self, out: &mut [u8]) -> Result<usize, NoErr> {
            let buf = self.buf.borrow();
            let start = usize::try_from(self.pos).unwrap().min(buf.len());
            let n = out.len().min(buf.len() - start);
            out[..n].copy_from_slice(&buf[start..start + n]);
            self.pos += n as u64;
            Ok(n)
        }
    }
    impl embedded_io::Write for SharedDev {
        fn write(&mut self, data: &[u8]) -> Result<usize, NoErr> {
            let mut buf = self.buf.borrow_mut();
            let start = usize::try_from(self.pos).unwrap();
            if start + data.len() > buf.len() {
                return Err(NoErr);
            }
            buf[start..start + data.len()].copy_from_slice(data);
            self.pos += data.len() as u64;
            Ok(data.len())
        }
        fn flush(&mut self) -> Result<(), NoErr> {
            Ok(())
        }
    }
    impl embedded_io::Seek for SharedDev {
        fn seek(&mut self, pos: embedded_io::SeekFrom) -> Result<u64, NoErr> {
            use embedded_io::SeekFrom::*;
            let len = self.buf.borrow().len() as u64;
            self.pos = match pos {
                Start(n) => n,
                End(d) => len.checked_add_signed(d).ok_or(NoErr)?,
                Current(d) => self.pos.checked_add_signed(d).ok_or(NoErr)?,
            };
            Ok(self.pos)
        }
    }

    let shared = Rc::new(RefCell::new(EXFAT.to_vec()));
    {
        let vol = Volume::mount(SharedDev {
            buf: shared.clone(),
            pos: 0,
        })
        .expect("mount");
        vol.write("dropped.bin", b"data").expect("write");
        // No flush, no into_storage: plain drop.
    }
    let img = shared.borrow();
    let flags = u16::from_le_bytes([img[106], img[107]]);
    assert_eq!(flags & 0x0002, 0, "VolumeDirty stranded after drop");
    drop(img);
    // And the write itself survived the drop.
    let img2 = shared.borrow().clone();
    let vol = Volume::mount(MemBlockDevice::new(img2)).expect("remount");
    assert_eq!(vol.read("dropped.bin").expect("read"), b"data");
}
