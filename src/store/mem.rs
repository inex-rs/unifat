//! In-memory block device for tests and RAM-backed hosts.
//!
//! Owns a growable byte image and implements
//! [`embedded_io::{Read, Write, Seek}`](embedded_io). Used by the
//! integration tests, and available to callers that want a simple RAM
//! disk without a real medium.

use alloc::vec::Vec;
use core::cmp;
use embedded_io::{Error, ErrorKind, ErrorType, Read, Seek, SeekFrom, Write};

/// RAM image used as a block storage medium — growable by default, or
/// fixed-size via [`Self::fixed`] to emulate a real bounded device.
///
/// Reads and writes are byte-addressed (not sector-windowed); the
/// filesystem's internal sector cache issues sector-sized
/// `read_exact` / write operations against this type.
#[derive(Clone, Default)]
pub struct MemBlockDevice {
    data: Vec<u8>,
    pos: u64,
    fixed: bool,
}

impl core::fmt::Debug for MemBlockDevice {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MemBlockDevice")
            .field("len", &self.data.len())
            .field("pos", &self.pos)
            .field("fixed", &self.fixed)
            .finish_non_exhaustive()
    }
}

impl MemBlockDevice {
    /// Create a growable device that owns `data` (cursor at byte 0).
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            pos: 0,
            fixed: false,
        }
    }

    /// Create a **fixed-size** device: writes past the end of `data`
    /// fail with [`MemError`] instead of growing the image, like a real
    /// bounded medium. Prefer this for testing filesystem code — a
    /// growable image silently absorbs out-of-bounds writes.
    pub fn fixed(data: Vec<u8>) -> Self {
        Self {
            data,
            pos: 0,
            fixed: true,
        }
    }

    /// Copy `bytes` into a new growable device (cursor at byte 0).
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self::new(bytes.to_vec())
    }

    /// Borrow the full image.
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Current image length in bytes.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the image is empty.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Current read/write cursor.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Consume the device and return the image bytes.
    pub fn into_inner(self) -> Vec<u8> {
        self.data
    }
}

/// I/O error for [`MemBlockDevice`] (invalid seeks; out-of-bounds writes on fixed-size devices).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemError;

impl Error for MemError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

impl core::fmt::Display for MemError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MemBlockDevice I/O error")
    }
}

impl core::error::Error for MemError {}

impl ErrorType for MemBlockDevice {
    type Error = MemError;
}

impl Read for MemBlockDevice {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        // A cursor past `usize::MAX` is past any possible in-memory length.
        let Ok(start) = usize::try_from(self.pos) else {
            return Ok(0);
        };
        if start >= self.data.len() {
            return Ok(0);
        }
        let n = cmp::min(buf.len(), self.data.len() - start);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for MemBlockDevice {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        // A write landing past `usize::MAX` cannot be backed by memory.
        let start = usize::try_from(self.pos).map_err(|_| MemError)?;
        let end = start.checked_add(buf.len()).ok_or(MemError)?;
        if end > self.data.len() {
            if self.fixed {
                // A bounded medium rejects out-of-range writes (never
                // `Ok(0)` — `write_all` panics on zero-length writes).
                return Err(MemError);
            }
            self.data.resize(end, 0);
        }
        self.data[start..end].copy_from_slice(buf);
        self.pos = end as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl Seek for MemBlockDevice {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        // Unsigned + checked signed offset: rejects seeks before byte 0
        // (and the degenerate wrap past `u64::MAX`) without `i64` casts.
        let next = match pos {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => (self.data.len() as u64).checked_add_signed(d),
        };
        self.pos = next.ok_or(MemError)?;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_write_seek_roundtrip() {
        let mut dev = MemBlockDevice::from_slice(&[0u8; 16]);
        dev.seek(SeekFrom::Start(4)).unwrap();
        assert_eq!(dev.write(&[1, 2, 3]).unwrap(), 3);
        dev.seek(SeekFrom::Start(4)).unwrap();
        let mut buf = [0u8; 3];
        assert_eq!(dev.read(&mut buf).unwrap(), 3);
        assert_eq!(buf, [1, 2, 3]);
        assert_eq!(&dev.as_slice()[4..7], &[1, 2, 3]);
    }

    #[test]
    fn seek_before_start_errors() {
        let mut dev = MemBlockDevice::from_slice(&[0u8; 4]);
        assert!(dev.seek(SeekFrom::Current(-1)).is_err());
    }

    #[test]
    fn write_grows_image() {
        let mut dev = MemBlockDevice::new(Vec::new());
        dev.write_all(&[9, 8, 7]).unwrap();
        assert_eq!(dev.as_slice(), &[9, 8, 7]);
    }
}
