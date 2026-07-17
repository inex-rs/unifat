//! Path-bearing directory entry types (I/O / VFS layer).
//!
//! Pure SFN layouts live in [`crate::codec::fat::sfn_layout`].

use super::*;

use core::ops;

use crate::fat::*;
use crate::path::*;

#[cfg(not(feature = "std"))]
use alloc::{boxed::Box, string::String};

use ::time;
use embedded_io::*;
use time::{Date, PrimitiveDateTime};

/// Re-export of the crate-wide attribute flags (FAT + ExFAT).
pub(crate) use crate::attrs::Attributes;

pub(crate) use crate::codec::fat::DIRENTRY_SIZE;
pub(crate) use crate::codec::fat::sfn_layout::{
    CURRENT_DIR_SFN, PARENT_DIR_SFN, SFN_EXT_LEN, SFN_NAME_LEN, Sfn,
};

/// The resolved metadata of one filesystem entry, path-anchored.
#[derive(Clone, Debug)]
pub(crate) struct Properties {
    pub(crate) path: Box<Path>,
    pub(crate) sfn: Sfn,
    pub(crate) is_dir: bool,
    pub(crate) attributes: Attributes,
    pub(crate) created: Option<PrimitiveDateTime>,
    pub(crate) modified: PrimitiveDateTime,
    pub(crate) accessed: Option<Date>,
    pub(crate) file_size: u32,
    pub(crate) data_cluster: u32,
    /// See `MinProperties::nt_res`.
    pub(crate) nt_res: u8,
    /// See `MinProperties::ea_handle`.
    pub(crate) ea_handle: Option<u16>,

    pub(crate) chain: SlotChain,
}

impl Properties {
    #[inline]
    /// The full path this entry was resolved from.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[inline]
    /// The decoded 8.3 short name backing this entry.
    #[allow(dead_code)] // SFN accessor for tooling / future public metadata
    pub(crate) fn sfn(&self) -> String {
        self.sfn.decode()
    }

    #[inline]
    /// `true` when the entry is a directory.
    pub(crate) fn is_dir(&self) -> bool {
        self.is_dir
    }

    #[inline]
    pub(crate) fn attributes(&self) -> &Attributes {
        &self.attributes
    }

    #[inline]
    /// When this entry was created (max resolution: 10 ms); optional in the FAT32 spec
    pub(crate) fn creation_time(&self) -> &Option<PrimitiveDateTime> {
        &self.created
    }

    #[inline]
    /// When this entry was last modified (max resolution: 2 secs)
    pub(crate) fn modification_time(&self) -> &PrimitiveDateTime {
        &self.modified
    }

    #[inline]
    /// When this entry was last accessed (max resolution: 1 day); optional in the FAT32 spec
    pub(crate) fn last_accessed_date(&self) -> &Option<Date> {
        &self.accessed
    }

    #[inline]
    /// The size of this entry; always `0` for directories
    pub(crate) fn file_size(&self) -> u32 {
        self.file_size
    }
}

impl Properties {
    pub(crate) fn from_raw(raw_props: RawProperties, path: Box<Path>) -> Self {
        Self {
            path,
            sfn: raw_props.sfn,
            is_dir: raw_props.is_dir,
            attributes: raw_props.attributes.into(),
            created: raw_props.created,
            modified: raw_props.modified,
            accessed: raw_props.accessed,
            file_size: raw_props.file_size,
            data_cluster: raw_props.data_cluster,
            nt_res: raw_props.nt_res,
            ea_handle: raw_props.ea_handle,
            chain: raw_props.chain,
        }
    }
}

/// One item yielded by a directory listing — [`Properties`] plus `Deref` sugar.
#[derive(Debug)]
pub(crate) struct DirEntry {
    pub(crate) entry: Properties,
}

impl ops::Deref for DirEntry {
    type Target = Properties;

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}

/// Yields each child of a directory in on-disk slot order.
///
/// Callers should treat that order as unspecified.
#[derive(Debug)]
pub(crate) struct ReadDir<'a, S>
where
    S: Read + Write + Seek,
{
    inner: FatSlotIter<'a, S>,
    parent: Box<Path>,
}

impl<'a, S> ReadDir<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn new<P>(fs: &'a FatVfs<S>, dir: FatDir, parent: P) -> Self
    where
        P: AsRef<Path>,
    {
        Self {
            inner: FatSlotIter::new(fs, dir),
            parent: parent.as_ref().into(),
        }
    }
}

impl<S> Iterator for ReadDir<'_, S>
where
    S: Read + Write + Seek,
{
    type Item = crate::FsResult<DirEntry, S::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.inner.next()? {
                Ok(value) => {
                    // std::fs-style: only `.`/`..` are hidden; callers
                    // filter hidden/system files via `Metadata`
                    if [path_consts::CURRENT_DIR_STR, path_consts::PARENT_DIR_STR]
                        .contains(&value.name.as_str())
                    {
                        continue;
                    }
                    return Some(Ok(value.into_dir_entry(&self.parent)));
                }
                Err(err) => return Some(Err(err)),
            }
        }
    }
}
