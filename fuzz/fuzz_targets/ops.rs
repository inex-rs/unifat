#![no_main]
//! Op-stream fuzzing with a differential model, against FAT16 and ExFAT.
//!
//! Every successful mutation updates an in-memory map of expected file
//! contents; `Read` ops compare live against it, and a final flush +
//! remount verifies every modeled file byte-for-byte. A file whose state
//! becomes unknowable (failed/partial write) is dropped from the model
//! but still tracked as "tainted" so existence checks stay sound.

use std::collections::{BTreeMap, BTreeSet};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use unifat::io::{Seek, SeekFrom, Write};
use unifat::{MemBlockDevice, Volume};

const FAT16: &[u8] = include_bytes!("../../tests/fixtures/fat16-3m.img");
const EXFAT: &[u8] = include_bytes!("../../tests/fixtures/exfat-1m.img");

/// Name vocabulary spanning 8.3-friendly, LFN, spaces, non-ASCII, and
/// long names; nested entries exercise parent resolution.
fn name(i: u8) -> String {
    match i % 6 {
        0 => "\\a.bin".into(),
        1 => "\\B.BIN".into(),
        2 => "\\d\\c.bin".into(),
        3 => "\\Mixed Case Name.txt".into(),
        4 => "\\файл.dat".into(),
        _ => format!("\\{}.long", "n".repeat(80)),
    }
}
fn dir(i: u8) -> &'static str {
    ["\\d", "\\d\\e"][(i as usize) % 2]
}

#[derive(Arbitrary, Debug)]
enum Op {
    Create(u8),
    Write(u8, u16, Vec<u8>),
    Read(u8),
    SetLen(u8, u16),
    Remove(u8),
    Rename(u8, u8),
    Mkdir(u8),
    Rmdir(u8),
    SetModified(u8),
    /// Toggle the hidden attribute; modeled and verified after remount.
    SetHidden(u8, bool),
}

fuzz_target!(|input: (bool, Vec<Op>)| {
    let (use_exfat, ops) = input;
    let image = if use_exfat { EXFAT } else { FAT16 };
    let Ok(vol) = Volume::mount(MemBlockDevice::from_slice(image)) else {
        return;
    };

    // Expected contents per path. Names are generated deterministically,
    // so string keys are stable; no two vocabulary names alias under
    // case folding.
    let mut model: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    // Modeled `hidden` attribute per path (files whose set succeeded).
    let mut attrs: BTreeMap<String, bool> = BTreeMap::new();
    // Files that exist on disk with unknowable contents.
    let mut tainted: BTreeSet<String> = BTreeSet::new();

    for op in ops.into_iter().take(64) {
        match op {
            Op::Create(n) => {
                let p = name(n);
                let existed = model.contains_key(&p) || tainted.contains(&p);
                if vol.create(&p).is_ok() {
                    model.insert(p.clone(), Vec::new());
                    tainted.remove(&p);
                    // A newly created entry has default attributes; a
                    // truncated existing one keeps them.
                    if !existed {
                        attrs.remove(&p);
                    }
                }
            }
            Op::Write(n, off, bytes) => {
                let p = name(n);
                let bytes = &bytes[..bytes.len().min(2048)];
                if bytes.is_empty() {
                    // An empty write is a no-op (no allocation, no length
                    // change), so it must not touch the model either.
                    continue;
                }
                let Ok(mut f) = vol.open_rw(&p) else { continue };
                if f.seek(SeekFrom::Start(u64::from(off))).is_err() {
                    continue;
                }
                let ok = f.write_all(bytes).is_ok() && f.flush().is_ok();
                drop(f);
                if ok {
                    if let Some(v) = model.get_mut(&p) {
                        let off = usize::from(off);
                        if v.len() < off + bytes.len() {
                            v.resize(off + bytes.len(), 0);
                        }
                        v[off..off + bytes.len()].copy_from_slice(bytes);
                    }
                } else {
                    // Partial write: contents unknowable from here on.
                    if model.remove(&p).is_some() {
                        tainted.insert(p);
                    }
                }
            }
            Op::Read(n) => {
                let p = name(n);
                match model.get(&p) {
                    Some(expected) => {
                        let got = vol
                            .read(&p)
                            .unwrap_or_else(|e| panic!("modeled file {p} unreadable: {e:?}"));
                        assert_eq!(&got, expected, "content mismatch for {p}");
                    }
                    None if !tainted.contains(&p) => {
                        assert!(vol.read(&p).is_err(), "unexpected file present: {p}");
                    }
                    None => {
                        let _ = vol.read(&p);
                    }
                }
            }
            Op::SetLen(n, len) => {
                let p = name(n);
                let Ok(mut f) = vol.open_rw(&p) else { continue };
                if f.set_len(u64::from(len)).is_ok() {
                    if let Some(v) = model.get_mut(&p) {
                        v.resize(usize::from(len), 0);
                    }
                } else if model.remove(&p).is_some() {
                    tainted.insert(p);
                }
            }
            Op::Remove(n) => {
                let p = name(n);
                if vol.remove_file(&p).is_ok() {
                    model.remove(&p);
                    tainted.remove(&p);
                    attrs.remove(&p);
                }
            }
            Op::Rename(a, b) => {
                let (pa, pb) = (name(a), name(b));
                if vol.rename(&pa, &pb).is_ok() {
                    if let Some(v) = model.remove(&pa) {
                        model.insert(pb.clone(), v);
                    }
                    if tainted.remove(&pa) {
                        tainted.insert(pb.clone());
                    }
                    // Attributes travel with the moved entry.
                    match attrs.remove(&pa) {
                        Some(h) => {
                            attrs.insert(pb, h);
                        }
                        None => {
                            attrs.remove(&pb);
                        }
                    }
                }
            }
            Op::Mkdir(n) => {
                let _ = vol.create_dir_all(dir(n));
            }
            Op::Rmdir(n) => {
                let _ = vol.remove_dir(dir(n));
            }
            Op::SetModified(n) => {
                let _ = vol.set_modified(name(n), unifat::EPOCH);
            }
            Op::SetHidden(n, hidden) => {
                let p = name(n);
                if vol
                    .set_attributes(
                        &p,
                        unifat::Attributes {
                            hidden,
                            ..Default::default()
                        },
                    )
                    .is_ok()
                    && !tainted.contains(&p)
                {
                    attrs.insert(p, hidden);
                }
            }
        }
    }

    // Flush, remount, and verify the model survives on disk.
    if vol.flush().is_err() {
        return;
    }
    let image = vol.into_storage().into_inner();
    let vol =
        Volume::mount(MemBlockDevice::new(image)).expect("a cleanly flushed volume must remount");
    for (path, expected) in &model {
        let got = vol
            .read(path)
            .unwrap_or_else(|e| panic!("modeled file {path} lost after remount: {e:?}"));
        assert_eq!(&got, expected, "content mismatch for {path} after remount");
    }
    // Attributes set on still-modeled files must survive the remount.
    for (path, hidden) in &attrs {
        if !model.contains_key(path) {
            continue; // removed/renamed since the set
        }
        let meta = vol
            .metadata(path)
            .unwrap_or_else(|e| panic!("modeled file {path} lost after remount: {e:?}"));
        assert_eq!(
            meta.attributes().hidden,
            *hidden,
            "hidden attribute mismatch for {path} after remount"
        );
    }
});
