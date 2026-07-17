//! Shared integration-test helpers (included via `#[path]` from each binary).

use embedded_io::Read;
use time::PrimitiveDateTime;
use unifat::{Clock, MemBlockDevice};

/// Build a **fixed-size** [`MemBlockDevice`] from fixture bytes
/// (cursor at 0): any driver write past the volume end fails loudly
/// instead of silently growing the image.
#[inline]
#[allow(dead_code)] // not every test binary uses raw fixtures
pub fn medium(bytes: &[u8]) -> MemBlockDevice {
    MemBlockDevice::fixed(bytes.to_vec())
}

/// Decompress a gzipped fixture (the FAT32 image is committed as .gz).
#[allow(dead_code)] // not every test binary uses the FAT32 fixture
pub fn gunzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_end(&mut out)
        .expect("gunzip fixture");
    out
}

/// Read a file handle to EOF into a `Vec`.
#[allow(dead_code)] // not every test binary reads through handles
pub fn read_all<S: Read>(file: &mut S) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = file.read(&mut buf).expect("read");
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

/// A [`Clock`] that always reports a fixed instant (deterministic timestamps).
#[derive(Debug)]
#[allow(dead_code)] // used by fat_read / exfat_rw timestamp tests
pub struct FixedClock(pub PrimitiveDateTime);

impl Clock for FixedClock {
    fn now(&self) -> PrimitiveDateTime {
        self.0
    }
}
