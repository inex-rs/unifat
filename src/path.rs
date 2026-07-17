//! FAT-native path handling: a minimal, alloc-only Windows-style path
//! type. FAT uses `\` as separator; `/` is accepted on input.

use alloc::borrow::{Borrow, ToOwned};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::ops::Deref;

pub(crate) mod path_consts {
    pub const SEPARATOR: char = '\\';
    pub const SEPARATOR_BYTE: u8 = b'\\';
    pub const SEPARATOR_STR: &str = "\\";
    pub const CURRENT_DIR_STR: &str = ".";
    pub const PARENT_DIR_STR: &str = "..";
    pub const CURRENT_DIR: &[u8] = b".";
    pub const PARENT_DIR: &[u8] = b"..";
}

/// True when `path` lies strictly below `prefix`: it starts with
/// `prefix` and the match ends on a component boundary (`\FOO` covers
/// `\FOO\X` but not `\FOOD`; the root `\` ends with the separator and
/// covers everything). Both strings must already be normalized and
/// case-folded by the caller.
pub(crate) fn is_strictly_under(path: &str, prefix: &str) -> bool {
    path.len() > prefix.len()
        && path.starts_with(prefix)
        && (prefix.ends_with(path_consts::SEPARATOR)
            || path.as_bytes()[prefix.len()] == path_consts::SEPARATOR_BYTE)
}

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// Chars FAT allows in an LFN entry: everything but control chars and
/// reserved punctuation. Separators never reach this — the caller splits on them.
fn is_valid_lfn_char(c: char) -> bool {
    !matches!(
        c,
        '\u{00}'..='\u{1F}' | '\u{7F}' | '"' | '*' | ':' | '<' | '>' | '?' | '|'
    )
}

/// Both FAT LFNs and ExFAT names cap at 255 UTF-16 units. Longer names
/// are unreadable by other implementations (and past 819 units a FAT LFN
/// order byte would alias the last-entry flag), so reject them up front.
const MAX_NAME_UTF16_UNITS: usize = 255;

/// A path segment is valid when non-empty, at most 255 UTF-16 units,
/// every char is LFN-legal, and it doesn't end in `.` or space
/// (`.`/`..` themselves are fine).
fn is_valid_component(comp: &str) -> bool {
    if comp.is_empty() {
        return false;
    }
    if comp == "." || comp == ".." {
        return true;
    }
    // FAT silently trims trailing dots and spaces on write, which would
    // make this component equal another on disk — reject up front.
    let last = comp.chars().next_back().unwrap();
    if last == '.' || last == ' ' {
        return false;
    }
    if comp.encode_utf16().count() > MAX_NAME_UTF16_UNITS {
        return false;
    }
    comp.chars().all(is_valid_lfn_char)
}

/// Borrowed path slice. `#[repr(transparent)]` over `str` so `Box<Path>`
/// and `&Path` can be allocated/coerced without custom vtables.
#[repr(transparent)]
#[derive(PartialEq, Eq, Hash)]
// PartialOrd/Ord below (byte order of the underlying str), so `[&Path]`
// sorts like `[PathBuf]`.
pub struct Path {
    inner: str,
}

impl Path {
    /// Wrap a string slice as a borrowed [`Path`] (zero-cost).
    pub fn new<S: AsRef<str> + ?Sized>(s: &S) -> &Self {
        // SAFETY: `Path` is `#[repr(transparent)]` over `str`.
        unsafe { &*(s.as_ref() as *const str as *const Self) }
    }

    /// The underlying string slice.
    pub fn as_str(&self) -> &str {
        &self.inner
    }

