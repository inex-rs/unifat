//! Pure FAT 32-byte directory entry layout (`DIR_*` / SFN record).

use crate::attrs::{AttrBits, Attributes};
use crate::codec::fat::DIRENTRY_SIZE;
use crate::codec::fat::entry_time::{
    EntryCreationTime, EntryLastAccessedTime, EntryModificationTime,
};
use crate::codec::fat::sfn_layout::Sfn;
use crate::codec::fat::types::FileSize;
use crate::codec::{FixedCodec, bytes, le_u16, le_u32, put_u16, put_u32};

/// On-disk FAT directory-entry attribute byte (`DIR_Attr`).
///
/// Unknown / reserved bits are preserved on round-trip. Bit values match
/// [`AttrBits`] / ExFAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub(crate) struct RawAttributes(u8);

impl RawAttributes {
    pub(crate) const VOLUME_ID: Self = Self(AttrBits::VOLUME_ID.as_u8());
    pub(crate) const DIRECTORY: Self = Self(AttrBits::DIRECTORY.as_u8());
    #[allow(dead_code)] // complete on-disk surface; archive is set via `AttrBits`
    pub(crate) const ARCHIVE: Self = Self(AttrBits::ARCHIVE.as_u8());
    pub(crate) const LFN: Self = Self(AttrBits::LFN.as_u8());

    #[inline]
    pub(crate) const fn bits(self) -> u8 {
        self.0
    }

    #[inline]
    pub(crate) const fn as_attr_bits(self) -> AttrBits {
        AttrBits::from_u8(self.0)
    }

    /// `true` if every bit in `flag` is set.
    #[inline]
    pub(crate) const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    pub(crate) fn from_attributes(attributes: Attributes, is_dir: bool) -> Self {
        Self(AttrBits::from_attributes(attributes, is_dir).as_u8())
    }
}

impl From<RawAttributes> for Attributes {
    fn from(value: RawAttributes) -> Self {
        value.as_attr_bits().to_attributes()
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RawDirEntry {
    pub(crate) sfn: Sfn,
    pub(crate) attributes: RawAttributes,
    pub(crate) _reserved: [u8; 1],
    pub(crate) created: EntryCreationTime,
    pub(crate) accessed: EntryLastAccessedTime,
    pub(crate) cluster_high: u16,
    pub(crate) modified: EntryModificationTime,
    pub(crate) cluster_low: u16,
    pub(crate) file_size: FileSize,
}

impl FixedCodec for RawDirEntry {
    const SIZE: usize = DIRENTRY_SIZE;

    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            sfn: Sfn {
                name: bytes(b, 0),
                ext: bytes(b, 8),
            },
            attributes: RawAttributes(b[11]),
            _reserved: bytes(b, 12),
            created: EntryCreationTime::decode(b[13], le_u16(b, 14), le_u16(b, 16)),
            accessed: EntryLastAccessedTime::decode(le_u16(b, 18)),
            cluster_high: le_u16(b, 20),
            modified: EntryModificationTime::decode(le_u16(b, 22), le_u16(b, 24)),
            cluster_low: le_u16(b, 26),
            file_size: le_u32(b, 28),
        })
    }

    fn write_into(&self, out: &mut [u8]) {
        let b = &mut out[..Self::SIZE];
        b[0..8].copy_from_slice(&self.sfn.name);
        b[8..11].copy_from_slice(&self.sfn.ext);
        b[11] = self.attributes.0;
        b[12..13].copy_from_slice(&self._reserved);
        let (tenths, ctime, cdate) = self.created.encode();
        b[13] = tenths;
        put_u16(b, 14, ctime);
        put_u16(b, 16, cdate);
        put_u16(b, 18, self.accessed.encode());
        put_u16(b, 20, self.cluster_high);
        let (mtime, mdate) = self.modified.encode();
        put_u16(b, 22, mtime);
        put_u16(b, 24, mdate);
        put_u16(b, 26, self.cluster_low);
        put_u32(b, 28, self.file_size);
    }
}
