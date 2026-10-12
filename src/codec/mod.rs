//! Pure on-disk codecs (bytes ↔ structs). No I/O, no paths, no RefCell.
//!
//! Every fixed record is a [`layline::Layout`] (`#[layout(bytes = N)]`,
//! little-endian): `T::decode(&[u8; N])` / `t.encode()`, with the derive
//! proving at compile time that the fields tile all `N` bytes. The exFAT
//! entry set, whose length and checksum depend on its contents, is a
//! [`layline::Message`].

pub(crate) mod exfat;
pub(crate) mod fat;
pub(crate) mod mbr;
pub(crate) mod time;

/// The first `N` bytes of `b` as a fixed record window, or `None` if `b`
/// is shorter.
#[inline]
pub(crate) fn window<const N: usize>(b: &[u8]) -> Option<&[u8; N]> {
    b.get(..N)?.try_into().ok()
}
