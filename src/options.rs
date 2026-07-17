//! Mount options shared by both filesystem backends.

#[cfg(not(feature = "std"))]
use alloc::boxed::Box;

use crate::time::{Clock, DefaultClock};

/// Mount options for a [`Volume`](crate::Volume): the [`Clock`] used to
/// stamp file timestamps and whether writes refresh modified/accessed
/// stamps automatically.
///
/// Name equality policy is **not** user-selectable here: classic FAT always
/// uses ASCII case-fold, and ExFAT always loads the volume upcase table
/// (see `NamePolicy` on the private VFS).
#[derive(Debug)]
pub struct FsOptions {
    pub(crate) clock: Box<dyn Clock>,
    pub(crate) auto_timestamps: bool,
}

impl FsOptions {
    #[inline]
    /// Create an options struct with the defaults (alias for [`Self::default`])
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether writes through a handle refresh the modified/accessed
    /// stamps from the [`Clock`]. Defaults to `true` (matching
    /// `std::fs` expectations); disable to reduce directory-entry
    /// writes or when no meaningful clock exists.
    pub fn set_auto_timestamps(&mut self, on: bool) {
        self.auto_timestamps = on;
    }

    /// [`Self::set_auto_timestamps`], chainable.
    pub fn with_auto_timestamps(mut self, on: bool) -> Self {
        self.auto_timestamps = on;
        self
    }

    /// Set the [`Clock`] used to stamp file timestamps. Defaults to
    /// [`DefaultClock`]; supply your own (e.g. RTC-backed) on `no_std` targets.
    pub fn set_clock(&mut self, clock: Box<dyn Clock>) {
        self.clock = clock;
    }

    /// Set the [`Clock`] used to stamp file timestamps (chainable).
    pub fn with_clock(mut self, clock: Box<dyn Clock>) -> Self {
        self.set_clock(clock);
        self
    }
}

impl Default for FsOptions {
    fn default() -> Self {
        Self {
            clock: Box::new(DefaultClock),
            auto_timestamps: true,
        }
    }
}
