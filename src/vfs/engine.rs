//! The generic path-level engine shared by both backends.
//!
//! Every path-level mutation — create, remove, rename, recursive delete,
//! metadata patches — is implemented **once** here, over the
//! [`Directory`] primitives a backend provides. Locking discipline,
//! validation order, crash ordering (link-before-unlink,
//! unlink-before-free), read-only enforcement, and error mapping
//! therefore cannot diverge between FAT and ExFAT: backends only supply
//! `open_dir`, the name policy, the handle table, a clock, and a commit
//! hook.
//!
//! Paths arriving here are already validated, normalized, and rooted by
//! [`prepare_path`](super::prepare_path) at the `Volume` boundary.

use alloc::string::String;

use crate::attrs::Attributes;
use crate::entry_times::EntryTimes;
use crate::error::{FsError, FsResult};
use crate::handles::HandleTable;
use crate::name::NamePolicy;
use crate::path::Path;
use crate::time::Clock;
use crate::vfs::{Directory, EntryPatch, NewEntry};

/// Split a non-root path into `(parent, file_name)`.
fn split_parent(path: &Path) -> Option<(&Path, &str)> {
    Some((path.parent()?, path.file_name()?))
}

/// Backend hooks for the generic path engine. All path-level operations
/// come as provided methods; implementors supply only the primitives.
pub(crate) trait PathEngine {
    type IoError: embedded_io::Error;
    /// The backend's [`Directory`] handle, borrowing the volume.
    type Dir<'a>: Directory<Error = FsError<Self::IoError>>
    where
        Self: 'a;

    /// Resolve the directory at `path` (root included). Fails with
    /// [`FsError::NotFound`] for missing components and
    /// [`FsError::NotADirectory`] when a component is a file.
    fn open_dir(&self, path: &Path) -> FsResult<Self::Dir<'_>, Self::IoError>;

    /// The volume's name-equality / case-folding policy.
    fn name_policy(&self) -> &NamePolicy;

    /// The volume's open-handle lock table.
    fn handles(&self) -> &HandleTable;

    /// The clock stamping newly created entries.
    fn clock(&self) -> &dyn Clock;

    /// Make this operation's mutations durable (ExFAT flushes eagerly;
    /// FAT defers to its sector cache and volume flush).
    fn commit(&self) -> FsResult<(), Self::IoError>;

    /// Device-order write barrier: everything written before this call is
    /// on the device before anything written after it. The write-back
    /// sector cache flushes dirty slots in arbitrary order, so the crash
    /// orderings below (unlink-before-free, link-before-unlink,
    /// allocate-before-link) are only real on disk when separated by a
    /// barrier.
    fn barrier(&self) -> FsResult<(), Self::IoError>;

    // ── Provided path-level operations ──────────────────────────────────

    /// Create an empty file at `path`; the parent must exist, the final
    /// component must not.
    fn create_empty_file(&self, path: &Path) -> FsResult<(), Self::IoError> {
        let (parent_path, name) = split_parent(path).ok_or(FsError::IsADirectory)?;
        let mut parent = self.open_dir(parent_path)?;
        parent.insert(NewEntry {
            name: String::from(name),
            attrs: Attributes {
                archive: true,
                ..Attributes::default()
            },
            is_dir: false,
            first_cluster: 0,
            size: 0,
            times: EntryTimes::now(self.clock()),
            // Spec (ExFAT): NoFatChain must be 0 with no allocation.
            no_fat_chain: Some(false),
            allocated_size: Some(0),
        })?;
        self.commit()
    }

    /// Create an empty directory at `path`; the parent must exist.
    fn create_dir(&self, path: &Path) -> FsResult<(), Self::IoError> {
        // The root always exists.
        let (parent_path, name) = split_parent(path).ok_or(FsError::AlreadyExists)?;
        let mut parent = self.open_dir(parent_path)?;
        parent.insert(NewEntry {
            name: String::from(name),
            attrs: Attributes::default(),
            is_dir: true,
            // 0 = the backend allocates and seeds the directory content
            // itself (after its duplicate check, so failures can't leak).
            first_cluster: 0,
            size: 0,
            times: EntryTimes::now(self.clock()),
            no_fat_chain: Some(true),
            allocated_size: None,
        })?;
        self.commit()
    }

