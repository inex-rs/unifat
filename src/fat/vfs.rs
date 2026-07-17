//! [`VfsBackend`] + [`PathEngine`] wiring for [`FatVfs`]: the generic
//! engine implements every path-level mutation; only the primitives
//! below are FAT-specific.

use super::{FatDirectory, FatVfs};
use crate::error::FsResult;
use crate::path::Path;
use crate::vfs::{PathEngine, VfsBackend};
use embedded_io::{Read, Seek, Write};

impl<S> PathEngine for FatVfs<S>
where
    S: Read + Write + Seek,
{
    type IoError = S::Error;
    type Dir<'a>
        = FatDirectory<'a, S>
    where
        Self: 'a;

    fn open_dir(&self, path: &Path) -> FsResult<Self::Dir<'_>, S::Error> {
        FatDirectory::open_path(self, path)
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
        // Per-op durability: without this, every mutation of a session
        // rides in the cache until an explicit flush and hits the device
        // in one arbitrarily-ordered batch.
        self.metadata_barrier()
    }

    fn barrier(&self) -> FsResult<(), S::Error> {
        self.metadata_barrier()
    }
}

impl<S> VfsBackend for FatVfs<S>
where
    S: Read + Write + Seek,
{
    type IoError = S::Error;

    fn format(&self) -> crate::Format {
        FatVfs::format(self)
    }

    fn lookup(&self, path: &Path) -> FsResult<crate::dir::Metadata, Self::IoError> {
        FatVfs::lookup(self, path)
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
        FatVfs::unmount(self)
    }
}
