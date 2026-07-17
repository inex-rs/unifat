//! [`VfsBackend`] + [`PathEngine`] wiring for [`ExfatVfs`]: the generic
//! engine implements every path-level mutation; only the primitives
//! below are ExFAT-specific.

use super::{ExfatDirectory, ExfatVfs};
use crate::error::FsResult;
use crate::path::Path;
use crate::vfs::{PathEngine, VfsBackend};
use embedded_io::{Read, Seek, Write};

impl<S> PathEngine for ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    type IoError = S::Error;
    type Dir<'a>
        = ExfatDirectory<'a, S>
    where
        Self: 'a;

    fn open_dir(&self, path: &Path) -> FsResult<Self::Dir<'_>, S::Error> {
        ExfatDirectory::open_path(self, path)
    }

    fn name_policy(&self) -> &crate::name::NamePolicy {
        &self.name_policy
    }

    fn handles(&self) -> &crate::handles::HandleTable {
        &self.handles
    }

    fn clock(&self) -> &dyn crate::time::Clock {
        &*self.options.clock
    }

    fn commit(&self) -> FsResult<(), S::Error> {
        // ExFAT writes metadata eagerly; sync per operation.
        self.sync()
    }

    fn barrier(&self) -> FsResult<(), S::Error> {
        self.sync()
    }
}

impl<S> VfsBackend for ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    type IoError = S::Error;

    fn format(&self) -> crate::Format {
        crate::Format::ExFat
    }

    fn lookup(&self, path: &Path) -> FsResult<crate::dir::Metadata, Self::IoError> {
        ExfatVfs::lookup(self, path)
    }

    fn create_dir(&self, path: &Path) -> FsResult<(), Self::IoError> {
        PathEngine::create_dir(self, path)
    }

    fn remove_file(&self, path: &Path) -> FsResult<(), Self::IoError> {
        PathEngine::remove_file(self, path)
    }

    fn remove_dir(&self, path: &Path) -> FsResult<(), Self::IoError> {
        PathEngine::remove_dir(self, path)
    }

    fn remove_dir_all(&self, path: &Path) -> FsResult<(), Self::IoError> {
        PathEngine::remove_dir_all(self, path)
    }

    fn rename(&self, from: &Path, to: &Path) -> FsResult<(), Self::IoError> {
        PathEngine::rename(self, from, to)
    }

    fn set_times(
        &self,
        path: &Path,
        times: crate::entry_times::EntryTimes,
    ) -> FsResult<(), Self::IoError> {
        PathEngine::patch_entry(
            self,
            path,
            crate::vfs::EntryPatch {
                times: Some(times),
                ..Default::default()
            },
        )
    }

    fn set_attributes(
        &self,
        path: &Path,
        attrs: crate::attrs::Attributes,
    ) -> FsResult<(), Self::IoError> {
        PathEngine::patch_entry(
            self,
            path,
            crate::vfs::EntryPatch {
                attrs: Some(attrs),
                ..Default::default()
            },
        )
    }

    fn flush(&self) -> FsResult<(), Self::IoError> {
        ExfatVfs::flush(self)
    }
}
