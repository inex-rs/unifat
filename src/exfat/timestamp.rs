//! ExFAT directory-entry timestamps: DOS-style packed `u32` (date in
//! the high 16 bits, time in the low 16, 2-second granularity) plus a
//! 10 ms-increment byte on create/modified. UTC-offset bytes are
//! ignored on read and cleared (`OffsetValid` = 0) on write —
//! timestamps are local time, matching the FAT backend.
//!
//! Encoding/decoding is shared with classic FAT via [`crate::codec::time`].
//! The stamp bundle is the crate-wide [`crate::entry_times::EntryTimes`].

pub(super) use crate::codec::time::{decode_date, decode_datetime, encode_date, encode_datetime};
pub(super) use crate::entry_times::EntryTimes;
