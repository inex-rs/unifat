//! Path resolution and directory listing over [`DirExtent`].

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_io::{Read, Seek, Write};

use super::direntry::{DirExtent, ExfatDirEntry};
use super::{EXFAT_ENTRY_SIZE, ExfatReadDir, ExfatVfs};
use crate::codec::exfat::raw_entry::ExfatStreamExtension;
use crate::error::{FsError, FsResult};
use crate::name::split_components;

impl<S> ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Find the child named `name` in directory `ext`, or [`FsError::NotFound`].
    fn child_entry(&self, ext: &DirExtent, name: &str) -> FsResult<ExfatDirEntry, S::Error> {
        self.list_entries_in(ext)?
            .into_iter()
            .find(|e| self.names_equal(&e.name, name))
            .ok_or(FsError::NotFound)
    }

    /// Resolve a path to the directory it names ("" / `/` / `\` = root).
    /// Missing components yield [`FsError::NotFound`]; non-directory
    /// components yield [`FsError::NotADirectory`].
    pub(crate) fn resolve_dir(&self, path: &crate::path::Path) -> FsResult<DirExtent, S::Error> {
        let mut extent = super::ExfatDirectory::root(self).extent();
        for component in split_components(path.as_str()) {
            let entry = self.child_entry(&extent, component)?;
            if !entry.is_dir() {
                return Err(FsError::NotADirectory);
            }
            extent = self.dir_extent(&entry);
        }
        Ok(extent)
    }

    /// Resolve a path into its parent directory and final component.
    /// An empty or root-only path yields [`FsError::InvalidInput`].
    pub(super) fn resolve_parent(
        &self,
        path: &crate::path::Path,
    ) -> FsResult<(DirExtent, String), S::Error> {
        let components: Vec<&str> = split_components(path.as_str()).collect();
        let (name, parent_components) = components.split_last().ok_or(FsError::InvalidInput)?;
        let mut extent = super::ExfatDirectory::root(self).extent();
        for component in parent_components {
            let entry = self.child_entry(&extent, component)?;
            if !entry.is_dir() {
                return Err(FsError::NotADirectory);
            }
            extent = self.dir_extent(&entry);
        }
        Ok((extent, (*name).to_string()))
    }

    /// Resolve a path to the entry it names, or [`FsError::NotFound`].
    pub(super) fn resolve_entry(
        &self,
        path: &crate::path::Path,
    ) -> FsResult<(DirExtent, ExfatDirEntry), S::Error> {
        let (parent, name) = self.resolve_parent(path)?;
        let entry = self.child_entry(&parent, &name)?;
        Ok((parent, entry))
    }

    /// Root-directory layout: FAT-chained, unbounded, no parent entry.
    pub(crate) fn root_extent(&self) -> DirExtent {
        DirExtent {
            first_cluster: self.boot.root_dir_cluster,
            no_fat_chain: false,
            max_clusters: u32::MAX,
            parent_entry: None,
        }
    }

    /// Drain a directory's entries into an owned `Vec` so callers can
    /// iterate without holding a borrow on `self`.
    pub(super) fn list_entries_in(
        &self,
        ext: &DirExtent,
    ) -> FsResult<Vec<ExfatDirEntry>, S::Error> {
        let ext = self.refreshed_extent(ext);
        let iter = ExfatReadDir::new(self, ext.first_cluster, ext.no_fat_chain, ext.max_clusters)?;
        Ok(iter.collect())
    }

    /// Re-derive a directory's walk parameters from its on-disk entry in
    /// the parent. A held `DirExtent` goes stale when the directory grows
    /// (growth bumps `DataLength` and clears `NoFatChain` on disk); the
    /// root has no parent entry and is FAT-chained/unbounded, so growth
    /// is followed naturally.
    pub(super) fn refreshed_extent(&self, ext: &DirExtent) -> DirExtent {
        let Some(loc) = ext.parent_entry else {
            return *ext;
        };
        let Ok(set) = self.read_set(&loc) else {
            return *ext;
        };
        let Some(stream) =
            ExfatStreamExtension::parse(&set[EXFAT_ENTRY_SIZE..EXFAT_ENTRY_SIZE * 2])
        else {
            return *ext;
        };
        let cluster_bytes = u64::from(self.boot.bytes_per_cluster());
        let dl = stream.data_length;
        let max_clusters = if dl == 0 {
            1
        } else {
            u32::try_from(dl.div_ceil(cluster_bytes)).unwrap_or(u32::MAX)
        };
        DirExtent {
            first_cluster: ext.first_cluster,
            no_fat_chain: stream.no_fat_chain(),
            max_clusters,
            parent_entry: ext.parent_entry,
        }
    }

    /// Cluster-layout descriptor for a sub-directory. `data_length` and
    /// `no_fat_chain` are re-read from disk when possible so long-lived
    /// `ExfatDirEntry` handles stay correct after directory growth.
    pub(super) fn dir_extent(&self, entry: &ExfatDirEntry) -> DirExtent {
        // A refreshable set holds at least the File + Stream Extension pair.
        const MIN_FILE_SET_BYTES: u64 = EXFAT_ENTRY_SIZE as u64 * 2;
        let (data_length, no_fat_chain) = match entry.loc {
            Some(loc) if u64::from(loc.len) >= MIN_FILE_SET_BYTES => self
                .refresh_stream_extension(&loc)
                .unwrap_or((entry.data_length, entry.no_fat_chain)),
            _ => (entry.data_length, entry.no_fat_chain),
        };
        let cluster_bytes = u64::from(self.boot.bytes_per_cluster());
        // Saturating: an out-of-range on-disk `data_length` must not
        // truncate the traversal cap to a bogus (possibly zero) value.
        let max_clusters = if data_length == 0 {
            1
        } else {
            u32::try_from(data_length.div_ceil(cluster_bytes)).unwrap_or(u32::MAX)
        };
        DirExtent {
            first_cluster: entry.first_cluster,
            no_fat_chain,
            max_clusters,
            parent_entry: entry.loc,
        }
    }

    /// Re-read `entry`'s Stream Extension fields from disk; in-memory
    /// copies go stale when directory growth updates the entry.
    fn refresh_stream_extension(&self, loc: &super::direntry::SetLoc) -> Option<(u64, bool)> {
        let set = self.read_set(loc).ok()?;
        let stream = ExfatStreamExtension::parse(&set[EXFAT_ENTRY_SIZE..EXFAT_ENTRY_SIZE * 2])?;
        Some((stream.data_length, stream.no_fat_chain()))
    }
}
