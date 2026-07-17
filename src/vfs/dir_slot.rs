//! Directory-entry metadata write-back for open streams.

use crate::vfs::directory::EntryPatch;

/// Writes size / times / cluster fields back to a directory entry set.
pub(crate) trait DirSlotWriter {
    type Error;

    fn write_patch(&mut self, patch: EntryPatch) -> Result<(), Self::Error>;
}
