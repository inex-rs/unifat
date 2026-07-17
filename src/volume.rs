//! [`Volume`]: the unified entry point that sniffs the boot sector at
//! mount and dispatches every operation to the FAT or ExFAT backend.

use alloc::vec::Vec;

use embedded_io::{Read, Seek, SeekFrom, Write};

use time::{Date, PrimitiveDateTime};

use crate::dir::{Metadata, ReadDir};
use crate::entry_times::EntryTimes;
use crate::error::{FsError, FsResult};
use crate::exfat::ExfatVfs;
use crate::fat::FatVfs;
use crate::file::File;
use crate::mbr::{Partition, PartitionTable};
use crate::options::FsOptions;
use crate::path::{Path, PathBuf, WindowsComponent};
use crate::vfs::{OpenFlags, PathEngine, VfsBackend, prepare_path};

/// The on-disk filesystem format of a mounted [`Volume`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// FAT16 — small volumes (~8 MB – 16 GB).
    Fat16,
    /// FAT32 — the common case (~256 MB – 16 TB).
    Fat32,
    /// ExFAT.
    ExFat,
}

/// A mounted filesystem — FAT or ExFAT — behind one unified API. Even
/// read access needs `S: Read + Write + Seek`; to mount a genuinely
/// read-only medium, wrap it in an adapter whose `Write` impl errors.
///
/// Path arguments accept anything that implements [`AsRef<Path>`] —
/// including `&str`, `String`, [`Path`], and [`PathBuf`]. Paths are
/// validated and normalized once at this boundary before reaching a backend.
pub struct Volume<S>
where
    S: Read + Write + Seek,
{
    inner: Backend<S>,
}

// A volume holds exactly one backend, so the size skew between the two
// variants doesn't matter — boxing would only add pointer chasing.
#[allow(clippy::large_enum_variant)]
enum Backend<S>
where
    S: Read + Write + Seek,
{
    Fat(FatVfs<S>),
    Exfat(ExfatVfs<S>),
}

/// Forward a call to whichever backend is inside — the enum's only job.
/// Paths are already normalized by [`Volume`].
macro_rules! dispatch {
    ($self:expr, $fs:ident => $e:expr) => {
        match $self {
            Backend::Fat($fs) => $e,
            Backend::Exfat($fs) => $e,
        }
    };
}

/// The [`VfsBackend`] contract, dispatched to the mounted backend. This
/// covers every method whose signature is format-agnostic; `Backend`'s
/// inherent impl below wraps the ones returning format-specific types.
impl<S> VfsBackend for Backend<S>
where
    S: Read + Write + Seek,
{
    type IoError = S::Error;

    fn format(&self) -> Format {
        dispatch!(self, fs => VfsBackend::format(fs))
    }

    fn lookup(&self, path: &Path) -> FsResult<Metadata, S::Error> {
        dispatch!(self, fs => VfsBackend::lookup(fs, path))
    }

    fn create_dir(&self, path: &Path) -> FsResult<(), S::Error> {
        dispatch!(self, fs => VfsBackend::create_dir(fs, path))
    }

    fn remove_file(&self, path: &Path) -> FsResult<(), S::Error> {
        dispatch!(self, fs => VfsBackend::remove_file(fs, path))
    }

    fn remove_dir(&self, path: &Path) -> FsResult<(), S::Error> {
        dispatch!(self, fs => VfsBackend::remove_dir(fs, path))
    }

    fn remove_dir_all(&self, path: &Path) -> FsResult<(), S::Error> {
        dispatch!(self, fs => VfsBackend::remove_dir_all(fs, path))
    }

    fn rename(&self, from: &Path, to: &Path) -> FsResult<(), S::Error> {
        dispatch!(self, fs => VfsBackend::rename(fs, from, to))
    }

    fn set_times(&self, path: &Path, times: EntryTimes) -> FsResult<(), S::Error> {
        dispatch!(self, fs => VfsBackend::set_times(fs, path, times))
    }

    fn set_attributes(
        &self,
        path: &Path,
        attrs: crate::attrs::Attributes,
    ) -> FsResult<(), S::Error> {
        dispatch!(self, fs => VfsBackend::set_attributes(fs, path, attrs))
    }

    fn flush(&self) -> FsResult<(), S::Error> {
        dispatch!(self, fs => VfsBackend::flush(fs))
    }
}

