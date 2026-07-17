//! Shared DOS-packed date/time codec (pure; no I/O).
//!
//! On disk the layout is little-endian:
//! - **time** `u16`: `seconds/2` (5) | `minutes` (6) | `hour` (5)
//! - **date** `u16`: `day` (5) | `month` (4) | `year - 1980` (7)
//!
//! ExFAT packs date in the high half of a `u32` and time in the low half,
//! plus a 10 ms-increment byte for create/modified.

use time::{Date, Month, PrimitiveDateTime, Time};

/// FAT/ExFAT epoch — packed years count from 1980.
pub(crate) const EPOCH_YEAR: i32 = 1980;

/// Decode a packed DOS time word into a [`Time`] (even seconds only).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn decode_time(bits: u16) -> Option<Time> {
    let two_seconds = (bits & 0x1F) as u8;
    let minute = ((bits >> 5) & 0x3F) as u8;
    let hour = ((bits >> 11) & 0x1F) as u8;
    Time::from_hms(hour, minute, two_seconds.saturating_mul(2)).ok()
}

/// Decode a packed DOS date word into a [`Date`].
pub(crate) fn decode_date_word(bits: u16) -> Option<Date> {
    let day = (bits & 0x1F) as u8;
    let month = ((bits >> 5) & 0x0F) as u8;
    let year = EPOCH_YEAR + i32::from((bits >> 9) & 0x7F);
    Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()
}

/// Encode a [`Time`] as a DOS time word (2-second resolution).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encode_time(time: Time) -> u16 {
    let hour = u16::from(time.hour());
    let minute = u16::from(time.minute());
    let two_seconds = u16::from(time.second() / 2);
    (hour << 11) | (minute << 5) | two_seconds
}

/// Encode a [`Date`] as a DOS date word. Years outside 1980..=2107 are clamped.
pub(crate) fn encode_date_word(date: Date) -> u16 {
    let year = u16::try_from((date.year() - EPOCH_YEAR).clamp(0, 127)).unwrap_or(0);
    let month = u16::from(u8::from(date.month()));
    let day = u16::from(date.day());
    (year << 9) | (month << 5) | day
}

/// Decode a packed ExFAT-style timestamp (`0` = "no timestamp") plus its
/// 10 ms increment. Every extracted field is masked to ≤ 8 bits.
#[allow(clippy::cast_possible_truncation)]
pub(crate) fn decode_datetime(ts: u32, incr_10ms: u8) -> Option<PrimitiveDateTime> {
    if ts == 0 {
        return None;
    }
    let two_seconds = (ts & 0x1F) as u8;
    let minute = ((ts >> 5) & 0x3F) as u8;
    let hour = ((ts >> 11) & 0x1F) as u8;
    let day = ((ts >> 16) & 0x1F) as u8;
    let month = ((ts >> 21) & 0x0F) as u8;
    let year = EPOCH_YEAR + i32::from(((ts >> 25) & 0x7F) as u8);

    // The increment (0..=199) supplies the odd second plus centiseconds.
    let second = two_seconds * 2 + incr_10ms / 100;
    let milli = u16::from(incr_10ms % 100) * 10;

    let date = Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
    let clock = Time::from_hms_milli(hour, minute, second, milli).ok()?;
    Some(PrimitiveDateTime::new(date, clock))
}

/// Decode only the date portion of a packed `u32` timestamp.
pub(crate) fn decode_date(ts: u32) -> Option<Date> {
    decode_datetime(ts, 0).map(PrimitiveDateTime::date)
}

/// Encode a datetime into `(packed_timestamp, incr_10ms)`. Years outside
/// the representable 1980..=2107 window are clamped.
pub(crate) fn encode_datetime(dt: PrimitiveDateTime) -> (u32, u8) {
    let year = u32::try_from((dt.year() - EPOCH_YEAR).clamp(0, 127)).unwrap_or(0);
    let month = u32::from(u8::from(dt.month()));
    let day = u32::from(dt.day());
    let hour = u32::from(dt.hour());
    let minute = u32::from(dt.minute());
    let second = u32::from(dt.second());

    let ts =
        (year << 25) | (month << 21) | (day << 16) | (hour << 11) | (minute << 5) | (second / 2);
    // Odd second (0/1 → 0/100) plus centiseconds; always ≤ 199, fits u8.
    let incr = u8::try_from((second % 2) * 100 + u32::from(dt.millisecond()) / 10).unwrap_or(0);
    (ts, incr)
}

/// Encode a date at midnight (LastAccessed has no increment byte).
pub(crate) fn encode_date(date: Date) -> u32 {
    encode_datetime(PrimitiveDateTime::new(date, Time::MIDNIGHT)).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    #[test]
    fn datetime_round_trips() {
        let dt = datetime!(2021-07-08 13:37:42.500);
        let (ts, incr) = encode_datetime(dt);
        assert_eq!(decode_datetime(ts, incr), Some(dt));
    }

    #[test]
    fn zero_is_none() {
        assert_eq!(decode_datetime(0, 0), None);
    }

    #[test]
    fn date_round_trips() {
        let d = date!(2019 - 03 - 14);
        assert_eq!(decode_date(encode_date(d)), Some(d));
    }

    #[test]
    fn year_clamps_below_epoch() {
        let d = date!(1970 - 01 - 01);
        let bits = encode_date_word(d);
        // year field = 0 → 1980
        assert_eq!(decode_date_word(bits).map(|x| x.year()), Some(1980));
    }

    #[test]
    fn time_date_words_round_trip() {
        let t = Time::from_hms(13, 37, 42).unwrap();
        let d = date!(2021 - 07 - 08);
        // even-second resolution
        assert_eq!(
            decode_time(encode_time(t)),
            Some(Time::from_hms(13, 37, 42).unwrap())
        );
        assert_eq!(decode_date_word(encode_date_word(d)), Some(d));
    }
}
