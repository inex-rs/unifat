//! Classic FAT directory-entry timestamp fields.
//!
//! FAT packs a date word into the high 16 bits and a time word into the low
//! 16 bits of a DOS timestamp — the same layout ExFAT uses — so all three
//! fields delegate to the shared [`crate::codec::time`] codec instead of
//! carrying their own bit-field types.

use crate::codec::time as dos;

use time::{Date, PrimitiveDateTime};

/// Combine a DOS date word (high half) and time word (low half).
#[inline]
fn pack(date: u16, time: u16) -> u32 {
    (u32::from(date) << 16) | u32::from(time)
}

/// The DOS time word (low half) of a packed timestamp.
#[inline]
fn time_word(ts: u32) -> u16 {
    (ts & 0xFFFF) as u16
}

/// The DOS date word (high half) of a packed timestamp.
#[inline]
fn date_word(ts: u32) -> u16 {
    (ts >> 16) as u16
}

/// Creation stamp: 10 ms-increment byte + time word + date word (`DIR_CrtTimeTenth`
/// / `DIR_CrtTime` / `DIR_CrtDate`). A zero DATE reads back as absent — a
/// zero time word is a legitimate midnight (00:00:00), common from tools
/// that only stamp the date; treating it as absent erased the creation
/// date on the next rewrite.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EntryCreationTime(Option<PrimitiveDateTime>);

impl EntryCreationTime {
    /// Decode from `DIR_CrtTimeTenth` + `DIR_CrtTime` + `DIR_CrtDate`.
    pub(crate) fn decode(incr_10ms: u8, time: u16, date: u16) -> Self {
        let stamp = (date != 0)
            .then(|| dos::decode_datetime(pack(date, time), incr_10ms))
            .flatten();
        Self(stamp)
    }

    /// Encode to `(tenths, time_word, date_word)`.
    pub(crate) fn encode(self) -> (u8, u16, u16) {
        match self.0 {
            Some(dt) => {
                let (ts, incr) = dos::encode_datetime(dt);
                (incr, time_word(ts), date_word(ts))
            }
            None => (0, 0, 0),
        }
    }
}

impl From<EntryCreationTime> for Option<PrimitiveDateTime> {
    fn from(value: EntryCreationTime) -> Self {
        value.0
    }
}

impl From<Option<PrimitiveDateTime>> for EntryCreationTime {
    fn from(value: Option<PrimitiveDateTime>) -> Self {
        Self(value)
    }
}

/// Modification stamp: time word + date word (`DIR_WrtTime` / `DIR_WrtDate`),
/// 2-second resolution and always present on a valid entry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EntryModificationTime(Option<PrimitiveDateTime>);

impl EntryModificationTime {
    /// Decode from `DIR_WrtTime` + `DIR_WrtDate`.
    pub(crate) fn decode(time: u16, date: u16) -> Self {
        Self(dos::decode_datetime(pack(date, time), 0))
    }

    /// Encode to `(time_word, date_word)`.
    pub(crate) fn encode(self) -> (u16, u16) {
        let ts = self.0.map_or(0, |dt| dos::encode_datetime(dt).0);
        (time_word(ts), date_word(ts))
    }
}

impl TryFrom<EntryModificationTime> for PrimitiveDateTime {
    type Error = ();
    fn try_from(value: EntryModificationTime) -> Result<Self, Self::Error> {
        value.0.ok_or(())
    }
}

impl From<PrimitiveDateTime> for EntryModificationTime {
    fn from(value: PrimitiveDateTime) -> Self {
        Self(Some(value))
    }
}

/// Last-accessed stamp: a date word only (`DIR_LstAccDate`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct EntryLastAccessedTime(Option<Date>);

impl EntryLastAccessedTime {
    /// Decode from `DIR_LstAccDate`.
    pub(crate) fn decode(date: u16) -> Self {
        Self((date != 0).then(|| dos::decode_date_word(date)).flatten())
    }

    /// Encode to the date word.
    pub(crate) fn encode(self) -> u16 {
        self.0.map_or(0u16, dos::encode_date_word)
    }
}

impl From<EntryLastAccessedTime> for Option<Date> {
    fn from(value: EntryLastAccessedTime) -> Self {
        value.0
    }
}

impl From<Option<Date>> for EntryLastAccessedTime {
    fn from(value: Option<Date>) -> Self {
        Self(value)
    }
}
