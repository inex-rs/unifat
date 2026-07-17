//! Backend-agnostic directory listing ([`ReadDir`], [`DirEntry`]) and [`Metadata`].

use alloc::string::String;

use embedded_io::{Read, Seek, Write};
use time::{Date, PrimitiveDateTime};

use crate::error::FsResult;
use crate::exfat::{ExfatDirEntry, ExfatReadDir};
use crate::fat::{DirEntry as FatDirEntry, ReadDir as FatReadDir};
use crate::path::{Path, PathBuf};

/// Metadata for a file or directory, common to both FAT and ExFAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub(crate) len: u64,
    pub(crate) is_dir: bool,
    pub(crate) attributes: crate::attrs::Attributes,
    pub(crate) created: Option<PrimitiveDateTime>,
    pub(crate) modified: Option<PrimitiveDateTime>,
    pub(crate) accessed: Option<Date>,
}

impl Metadata {
    /// Size of the file's contents in bytes (`0` for directories).
    #[must_use]
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the entry has no content bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether the entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.is_dir
    }

    /// Whether the entry is a regular file.
    pub fn is_file(&self) -> bool {
        !self.is_dir
    }

    /// The entry's attribute flags (settable via
    /// [`Volume::set_attributes`](crate::Volume::set_attributes)).
    pub fn attributes(&self) -> crate::attrs::Attributes {
        self.attributes
    }

    /// Whether the read-only attribute is set.
    pub fn is_read_only(&self) -> bool {
        self.attributes.read_only
    }

    /// Whether the hidden attribute is set.
    pub fn is_hidden(&self) -> bool {
        self.attributes.hidden
    }

    /// Whether the system attribute is set.
    pub fn is_system(&self) -> bool {
        self.attributes.system
    }

    /// Creation timestamp, if the backend records one.
    pub fn created(&self) -> Option<PrimitiveDateTime> {
        self.created
    }

    /// Last-modification timestamp, if the backend records one.
    pub fn modified(&self) -> Option<PrimitiveDateTime> {
        self.modified
    }

    /// Last-access date, if the backend records one.
    pub fn accessed(&self) -> Option<Date> {
        self.accessed
    }

    pub(crate) fn root() -> Self {
        Metadata {
            len: 0,
            is_dir: true,
            attributes: crate::attrs::Attributes::default(),
            created: None,
            modified: None,
            accessed: None,
        }
    }

    pub(crate) fn from_fat(entry: &FatDirEntry) -> Self {
        Metadata {
            len: u64::from(entry.file_size()),
            is_dir: entry.is_dir(),
            attributes: *entry.attributes(),
            created: *entry.creation_time(),
            modified: Some(*entry.modification_time()),
            accessed: *entry.last_accessed_date(),
        }
    }

    pub(crate) fn from_exfat(entry: &ExfatDirEntry) -> Self {
        Metadata {
            // DataLength is the file's byte size per spec (what Windows
            // shows); ValidDataLength only marks the initialized prefix.
            // Directories report 0 like the FAT backend (their DataLength
            // is the cluster allocation, not user-meaningful content).
            len: if entry.is_dir() { 0 } else { entry.data_length },
            is_dir: entry.is_dir(),
            attributes: entry.attributes.to_attributes(),
            created: entry.created,
            modified: entry.modified,
            accessed: entry.accessed,
        }
    }
}

/// A single entry from [`Volume::read_dir`](crate::Volume::read_dir); `.`/`..` are never surfaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    name: String,
    path: PathBuf,
    metadata: Metadata,
}

impl DirEntry {
    /// The entry's file name (final path component).
    pub fn file_name(&self) -> &str {
        &self.name
    }

    /// The entry's full path from the volume root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The entry's [`Metadata`].
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Whether the entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.metadata.is_dir
    }

    /// Whether the entry is a regular file.
    pub fn is_file(&self) -> bool {
        !self.metadata.is_dir
    }
}

/// Iterator over the entries of a directory
/// ([`Volume::read_dir`](crate::Volume::read_dir)); yields `Result<DirEntry, _>`.
pub struct ReadDir<'a, S>
where
    S: Read + Write + Seek,
{
    base: PathBuf,
    inner: ReadDirInner<'a, S>,
}

enum ReadDirInner<'a, S>
where
    S: Read + Write + Seek,
{
    Fat(FatReadDir<'a, S>),
    Exfat(ExfatReadDir<'a, S>),
}

impl<S> core::fmt::Debug for ReadDir<'_, S>
where
    S: Read + Write + Seek,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ReadDir").field("base", &self.base).finish()
    }
}

impl<'a, S> ReadDir<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn fat(base: PathBuf, inner: FatReadDir<'a, S>) -> Self {
        ReadDir {
            base,
            inner: ReadDirInner::Fat(inner),
        }
    }

    pub(crate) fn exfat(base: PathBuf, inner: ExfatReadDir<'a, S>) -> Self {
        ReadDir {
            base,
            inner: ReadDirInner::Exfat(inner),
        }
    }
}

impl<S> Iterator for ReadDir<'_, S>
where
    S: Read + Write + Seek,
{
    type Item = FsResult<DirEntry, S::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            ReadDirInner::Fat(rd) => match rd.next()? {
                Ok(entry) => {
                    let name = entry.path().file_name().unwrap_or_default().into();
                    let metadata = Metadata::from_fat(&entry);
                    let path = self.base.join(&name);
                    Some(Ok(DirEntry {
                        name,
                        path,
                        metadata,
                    }))
                }
                Err(e) => Some(Err(e)),
            },
            ReadDirInner::Exfat(rd) => {
                let entry = rd.next()?;
                let metadata = Metadata::from_exfat(&entry);
                let path = self.base.join(&entry.name);
                Some(Ok(DirEntry {
                    name: entry.name,
                    path,
                    metadata,
                }))
            }
        }
    }
}