/// Operations returning format-specific stream/iterator types, wrapped
/// into the unified public handles here.
impl<S> Backend<S>
where
    S: Read + Write + Seek,
{
    fn read_dir(&self, path: &Path) -> FsResult<ReadDir<'_, S>, S::Error> {
        let base = path.to_path_buf();
        match self {
            Backend::Fat(fs) => Ok(ReadDir::fat(base, fs.read_dir(path)?)),
            Backend::Exfat(fs) => Ok(ReadDir::exfat(base, fs.read_dir(path)?)),
        }
    }

    fn open(&self, path: &Path, flags: OpenFlags) -> FsResult<File<'_, S>, S::Error> {
        match (self, flags) {
            (Backend::Fat(fs), OpenFlags::Read) => Ok(File::fat(fs.get_ro_file(path)?)),
            (Backend::Fat(fs), OpenFlags::ReadWrite) => Ok(File::fat(fs.get_rw_file(path)?)),
            (Backend::Exfat(fs), OpenFlags::Read) => Ok(File::exfat(fs.open_file(path)?)),
            (Backend::Exfat(fs), OpenFlags::ReadWrite) => Ok(File::exfat(fs.open_file_rw(path)?)),
        }
    }

    /// Create the file if missing, or open and truncate if it exists.
    fn create_or_truncate(&self, path: &Path) -> FsResult<File<'_, S>, S::Error> {
        match self.open(path, OpenFlags::ReadWrite) {
            Ok(mut file) => {
                file.set_len(0).map_err(FsError::from)?;
                Ok(file)
            }
            Err(FsError::NotFound) => {
                dispatch!(self, fs => PathEngine::create_empty_file(fs, path))?;
                self.open(path, OpenFlags::ReadWrite)
            }
            Err(e) => Err(e),
        }
    }

    fn into_storage(self) -> S {
        dispatch!(self, fs => fs.into_inner())
    }
}

impl<S> core::fmt::Debug for Volume<S>
where
    S: Read + Write + Seek,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Volume")
            .field("format", &self.format())
            .finish()
    }
}

