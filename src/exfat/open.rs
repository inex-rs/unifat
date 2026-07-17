//! Open and read_dir helpers for [`ExfatVfs`].

use embedded_io::{Read, Seek, Write};

use super::direntry::ExfatDirEntry;
use super::{
    ExfatClusterMap, ExfatDataStore, ExfatDirSlotWriter, ExfatReadDir, ExfatStreamFile, ExfatVfs,
};
use crate::error::{FsError, FsResult};

impl<S> ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Iterate the directory named by `path` (`/` or `\` separated;
    /// "" / `/` / `\` name the root).
    pub(crate) fn read_dir(
        &self,
        path: &crate::path::Path,
    ) -> FsResult<ExfatReadDir<'_, S>, S::Error> {
        use super::ExfatDirectory;

        let dir = ExfatDirectory::open_path(self, path)?;
        let extent = dir.extent();
        ExfatReadDir::new(
            self,
            extent.first_cluster,
            extent.no_fat_chain,
            extent.max_clusters,
        )
    }

    /// Build a [`ExfatStreamFile`] from a resolved directory entry.
    /// `loc` is `None` for read-only handles (no write-back target).
    pub(super) fn stream_from_entry(
        &self,
        path_key: crate::path::PathBuf,
        entry: &ExfatDirEntry,
        loc: Option<super::direntry::SetLoc>,
        writable: bool,
    ) -> ExfatStreamFile<'_, S> {
        use crate::entry_times::EntryTimes;
        use crate::vfs::{StreamFile, StreamInit};

        // DataLength IS the byte size; allocation is its cluster-rounding.
        let cluster_bytes = u64::from(self.boot.bytes_per_cluster());
        let allocated = if entry.first_cluster >= 2 {
            entry
                .data_length
                .div_ceil(cluster_bytes)
                .saturating_mul(cluster_bytes)
        } else {
            0
        };
        let map = ExfatClusterMap::new(self, entry.first_cluster, allocated, entry.no_fat_chain);
        let writer =
            ExfatDirSlotWriter::new(self, loc, entry.created, entry.modified, entry.accessed);
        let store = ExfatDataStore::new(self);
        let times = EntryTimes {
            created: entry.created,
            modified: entry.modified,
            accessed: entry.accessed,
        };
        StreamFile::new(
            store,
            map,
            writer,
            StreamInit {
                first_cluster: entry.first_cluster,
                len: entry.data_length,
                valid_len: entry.valid_data_length,
                lock_key: path_key,
                times,
                attributes: entry.attributes.to_attributes(),
                handles: &self.handles,
                clock: &*self.options.clock,
                writable,
                owns_lock: false,
                update_times: self.options.auto_timestamps,
                fat_size_cap: false,
            },
        )
    }

    /// Open the file at `path` for reading.
    pub(crate) fn open_file(
        &self,
        path: &crate::path::Path,
    ) -> FsResult<ExfatStreamFile<'_, S>, S::Error> {
        // Case-folded: lock keys must collide across case variants.
        let path_key = self.name_policy.fold_path(path);
        let (_, entry) = self.resolve_entry(path)?;
        if entry.is_dir() {
            return Err(FsError::IsADirectory);
        }
        self.lock_ro(&path_key)?;
        let mut file = self.stream_from_entry(path_key, &entry, None, false);
        file.set_owns_lock(true);
        Ok(file)
    }

    /// Open an existing file for read/write. Growth past the current
    /// allocation transparently extends the FAT chain; `flush()` / drop
    /// writes the entry's lengths, timestamps, and checksum back.
    ///
    /// A contiguous (`NoFatChain=1`) file keeps its fast-path layout
    /// until it actually grows — `ExfatClusterMap::extend` materializes
    /// the FAT chain lazily at that point. Exclusive: fails with
    /// [`FsError::FileLocked`] if any other handle to the same path is
    /// open.
    pub(crate) fn open_file_rw(
        &self,
        path: &crate::path::Path,
    ) -> FsResult<ExfatStreamFile<'_, S>, S::Error> {
        // Case-folded: lock keys must collide across case variants.
        let path_key = self.name_policy.fold_path(path);
        let (parent, name) = self.resolve_parent(path)?;
        let entries = self.list_entries_in(&parent)?;
        let entry = entries
            .iter()
            .find(|e| self.names_equal(&e.name, &name))
            .cloned()
            .ok_or(FsError::NotFound)?;
        if entry.is_dir() {
            return Err(FsError::IsADirectory);
        }
        if entry.attributes.contains(crate::attrs::AttrBits::READ_ONLY) {
            return Err(FsError::ReadOnlyFile);
        }
        let loc = self
            .locate_entry_set_in(&parent, &entry.name)?
            .ok_or(FsError::NotFound)?;

        self.lock_rw(&path_key)?;
        let mut file = self.stream_from_entry(path_key, &entry, Some(loc), true);
        file.set_owns_lock(true);
        Ok(file)
    }
}
