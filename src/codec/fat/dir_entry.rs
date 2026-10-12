//! Pure FAT 32-byte directory entry layout (`DIR_*` / SFN record).

use crate::attrs::{AttrBits, Attributes};
use crate::codec::fat::entry_time::{
    EntryCreationTime, EntryLastAccessedTime, EntryModificationTime,
};
use crate::codec::fat::sfn_layout::Sfn;
use layline::{FieldCodec, Layout};

/// On-disk FAT directory-entry attribute byte (`DIR_Attr`).
///
/// Unknown / reserved bits are preserved on round-trip. Bit values match
/// [`AttrBits`] / ExFAT.
#[derive(FieldCodec, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[bits(8)]
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

#[derive(Layout, Debug, Clone, Copy)]
#[layout(bytes = 32, endian = le)]
pub(crate) struct RawDirEntry {
    #[bytes(11)]
    pub(crate) sfn: Sfn,
    #[codec(8)]
    pub(crate) attributes: RawAttributes,
    pub(crate) _reserved: [u8; 1],
    #[bytes(5)]
    pub(crate) created: EntryCreationTime,
    #[bytes(2)]
    pub(crate) accessed: EntryLastAccessedTime,
    pub(crate) cluster_high: u16,
    #[bytes(4)]
    pub(crate) modified: EntryModificationTime,
    pub(crate) cluster_low: u16,
    pub(crate) file_size: u32,
}
