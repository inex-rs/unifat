//! Directory properties that need volume location types.
//!
//! Pure on-disk layouts (`RawDirEntry`, `RawAttributes`) live in
//! [`crate::codec::fat::dir_entry`].

use super::*;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String};

use crate::fat::{ClusterIndex, FileSize, SlotChain};
use crate::path::*;
use ::time::{Date, PrimitiveDateTime};

pub(crate) use crate::codec::fat::dir_entry::{RawAttributes, RawDirEntry};

/// The subset of entry fields needed to write a slot set to disk.
#[derive(Debug, Clone)]
pub(crate) struct MinProperties {
    pub(crate) name: Box<str>,
    pub(crate) sfn: Sfn,
    pub(crate) attributes: RawAttributes,
    pub(crate) created: Option<PrimitiveDateTime>,
    pub(crate) modified: PrimitiveDateTime,
    pub(crate) accessed: Option<Date>,
    pub(crate) file_size: FileSize,
    pub(crate) data_cluster: ClusterIndex,
    /// `DIR_NTRes` (byte 12) preserved through rewrites: Windows stores
    /// lowercase-name/ext bits (0x08/0x10) there for LFN-less aliases.
    pub(crate) nt_res: u8,
    /// `Some(raw)` on FAT12/16, where bytes 20-21 are NOT a cluster-high
    /// word but the OS/2/NT EA handle — written back verbatim. `None` on
    /// FAT32 (recomputed from `data_cluster`).
    pub(crate) ea_handle: Option<u16>,
}

impl From<RawProperties> for MinProperties {
    fn from(value: RawProperties) -> Self {
        Self {
            name: Box::from(value.name),
            sfn: value.sfn,
            attributes: value.attributes,
            created: value.created,
            modified: value.modified,
            accessed: value.accessed,
            file_size: value.file_size,
            data_cluster: value.data_cluster,
            nt_res: value.nt_res,
            ea_handle: value.ea_handle,
        }
    }
}

/// A fully parsed directory entry, before it is bound to a path.
#[derive(Debug, Clone)]
pub(crate) struct RawProperties {
    pub(crate) name: String,
    pub(crate) sfn: Sfn,
    pub(crate) is_dir: bool,
    pub(crate) attributes: RawAttributes,
    pub(crate) created: Option<PrimitiveDateTime>,
    pub(crate) modified: PrimitiveDateTime,
    pub(crate) accessed: Option<Date>,
    pub(crate) file_size: FileSize,
    pub(crate) data_cluster: ClusterIndex,
    /// See [`MinProperties::nt_res`].
    pub(crate) nt_res: u8,
    /// See [`MinProperties::ea_handle`].
    pub(crate) ea_handle: Option<u16>,

    pub(crate) chain: SlotChain,
}

impl RawProperties {
    pub(crate) fn into_dir_entry<P>(self, path: P) -> DirEntry
    where
        P: AsRef<Path>,
    {
        let entry_path = path.as_ref().join(&self.name);

        DirEntry {
            entry: Properties::from_raw(self, entry_path.into()),
        }
    }

    pub(crate) fn from_chain(props: MinProperties, chain: SlotChain) -> Self {
        Self {
            name: String::from(props.name),
            sfn: props.sfn,
            is_dir: props.attributes.contains(RawAttributes::DIRECTORY),
            attributes: props.attributes,
            created: props.created,
            modified: props.modified,
            accessed: props.accessed,
            file_size: props.file_size,
            data_cluster: props.data_cluster,
            nt_res: props.nt_res,
            ea_handle: props.ea_handle,
            chain,
        }
    }
}

impl From<MinProperties> for RawDirEntry {
    fn from(value: MinProperties) -> Self {
        Self {
            sfn: value.sfn,
            attributes: value.attributes,
            _reserved: [value.nt_res],
            created: value.created.into(),
            accessed: value.accessed.into(),
            // FAT12/16: bytes 20-21 are the EA handle, preserved verbatim;
            // FAT32: the cluster-high word.
            cluster_high: value
                .ea_handle
                .unwrap_or((value.data_cluster >> (u32::BITS / 2)) as u16),
            modified: value.modified.into(),
            #[allow(clippy::cast_possible_truncation)] // we are splitting a u32 here
            cluster_low: value.data_cluster as u16,
            file_size: value.file_size,
        }
    }
}
