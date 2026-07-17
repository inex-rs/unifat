//! FAT short-name generation and LFN UTF-16 helpers (path-pure).

#[cfg(not(feature = "std"))]
use alloc::{format, string::String};

use alloc::string::FromUtf16Error;

use crate::FsResult;
use crate::error::FsError;
use crate::fat::{CURRENT_DIR_SFN, FatDirectory, PARENT_DIR_SFN, SFN_EXT_LEN, SFN_NAME_LEN, Sfn};

use embedded_io::*;

/// Decode one LFN character run: UCS-2 units up to the NUL terminator
/// (`0xFFFF` padding beyond it is ignored).
pub(crate) fn string_from_lfn(units: &[u16]) -> Result<String, FromUtf16Error> {
    let end = units.iter().take_while(|&&u| u != 0).count();
    String::from_utf16(&units[..end])
}

/// Whether `c` can appear in a pure 8.3 short name: uppercase letters,
/// digits, and the spec's punctuation set. Notably excludes `.` (the
/// separator, handled by the caller), space, and `+ , ; = [ ]`.
#[inline]
fn is_sfn_char(c: char) -> bool {
    c.is_ascii_uppercase()
        || c.is_ascii_digit()
        || matches!(
            c,
            '$' | '%'
                | '\''
                | '-'
                | '_'
                | '@'
                | '~'
                | '`'
                | '!'
                | '('
                | ')'
                | '{'
                | '}'
                | '^'
                | '#'
                | '&'
        )
}

pub(crate) fn as_sfn(string: &str) -> Option<Sfn> {
    // Special directory names: `split_once('.')` would mangle these into empty
    // name/ext and fail to match CURRENT_DIR_SFN / PARENT_DIR_SFN, causing
    // EntryComposer to emit spurious LFN slots for `.` / `..`.
    if string == "." {
        return Some(CURRENT_DIR_SFN);
    }
    if string == ".." {
        return Some(PARENT_DIR_SFN);
    }

    // A pure 8.3 name has at most one dot (the name/ext separator) and
    // only SFN-legal characters. Anything else (lowercase, non-ASCII,
    // spaces, `+`, a second dot, …) needs an LFN plus a generated alias —
    // a raw `.` or `+` inside the 11-byte field is invalid on disk.
    if string.matches('.').count() > 1 {
        return None;
    }
    if !string.chars().all(|c| c == '.' || is_sfn_char(c)) {
        return None;
    }

    // ASCII, so 1 byte = 1 char
    if string.len() > SFN_NAME_LEN + 1 + SFN_EXT_LEN {
        return None;
    }

    let (name, ext) = string.split_once('.').unwrap_or((string, ""));

    if name.is_empty() || name.len() > SFN_NAME_LEN || ext.len() > SFN_EXT_LEN {
        return None;
    }

    // space-pad to SFN_NAME_LEN (8) and SFN_EXT_LEN (3)
    let (name, ext) = (format!("{name:<8}"), format!("{ext:<3}"));

    Some(Sfn {
        name: name.as_bytes().try_into().unwrap(),
        ext: ext.as_bytes().try_into().unwrap(),
    })
}

/// The stem and 3-byte extension a generated short name is built from:
/// split at the last dot, uppercase, drop spaces/dots/non-ASCII, and
/// replace remaining SFN-illegal ASCII with `_` (Windows-alias style).
fn sfn_stem_ext(long_name: &str) -> (String, [u8; SFN_EXT_LEN]) {
    fn alias_chars(s: &str) -> String {
        s.chars()
            .filter(|c| c.is_ascii() && !matches!(c, ' ' | '.'))
            .map(|c| {
                let up = c.to_ascii_uppercase();
                if is_sfn_char(up) { up } else { '_' }
            })
            .collect()
    }
    let (stem_src, ext_src) = match long_name.rsplit_once('.') {
        Some((stem, ext)) => (stem, ext),
        None => (long_name, ""),
    };
    let stem = alias_chars(stem_src);
    let ext = alias_chars(ext_src);

    let mut ext_bytes = [b' '; SFN_EXT_LEN];
    for (slot, b) in ext_bytes.iter_mut().zip(ext.bytes()) {
        *slot = b;
    }
    (stem, ext_bytes)
}