    /// Clone into an owned [`PathBuf`].
    #[must_use]
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf {
            inner: self.inner.to_owned(),
        }
    }

    /// True for FAT-well-formed paths: every component non-empty and LFN-legal.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let trimmed = self.inner.trim_matches(is_sep);
        // All-separators and empty strings both mean the root — valid.
        // Interior double separators yield an empty component, which
        // `is_valid_component` rejects.
        trimmed.is_empty() || trimmed.split(is_sep).all(is_valid_component)
    }

    /// Whether the path begins at the root (leading `/` or `\`).
    #[must_use]
    pub fn is_absolute(&self) -> bool {
        self.inner.starts_with(is_sep)
    }

    /// Iterate the path's [`WindowsComponent`]s.
    pub fn components(&self) -> Components<'_> {
        Components::new(&self.inner)
    }

    /// Drop the final component; `None` at the root.
    #[must_use]
    pub fn parent(&self) -> Option<&Path> {
        let s = &self.inner;
        let stripped = s.trim_end_matches(is_sep);
        if stripped.is_empty() {
            return None;
        }
        let last = stripped.rfind(is_sep)?;
        if last == 0 {
            Some(Path::new(path_consts::SEPARATOR_STR))
        } else {
            Some(Path::new(&stripped[..last]))
        }
    }

    /// The final component, if it is a normal name (not root / `.` / `..`).
    #[must_use]
    pub fn file_name(&self) -> Option<&str> {
        self.components().next_back().and_then(|c| match c {
            WindowsComponent::Normal(s) => Some(s),
            _ => None,
        })
    }

    /// The extension of [`Self::file_name`]: the suffix after the last
    /// `.`, unless the name starts with that dot or ends with it.
    /// Mirrors `std::path::Path::extension`.
    #[must_use]
    pub fn extension(&self) -> Option<&str> {
        let name = self.file_name()?;
        let (stem, ext) = name.rsplit_once('.')?;
        (!stem.is_empty() && !ext.is_empty()).then_some(ext)
    }

    /// [`Self::file_name`] without its [`Self::extension`].
    /// Mirrors `std::path::Path::file_stem`.
    #[must_use]
    pub fn file_stem(&self) -> Option<&str> {
        let name = self.file_name()?;
        match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => Some(stem),
            _ => Some(name),
        }
    }

    /// Whether `base` is a component-wise prefix of this path
    /// (case-sensitive, like `std::path::Path::starts_with` — the
    /// volume's own name comparisons are separately case-folded).
    #[must_use]
    pub fn starts_with<P: AsRef<Path>>(&self, base: P) -> bool {
        let mut mine = self.components();
        for theirs in base.as_ref().components() {
            if mine.next() != Some(theirs) {
                return false;
            }
        }
        true
    }

    /// Resolve `.`/`..` components (clamped at root), preserving a leading root.
    #[must_use]
    pub fn normalize(&self) -> PathBuf {
        let mut out: Vec<&str> = Vec::new();
        let mut has_root = false;
        for c in self.components() {
            match c {
                WindowsComponent::RootDir => {
                    has_root = true;
                    out.clear();
                }
                WindowsComponent::CurDir => {}
                WindowsComponent::ParentDir => {
                    out.pop();
                }
                WindowsComponent::Normal(s) => out.push(s),
            }
        }
        let mut buf = String::new();
        if has_root {
            buf.push(path_consts::SEPARATOR);
        }
        for (i, seg) in out.iter().enumerate() {
            if i > 0 {
                buf.push(path_consts::SEPARATOR);
            }
            buf.push_str(seg);
        }
        if buf.is_empty() {
            buf.push(path_consts::SEPARATOR);
        }
        PathBuf { inner: buf }
    }

    /// Append `child` to this path, returning a new [`PathBuf`].
    #[must_use]
    pub fn join<S: AsRef<str>>(&self, child: S) -> PathBuf {
        let mut pb = self.to_path_buf();
        pb.push(child);
        pb
    }

    /// Iterate parents, deepest-first, inclusive of `self`.
    pub fn ancestors(&self) -> Ancestors<'_> {
        Ancestors { next: Some(self) }
    }
}

impl fmt::Debug for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Path({:?})", &self.inner)
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.inner)
    }
}

impl PartialOrd for Path {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Path {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl AsRef<Path> for Path {
    fn as_ref(&self) -> &Path {
        self
    }
}

impl AsRef<Path> for str {
    fn as_ref(&self) -> &Path {
        Path::new(self)
    }
}

impl AsRef<Path> for String {
    fn as_ref(&self) -> &Path {
        Path::new(self.as_str())
    }
}

impl ToOwned for Path {
    type Owned = PathBuf;
    fn to_owned(&self) -> PathBuf {
        self.to_path_buf()
    }
}

/// Iterator over a path and its parents, deepest first ([`Path::ancestors`]).
#[derive(Debug, Clone)]
pub struct Ancestors<'a> {
    next: Option<&'a Path>,
}

impl<'a> Iterator for Ancestors<'a> {
    type Item = &'a Path;
    fn next(&mut self) -> Option<Self::Item> {
        let current = self.next?;
        self.next = current.parent();
        Some(current)
    }
}

/// An owned, mutable FAT-style path (the [`String`] to [`Path`]'s `str`).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathBuf {
    inner: String,
}

