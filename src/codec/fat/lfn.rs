//! Pure LFN entry layout and free/used slot markers.

use core::mem;

use crate::codec::{FixedCodec, bytes};

const LFN_FIRST_CHARS: usize = 5;
const LFN_MID_CHARS: usize = 6;
const LFN_LAST_CHARS: usize = 2;
pub(crate) const CHARS_PER_LFN_ENTRY: usize = LFN_FIRST_CHARS + LFN_MID_CHARS + LFN_LAST_CHARS;
pub(crate) const LFN_CHAR_LIMIT: usize = 255; // not including the trailing null

/// Free directory entry (first byte of SFN name).
pub(crate) const UNUSED_ENTRY: u8 = 0xE5;
/// End-of-directory marker (first byte of SFN name).
pub(crate) const LAST_AND_UNUSED_ENTRY: u8 = 0x00;

#[derive(Debug)]
pub(crate) struct LfnEntry {
    /// Bit `0x40` (`LAST_LONG_ENTRY`) is set on the final entry of the set.
    pub(crate) order: u8,
    pub(crate) first_chars: [u8; LFN_FIRST_CHARS * 2],
    /// Always equals 0x0F.
    pub(crate) _lfn_attribute: u8,
    /// Always 0 per the FAT specification.
    pub(crate) _long_entry_type: u8,
    /// Checksum of the paired SFN.
    pub(crate) checksum: u8,
    pub(crate) mid_chars: [u8; LFN_MID_CHARS * 2],
    pub(crate) _zeroed: [u8; 2],
    pub(crate) last_chars: [u8; LFN_LAST_CHARS * 2],
}

impl FixedCodec for LfnEntry {
    const SIZE: usize = crate::codec::fat::DIRENTRY_SIZE;

    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            order: b[0],
            first_chars: bytes(b, 1),
            _lfn_attribute: b[11],
            _long_entry_type: b[12],
            checksum: b[13],
            mid_chars: bytes(b, 14),
            _zeroed: bytes(b, 26),
            last_chars: bytes(b, 28),
        })
    }

    fn write_into(&self, out: &mut [u8]) {
        let b = &mut out[..Self::SIZE];
        b[0] = self.order;
        b[1..11].copy_from_slice(&self.first_chars);
        b[11] = self._lfn_attribute;
        b[12] = self._long_entry_type;
        b[13] = self.checksum;
        b[14..26].copy_from_slice(&self.mid_chars);
        b[26..28].copy_from_slice(&self._zeroed);
        b[28..32].copy_from_slice(&self.last_chars);
    }
}

impl LfnEntry {
    pub(crate) fn utf16_units(&self) -> [u16; CHARS_PER_LFN_ENTRY] {
        let mut slice = [0_u8; CHARS_PER_LFN_ENTRY * mem::size_of::<u16>()];

        slice[..LFN_FIRST_CHARS * 2].copy_from_slice(&self.first_chars);
        slice[LFN_FIRST_CHARS * 2..(LFN_FIRST_CHARS + LFN_MID_CHARS) * 2]
            .copy_from_slice(&self.mid_chars);
        slice[(LFN_FIRST_CHARS + LFN_MID_CHARS) * 2..].copy_from_slice(&self.last_chars);

        let mut out_slice = [0_u16; CHARS_PER_LFN_ENTRY];
        for (i, chunk) in slice.chunks(mem::size_of::<u16>()).enumerate() {
            out_slice[i] = u16::from_le_bytes(chunk.try_into().unwrap());
        }
        out_slice
    }

    #[inline]
    pub(crate) fn verify_signature(&self) -> bool {
        self._long_entry_type == 0 && self._zeroed.iter().all(|v| *v == 0)
    }
}