/// Fit `head` + `tail` into the 8-byte name field, trimming `head` so `tail`
/// (the `~N` suffix) always survives, then space-padding.
fn compose_short_name(stem: &str, tail: &str, ext: [u8; SFN_EXT_LEN]) -> Sfn {
    let keep = SFN_NAME_LEN.saturating_sub(tail.len());
    let head: String = stem.chars().take(keep).collect();
    let mut name = [b' '; SFN_NAME_LEN];
    for (slot, b) in name.iter_mut().zip(head.bytes().chain(tail.bytes())) {
        *slot = b;
    }
    Sfn { name, ext }
}

/// A 16-bit FNV-1a digest, folded, for the hashed short-name tail.
#[allow(clippy::cast_possible_truncation)] // XOR-fold to 16 bits is the intent
fn name_hash(long_name: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in long_name.bytes() {
        h = (h ^ u32::from(b)).wrapping_mul(0x0100_0193);
    }
    (h ^ (h >> 16)) as u16
}

/// Whether some entry in `dir` already carries short name `sfn`.
fn sfn_taken<S>(dir: &FatDirectory<'_, S>, sfn: Sfn) -> FsResult<bool, S::Error>
where
    S: Read + Write + Seek,
{
    for entry in dir.iter_raw() {
        if entry?.sfn == sfn {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Generate a unique 8.3 short name for `long_name` within `dir`.
///
/// A name that is already valid 8.3 is used verbatim if free. Otherwise a
/// short numeric tail (`STEM~1`..`~4`) is tried first, then — to avoid an
/// O(collisions) sweep in a crowded directory — a name-derived hex tail
/// (`ST~HHHH`), matching Windows' behaviour for large collision counts.
pub(crate) fn gen_sfn<S>(long_name: &str, dir: &FatDirectory<'_, S>) -> FsResult<Sfn, S::Error>
where
    S: Read + Write + Seek,
{
    if let Some(sfn) = as_sfn(long_name)
        && !sfn_taken(dir, sfn)?
    {
        return Ok(sfn);
    }

    let (stem, ext) = sfn_stem_ext(long_name);

    for n in 1u32..=4 {
        let candidate = compose_short_name(&stem, &format!("~{n}"), ext);
        if !sfn_taken(dir, candidate)? {
            return Ok(candidate);
        }
    }

    let mut tag = name_hash(long_name);
    for _ in 0..=u16::MAX {
        let candidate = compose_short_name(&stem, &format!("~{tag:04X}"), ext);
        if !sfn_taken(dir, candidate)? {
            return Ok(candidate);
        }
        tag = tag.wrapping_add(1);
    }

    // Every one of the 65536 hex tails collided — the directory is full.
    Err(FsError::StorageFull)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_sfn_accepts_plain_8_3() {
        assert!(as_sfn("HELLO.TXT").is_some());
        assert!(as_sfn("NOEXT").is_some());
        assert!(as_sfn("A$%-_@~1.X_Y").is_some());
        assert_eq!(as_sfn("."), Some(CURRENT_DIR_SFN));
        assert_eq!(as_sfn(".."), Some(PARENT_DIR_SFN));
    }

    #[test]
    fn as_sfn_rejects_names_needing_an_lfn() {
        // Multi-dot: a raw '.' inside the 11-byte field is invalid.
        assert!(as_sfn("A.B.C").is_none());
        // SFN-illegal characters.
        assert!(as_sfn("A+B.TXT").is_none());
        assert!(as_sfn("A B.TXT").is_none());
        assert!(as_sfn("A[1].TXT").is_none());
        // Lowercase and non-ASCII need the LFN.
        assert!(as_sfn("hello.txt").is_none());
        assert!(as_sfn("б.txt").is_none());
        // Length limits and empty stem.
        assert!(as_sfn("TOOLONGNAME.TXT").is_none());
        assert!(as_sfn("NAME.LONG").is_none());
        assert!(as_sfn(".GIT").is_none());
    }

    #[test]
    fn alias_stems_replace_illegal_chars() {
        // Spaces and dots dropped; illegal ASCII becomes '_'.
        let (stem, ext) = sfn_stem_ext("My File+x.v1.txt");
        assert_eq!(stem, "MYFILE_XV1");
        assert_eq!(&ext, b"TXT");
        // Fully non-ASCII stems degrade to the numeric tail alone.
        let (stem, _) = sfn_stem_ext("файл");
        assert!(stem.is_empty());
    }
}
