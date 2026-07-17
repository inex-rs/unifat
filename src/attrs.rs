//! File attribute flags shared by classic FAT and ExFAT.
//!
//! The public surface is the bool-field [`Attributes`] struct. On disk both
//! formats use the same low-byte layout (`READ_ONLY=0x01` … `ARCHIVE=0x20`);
//! [`AttrBits`] is the internal mask used when packing/unpacking.

/// User-visible attribute flags (directory-ness lives on [`Metadata`](crate::Metadata)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Attributes {
    /// Read-only; writes should fail.
    pub read_only: bool,
    /// Hidden unless explicitly requested.
    pub hidden: bool,
    /// System file; often hidden unless explicitly requested.
    pub system: bool,
    /// Archive (modified since last backup); only concerns archival software.
    pub archive: bool,
}

/// Shared on-disk attribute bit values (FAT low byte / ExFAT `u16`).
pub(crate) mod bits {
    pub const READ_ONLY: u16 = 0x0001;
    pub const HIDDEN: u16 = 0x0002;
    pub const SYSTEM: u16 = 0x0004;
    pub const VOLUME_ID: u16 = 0x0008;
    pub const DIRECTORY: u16 = 0x0010;
    pub const ARCHIVE: u16 = 0x0020;
    /// LFN slot marker: `READ_ONLY | HIDDEN | SYSTEM | VOLUME_ID`.
    pub const LFN: u16 = 0x000F;
}

/// On-disk attribute mask. Unknown/reserved high bits are preserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub(crate) struct AttrBits(u16);

impl AttrBits {
    pub(crate) const READ_ONLY: Self = Self(bits::READ_ONLY);
    pub(crate) const HIDDEN: Self = Self(bits::HIDDEN);
    pub(crate) const SYSTEM: Self = Self(bits::SYSTEM);
    pub(crate) const VOLUME_ID: Self = Self(bits::VOLUME_ID);
    pub(crate) const DIRECTORY: Self = Self(bits::DIRECTORY);
    pub(crate) const ARCHIVE: Self = Self(bits::ARCHIVE);
    pub(crate) const LFN: Self = Self(bits::LFN);

    #[inline]
    pub(crate) const fn empty() -> Self {
        Self(0)
    }

    #[inline]
    pub(crate) const fn from_u8(bits: u8) -> Self {
        Self(bits as u16)
    }

    #[inline]
    pub(crate) const fn from_u16(bits: u16) -> Self {
        Self(bits)
    }

    /// On-disk FAT attribute byte — FAT stores only the low byte of the
    /// shared mask (every FAT bit is ≤ `0x3F`; high bits are ExFAT-only).
    #[inline]
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) const fn as_u8(self) -> u8 {
        self.0 as u8
    }

    /// On-disk `u16` mask (ExFAT attribute field).
    #[inline]
    pub(crate) const fn bits(self) -> u16 {
        self.0
    }

    /// `true` if every bit in `flag` is set.
    #[inline]
    pub(crate) const fn contains(self, flag: Self) -> bool {
        self.0 & flag.0 == flag.0
    }

    #[inline]
    pub(crate) fn set(&mut self, flag: Self, on: bool) {
        if on {
            self.0 |= flag.0;
        } else {
            self.0 &= !flag.0;
        }
    }

    pub(crate) fn from_attributes(attributes: Attributes, is_dir: bool) -> Self {
        let mut raw = Self::empty();
        raw.set(Self::READ_ONLY, attributes.read_only);
        raw.set(Self::HIDDEN, attributes.hidden);
        raw.set(Self::SYSTEM, attributes.system);
        raw.set(Self::ARCHIVE, attributes.archive);
        raw.set(Self::DIRECTORY, is_dir);
        raw
    }

    pub(crate) const fn to_attributes(self) -> Attributes {
        Attributes {
            read_only: self.contains(Self::READ_ONLY),
            hidden: self.contains(Self::HIDDEN),
            system: self.contains(Self::SYSTEM),
            archive: self.contains(Self::ARCHIVE),
        }
    }

    #[inline]
    pub(crate) const fn is_dir(self) -> bool {
        self.contains(Self::DIRECTORY)
    }
}
