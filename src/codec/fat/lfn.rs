//! Pure LFN entry layout and free/used slot markers.

use layline::Layout;

const LFN_FIRST_CHARS: usize = 5;
const LFN_MID_CHARS: usize = 6;
const LFN_LAST_CHARS: usize = 2;
pub(crate) const CHARS_PER_LFN_ENTRY: usize = LFN_FIRST_CHARS + LFN_MID_CHARS + LFN_LAST_CHARS;
pub(crate) const LFN_CHAR_LIMIT: usize = 255; // not including the trailing null

/// Free directory entry (first byte of SFN name).
pub(crate) const UNUSED_ENTRY: u8 = 0xE5;
/// End-of-directory marker (first byte of SFN name).
pub(crate) const LAST_AND_UNUSED_ENTRY: u8 = 0x00;

#[derive(Layout, Debug)]
#[layout(bytes = 32, endian = le)]
pub(crate) struct LfnEntry {
    /// Bit `0x40` (`LAST_LONG_ENTRY`) is set on the final entry of the set.
    pub(crate) order: u8,
    pub(crate) first_chars: [u16; 5],
    /// Always equals 0x0F.
    pub(crate) _lfn_attribute: u8,
    /// Always 0 per the FAT specification.
    pub(crate) _long_entry_type: u8,
    /// Checksum of the paired SFN.
    pub(crate) checksum: u8,
    pub(crate) mid_chars: [u16; 6],
    pub(crate) _zeroed: [u8; 2],
    pub(crate) last_chars: [u16; 2],
}

impl LfnEntry {
    pub(crate) fn utf16_units(&self) -> [u16; CHARS_PER_LFN_ENTRY] {
        let mut units = [0_u16; CHARS_PER_LFN_ENTRY];
        let (first, rest) = units.split_at_mut(LFN_FIRST_CHARS);
        let (mid, last) = rest.split_at_mut(LFN_MID_CHARS);
        first.copy_from_slice(&self.first_chars);
        mid.copy_from_slice(&self.mid_chars);
        last.copy_from_slice(&self.last_chars);
        units
    }

    /// Split up to [`CHARS_PER_LFN_ENTRY`] units across the three name
    /// runs of one entry.
    pub(crate) fn set_utf16_units(&mut self, units: &[u16; CHARS_PER_LFN_ENTRY]) {
        let (first, rest) = units.split_at(LFN_FIRST_CHARS);
        let (mid, last) = rest.split_at(LFN_MID_CHARS);
        self.first_chars.copy_from_slice(first);
        self.mid_chars.copy_from_slice(mid);
        self.last_chars.copy_from_slice(last);
    }

    #[inline]
    pub(crate) fn verify_signature(&self) -> bool {
        self._long_entry_type == 0 && self._zeroed.iter().all(|v| *v == 0)
    }
}