    /// Remove the file at `path`.
    fn remove_file(&self, path: &Path) -> FsResult<(), Self::IoError> {
        let (parent_path, name) = split_parent(path).ok_or(FsError::IsADirectory)?;
        let key = self.name_policy().fold_path(path);
        self.handles().lock_rw(&key)?;
        let result = (|| {
            let mut parent = self.open_dir(parent_path)?;
            let (entry, meta) =
                Directory::lookup(&parent, name, self.name_policy())?.ok_or(FsError::NotFound)?;
            if meta.is_dir() {
                return Err(FsError::IsADirectory);
            }
            if meta.attributes().read_only {
                return Err(FsError::ReadOnlyFile);
            }
            // Unlink first, free after: a crash in between leaks clusters
            // (fsck-benign) instead of leaving a live entry pointing at
            // free clusters (a future cross-link). The barrier makes the
            // ordering real on the device, not just in the cache.
            parent.remove(&entry)?;
            self.barrier()?;
            parent.free_data(&entry)?;
            Ok(())
        })();
        self.handles().release_rw(&key);
        result.and_then(|()| self.commit())
    }

    /// Remove the empty directory at `path`.
    fn remove_dir(&self, path: &Path) -> FsResult<(), Self::IoError> {
        let (parent_path, name) = split_parent(path).ok_or(FsError::InvalidInput)?;
        let key = self.name_policy().fold_path(path);
        self.handles().lock_rw(&key)?;
        let result = (|| {
            // Resolving `path` as a directory also yields NotADirectory
            // for files and NotFound for missing entries.
            let child = self.open_dir(path)?;
            if !child.list_entries()?.is_empty() {
                return Err(FsError::DirectoryNotEmpty);
            }
            drop(child);
            let mut parent = self.open_dir(parent_path)?;
            let (entry, meta) =
                Directory::lookup(&parent, name, self.name_policy())?.ok_or(FsError::NotFound)?;
            if !meta.is_dir() {
                return Err(FsError::NotADirectory);
            }
            // Unlink-before-free, made durable in that order.
            parent.remove(&entry)?;
            self.barrier()?;
            parent.free_data(&entry)?;
            Ok(())
        })();
        self.handles().release_rw(&key);
        result.and_then(|()| self.commit())
    }