impl PathBuf {
    /// An empty path buffer.
    pub fn new() -> Self {
        Self {
            inner: String::new(),
        }
    }

    /// Borrow as a [`Path`].
    pub fn as_path(&self) -> &Path {
        Path::new(self.inner.as_str())
    }

    /// The underlying string slice.
    pub fn as_str(&self) -> &str {
        self.inner.as_str()
    }

    /// Append `component`. An absolute `component` replaces the buffer.
    pub fn push<S: AsRef<str>>(&mut self, component: S) {
        let s = component.as_ref();
        if s.is_empty() {
            return;
        }
        if s.starts_with(is_sep) {
            self.inner.clear();
            self.inner.push_str(s);
            return;
        }
        if !self.inner.is_empty() && !self.inner.ends_with(is_sep) {
            self.inner.push(path_consts::SEPARATOR);
        }
        self.inner.push_str(s);
    }

    /// Remove the final component. Returns `false` at the root.
    pub fn pop(&mut self) -> bool {
        match self.as_path().parent() {
            Some(p) => {
                let s = p.as_str().to_owned();
                self.inner = s;
                true
            }
            None => false,
        }
    }
}

impl Default for PathBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PathBuf({:?})", self.inner)
    }
}

impl fmt::Display for PathBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.inner)
    }
}

impl Deref for PathBuf {
    type Target = Path;
    fn deref(&self) -> &Path {
        self.as_path()
    }
}

