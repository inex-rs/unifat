//! [`DataStore`] over raw ExFAT volume storage (file payload I/O).

use embedded_io::{Read, Seek, Write};

use crate::error::FileError;
use crate::exfat::ExfatVfs;
use crate::vfs::DataStore;

/// Payload store: uncached bulk I/O, kept coherent with the volume's
/// metadata cache by the through-path helpers.
pub(crate) struct ExfatDataStore<'a, S>(&'a ExfatVfs<S>)
where
    S: Read + Write + Seek;

impl<'a, S> ExfatDataStore<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn new(vol: &'a ExfatVfs<S>) -> Self {
        Self(vol)
    }
}

impl<S> DataStore for ExfatDataStore<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FileError<S::Error>;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), FileError<S::Error>> {
        self.0.read_through_at(offset, buf).map_err(FileError::from)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), FileError<S::Error>> {
        self.0
            .write_through_at(offset, buf)
            .map_err(FileError::from)
    }

    fn flush(&self) -> Result<(), FileError<S::Error>> {
        self.0.sync().map_err(FileError::from)
    }
}