    /// Whether any regular file in the tree at `path` carries the
    /// read-only attribute (directories themselves don't block).
    fn subtree_has_readonly_file(&self, path: &Path) -> FsResult<bool, Self::IoError> {
        let entries = self.open_dir(path)?.list_entries()?;
        for (_, name, meta) in entries {
            let found = if meta.is_dir() {
                self.subtree_has_readonly_file(&path.join(&name))?
            } else {
                meta.attributes().read_only
            };
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Recursively remove the directory at `path` and all its contents.
    /// Fails atomically up front when any handle is open under the tree
    /// or any contained file is read-only.
    fn remove_dir_all(&self, path: &Path) -> FsResult<(), Self::IoError> {
        // Reject the root BEFORE any deletion: remove_tree would otherwise
        // empty the volume and only then fail to unlink the root itself,
        // reporting an error for an operation that already destroyed data.
        if split_parent(path).is_none() {
            return Err(FsError::InvalidInput);
        }
        let key = self.name_policy().fold_path(path);
        if self.handles().any_open_under(&key) {
            return Err(FsError::FileLocked);
        }
        // Resolves the target too: files fail with NotADirectory here.
        if self.subtree_has_readonly_file(path)? {
            return Err(FsError::ReadOnlyFile);
        }
        self.handles().lock_rw(&key)?;
        let result = self.remove_tree(path);
        self.handles().release_rw(&key);
        result.and_then(|()| self.commit())
    }

    /// Depth-first delete of `path` and everything below it (no checks —
    /// [`Self::remove_dir_all`] performs them up front).
    fn remove_tree(&self, path: &Path) -> FsResult<(), Self::IoError> {
        // Snapshot before mutating the directory being iterated.
        let entries = self.open_dir(path)?.list_entries()?;
        let mut dir = self.open_dir(path)?;
        for (entry, name, meta) in entries {
            if meta.is_dir() {
                self.remove_tree(&path.join(&name))?;
            } else {
                // Unlink-before-free, made durable in that order.
                dir.remove(&entry)?;
                self.barrier()?;
                dir.free_data(&entry)?;
            }
        }
        drop(dir);
        // Remove the (now empty) directory itself from its parent.
        let (parent_path, name) = split_parent(path).ok_or(FsError::InvalidInput)?;
        let mut parent = self.open_dir(parent_path)?;
        let (entry, _) =
            Directory::lookup(&parent, name, self.name_policy())?.ok_or(FsError::NotFound)?;
        parent.remove(&entry)?;
        self.barrier()?;
        parent.free_data(&entry)?;
        Ok(())
    }

    /// Rename / move `from` to `to`. Fails when `to` exists, when the
    /// move would place a directory inside its own subtree, or when any
    /// handle is open at or under `from`.
    fn rename(&self, from: &Path, to: &Path) -> FsResult<(), Self::IoError> {
        let (from_parent, from_name) = split_parent(from).ok_or(FsError::PermissionDenied)?;
        let (to_parent, to_name) = split_parent(to).ok_or(FsError::PermissionDenied)?;

        // Moving a directory into its own subtree would detach it into
        // an unreachable cycle and leak every cluster below it.
        if self.name_policy().is_strict_ancestor(from, to) {
            return Err(FsError::InvalidInput);
        }
        // Entry locations and lock keys are path-based, so any open
        // handle at or under `from` makes the move unsafe.
        let from_key = self.name_policy().fold_path(from);
        if self.handles().any_open_under(&from_key) {
            return Err(FsError::FileLocked);
        }
        // `from` and `to` naming the same entry (case-fold equal) is a
        // case-only respelling — or, with identical strings, a no-op
        // (std::fs::rename(x, x) succeeds).
        let to_key = self.name_policy().fold_path(to);
        let same_entry = from_key == to_key;
        if same_entry && from.as_str() == to.as_str() {
            // Still requires the entry to exist.
            let from_dir = self.open_dir(from_parent)?;
            Directory::lookup(&from_dir, from_name, self.name_policy())?
                .ok_or(FsError::NotFound)?;
            return Ok(());
        }

        self.handles().lock_rw(&to_key)?;
        let result = (|| {
            let from_dir = self.open_dir(from_parent)?;
            let (entry, _) = Directory::lookup(&from_dir, from_name, self.name_policy())?
                .ok_or(FsError::NotFound)?;
            drop(from_dir);

            if same_entry {
                // Case-only respelling: the fold-equal name means the
                // link-first ordering is impossible (the insert would see
                // itself as a duplicate). Unlink, then reinsert under the
                // new spelling; restore the old name if that fails.
                let mut dir = self.open_dir(from_parent)?;
                dir.remove(&entry)?;
                self.barrier()?;
                if let Err(e) = dir.link(&entry, to_name) {
                    let _ = dir.link(&entry, from_name);
                    return Err(e);
                }
                return Ok(());
            }

            let mut to_dir = self.open_dir(to_parent)?;
            if Directory::lookup(&to_dir, to_name, self.name_policy())?.is_some() {
                return Err(FsError::AlreadyExists);
            }
            // Link-before-unlink: a crash mid-rename leaves two links to
            // the same data (fsck-recoverable), never zero. The barrier
            // stops the cache flushing the unlink before the link — that
            // inversion would lose the file entirely.
            to_dir.link(&entry, to_name)?;
            drop(to_dir);
            self.barrier()?;

            let mut from_dir = self.open_dir(from_parent)?;
            from_dir.remove(&entry)?;
            Ok(())
        })();
        self.handles().release_rw(&to_key);
        result.and_then(|()| self.commit())
    }

    /// Patch entry metadata (timestamps / attributes) at `path` without
    /// an open handle. Takes the exclusive lock for the duration.
    fn patch_entry(&self, path: &Path, patch: EntryPatch) -> FsResult<(), Self::IoError> {
        // The root has no directory entry to patch.
        let (parent_path, name) = split_parent(path).ok_or(FsError::PermissionDenied)?;
        let key = self.name_policy().fold_path(path);
        self.handles().lock_rw(&key)?;
        let result = (|| {
            let mut parent = self.open_dir(parent_path)?;
            let (entry, _) =
                Directory::lookup(&parent, name, self.name_policy())?.ok_or(FsError::NotFound)?;
            parent.update(entry, patch)
        })();
        self.handles().release_rw(&key);
        result.and_then(|()| self.commit())
    }
}
