//! Open, lookup, and read_dir for [`FatVfs`].

use super::*;
use crate::error::*;
use crate::path::*;
use embedded_io::*;

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Read the entries of the directory at `path` into [`ReadDir`];
    /// fails if it isn't an existing directory. Path-pure (no ambient cwd).
    pub(crate) fn read_dir<P: AsRef<Path>>(&self, path: P) -> FsResult<ReadDir<'_, S>, S::Error> {
        use crate::fat::FatDirectory;

        let dir = FatDirectory::open_path(self, path.as_ref())?;
        Ok(ReadDir::new(self, dir.anchor(), dir.path()))
    }

    /// Build a [`FatStreamFile`] from resolved directory properties.
    pub(crate) fn stream_from_props(
        &self,
        props: Properties,
        writable: bool,
    ) -> crate::fat::FatStreamFile<'_, S> {
        use crate::entry_times::EntryTimes;
        use crate::fat::{FatClusterMap, FatDataStore, FatDirSlotWriter};
        use crate::vfs::{StreamFile, StreamInit};

        let first = props.data_cluster;
        let len = u64::from(props.file_size);
        let map = FatClusterMap::new(self, props.file_size, first);
        let writer = FatDirSlotWriter::from_props(self, &props);
        let store = FatDataStore::new(self);
        let times = EntryTimes {
            created: props.created,
            modified: Some(props.modified),
            accessed: props.accessed,
        };
        StreamFile::new(
            store,
            map,
            writer,
            StreamInit {
                first_cluster: first,
                len,
                // FAT has no ValidDataLength: everything up to `len` is data.
                valid_len: len,
                lock_key: self.lock_key(&props.path),
                times,
                attributes: props.attributes,
                handles: &self.handles,
                clock: &*self.options.clock,
                writable,
                owns_lock: false,
                update_times: self.options.auto_timestamps,
                fat_size_cap: true,
            },
        )
    }

    fn chain_is_healthy(&self, first: u32, file_size: u32) -> FsResult<bool, S::Error> {
        if first < 2 {
            return Ok(true);
        }
        let mut current = first;
        let mut count = 0u32;
        let cs = self.cluster_size();
        loop {
            count = count.saturating_add(1);
            if count.saturating_mul(cs) >= file_size {
                return Ok(true);
            }
            // A walk longer than the volume's cluster count is a cycle.
            if count > self.props.max_cluster {
                return Ok(false);
            }
            match self.read_fat(current)? {
                FatEntry::Next(n) => current = n,
                _ => return Ok(false),
            }
        }
    }

    /// Open the file at `path` for reading; fails if it isn't an existing file
    pub(crate) fn get_ro_file<P: AsRef<Path>>(
        &self,
        path: P,
    ) -> FsResult<crate::fat::FatStreamFile<'_, S>, S::Error> {
        let path = path.as_ref();
        let mut file = self.open_stream(path, false)?;
        self.lock_ro(path)?;
        file.set_owns_lock(true);
        Ok(file)
    }

    /// Stat a path without building a public directory iterator (path-pure).
    pub(crate) fn lookup<P: AsRef<Path>>(
        &self,
        path: P,
    ) -> FsResult<crate::dir::Metadata, S::Error> {
        use crate::dir::Metadata;
        use crate::fat::FatDirectory;

        let path = path.as_ref();
        if !path.is_valid() {
            return Err(FsError::InvalidInput);
        }
        let normalized = path.normalize();
        let Some(file_name) = normalized.file_name() else {
            return Ok(Metadata::root());
        };
        let parent = normalized
            .parent()
            .expect("file_name is Some so parent exists for non-root");

        let dir = FatDirectory::open_path(self, parent)?;
        let entry = dir.lookup(file_name)?.ok_or(FsError::NotFound)?;
        let dir_entry = entry.into_dir_entry(parent);
        Ok(Metadata::from_fat(&dir_entry))
    }

    /// Resolve `path` to a stream without locking; the caller registers the RO/RW lock.
    pub(crate) fn open_stream(
        &self,
        path: &Path,
        writable: bool,
    ) -> FsResult<crate::fat::FatStreamFile<'_, S>, S::Error> {
        use crate::fat::FatDirectory;

        if !path.is_valid() {
            return Err(FsError::InvalidInput);
        }

        let Some(file_name) = path.file_name() else {
            log_error!("Is a directory (not a file)");
            return Err(FsError::IsADirectory);
        };

        let parent = path
            .parent()
            .expect("file_name is Some, so the path is not the root");

        let dir = FatDirectory::open_path(self, parent)?;
        let dir_entry = dir.lookup(file_name)?.ok_or_else(|| {
            log_error!("File {path} not found");
            FsError::NotFound
        })?;
        if dir_entry.is_dir {
            return Err(FsError::IsADirectory);
        }
        if !self.chain_is_healthy(dir_entry.data_cluster, dir_entry.file_size)? {
            log_error!("The cluster chain of a file is malformed");
            return Err(FsError::Corrupt(CorruptKind::ClusterChain));
        }
        let props = Properties::from_raw(dir_entry, path.normalize().into());
        Ok(self.stream_from_props(props, writable))
    }

    /// Open the file at `path` for reading and writing; fails if it isn't an existing file
    pub(crate) fn get_rw_file<P: AsRef<Path>>(
        &self,
        path: P,
    ) -> FsResult<crate::fat::FatStreamFile<'_, S>, S::Error> {
        let path = path.as_ref();
        // Reject RO attribute via a lightweight lookup first.
        {
            use crate::fat::FatDirectory;
            let normalized = path.normalize();
            let file_name = normalized.file_name().ok_or(FsError::IsADirectory)?;
            let parent = normalized.parent().ok_or(FsError::IsADirectory)?;
            let dir = FatDirectory::open_path(self, parent)?;
            let entry = dir.lookup(file_name)?.ok_or(FsError::NotFound)?;
            if entry.is_dir {
                return Err(FsError::IsADirectory);
            }
            if crate::attrs::Attributes::from(entry.attributes).read_only {
                return Err(FsError::ReadOnlyFile);
            }
        }
        let mut file = self.open_stream(path, true)?;
        self.lock_rw(path)?;
        file.set_owns_lock(true);
        Ok(file)
    }
}
