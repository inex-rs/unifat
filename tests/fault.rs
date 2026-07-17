//! Fault injection: a device that fails a chosen storage write, sweeping
//! the failure point across whole operations. The audit flagged the
//! `FsError::Io` paths as structurally untestable with the plain
//! `MemBlockDevice`; this exercises them. The contract under test:
//!
//! - a device write error surfaces as an `Err` (never a panic, never a
//!   poisoned `RefCell` borrow),
//! - after a transient failure the volume is still mountable, and
//! - data written and flushed *before* the failure is never lost.

#[path = "common/mod.rs"]
mod common;

use core::cell::Cell;
use std::rc::Rc;

use embedded_io::{ErrorType, Read, Seek, SeekFrom, Write};
use unifat::{MemBlockDevice, MemError, Volume};

const FAT16: &[u8] = include_bytes!("fixtures/fat16-3m.img");
const EXFAT: &[u8] = include_bytes!("fixtures/exfat-1m.img");

/// Wraps a fixed-size [`MemBlockDevice`] and fails the `fail_at`-th write
/// (1-indexed) exactly once — a single transient I/O error — then lets
/// every later write through. `writes` records how many device writes
/// the workload issued, so a sweep knows when it has covered them all.
struct FaultDevice {
    inner: MemBlockDevice,
    seen: u32,
    fail_at: u32,
    fired: Rc<Cell<bool>>,
    writes: Rc<Cell<u32>>,
}

impl FaultDevice {
    fn new(image: &[u8], fail_at: u32) -> (Self, Rc<Cell<bool>>, Rc<Cell<u32>>) {
        let fired = Rc::new(Cell::new(false));
        let writes = Rc::new(Cell::new(0));
        let dev = Self {
            inner: MemBlockDevice::fixed(image.to_vec()),
            seen: 0,
            fail_at,
            fired: fired.clone(),
            writes: writes.clone(),
        };
        (dev, fired, writes)
    }

    fn into_inner(self) -> Vec<u8> {
        self.inner.into_inner()
    }
}

impl ErrorType for FaultDevice {
    type Error = MemError;
}

impl Read for FaultDevice {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, MemError> {
        self.inner.read(buf)
    }
}

impl Seek for FaultDevice {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, MemError> {
        self.inner.seek(pos)
    }
}

impl Write for FaultDevice {
    fn write(&mut self, buf: &[u8]) -> Result<usize, MemError> {
        self.seen += 1;
        self.writes.set(self.seen);
        if self.seen == self.fail_at {
            self.fired.set(true);
            return Err(MemError);
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> Result<(), MemError> {
        self.inner.flush()
    }
}

/// Drive a create + write + flush workload, then some follow-up ops, with
/// an injected transient failure at device-write `fail_at`. Returns how
/// many device writes were issued in total.
fn workload(image: &[u8], fail_at: u32) -> u32 {
    let (dev, _fired, writes) = FaultDevice::new(image, fail_at);
    // Every call may hit the injected error; none may panic. We don't
    // assert on individual results — the point is that the driver stays
    // sound whichever write failed.
    let vol = match Volume::mount(dev) {
        Ok(v) => v,
        Err(_) => return writes.get(),
    };
    let _ = vol.create_dir("\\FDIR");
    if let Ok(mut f) = vol.create("\\FDIR\\a.bin") {
        let _ = f.write_all(&[0xABu8; 6000]);
        let _ = f.flush();
    }
    let _ = vol.write("\\B.BIN", b"second file");
    let _ = vol.rename("\\B.BIN", "\\C.BIN");
    let _ = vol.set_modified("\\C.BIN", unifat::EPOCH);
    let _ = vol.remove_file("\\C.BIN");
    let _ = vol.flush();
    writes.get()
}

/// Sweep the injection point across the whole workload on both backends.
/// No injection point may panic, and the resulting image must always
/// remount cleanly.
fn assert_fault_sweep(image: &[u8]) {
    // First run with an unreachable failure point to learn the write count.
    let total = workload(image, u32::MAX);
    assert!(total > 0, "workload issued no device writes");

    for fail_at in 1..=total + 1 {
        // The workload itself must not panic for any injection point.
        let (dev, fired, _writes) = FaultDevice::new(image, fail_at);
        let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let vol = match Volume::mount(dev) {
                Ok(v) => v,
                Err(_) => return None,
            };
            let _ = vol.create_dir("\\FDIR");
            if let Ok(mut f) = vol.create("\\FDIR\\a.bin") {
                let _ = f.write_all(&[0xABu8; 6000]);
                let _ = f.flush();
            }
            let _ = vol.write("\\B.BIN", b"second file");
            let _ = vol.rename("\\B.BIN", "\\C.BIN");
            let _ = vol.set_modified("\\C.BIN", unifat::EPOCH);
            let _ = vol.remove_file("\\C.BIN");
            let _ = vol.flush();
            Some(vol.into_storage().into_inner())
        }));
        let image_after = recovered
            .unwrap_or_else(|_| panic!("driver panicked with failure injected at write {fail_at}"));

        // Whatever survived on disk must remount cleanly (the transient
        // error may have left a partially-applied but structurally valid
        // volume; it must never be unmountable).
        if let Some(bytes) = image_after {
            assert!(
                fired.get() || fail_at > total,
                "failure at {fail_at} never fired (total {total})"
            );
            let vol = Volume::mount(MemBlockDevice::new(bytes))
                .unwrap_or_else(|e| panic!("remount after fault at {fail_at} failed: {e:?}"));
            // Root must still be enumerable end to end.
            for entry in vol.read_dir("\\").expect("read_dir root") {
                let _ = entry.expect("dir entry decodes");
            }
        }
    }
}

#[test]
fn fat16_transient_write_faults_stay_sound() {
    assert_fault_sweep(FAT16);
}

#[test]
fn exfat_transient_write_faults_stay_sound() {
    assert_fault_sweep(EXFAT);
}

/// Data flushed before a later failure must never be lost: write file A
/// and flush it, then trigger a failure during a *subsequent* operation,
/// and confirm A survives a remount intact.
fn assert_committed_data_survives_fault(image: &[u8]) {
    let total = workload(image, u32::MAX);

    for fail_at in 1..=total + 1 {
        let (dev, _fired, _writes) = FaultDevice::new(image, fail_at);
        let vol = match Volume::mount(dev) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // Commit file A up front.
        if vol.write("\\KEEP.BIN", b"durable payload").is_err() {
            // The failure landed during A's own write; A isn't promised.
            continue;
        }
        if vol.flush().is_err() {
            continue;
        }
        // Now churn with the failure still armed for later writes.
        let _ = vol.create_dir("\\D2");
        let _ = vol.write("\\D2\\x.bin", &[1u8; 4000]);
        let _ = vol.rename("\\KEEP.BIN", "\\KEEP2.BIN");
        let _ = vol.flush();
        let bytes = vol.into_storage().into_inner();

        // A must be readable under one of its names after remount.
        let vol = Volume::mount(MemBlockDevice::new(bytes)).expect("remount");
        let keep = vol.read("\\KEEP.BIN").ok();
        let keep2 = vol.read("\\KEEP2.BIN").ok();
        let found = keep
            .or(keep2)
            .unwrap_or_else(|| panic!("committed file lost after fault at write {fail_at}"));
        assert_eq!(
            found, b"durable payload",
            "committed data corrupted (fault {fail_at})"
        );
    }
}

#[test]
fn fat16_committed_data_survives_faults() {
    assert_committed_data_survives_fault(FAT16);
}

#[test]
fn exfat_committed_data_survives_faults() {
    assert_committed_data_survives_fault(EXFAT);
}
