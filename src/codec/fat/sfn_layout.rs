//! Pure 8.3 SFN on-disk layout and helpers.

use alloc::string::String;

use crate::path::path_consts;

pub(crate) const SFN_NAME_LEN: usize = 8;
pub(crate) const SFN_EXT_LEN: usize = 3;
/// Includes the `.` between name and extension when decoded.
pub(crate) const SFN_LEN: usize = SFN_NAME_LEN + 1 + SFN_EXT_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// The 8.3 short filename every FAT entry carries alongside any LFN.
pub(crate) struct Sfn {
    pub(crate) name: [u8; SFN_NAME_LEN],
    pub(crate) ext: [u8; SFN_EXT_LEN],
}

pub(crate) const CURRENT_DIR_SFN: Sfn = Sfn {
    name: {
        let mut s = [b' '; SFN_NAME_LEN];
        s[0] = path_consts::CURRENT_DIR[0];
        s
    },
    ext: [b' '; SFN_EXT_LEN],
};

pub(crate) const PARENT_DIR_SFN: Sfn = Sfn {
    name: {
        let mut s = [b' '; SFN_NAME_LEN];
        s[0] = path_consts::PARENT_DIR[0];
        s[1] = path_consts::PARENT_DIR[1];
        s
    },
    ext: [b' '; SFN_EXT_LEN],
};

impl Sfn {
    fn bytes(&self) -> [u8; SFN_NAME_LEN + SFN_EXT_LEN] {
        let mut slice = [0; SFN_NAME_LEN + SFN_EXT_LEN];
        slice[..SFN_NAME_LEN].copy_from_slice(&self.name);
        slice[SFN_NAME_LEN..].copy_from_slice(&self.ext);
        slice
    }

    pub(crate) fn gen_checksum(&self) -> u8 {
        let mut sum = 0;
        for c in self.bytes() {
            sum = (if (sum & 1) != 0 { 0x80_u8 } else { 0_u8 })
                .wrapping_add(sum >> 1)
                .wrapping_add(c)
        }
        sum
    }

    pub(crate) fn decode(&self) -> String {
        let mut name = self.name;
        // 0x05 is the on-disk escape for a real leading 0xE5 (0xE5 marks
        // a deleted slot); restore it before decoding.
        if name[0] == 0x05 {
            name[0] = 0xE5;
        }
        let mut string = String::with_capacity(SFN_LEN);
        string.push_str(decode_sfn_bytes(&name).trim_end());
        let ext = decode_sfn_bytes(&self.ext);
        let ext = ext.trim_end();
        if !ext.is_empty() {
            string.push('.');
            string.push_str(ext);
        }
        string
    }
}

fn decode_sfn_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if b.is_ascii() {
                char::from(b)
            } else {
                char::REPLACEMENT_CHARACTER
            }
        })
        .collect()
}
