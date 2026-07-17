//! Filename comparison policy shared by both backends.
//!
//! Format-defined at mount: FAT always uses ASCII case-fold; ExFAT uses the
//! volume upcase table. Callers go through [`NamePolicy`] / [`NameEq`].

use alloc::string::String;
use alloc::vec::Vec;

use crate::path::{Path, PathBuf};

/// Policy object for comparing entry names (ASCII fold vs ExFAT upcase table).
pub(crate) trait NameEq {
    fn names_equal(&self, a: &str, b: &str) -> bool;
}

/// ASCII case-fold name equality (classic FAT / VFAT).
///
/// Folds only ASCII A–Z/a–z — enough for the overwhelming majority of
/// media and matching Windows/Linux default behaviour for Latin names.
/// Non-ASCII codepoints compare exactly (no full Unicode case fold).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AsciiNameEq;

impl NameEq for AsciiNameEq {
    #[inline]
    fn names_equal(&self, a: &str, b: &str) -> bool {
        a.eq_ignore_ascii_case(b)
    }
}

/// ExFAT upcase-table name equality (BMP codepoints via on-disk table).
#[derive(Debug, Clone)]
pub(crate) struct UpcaseNameEq {
    /// Sparse, codepoint-sorted non-identity mappings (`unit → upcased`).
    /// Everything not listed folds to itself. A real Windows table has
    /// ~2.6K entries — a flat 64K map would cost 128 KiB per mount.
    table: Vec<(u16, u16)>,
}

impl UpcaseNameEq {
    /// Build from codepoint-sorted non-identity mappings.
    pub(crate) fn from_table(table: Vec<(u16, u16)>) -> Self {
        debug_assert!(table.is_sorted_by_key(|&(cp, _)| cp));
        Self { table }
    }

    /// Case-fold a `char` via the upcase table; non-BMP maps to itself.
    fn upcase_char(&self, c: char) -> char {
        let Ok(cp) = u16::try_from(c as u32) else {
            return c;
        };
        char::from_u32(u32::from(self.upcase_unit(cp))).unwrap_or(c)
    }

    /// Case-fold one UTF-16 unit via the upcase table (identity for
    /// unlisted units; surrogates are never listed in real tables).
    fn upcase_unit(&self, unit: u16) -> u16 {
        match self.table.binary_search_by_key(&unit, |&(cp, _)| cp) {
            Ok(i) => self.table[i].1,
            Err(_) => unit,
        }
    }
}

impl NameEq for UpcaseNameEq {
    fn names_equal(&self, a: &str, b: &str) -> bool {
        let mut ai = a.chars();
        let mut bi = b.chars();
        loop {
            match (ai.next(), bi.next()) {
                (None, None) => return true,
                (Some(x), Some(y)) if self.upcase_char(x) == self.upcase_char(y) => continue,
                _ => return false,
            }
        }
    }
}

/// Concrete name policy stored on the VFS at mount (no `dyn` on hot path).
///
/// Not user-selected via [`FsOptions`](crate::FsOptions): FatVfs always uses
/// [`NamePolicy::Ascii`]; ExfatVfs loads the volume upcase table into
/// [`NamePolicy::Upcase`].
#[derive(Debug, Clone)]
pub(crate) enum NamePolicy {
    /// Classic FAT / VFAT ASCII case-fold.
    Ascii(AsciiNameEq),
    /// ExFAT upcase table.
    Upcase(UpcaseNameEq),
}

impl NamePolicy {
    /// FAT volumes always use ASCII case-fold.
    #[inline]
    pub(crate) fn ascii() -> Self {
        NamePolicy::Ascii(AsciiNameEq)
    }

    /// ExFAT volumes after loading the on-disk upcase table (sparse,
    /// codepoint-sorted non-identity mappings).
    #[inline]
    pub(crate) fn upcase(table: Vec<(u16, u16)>) -> Self {
        NamePolicy::Upcase(UpcaseNameEq::from_table(table))
    }

