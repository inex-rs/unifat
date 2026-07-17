//! Unified directory trait over the path-pure FAT / ExFAT ops.
//!
//! [`Directory`] is the internal mutation/lookup surface used by VFS ops.
//! Public [`crate::dir::ReadDir`] remains a separate streaming wrapper.

use alloc::string::String;
use alloc::vec::Vec;

use crate::attrs::Attributes;
use crate::dir::Metadata;
use crate::entry_times::EntryTimes;
use crate::name::NameEq;

/// Patch applied on file flush / directory metadata update.
/// Format-irrelevant fields are ignored by the backend writer.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EntryPatch {
    /// Logical file size in bytes (ExFAT `DataLength` / FAT `DIR_FileSize`).
    pub size: Option<u64>,
    /// ExFAT `ValidDataLength` (≤ `size`); ignored on FAT.
    pub valid_size: Option<u64>,
    pub first_cluster: Option<u32>,
    /// ExFAT only.
    pub no_fat_chain: Option<bool>,
    pub times: Option<EntryTimes>,
    pub attrs: Option<Attributes>,
}

/// Builder for directory insert — ExFAT-only fields are `Option`.
#[derive(Debug, Clone)]
pub(crate) struct NewEntry {
    pub name: String,
    pub attrs: Attributes,
    pub is_dir: bool,
    pub first_cluster: u32,
    pub size: u64,
    pub times: EntryTimes,
    /// ExFAT only; `None` on FAT.
    pub no_fat_chain: Option<bool>,
    /// ExFAT allocated size; `None` → backend default.
    pub allocated_size: Option<u64>,
}

/// One row of a directory listing: entry ref + name + metadata.
pub(crate) type ListingRow<R> = (R, String, Metadata);

/// Format-agnostic directory operations on an ephemeral parent handle.
///
/// `EntryRef` is backend-associated so `vfs` does not depend on concrete
/// FAT/ExFAT location types (avoids a fat↔vfs cycle). `DirSlotWriter` /
/// StreamFile consume entry metadata for flush.
pub(crate) trait Directory {
    type Error;
    type EntryRef;

    /// Look up `name` using the mount's name-equality policy.
    fn lookup(
        &self,
        name: &str,
        eq: &dyn NameEq,
    ) -> Result<Option<(Self::EntryRef, Metadata)>, Self::Error>;

    /// Owned listing for mutation planning (skips `.` / `..` on FAT).
    fn list_entries(&self) -> Result<Vec<ListingRow<Self::EntryRef>>, Self::Error>;

    /// Insert a new named entry; returns a ref to the on-disk set. A
    /// directory entry with `first_cluster == 0` allocates and seeds its
    /// content (FAT `.`/`..` cluster, ExFAT zeroed cluster) itself.
    fn insert(&mut self, entry: NewEntry) -> Result<Self::EntryRef, Self::Error>;

    /// Mark the entry set unused (does not free data clusters).
    fn remove(&mut self, entry: &Self::EntryRef) -> Result<(), Self::Error>;

    /// Free the data clusters referenced by `entry` (call after
    /// [`Self::remove`] — unlink-first ordering means a crash leaks
    /// clusters instead of leaving a live entry over free space).
    fn free_data(&mut self, entry: &Self::EntryRef) -> Result<(), Self::Error>;

    /// Create an entry named `new_name` in **this** directory referencing
    /// the same data as `entry`, preserving its metadata (sizes, times,
    /// attributes). Backends also fix relocation bookkeeping here (FAT
    /// rewrites the moved directory's `..`). The source entry is left
    /// untouched — the caller removes it afterwards, so a crash in
    /// between leaves two links, never zero.
    fn link(&mut self, entry: &Self::EntryRef, new_name: &str) -> Result<(), Self::Error>;

    /// Patch size / times / cluster fields on an existing entry.
    ///
    /// File streams also flush via [`crate::vfs::DirSlotWriter`]; this is the
    /// Directory-shaped path for metadata updates without an open handle
    /// (backing [`crate::vfs::VfsBackend::set_times`]).
    fn update(&mut self, entry: Self::EntryRef, patch: EntryPatch) -> Result<(), Self::Error>;
}
