//! Byte-level payload I/O for [`super::stream::StreamFile`].

/// Random-access store for file **data** clusters only (not FAT/dir/bitmap).
pub(crate) trait DataStore {
    type Error;

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), Self::Error>;
    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), Self::Error>;
    fn flush(&self) -> Result<(), Self::Error>;
}