    /// Case-fold one UTF-16 unit (ASCII fold or upcase table). Used for the
    /// exFAT NameHash, which is computed over up-cased UTF-16LE units.
    pub(crate) fn upcase_unit(&self, unit: u16) -> u16 {
        match self {
            NamePolicy::Ascii(_) => {
                if (u16::from(b'a')..=u16::from(b'z')).contains(&unit) {
                    unit - u16::from(b'a') + u16::from(b'A')
                } else {
                    unit
                }
            }
            NamePolicy::Upcase(p) => p.upcase_unit(unit),
        }
    }

    /// `true` when `descendant` lies strictly below `ancestor` (case-folded,
    /// component-boundary-aware). Used to reject renaming a directory into
    /// its own subtree, which would detach it into an unreachable cycle.
    pub(crate) fn is_strict_ancestor(&self, ancestor: &Path, descendant: &Path) -> bool {
        let a = self.fold_path(ancestor);
        let d = self.fold_path(descendant);
        crate::path::is_strictly_under(d.as_str(), a.as_str())
    }

    /// Canonical case-folded key for handle-table locking.
    ///
    /// The volume resolves names case-insensitively, so lock keys must be
    /// folded through the same policy — otherwise `FOO.TXT` and `foo.txt`
    /// take separate "exclusive" locks on one file. Normalizes first;
    /// separators are unaffected by either fold.
    pub(crate) fn fold_path(&self, path: &Path) -> PathBuf {
        let norm = path.normalize();
        match self {
            NamePolicy::Ascii(_) => PathBuf::from(norm.as_str().to_ascii_uppercase()),
            NamePolicy::Upcase(p) => PathBuf::from(
                norm.as_str()
                    .chars()
                    .map(|c| p.upcase_char(c))
                    .collect::<String>(),
            ),
        }
    }
}

impl NameEq for NamePolicy {
    #[inline]
    fn names_equal(&self, a: &str, b: &str) -> bool {
        match self {
            NamePolicy::Ascii(p) => p.names_equal(a, b),
            NamePolicy::Upcase(p) => p.names_equal(a, b),
        }
    }
}

impl NameEq for &NamePolicy {
    #[inline]
    fn names_equal(&self, a: &str, b: &str) -> bool {
        (*self).names_equal(a, b)
    }
}

/// Path components as normal segments only (`/` and `\` both accepted).
pub(crate) fn split_components(path: &str) -> impl Iterator<Item = &str> {
    path.split(['/', '\\']).filter(|c| !c.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_policy_matches_ci() {
        let p = NamePolicy::ascii();
        assert!(p.names_equal("Foo.TXT", "foo.txt"));
        assert!(!p.names_equal("foo", "bar"));
    }

    #[test]
    fn strict_ancestor_component_boundaries() {
        use crate::path::Path;
        let p = NamePolicy::ascii();
        assert!(p.is_strict_ancestor(Path::new("\\foo"), Path::new("\\foo\\bar")));
        assert!(p.is_strict_ancestor(Path::new("\\FOO"), Path::new("\\foo\\bar\\baz")));
        assert!(p.is_strict_ancestor(Path::new("\\"), Path::new("\\foo")));
        assert!(!p.is_strict_ancestor(Path::new("\\foo"), Path::new("\\food")));
        assert!(!p.is_strict_ancestor(Path::new("\\foo"), Path::new("\\foo")));
        assert!(!p.is_strict_ancestor(Path::new("\\foo\\bar"), Path::new("\\foo")));
    }

    #[test]
    fn upcase_policy_sparse_table() {
        // Sparse non-identity mappings: 'a'..='z' fold to 'A'..='Z',
        // everything else (including 'ф' here) folds to itself.
        let table: Vec<(u16, u16)> = (b'a'..=b'z')
            .map(|c| (u16::from(c), u16::from(c - b'a' + b'A')))
            .collect();
        let p = NamePolicy::upcase(table);
        assert!(p.names_equal("File", "FILE"));
        assert!(p.names_equal("file", "FILE"));
        assert!(!p.names_equal("file", "flie"));
        // Unlisted units compare exactly.
        assert!(p.names_equal("ф1", "ф1"));
        assert!(!p.names_equal("ф", "Ф"));
    }
}
