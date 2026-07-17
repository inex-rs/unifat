//! Timestamp clock abstraction used to stamp created/modified/accessed.

use core::fmt;

use time::{PrimitiveDateTime, macros::date};

/// The earliest timestamp FAT can represent (1980-01-01).
pub const EPOCH: PrimitiveDateTime = date!(1980 - 01 - 01).midnight();

/// An object that can measure and return the current time.
pub trait Clock: fmt::Debug {
    /// The current date and time in the local timezone, as FAT expects
    /// (<https://learn.microsoft.com/en-us/windows/win32/sysinfo/file-times>).
    fn now(&self) -> PrimitiveDateTime;
}

/// The default [`Clock`]: current local time under `std`, the [`EPOCH`] otherwise.
#[derive(Debug, Default)]
#[allow(missing_copy_implementations)]
pub struct DefaultClock;

impl Clock for DefaultClock {
    fn now(&self) -> PrimitiveDateTime {
        #[cfg(feature = "std")]
        {
            use time::OffsetDateTime;

            // Local TZ lookup can fail on some hosts; fall back to UTC so
            // stamps are still written. Supply a custom `Clock` if you need
            // strict local time or a different failure policy.
            let now_odt = OffsetDateTime::now_local().unwrap_or(OffsetDateTime::now_utc());

            PrimitiveDateTime::new(now_odt.date(), now_odt.time())
        }
        #[cfg(not(feature = "std"))]
        EPOCH
    }
}
