//! Concurrent open-handle tracking shared by FAT and ExFAT.
//!
//! Writable opens are exclusive; read-only opens may be shared. Backends
//! pass keys already case-folded via [`crate::name::NamePolicy::fold_path`]
//! (names resolve case-insensitively, so `foo.TXT` and `FOO.txt` must
//! collide); this table only re-normalizes, it never folds.

use alloc::collections::BTreeMap;
use core::cell::RefCell;

use embedded_io::Error;

use crate::error::{FsError, FsResult};
use crate::path::{Path, PathBuf};

/// Book-keeping for a tracked open handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenMode {
    /// One or more read-only handles.
    ReadOnly(u32),
    /// Exactly one read-write handle.
    ReadWrite,
}

/// Path-keyed table of live opens.
#[derive(Debug, Default)]
pub(crate) struct HandleTable {
    map: RefCell<BTreeMap<PathBuf, OpenMode>>,
}

impl HandleTable {
    #[inline]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Canonical key for the handle table.
    #[inline]
    pub(crate) fn key(path: &Path) -> PathBuf {
        path.normalize()
    }

    /// Register a read-only open. Fails if a RW handle is outstanding.
    pub(crate) fn lock_ro<E: Error>(&self, path: &Path) -> FsResult<(), E> {
        let key = Self::key(path);
        let mut handles = self.map.borrow_mut();
        match handles.get_mut(&key) {
            None => {
                handles.insert(key, OpenMode::ReadOnly(1));
                Ok(())
            }
            Some(OpenMode::ReadOnly(count)) => {
                *count += 1;
                Ok(())
            }
            Some(OpenMode::ReadWrite) => Err(FsError::FileLocked),
        }
    }

    /// Register a read-write open. Exclusive: fails if any other handle exists.
    pub(crate) fn lock_rw<E: Error>(&self, path: &Path) -> FsResult<(), E> {
        let key = Self::key(path);
        let mut handles = self.map.borrow_mut();
        if handles.contains_key(&key) {
            return Err(FsError::FileLocked);
        }
        handles.insert(key, OpenMode::ReadWrite);
        Ok(())
    }

    /// `true` when any live handle is registered at `prefix` or anywhere
    /// below it (same convention as the lock methods: `prefix` arrives
    /// already case-folded). Used to guard rename/remove of directories.
    pub(crate) fn any_open_under(&self, prefix: &Path) -> bool {
        let key = Self::key(prefix);
        let map = self.map.borrow();
        // Keys sharing a string prefix sort contiguously in the BTreeMap,
        // so scan from `key` and stop at the first non-prefixed key.
        for k in map.range(key.clone()..).map(|(k, _)| k) {
            let (ks, ps) = (k.as_str(), key.as_str());
            if !ks.starts_with(ps) {
                break;
            }
            // Exact match, or a key strictly below the prefix.
            if ks.len() == ps.len() || crate::path::is_strictly_under(ks, ps) {
                return true;
            }
        }
        false
    }

    /// Release a read-only handle. Idempotent so Drop can call it freely.
    pub(crate) fn release_ro(&self, path: &Path) {
        let key = Self::key(path);
        let mut handles = self.map.borrow_mut();
        if let Some(OpenMode::ReadOnly(count)) = handles.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                handles.remove(&key);
            }
        }
    }

    /// Release a read-write handle.
    pub(crate) fn release_rw(&self, path: &Path) {
        let key = Self::key(path);
        self.map.borrow_mut().remove(&key);
    }
}