impl<S> Volume<S>
where
    S: Read + Write + Seek,
{
    /// Mount the filesystem on `storage` (cursor at byte 0),
    /// auto-detecting FAT vs ExFAT, with default [`FsOptions`].
    ///
    /// Fails with [`FsError::Unsupported`] for FAT12 or an unrecognized
    /// format, and [`FsError::Corrupt`] for an invalid boot sector.
    pub fn mount(storage: S) -> FsResult<Self, S::Error> {
        Self::mount_with(storage, FsOptions::default())
    }

    /// Mount with explicit [`FsOptions`] (clock, auto-timestamps).
    pub fn mount_with(mut storage: S, options: FsOptions) -> FsResult<Self, S::Error> {
        let mut peek = [0u8; 11];
        storage.read_exact(&mut peek)?;
        storage.seek(SeekFrom::Start(0))?;

        let inner = if crate::boot::has_exfat_magic(&peek) {
            Backend::Exfat(ExfatVfs::mount(storage, options)?)
        } else {
            Backend::Fat(FatVfs::new(storage, options)?)
        };
        Ok(Volume { inner })
    }

    /// The on-disk [`Format`] of the mounted volume.
    pub fn format(&self) -> Format {
        self.inner.format()
    }

    /// Iterate the entries of the directory at `path` (`/` or `\` as
    /// separators); `.`/`..` are not surfaced.
    pub fn read_dir<P: AsRef<Path>>(&self, path: P) -> FsResult<ReadDir<'_, S>, S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.read_dir(prepared.as_path())
    }

    /// Fetch [`Metadata`] for the file or directory at `path`.
    pub fn metadata<P: AsRef<Path>>(&self, path: P) -> FsResult<Metadata, S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.lookup(prepared.as_path())
    }

    /// Open the file at `path` for reading. Read-only handles may coexist;
    /// writing through one fails with [`FileError::ReadOnly`](crate::FileError::ReadOnly).
    pub fn open<P: AsRef<Path>>(&self, path: P) -> FsResult<File<'_, S>, S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.open(prepared.as_path(), OpenFlags::Read)
    }

    /// Open the existing file at `path` for reading and writing.
    /// Exclusive: fails with [`FsError::FileLocked`] if any other handle
    /// to the same path is already open (both FAT and ExFAT).
    pub fn open_rw<P: AsRef<Path>>(&self, path: P) -> FsResult<File<'_, S>, S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.open(prepared.as_path(), OpenFlags::ReadWrite)
    }

    /// Create the file at `path` (truncating if it exists) and return a
    /// writable handle. Intermediate directories must already exist.
    /// Fails with [`FsError::FileLocked`] while any handle to the path
    /// is open, and [`FsError::IsADirectory`] if `path` is a directory.
    pub fn create<P: AsRef<Path>>(&self, path: P) -> FsResult<File<'_, S>, S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.create_or_truncate(prepared.as_path())
    }

    /// Read the entire contents of the file at `path` into a `Vec`.
    pub fn read<P: AsRef<Path>>(&self, path: P) -> FsResult<Vec<u8>, S::Error> {
        let mut file = self.open(path)?;
        // Grow with the data actually read rather than trusting the entry's
        // declared size up front: a corrupt size field would otherwise drive
        // a multi-GiB allocation (and > usize::MAX panics on 32-bit).
        let hint = usize::try_from(file.len()).unwrap_or(usize::MAX);
        let mut out: Vec<u8> = Vec::new();
        let mut done = 0;
        loop {
            // Extend by up to 1 MiB at a time, never past the declared size.
            let step = hint.saturating_sub(done).min(1 << 20);
            if step == 0 {
                break;
            }
            out.resize(done + step, 0);
            let n = file.read(&mut out[done..])?;
            if n == 0 {
                break;
            }
            done += n;
        }
        out.truncate(done);
        Ok(out)
    }

    /// Write `contents` to `path`, creating/truncating the file and flushing before returning.
    pub fn write<P: AsRef<Path>>(&self, path: P, contents: &[u8]) -> FsResult<(), S::Error> {
        let mut file = self.create(path)?;
        file.write_all(contents).map_err(FsError::from)?;
        file.flush()?;
        Ok(())
    }

    /// Create an empty directory at `path`; parents must already exist.
    pub fn create_dir<P: AsRef<Path>>(&self, path: P) -> FsResult<(), S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.create_dir(prepared.as_path())
    }

    /// Recursively create `path` and any missing parents (existing directories are fine).
    pub fn create_dir_all<P: AsRef<Path>>(&self, path: P) -> FsResult<(), S::Error> {
        let prepared = prepare_path(path)?;
        if prepared.is_root() {
            return Ok(());
        }
        let mut cur = PathBuf::from("\\");
        let mut existed = false;
        for component in prepared.as_path().components() {
            if let WindowsComponent::Normal(segment) = component {
                cur.push(segment);
                match self.inner.create_dir(&cur) {
                    Ok(()) => existed = false,
                    Err(FsError::AlreadyExists) => existed = true,
                    Err(e) => return Err(e),
                }
            }
        }
        // Intermediate file components surface NotADirectory on the next
        // level, but a FINAL component that already exists as a regular
        // file would otherwise be silently accepted (std errors here).
        if existed && !self.inner.lookup(prepared.as_path())?.is_dir() {
            return Err(FsError::NotADirectory);
        }
        Ok(())
    }

    /// Remove the file at `path`. Fails with [`FsError::IsADirectory`] if
    /// `path` names a directory.
    pub fn remove_file<P: AsRef<Path>>(&self, path: P) -> FsResult<(), S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.remove_file(prepared.as_path())
    }

    /// Remove the empty directory at `path`. Fails with
    /// [`FsError::DirectoryNotEmpty`] if it still has children.
    pub fn remove_dir<P: AsRef<Path>>(&self, path: P) -> FsResult<(), S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.remove_dir(prepared.as_path())
    }

    /// Recursively remove the directory at `path` and all its contents.
    pub fn remove_dir_all<P: AsRef<Path>>(&self, path: P) -> FsResult<(), S::Error> {
        let prepared = prepare_path(path)?;
        self.inner.remove_dir_all(prepared.as_path())
    }

    /// Rename / move `from` to `to`.
    ///
    /// `rename(x, x)` succeeds as a no-op, and a case-only respelling
    /// (`foo.txt` → `FOO.TXT`) is permitted. Fails with
    /// [`FsError::AlreadyExists`] if a *different* entry exists at `to`
    /// (unlike POSIX/std, the target is never replaced),
    /// [`FsError::NotFound`] if `from` does not exist,
    /// [`FsError::FileLocked`] while a handle is open at or under either
    /// path, and [`FsError::InvalidInput`] when `to` lies inside the
    /// directory being moved.
    pub fn rename<P: AsRef<Path>, Q: AsRef<Path>>(&self, from: P, to: Q) -> FsResult<(), S::Error> {
        let from = prepare_path(from)?;
        let to = prepare_path(to)?;
        self.inner.rename(from.as_path(), to.as_path())
    }

    /// Set the creation timestamp of the entry at `path` (on-disk
    /// resolution: 10 ms on both formats). Fails with [`FsError::FileLocked`] while any handle
    /// to the file is open, and [`FsError::PermissionDenied`] on the
    /// root directory (it has no entry to stamp).
    pub fn set_created<P: AsRef<Path>>(
        &self,
        path: P,
        when: PrimitiveDateTime,
    ) -> FsResult<(), S::Error> {
        let path = prepare_path(path)?;
        self.inner.set_times(
            path.as_path(),
            EntryTimes {
                created: Some(when),
                ..EntryTimes::default()
            },
        )
    }

    /// Set the modification timestamp of the entry at `path` (on-disk
    /// resolution: 2 s on FAT, 10 ms on ExFAT). Same locking rules as
    /// [`Self::set_created`].
    pub fn set_modified<P: AsRef<Path>>(
        &self,
        path: P,
        when: PrimitiveDateTime,
    ) -> FsResult<(), S::Error> {
        let path = prepare_path(path)?;
        self.inner.set_times(
            path.as_path(),
            EntryTimes {
                modified: Some(when),
                ..EntryTimes::default()
            },
        )
    }

    /// Set the last-accessed date of the entry at `path` (FAT stores
    /// dates only). Same locking rules as [`Self::set_created`].
    pub fn set_accessed<P: AsRef<Path>>(&self, path: P, when: Date) -> FsResult<(), S::Error> {
        let path = prepare_path(path)?;
        self.inner.set_times(
            path.as_path(),
            EntryTimes {
                accessed: Some(when),
                ..EntryTimes::default()
            },
        )
    }

    /// Replace the attribute flags (read-only / hidden / system /
    /// archive) of the entry at `path`. Same locking rules as
    /// [`Self::set_created`]; the root has no entry to stamp.
    pub fn set_attributes<P: AsRef<Path>>(
        &self,
        path: P,
        attrs: crate::Attributes,
    ) -> FsResult<(), S::Error> {
        let path = prepare_path(path)?;
        self.inner.set_attributes(path.as_path(), attrs)
    }

    /// Flush all pending changes to storage. Call before dropping to
    /// observe write-back errors — they are swallowed on `Drop`.
    pub fn flush(&self) -> FsResult<(), S::Error> {
        self.inner.flush()
    }

    /// Unmount and return the underlying storage, flushing first.
    /// Flush errors are swallowed here — call [`Self::flush`] first to
    /// observe them.
    pub fn into_storage(self) -> S {
        self.inner.into_storage()
    }
}

