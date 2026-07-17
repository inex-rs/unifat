//! Shared create/modify/access stamp bundle used when building new entries.

use time::{Date, PrimitiveDateTime};

use crate::time::Clock;

/// The three timestamps carried by a directory entry on both backends.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct EntryTimes {
    pub created: Option<PrimitiveDateTime>,
    pub modified: Option<PrimitiveDateTime>,
    pub accessed: Option<Date>,
}

impl EntryTimes {
    /// All three stamped to "now" (accessed truncated to a date), for a
    /// freshly-created entry.
    pub(crate) fn now(clock: &dyn Clock) -> Self {
        let now = clock.now();
        EntryTimes {
            created: Some(now),
            modified: Some(now),
            accessed: Some(now.date()),
        }
    }

    /// The modification stamp, defaulting to epoch if somehow absent
    /// (callers always fill via [`Self::now`]).
    #[inline]
    pub(crate) fn modified_or_epoch(self) -> PrimitiveDateTime {
        self.modified.unwrap_or(crate::time::EPOCH)
    }
}
