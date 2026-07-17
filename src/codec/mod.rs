//! Pure on-disk codecs (bytes ↔ structs). No I/O, no paths, no RefCell.
//!
//! Every layout is fixed-offset little-endian, hand-coded via the
//! helpers below (no codec dependency): decode with `parse`, encode
//! with `write_into`, both against exactly [`FixedCodec::SIZE`] bytes.

pub(crate) mod exfat;
pub(crate) mod fat;
pub(crate) mod time;

/// A fixed-size on-disk record. `parse` returns `None` when a
/// validating field rejects the bytes or the slice is shorter than
/// [`Self::SIZE`]; `write_into` fills the first [`Self::SIZE`] bytes of
/// `out`.
///
/// Both directions expect correctly sized slices from their callers —
/// record sizes are compile-time constants at every call site.
pub(crate) trait FixedCodec: Sized {
    const SIZE: usize;
    fn parse(bytes: &[u8]) -> Option<Self>;
    fn write_into(&self, out: &mut [u8]);
}

// ── Little-endian field helpers ─────────────────────────────────────────

#[inline]
pub(crate) fn le_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(b[off..off + 2].try_into().expect("caller sized the slice"))
}

#[inline]
pub(crate) fn le_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().expect("caller sized the slice"))
}

#[inline]
pub(crate) fn le_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().expect("caller sized the slice"))
}

#[inline]
pub(crate) fn put_u16(b: &mut [u8], off: usize, v: u16) {
    b[off..off + 2].copy_from_slice(&v.to_le_bytes());
}

#[inline]
pub(crate) fn put_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

#[inline]
pub(crate) fn put_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

/// Copy a fixed-size byte array field out of a record.
#[inline]
pub(crate) fn bytes<const N: usize>(b: &[u8], off: usize) -> [u8; N] {
    b[off..off + N].try_into().expect("caller sized the slice")
}
