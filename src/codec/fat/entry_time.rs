//! Classic FAT directory-entry timestamp fields.
//!
//! Each stamp is stored as its raw on-disk words, so a directory entry
//! round-trips byte-for-byte; the conversions to and from `time` types
//! go through the shared [`crate::codec::time`] codec. FAT packs a date
//! word into the high 16 bits and a time word into the low 16 bits of a
//! DOS timestamp — the same layout ExFAT uses.

use layline::Layout;
use time::{Date, PrimitiveDateTime};

use crate::codec::time as dos;

/// Combine a DOS date word (high half) and time word (low half).
#[inline]
fn pack(date: u16, time: u16) -> u32 {
    (u32::from(date) << 16) | u32::from(time)
}

/// Split a packed timestamp into `(time_word, date_word)`.
#[inline]
#[allow(clippy::cast_possible_truncation)] // each half is masked to 16 bits
fn split(ts: u32) -> (u16, u16) {
    ((ts & 0xFFFF) as u16, (ts >> 16) as u16)
}

/// Creation stamp: 10 ms-increment byte + time word + date word (`DIR_CrtTimeTenth`
/// / `DIR_CrtTime` / `DIR_CrtDate`). A zero DATE reads back as absent — a
/// zero time word is a legitimate midnight (00:00:00), common from tools
/// that only stamp the date; treating it as absent erased the creation
/// date on the next rewrite.
#[derive(Layout, Debug, Clone, Copy, PartialEq)]
#[layout(bytes = 5, endian = le)]
pub(crate) struct EntryCreationTime {
    incr_10ms: u8,
    time: u16,
    date: u16,
}

impl From<EntryCreationTime> for Option<PrimitiveDateTime> {
    fn from(value: EntryCreationTime) -> Self {
        (value.date != 0)
            .then(|| dos::decode_datetime(pack(value.date, value.time), value.incr_10ms))
            .flatten()
    }
}

impl From<Option<PrimitiveDateTime>> for EntryCreationTime {
    fn from(value: Option<PrimitiveDateTime>) -> Self {
        let (ts, incr_10ms) = value.map_or((0, 0), dos::encode_datetime);
        let (time, date) = split(ts);
        Self {
            incr_10ms,
            time,
            date,
        }
    }
}

/// Modification stamp: time word + date word (`DIR_WrtTime` / `DIR_WrtDate`),
/// 2-second resolution and always present on a valid entry.
#[derive(Layout, Debug, Clone, Copy, PartialEq)]
#[layout(bytes = 4, endian = le)]
pub(crate) struct EntryModificationTime {
    time: u16,
    date: u16,
}

impl TryFrom<EntryModificationTime> for PrimitiveDateTime {
    type Error = ();
    fn try_from(value: EntryModificationTime) -> Result<Self, Self::Error> {
        dos::decode_datetime(pack(value.date, value.time), 0).ok_or(())
    }
}

impl From<PrimitiveDateTime> for EntryModificationTime {
    fn from(value: PrimitiveDateTime) -> Self {
        let (time, date) = split(dos::encode_datetime(value).0);
        Self { time, date }
    }
}

/// Last-accessed stamp: a date word only (`DIR_LstAccDate`).
#[derive(Layout, Debug, Clone, Copy, PartialEq)]
#[layout(bytes = 2, endian = le)]
pub(crate) struct EntryLastAccessedTime {
    date: u16,
}

impl From<EntryLastAccessedTime> for Option<Date> {
    fn from(value: EntryLastAccessedTime) -> Self {
        (value.date != 0)
            .then(|| dos::decode_date_word(value.date))
            .flatten()
    }
}

impl From<Option<Date>> for EntryLastAccessedTime {
    fn from(value: Option<Date>) -> Self {
        Self {
            date: value.map_or(0, dos::encode_date_word),
        }
    }
}
