//! Path preparation shared by Volume and backends.

use crate::error::{FsError, FsResult};
use crate::path::{Path, PathBuf};

/// Normalized path ready for backend use, with common split helpers.
#[derive(Debug, Clone)]
pub(crate) struct PreparedPath {
    path: PathBuf,
}

impl PreparedPath {
    /// Borrow as [`Path`].
    #[inline]
    pub(crate) fn as_path(&self) -> &Path {
        &self.path
    }

    /// Final component if this is not the root.
    #[inline]
    pub(crate) fn file_name(&self) -> Option<&str> {
        self.path.file_name()
    }

    /// `true` when this is the volume root (no file name).
    #[inline]
    pub(crate) fn is_root(&self) -> bool {
        self.file_name().is_none()
    }
}

/// Validate and normalize a user path for filesystem operations.
///
/// Returns [`FsError::InvalidInput`] when the path fails FAT/ExFAT name rules.
pub(crate) fn prepare_path<P, E>(path: P) -> FsResult<PreparedPath, E>
where
    P: AsRef<Path>,
    E: embedded_io::Error,
{
    let path = path.as_ref();
    if !path.is_valid() {
        return Err(FsError::InvalidInput);
    }
    let normalized = path.normalize();
    // All paths are volume-root-relative; root them here so that
    // `Path::parent`, backend resolution, and handle-lock keys behave
    // identically for "a.txt" and "\a.txt".
    let path = if normalized.is_absolute() {
        normalized
    } else {
        let mut rooted = PathBuf::from(crate::path::path_consts::SEPARATOR_STR);
        rooted.push(normalized.as_str());
        rooted
    };
    Ok(PreparedPath { path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FsError;

    #[derive(Debug)]
    struct DummyErr;
    impl embedded_io::Error for DummyErr {
        fn kind(&self) -> embedded_io::ErrorKind {
            embedded_io::ErrorKind::Other
        }
    }
    impl core::fmt::Display for DummyErr {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("dummy")
        }
    }
    impl core::error::Error for DummyErr {}

    #[test]
    fn prepare_accepts_normal_path() {
        let p = prepare_path::<_, DummyErr>("saves/slot1").unwrap();
        assert_eq!(p.file_name(), Some("slot1"));
        assert!(!p.is_root());
    }

    #[test]
    fn prepare_rejects_invalid() {
        let err = prepare_path::<_, DummyErr>("bad*name").unwrap_err();
        assert!(matches!(err, FsError::InvalidInput));
    }

    #[test]
    fn prepare_root() {
        let p = prepare_path::<_, DummyErr>("\\").unwrap();
        assert!(p.is_root());
    }
}