impl AsRef<Path> for PathBuf {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl Borrow<Path> for PathBuf {
    fn borrow(&self) -> &Path {
        self.as_path()
    }
}

impl From<&str> for PathBuf {
    fn from(s: &str) -> Self {
        Self {
            inner: s.to_owned(),
        }
    }
}

impl From<String> for PathBuf {
    fn from(inner: String) -> Self {
        Self { inner }
    }
}

impl From<&String> for PathBuf {
    fn from(s: &String) -> Self {
        Self { inner: s.clone() }
    }
}

impl From<&Path> for PathBuf {
    fn from(p: &Path) -> Self {
        p.to_path_buf()
    }
}

impl PartialEq<PathBuf> for Path {
    fn eq(&self, other: &PathBuf) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<Path> for PathBuf {
    fn eq(&self, other: &Path) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<&Path> for PathBuf {
    fn eq(&self, other: &&Path) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<PathBuf> for &Path {
    fn eq(&self, other: &PathBuf) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for Path {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Path {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl From<&Path> for Box<Path> {
    fn from(p: &Path) -> Self {
        let boxed: Box<str> = Box::from(p.as_str());
        let ptr = Box::into_raw(boxed) as *mut Path;
        // SAFETY: `Path` is `repr(transparent)` over `str`, so reinterpreting
        // the raw pointer is sound.
        unsafe { Box::from_raw(ptr) }
    }
}

impl From<PathBuf> for Box<Path> {
    fn from(p: PathBuf) -> Self {
        Box::from(p.as_path())
    }
}

impl Clone for Box<Path> {
    fn clone(&self) -> Self {
        Box::<Path>::from(&**self)
    }
}

/// A single component of a [`Path`], as yielded by [`Components`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsComponent<'a> {
    /// The root directory (a leading separator).
    RootDir,
    /// The current directory, `.`.
    CurDir,
    /// The parent directory, `..`.
    ParentDir,
    /// A normal named component.
    Normal(&'a str),
}

impl<'a> WindowsComponent<'a> {
    /// The component as a string slice.
    pub fn as_str(&self) -> &'a str {
        match *self {
            WindowsComponent::RootDir => path_consts::SEPARATOR_STR,
            WindowsComponent::CurDir => path_consts::CURRENT_DIR_STR,
            WindowsComponent::ParentDir => path_consts::PARENT_DIR_STR,
            WindowsComponent::Normal(s) => s,
        }
    }
}

/// Iterator over the [`WindowsComponent`]s of a [`Path`] ([`Path::components`]).
#[derive(Debug, Clone)]
pub struct Components<'a> {
    rest: &'a str,
    yielded_root: bool,
    had_root: bool,
}

impl<'a> Components<'a> {
    fn new(s: &'a str) -> Self {
        let had_root = s.starts_with(is_sep);
        let rest = if had_root {
            s.trim_start_matches(is_sep)
        } else {
            s
        };
        Self {
            rest,
            yielded_root: false,
            had_root,
        }
    }

    fn split_head(s: &str) -> Option<(&str, &str)> {
        if s.is_empty() {
            return None;
        }
        match s.find(is_sep) {
            Some(i) => {
                let (head, tail) = s.split_at(i);
                let tail = tail.trim_start_matches(is_sep);
                Some((head, tail))
            }
            None => Some((s, "")),
        }
    }

    fn split_tail(s: &str) -> Option<(&str, &str)> {
        // Trailing separators carry no component ("\a\b\" ends with "b");
        // trimming first keeps reverse iteration consistent with forward.
        let s = s.trim_end_matches(is_sep);
        if s.is_empty() {
            return None;
        }
        match s.rfind(is_sep) {
            Some(i) => {
                let (rest, last) = s.split_at(i);
                let last = last.trim_start_matches(is_sep);
                let rest = rest.trim_end_matches(is_sep);
                Some((last, rest))
            }
            None => Some((s, "")),
        }
    }

    fn classify(seg: &'a str) -> WindowsComponent<'a> {
        match seg {
            path_consts::CURRENT_DIR_STR => WindowsComponent::CurDir,
            path_consts::PARENT_DIR_STR => WindowsComponent::ParentDir,
            _ => WindowsComponent::Normal(seg),
        }
    }
}

impl<'a> Iterator for Components<'a> {
    type Item = WindowsComponent<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.had_root && !self.yielded_root {
            self.yielded_root = true;
            return Some(WindowsComponent::RootDir);
        }
        let (head, tail) = Self::split_head(self.rest)?;
        self.rest = tail;
        Some(Self::classify(head))
    }
}

impl DoubleEndedIterator for Components<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if let Some((last, head)) = Self::split_tail(self.rest) {
            self.rest = head;
            return Some(Self::classify(last));
        }
        if self.had_root && !self.yielded_root {
            self.yielded_root = true;
            return Some(WindowsComponent::RootDir);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn components_basic() {
        let p = Path::new("\\foo\\bar\\baz");
        let got: Vec<_> = p.components().collect();
        assert_eq!(
            got,
            vec![
                WindowsComponent::RootDir,
                WindowsComponent::Normal("foo"),
                WindowsComponent::Normal("bar"),
                WindowsComponent::Normal("baz"),
            ]
        );
    }

    #[test]
    fn extension_and_stem_mirror_std() {
        assert_eq!(Path::new("\\a\\b.txt").extension(), Some("txt"));
        assert_eq!(Path::new("\\a\\b.txt").file_stem(), Some("b"));
        assert_eq!(Path::new("\\a\\archive.tar.gz").extension(), Some("gz"));
        assert_eq!(
            Path::new("\\a\\archive.tar.gz").file_stem(),
            Some("archive.tar")
        );
        assert_eq!(Path::new("\\a\\.hidden").extension(), None);
        assert_eq!(Path::new("\\a\\.hidden").file_stem(), Some(".hidden"));
        assert_eq!(Path::new("\\a\\noext").extension(), None);
        assert_eq!(Path::new("\\").extension(), None);
    }

    #[test]
    fn starts_with_is_component_wise() {
        assert!(Path::new("\\foo\\bar").starts_with("\\foo"));
        assert!(Path::new("\\foo\\bar").starts_with("\\foo\\bar"));
        assert!(!Path::new("\\food").starts_with("\\foo"));
        assert!(!Path::new("\\foo").starts_with("\\foo\\bar"));
        assert!(Path::new("\\foo").starts_with("\\"));
    }

    #[test]
    fn trailing_separator_file_name_and_reverse_components() {
        // R26: a trailing separator carries no component.
        assert_eq!(Path::new("\\foo\\bar\\").file_name(), Some("bar"));
        let fwd: alloc::vec::Vec<_> = Path::new("\\a\\b\\").components().collect();
        let mut rev: alloc::vec::Vec<_> = Path::new("\\a\\b\\").components().rev().collect();
        rev.reverse();
        assert_eq!(fwd, rev, "forward and reverse iteration must agree");
    }

    #[test]
    fn paths_are_ord() {
        let mut v = alloc::vec![Path::new("\\b"), Path::new("\\a")];
        v.sort();
        assert_eq!(v[0].as_str(), "\\a");
    }

    #[test]
    fn components_reverse_includes_root_last() {
        let p = Path::new("\\foo\\bar");
        let got: Vec<_> = p.components().rev().collect();
        assert_eq!(
            got,
            vec![
                WindowsComponent::Normal("bar"),
                WindowsComponent::Normal("foo"),
                WindowsComponent::RootDir,
            ]
        );
    }

    #[test]
    fn normalize_collapses_dots() {
        let p = Path::new("\\a\\.\\b\\..\\c");
        assert_eq!(p.normalize().as_str(), "\\a\\c");
    }

    #[test]
    fn normalize_clamps_parent_at_root() {
        let p = Path::new("\\..\\..\\foo");
        assert_eq!(p.normalize().as_str(), "\\foo");
    }

    #[test]
    fn parent_of_root_is_none() {
        let p = Path::new("\\");
        assert!(p.parent().is_none());
    }

    #[test]
    fn parent_trims_trailing_separator() {
        let p = Path::new("\\foo\\bar\\");
        assert_eq!(p.parent().unwrap().as_str(), "\\foo");
    }

    #[test]
    fn file_name_returns_last_normal() {
        assert_eq!(Path::new("\\foo\\bar").file_name(), Some("bar"));
        assert_eq!(Path::new("\\").file_name(), None);
    }

    #[test]
    fn pathbuf_push_absolute_replaces() {
        let mut pb = PathBuf::from("\\foo\\bar");
        pb.push("\\abs");
        assert_eq!(pb.as_str(), "\\abs");
    }

    #[test]
    fn pathbuf_push_relative_appends() {
        let mut pb = PathBuf::from("\\foo");
        pb.push("bar");
        assert_eq!(pb.as_str(), "\\foo\\bar");
    }

    #[test]
    fn box_path_clone_roundtrip() {
        let p: Box<Path> = Box::from(Path::new("\\x\\y"));
        let q = p.clone();
        // Read `p` after cloning so clippy::redundant_clone stays quiet.
        assert_eq!(q.as_str(), "\\x\\y");
        assert_eq!(q.as_str(), p.as_str());
    }

    #[test]
    fn cross_type_eq() {
        let pb = PathBuf::from("\\foo");
        let p = Path::new("\\foo");
        assert_eq!(pb, *p);
        assert_eq!(*p, pb);
    }

    #[test]
    fn is_valid_rejects_reserved_chars() {
        for bad in &[
            r"\a:b.txt",
            r"\bad<.txt",
            r"\pipe|file",
            r"\que?ry",
            r"\aster*isk",
            r"\gt>lt<",
            "\\quo\"te",
        ] {
            assert!(!Path::new(*bad).is_valid(), "expected invalid: {bad:?}");
        }
    }

    #[test]
    fn is_valid_rejects_control_chars() {
        let p = "\\file\u{01}.txt";
        assert!(!Path::new(p).is_valid());
        let p2 = "\\file\u{7F}.txt";
        assert!(!Path::new(p2).is_valid());
    }

    #[test]
    fn is_valid_rejects_trailing_dot_or_space() {
        assert!(!Path::new(r"\file.").is_valid());
        assert!(!Path::new(r"\dir ").is_valid());
        assert!(!Path::new(r"\dir \sub").is_valid());
    }

    #[test]
    fn is_valid_caps_components_at_255_utf16_units() {
        use alloc::format;
        use alloc::string::String;
        // 255 ASCII chars = 255 units: fine.
        let ok: String = "a".repeat(255);
        assert!(Path::new(&format!("\\{ok}")).is_valid());
        // 256 units: rejected.
        let too_long: String = "a".repeat(256);
        assert!(!Path::new(&format!("\\{too_long}")).is_valid());
        // Units, not bytes: 200 two-byte-UTF-8 Cyrillic chars are 200 units.
        let cyr: String = "б".repeat(200);
        assert!(Path::new(&format!("\\{cyr}")).is_valid());
        // Non-BMP chars count as 2 units (surrogate pair): 128 of them = 256.
        let emoji: String = "😀".repeat(128);
        assert!(!Path::new(&format!("\\{emoji}")).is_valid());
        assert!(Path::new(&format!("\\{}", "😀".repeat(127))).is_valid());
    }

    #[test]
    fn is_valid_accepts_normal_names() {
        assert!(Path::new(r"\hello.txt").is_valid());
        assert!(Path::new(r"\games\Super Mario.nds").is_valid());
        assert!(Path::new(r"\.").is_valid());
        assert!(Path::new(r"\..").is_valid());
    }

    #[test]
    fn ancestors_walks_up_to_root() {
        let p = Path::new("\\a\\b\\c");
        let got: Vec<_> = p.ancestors().map(|p| p.as_str()).collect();
        assert_eq!(got, vec!["\\a\\b\\c", "\\a\\b", "\\a", "\\"]);
    }
}
