//! The unified file handle, [`File`], returned by both backends.

use embedded_io::{Read, Seek, SeekFrom, Write};
use time::{Date, PrimitiveDateTime};

use crate::error::FileError;
use crate::exfat::ExfatStreamFile;
use crate::fat::FatStreamFile;

/// An open file on a mounted [`Volume`](crate::Volume), borrowing it.
/// Implements the embedded-io I/O traits with errors funnelled through
/// [`FileError`]. Read-only handles fail writes with
/// [`FileError::ReadOnly`]; writable handles persist their
/// directory-entry bookkeeping when flushed or dropped.
pub struct File<'a, S>
where
    S: Read + Write + Seek,
{
    inner: Inner<'a, S>,
}

enum Inner<'a, S>
where
    S: Read + Write + Seek,
{
    // Both variants are the shared `StreamFile` over backend-specific traits.
    Fat(FatStreamFile<'a, S>),
    Exfat(ExfatStreamFile<'a, S>),
}

/// Forward a method call to whichever backend file is inside.
macro_rules! dispatch {
    ($self:ident, $f:ident => $e:expr) => {
        match &mut $self.inner {
            Inner::Fat($f) => $e,
            Inner::Exfat($f) => $e,
        }
    };
}

impl<'a, S> File<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn fat(file: FatStreamFile<'a, S>) -> Self {
        File {
            inner: Inner::Fat(file),
        }
    }

    pub(crate) fn exfat(file: ExfatStreamFile<'a, S>) -> Self {
        File {
            inner: Inner::Exfat(file),
        }
    }

    /// The file's length in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        match &self.inner {
            Inner::Fat(f) => f.len(),
            Inner::Exfat(f) => f.len(),
        }
    }

    /// Whether the file has no contents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// [`Metadata`](crate::Metadata) for this open file. The length and
    /// timestamps reflect the handle's current state (they may have
    /// advanced through writes on this handle); attributes are as of open.
    #[must_use]
    pub fn metadata(&self) -> crate::dir::Metadata {
        match &self.inner {
            Inner::Fat(f) => f.metadata(),
            Inner::Exfat(f) => f.metadata(),
        }
    }

    /// Resize the file to `size` bytes (like `std::fs::File::set_len`).
    /// Shrinking frees the tail; growing allocates, and the new range
    /// reads back as zeros. The cursor is left unchanged. Fails with
    /// [`FileError::ReadOnly`] on a read-only handle and
    /// [`FileError::FileTooLarge`] past the format's size cap.
    pub fn set_len(&mut self, size: u64) -> Result<(), FileError<S::Error>> {
        dispatch!(self, f => f.set_len(size))
    }

    /// Set the creation timestamp, written back on flush / drop. Fails
    /// with [`FileError::ReadOnly`] on a read-only handle.
    pub fn set_created(&mut self, when: PrimitiveDateTime) -> Result<(), FileError<S::Error>> {
        dispatch!(self, f => f.set_created(when))
    }

    /// Like [`set_created`](File::set_created), for the last-modification timestamp.
    pub fn set_modified(&mut self, when: PrimitiveDateTime) -> Result<(), FileError<S::Error>> {
        dispatch!(self, f => f.set_modified(when))
    }

    /// Like [`set_created`](File::set_created), for the last-access date.
    pub fn set_accessed(&mut self, when: Date) -> Result<(), FileError<S::Error>> {
        dispatch!(self, f => f.set_accessed(when))
    }
}

impl<S> core::fmt::Debug for File<'_, S>
where
    S: Read + Write + Seek,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let backend = match &self.inner {
            Inner::Fat(_) => "fat",
            Inner::Exfat(_) => "exfat",
        };
        f.debug_struct("File")
            .field("backend", &backend)
            .field("len", &self.len())
            .finish()
    }
}

impl<S> embedded_io::ErrorType for File<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FileError<S::Error>;
}

impl<S> Read for File<'_, S>
where
    S: Read + Write + Seek,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        dispatch!(self, f => f.read(buf))
    }
}

impl<S> Write for File<'_, S>
where
    S: Read + Write + Seek,
{
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        dispatch!(self, f => f.write(buf))
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        dispatch!(self, f => f.flush())
    }
}

impl<S> Seek for File<'_, S>
where
    S: Read + Write + Seek,
{
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        dispatch!(self, f => f.seek(pos))
    }
}
