//! Path-pure ExFAT directory handle ([`ExfatDirectory`]) and [`Directory`] impl.

use alloc::string::String;
use alloc::vec::Vec;

use embedded_io::{Read, Seek, Write};

use crate::dir::Metadata;
use crate::error::{FsError, FsResult};
use crate::name::NameEq;
use crate::vfs::{Directory, EntryPatch, NewEntry};

use super::ExfatVfs;
use super::direntry::{DirExtent, ExfatDirEntry};

/// Ephemeral view of one ExFAT directory ([`DirExtent`]).
pub(crate) struct ExfatDirectory<'a, S>
where
    S: Read + Write + Seek,
{
    vol: &'a ExfatVfs<S>,
    extent: DirExtent,
}

impl<S> core::fmt::Debug for ExfatDirectory<'_, S>
where
    S: Read + Write + Seek,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExfatDirectory")
            .field("extent", &self.extent)
            .finish_non_exhaustive()
    }
}

impl<'a, S> ExfatDirectory<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn new(vol: &'a ExfatVfs<S>, extent: DirExtent) -> Self {
        Self { vol, extent }
    }

    /// Root directory of the volume.
    #[inline]
    pub(crate) fn root(vol: &'a ExfatVfs<S>) -> Self {
        Self::new(vol, vol.root_extent())
    }

    /// Open the directory at `path` (path-pure; no ambient cwd).
    pub(crate) fn open_path(
        vol: &'a ExfatVfs<S>,
        path: &crate::path::Path,
    ) -> FsResult<Self, S::Error> {
        let extent = vol.resolve_dir(path)?;
        Ok(Self::new(vol, extent))
    }

    #[inline]
    pub(crate) fn extent(&self) -> DirExtent {
        self.extent
    }
}

impl<S> Directory for ExfatDirectory<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FsError<S::Error>;
    type EntryRef = ExfatDirEntry;

    fn lookup(
        &self,
        name: &str,
        eq: &dyn NameEq,
    ) -> Result<Option<(Self::EntryRef, Metadata)>, Self::Error> {
        for entry in self.vol.list_entries_in(&self.extent)? {
            if eq.names_equal(&entry.name, name) {
                let meta = Metadata::from_exfat(&entry);
                return Ok(Some((entry, meta)));
            }
        }
        Ok(None)
    }

    fn list_entries(&self) -> Result<Vec<(Self::EntryRef, String, Metadata)>, Self::Error> {
        let mut out = Vec::new();
        for entry in self.vol.list_entries_in(&self.extent)? {
            let name = entry.name.clone();
            let meta = Metadata::from_exfat(&entry);
            out.push((entry, name, meta));
        }
        Ok(out)
    }

    fn insert(&mut self, entry: NewEntry) -> Result<Self::EntryRef, Self::Error> {
        let name = entry.name.clone();
        self.vol.insert_new_entry_in_extent(self.extent, entry)?;
        self.lookup(&name, &self.vol.name_policy)?
            .map(|(e, _)| e)
            .ok_or(FsError::NotFound)
    }

    fn remove(&mut self, entry: &Self::EntryRef) -> Result<(), Self::Error> {
        self.vol.unlink_entry_set_in(&self.extent, &entry.name)
    }

    fn free_data(&mut self, entry: &Self::EntryRef) -> Result<(), Self::Error> {
        self.vol
            .free_entry_clusters(entry.first_cluster, entry.data_length, entry.no_fat_chain)
    }

    fn link(&mut self, entry: &Self::EntryRef, new_name: &str) -> Result<(), Self::Error> {
        // Preserve every field, including the original timestamps.
        // Cannot use `insert` — that would allocate fresh clusters.
        let entry_set = super::compose::compose_file_entry_set_full(
            new_name,
            self.vol.name_hash(new_name),
            entry.attributes,
            entry.first_cluster,
            entry.valid_data_length,
            entry.data_length,
            entry.no_fat_chain,
            crate::entry_times::EntryTimes {
                created: entry.created,
                modified: entry.modified,
                accessed: entry.accessed,
            },
        );
        self.vol.append_entry_set_in(&self.extent, &entry_set)
    }

    fn update(&mut self, entry: Self::EntryRef, patch: EntryPatch) -> Result<(), Self::Error> {
        if patch.size.is_none()
            && patch.valid_size.is_none()
            && patch.first_cluster.is_none()
            && patch.no_fat_chain.is_none()
            && patch.times.is_none()
            && patch.attrs.is_none()
        {
            return Ok(());
        }
        // Same on-disk rewrite as StreamFile flush (`DirSlotWriter`).
        let mut writer = super::ExfatDirSlotWriter::new(
            self.vol,
            entry.loc,
            entry.created,
            entry.modified,
            entry.accessed,
        );
        use crate::vfs::DirSlotWriter;
        writer.write_patch(patch).map_err(FsError::from)
    }
}