/// Mounting a filesystem inside an MBR partition (e.g. a raw SD card):
/// slices `storage` with a [`Partition`] and mounts the filesystem
/// inside it. Use [`mount`](Volume::mount) for a bare filesystem image.
/// I/O errors surface as [`PartitionError`](crate::PartitionError)`<S::Error>`.
impl<S> Volume<Partition<S>>
where
    S: Read + Write + Seek,
{
    /// Mount the filesystem in MBR partition `index` (`0..4`). Fails with
    /// [`FsError::Unsupported`] if `storage` isn't an MBR, and
    /// [`FsError::NotFound`] if that partition slot is empty.
    pub fn mount_partition(
        storage: S,
        index: usize,
    ) -> FsResult<Self, crate::mbr::PartitionError<S::Error>> {
        Self::mount_partition_with(storage, index, FsOptions::default())
    }

    /// [`mount_partition`](Volume::mount_partition) with explicit options.
    pub fn mount_partition_with(
        mut storage: S,
        index: usize,
        options: FsOptions,
    ) -> FsResult<Self, crate::mbr::PartitionError<S::Error>> {
        let table = PartitionTable::read(&mut storage).map_err(crate::mbr::wrap_fs_err)?;
        let entry = table
            .partitions
            .get(index)
            .and_then(|slot| slot.as_ref())
            .ok_or(FsError::NotFound)?;
        Volume::mount_with(Partition::from_entry(storage, entry), options)
    }

    /// Mount the first FAT/ExFAT partition found in the MBR. Fails with
    /// [`FsError::NotFound`] if no filesystem partition exists.
    pub fn mount_first_partition(
        storage: S,
    ) -> FsResult<Self, crate::mbr::PartitionError<S::Error>> {
        Self::mount_first_partition_with(storage, FsOptions::default())
    }

    /// [`mount_first_partition`](Volume::mount_first_partition) with explicit options.
    pub fn mount_first_partition_with(
        mut storage: S,
        options: FsOptions,
    ) -> FsResult<Self, crate::mbr::PartitionError<S::Error>> {
        let table = PartitionTable::read(&mut storage).map_err(crate::mbr::wrap_fs_err)?;
        let index = table.first_filesystem().ok_or(FsError::NotFound)?;
        let entry = table.partitions[index].expect("first_filesystem returned a populated slot");
        Volume::mount_with(Partition::from_entry(storage, &entry), options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemBlockDevice;

    const FAT16: &[u8] = include_bytes!("../tests/fixtures/fat16-3m.img");

    #[test]
    fn path_api_accepts_str_and_pathbuf() {
        let vol = Volume::mount(MemBlockDevice::from_slice(FAT16)).expect("mount");
        assert!(vol.metadata("/HELLO.TXT").is_ok());
        let pb = PathBuf::from("\\SUBDIR");
        assert!(vol.metadata(&pb).is_ok());
        assert!(vol.metadata(pb.as_path()).is_ok());
        let out = PathBuf::from("\\PATHAPI.BIN");
        {
            use embedded_io::Write;
            let mut f = vol.create(&out).expect("create");
            f.write_all(b"ok").expect("write");
        }
        assert_eq!(vol.read(&out).expect("read"), b"ok");
    }
}
