//! Private VFS backend contract implemented by FatVfs / ExfatVfs.
//!
//! [`crate::volume::Volume`] dispatches through this trait for path-level
//! mutators and metadata. Open / `read_dir` stay on the concrete backends
//! (they return format-specific stream types). Hot path stays monomorphized
//! enum match — no `dyn`.

use crate::dir::Metadata;
use crate::entry_times::EntryTimes;
use crate::error::FsResult;
use crate::path::Path;
use crate::volume::Format;

/// Open mode for path-based file open (Volume / Backend dispatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenFlags {
    /// Shared read-only open.
    Read,
    /// Exclusive read-write open.
    ReadWrite,
}

/// Format-agnostic filesystem operations (private to the crate).
pub(crate) trait VfsBackend {
    type IoError: embedded_io::Error;

    fn format(&self) -> Format;

    fn lookup(&self, path: &Path) -> FsResult<Metadata, Self::IoError>;

    fn create_dir(&self, path: &Path) -> FsResult<(), Self::IoError>;

    fn remove_file(&self, path: &Path) -> FsResult<(), Self::IoError>;

    fn remove_dir(&self, path: &Path) -> FsResult<(), Self::IoError>;

    fn remove_dir_all(&self, path: &Path) -> FsResult<(), Self::IoError>;

    fn rename(&self, from: &Path, to: &Path) -> FsResult<(), Self::IoError>;

    /// Patch entry timestamps at `path` without opening a handle
    /// (`None` fields keep their current value). Takes the exclusive
    /// lock for the duration — fails `FileLocked` if a handle is open.
    fn set_times(&self, path: &Path, times: EntryTimes) -> FsResult<(), Self::IoError>;

    /// Replace the user-settable attribute flags of the entry at `path`.
    /// Same locking rules as [`Self::set_times`].
    fn set_attributes(
        &self,
        path: &Path,
        attrs: crate::attrs::Attributes,
    ) -> FsResult<(), Self::IoError>;

    /// Flush volume-level dirty state (sector cache, FSInfo, storage).
    fn flush(&self) -> FsResult<(), Self::IoError>;
}
