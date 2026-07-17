//! [`DataStore`] over the FAT volume sector cache (file payload I/O).

use embedded_io::{Read, Seek, Write};

use crate::error::FileError;
use crate::fat::FatVfs;
use crate::vfs::DataStore;

/// Payload store: uncached bulk I/O, kept coherent with the metadata
/// cache by the through-path helpers.
pub(crate) struct FatDataStore<'a, S>(&'a FatVfs<S>)
where
    S: Read + Write + Seek;

impl<'a, S> FatDataStore<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn new(fs: &'a FatVfs<S>) -> Self {
        Self(fs)
    }
}

impl<S> DataStore for FatDataStore<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FileError<S::Error>;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), FileError<S::Error>> {
        self.0
            .read_bytes_through(offset, buf)
            .map_err(FileError::from)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), FileError<S::Error>> {
        self.0
            .write_bytes_through(offset, buf)
            .map_err(FileError::from)
    }

    fn flush(&self) -> Result<(), FileError<S::Error>> {
        self.0.metadata_barrier().map_err(FileError::from)
    }
}
