//! ExFAT entry-set write-back for open streams.

use embedded_io::{Read, Seek, Write};
use time::{Date, PrimitiveDateTime};

use crate::error::FileError;
use crate::exfat::ExfatVfs;
use crate::vfs::{DirSlotWriter, EntryPatch};

use super::direntry::SetLoc;
use super::timestamp::{encode_date, encode_datetime};
use super::{EXFAT_ENTRY_SIZE, STREAM_FLAG_ALLOCATION_POSSIBLE, entry_set_checksum};
use crate::codec::exfat::raw_entry::{ExfatFileEntry, ExfatStreamExtension};

/// Writes a whole File+Stream+Name entry set (with checksum) for one open file.
pub(crate) struct ExfatDirSlotWriter<'a, S>
where
    S: Read + Write + Seek,
{
    vol: &'a ExfatVfs<S>,
    /// Location of the entry set; `None` for read-only handles, whose
    /// patches are no-ops.
    loc: Option<SetLoc>,
    /// Cached timestamps for merge when patch only supplies some fields.
    created: Option<PrimitiveDateTime>,
    modified: Option<PrimitiveDateTime>,
    accessed: Option<Date>,
}

impl<'a, S> ExfatDirSlotWriter<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn new(
        vol: &'a ExfatVfs<S>,
        loc: Option<SetLoc>,
        created: Option<PrimitiveDateTime>,
        modified: Option<PrimitiveDateTime>,
        accessed: Option<Date>,
    ) -> Self {
        Self {
            vol,
            loc,
            created,
            modified,
            accessed,
        }
    }
}

impl<S> DirSlotWriter for ExfatDirSlotWriter<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FileError<S::Error>;

    fn write_patch(&mut self, patch: EntryPatch) -> Result<(), FileError<S::Error>> {
        let Some(loc) = self.loc else {
            return Ok(());
        };
        if let Some(times) = patch.times {
            if times.created.is_some() {
                self.created = times.created;
            }
            if times.modified.is_some() {
                self.modified = times.modified;
            }
            if times.accessed.is_some() {
                self.accessed = times.accessed;
            }
        }

        let mut set = self.vol.read_set(&loc).map_err(FileError::from)?;

        let stream_off = EXFAT_ENTRY_SIZE;
        let mut stream =
            ExfatStreamExtension::parse(&set[stream_off..stream_off + EXFAT_ENTRY_SIZE])
                .ok_or(FileError::Corrupt)?;

        if let Some(nfc) = patch.no_fat_chain {
            stream.set_no_fat_chain(nfc);
        }
        // DataLength IS the file's byte size per spec (Windows shows it);
        // ValidDataLength marks the initialized prefix (VDL ≤ DL).
        if let Some(size) = patch.size {
            stream.data_length = size;
            if size > 0 {
                stream.general_secondary_flags |= STREAM_FLAG_ALLOCATION_POSSIBLE;
            }
        }
        if let Some(valid) = patch.valid_size {
            stream.valid_data_length = valid.min(stream.data_length);
        }
        if let Some(fc) = patch.first_cluster {
            stream.first_cluster = fc;
        }
        stream.write_into(&mut set[stream_off..stream_off + EXFAT_ENTRY_SIZE]);

        let mut file = ExfatFileEntry::parse(&set[..EXFAT_ENTRY_SIZE]).ok_or(FileError::Corrupt)?;
        if let Some(attrs) = patch.attrs {
            // Replace only the four user-settable bits; the directory bit
            // and any reserved high bits are preserved.
            let user_bits = crate::attrs::bits::READ_ONLY
                | crate::attrs::bits::HIDDEN
                | crate::attrs::bits::SYSTEM
                | crate::attrs::bits::ARCHIVE;
            file.attributes = (file.attributes & !user_bits)
                | (crate::attrs::AttrBits::from_attributes(attrs, false).bits() & user_bits);
        }
        // Stamps written by this offset-less driver clear the matching
        // UtcOffset byte (OffsetValid=0): keeping a foreign entry's old
        // offset would declare our local time to be in that zone.
        if let Some(dt) = self.created {
            let (ts, incr) = encode_datetime(dt);
            file.create_timestamp = ts;
            file.create_10ms = incr;
            file.create_utc_offset = 0;
        }
        if let Some(dt) = self.modified {
            let (ts, incr) = encode_datetime(dt);
            file.modified_timestamp = ts;
            file.modified_10ms = incr;
            file.modified_utc_offset = 0;
        }
        if let Some(date) = self.accessed {
            file.accessed_timestamp = encode_date(date);
            file.accessed_utc_offset = 0;
        }
        file.set_checksum = 0;
        file.write_into(&mut set[..EXFAT_ENTRY_SIZE]);

        set[0x02] = 0;
        set[0x03] = 0;
        let sum = entry_set_checksum(&set);
        set[0x02..0x04].copy_from_slice(&sum.to_le_bytes());

        self.vol.write_set(&loc, &set).map_err(FileError::from)?;
        self.vol.sync().map_err(FileError::from)?;
        Ok(())
    }
}
