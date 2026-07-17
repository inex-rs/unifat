//! Volume and file I/O errors.

use embedded_io::*;

/// Classification of on-disk corruption (no codec library types leak out).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorruptKind {
    /// Boot sector / BPB / VBR invalid.
    BootSector,
    /// FAT32 FSInfo structure invalid.
    FsInfo,
    /// FAT table copies disagree or are unreadable.
    FatTables,
    /// Cluster chain is malformed or cyclic.
    ClusterChain,
    /// Directory entry chain or set is malformed.
    DirEntry,
    /// On-disk structure failed codec (de)serialization.
    Codec,
    /// Other structural problem (e.g. medium too small, unexpected EOF).
    Other,
}

/// Filesystem operation error (volume / path / metadata operations).
/// Byte-stream I/O on an open [`File`](crate::File) uses the narrower
/// [`FileError`] instead.
#[non_exhaustive]
#[derive(Debug)]
pub enum FsError<I>
where
    I: embedded_io::Error,
{
    /// Underlying storage I/O failed.
    Io(I),
    /// Path or name was not found.
    NotFound,
    /// Path already exists.
    AlreadyExists,
    /// Expected a directory.
    NotADirectory,
    /// Found a directory when a file was required.
    IsADirectory,
    /// Directory still has children.
    DirectoryNotEmpty,
    /// File is open with an incompatible mode (writable is exclusive).
    FileLocked,
    /// Volume or directory has no free space / slots.
    StorageFull,
    /// FAT16 fixed root has no free directory slots.
    RootDirectoryFull,
    /// Path or argument rejected (including malformed paths).
    InvalidInput,
    /// The file reached the format's maximum size (4 GiB - 1 on FAT).
    FileTooLarge,
    /// Target is marked read-only (or equivalent).
    ReadOnlyFile,
    /// Operation not permitted (e.g. rename root).
    PermissionDenied,
    /// On-disk structures are corrupt or inconsistent.
    Corrupt(CorruptKind),
    /// Format or feature is not supported (e.g. FAT12, non-MBR).
    Unsupported,
}

/// A [`Result`] whose error is [`FsError`] over storage error `E`.
pub type FsResult<T, E> = Result<T, FsError<E>>;

impl<I> From<I> for FsError<I>
where
    I: embedded_io::Error,
{
    #[inline]
    fn from(value: I) -> Self {
        FsError::Io(value)
    }
}

impl<I> From<ReadExactError<I>> for FsError<I>
where
    I: embedded_io::Error,
{
    #[inline]
    fn from(value: ReadExactError<I>) -> Self {
        match value {
            ReadExactError::UnexpectedEof => FsError::Corrupt(CorruptKind::Other),
            ReadExactError::Other(e) => FsError::Io(e),
        }
    }
}

impl<I> core::fmt::Display for FsError<I>
where
    I: embedded_io::Error,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FsError::Io(e) => write!(f, "I/O error: {e}"),
            FsError::NotFound => f.write_str("not found"),
            FsError::AlreadyExists => f.write_str("already exists"),
            FsError::NotADirectory => f.write_str("not a directory"),
            FsError::IsADirectory => f.write_str("is a directory"),
            FsError::DirectoryNotEmpty => f.write_str("directory not empty"),
            FsError::FileLocked => f.write_str("file locked"),
            FsError::StorageFull => f.write_str("storage full"),
            FsError::RootDirectoryFull => f.write_str("root directory full"),
            FsError::InvalidInput => f.write_str("invalid input"),
            FsError::FileTooLarge => f.write_str("file too large for the filesystem"),
            FsError::ReadOnlyFile => f.write_str("read-only file"),
            FsError::PermissionDenied => f.write_str("permission denied"),
            FsError::Corrupt(k) => write!(f, "corrupt volume ({k:?})"),
            FsError::Unsupported => f.write_str("unsupported filesystem"),
        }
    }
}

impl<I> core::error::Error for FsError<I> where I: embedded_io::Error {}

/// Error surfaced by [`File`](crate::File) I/O. Implements
/// [`embedded_io::Error`], and intentionally exposes only the cases a
/// byte-stream operation can hit (a read past EOF simply returns `Ok(0)`).
#[non_exhaustive]
#[derive(Debug)]
pub enum FileError<I>
where
    I: embedded_io::Error,
{
    /// The underlying storage returned an error.
    Io(I),
    /// The volume ran out of free space while growing the file.
    StorageFull,
    /// The file reached the format's maximum size (4 GiB - 1 on FAT).
    FileTooLarge,
    /// A write was attempted on a handle opened for reading only.
    ReadOnly,
    /// A seek would place the cursor before byte 0.
    InvalidSeek,
    /// The on-disk structures backing this file are malformed.
    Corrupt,
}

impl<I> core::fmt::Display for FileError<I>
where
    I: embedded_io::Error,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FileError::Io(e) => write!(f, "underlying storage error: {e}"),
            FileError::StorageFull => f.write_str("the volume ran out of free space"),
            FileError::FileTooLarge => f.write_str("the file reached the format's maximum size"),
            FileError::ReadOnly => f.write_str("write attempted on a read-only handle"),
            FileError::InvalidSeek => f.write_str("seek before the start of the file"),
            FileError::Corrupt => {
                f.write_str("the on-disk structures backing this file are malformed")
            }
        }
    }
}

impl<I> core::error::Error for FileError<I> where I: embedded_io::Error {}

impl<I> embedded_io::Error for FileError<I>
where
    I: embedded_io::Error,
{
    #[inline]
    fn kind(&self) -> ErrorKind {
        match self {
            FileError::Io(e) => e.kind(),
            // No disk-full kind exists in embedded-io; OutOfMemory
            // (RAM) would be actively misleading.
            FileError::StorageFull => ErrorKind::Other,
            FileError::FileTooLarge => ErrorKind::Other,
            FileError::ReadOnly => ErrorKind::PermissionDenied,
            FileError::InvalidSeek => ErrorKind::InvalidInput,
            FileError::Corrupt => ErrorKind::InvalidData,
        }
    }
}

impl<I> From<FsError<I>> for FileError<I>
where
    I: embedded_io::Error,
{
    fn from(value: FsError<I>) -> Self {
        match value {
            FsError::Io(e) => FileError::Io(e),
            FsError::StorageFull | FsError::RootDirectoryFull => FileError::StorageFull,
            FsError::FileTooLarge => FileError::FileTooLarge,
            FsError::ReadOnlyFile => FileError::ReadOnly,
            _ => FileError::Corrupt,
        }
    }
}

impl<I> From<FileError<I>> for FsError<I>
where
    I: embedded_io::Error,
{
    fn from(value: FileError<I>) -> Self {
        match value {
            FileError::Io(e) => FsError::Io(e),
            FileError::StorageFull => FsError::StorageFull,
            FileError::FileTooLarge => FsError::FileTooLarge,
            FileError::ReadOnly => FsError::ReadOnlyFile,
            FileError::InvalidSeek => FsError::InvalidInput,
            // `FileError::Corrupt` carries no structure kind; don't
            // invent one on the way back.
            FileError::Corrupt => FsError::Corrupt(CorruptKind::Other),
        }
    }
}
