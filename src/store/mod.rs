//! Block device abstraction and write-back sector cache.

mod cache;
mod mem;

pub(crate) use cache::SectorCache;
pub use mem::{MemBlockDevice, MemError};

/// Media that can host a volume: random-access read + write + seek.
///
/// Used as the bound on [`SectorCache`] so the store layer names the
/// contract explicitly (call sites still monomorphize on concrete `S`).
pub(crate) trait BlockDevice:
    embedded_io::Read + embedded_io::Write + embedded_io::Seek
{
}

impl<T> BlockDevice for T where T: embedded_io::Read + embedded_io::Write + embedded_io::Seek {}

/// Fill `buf` as far as the device allows, looping over short reads
/// (`Read::read` may legally return fewer bytes than requested). Returns
/// the number of bytes read; less than `buf.len()` only at end-of-medium.
pub(crate) fn read_full<S: embedded_io::Read>(
    storage: &mut S,
    buf: &mut [u8],
) -> Result<usize, S::Error> {
    let mut done = 0;
    while done < buf.len() {
        let n = storage.read(&mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    Ok(done)
}
