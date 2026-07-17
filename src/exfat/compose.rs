//! Pure entry-set composition helpers (no I/O beyond byte packing).

use alloc::vec;
use alloc::vec::Vec;

use super::timestamp::{EntryTimes, encode_date, encode_datetime};
use super::{ExfatAttributes, entry_set_checksum};
use crate::codec::exfat::consts::EXFAT_ENTRY_SIZE;
use crate::codec::exfat::raw_entry::{ExfatFileEntry, ExfatFileNameEntry, ExfatStreamExtension};

/// Compose a File + Stream Extension + File Name entry set with a valid
/// `SetChecksum`. Pure function; the caller must reject names longer than 255
/// UCS-2 units, supply `name_hash` computed over the up-cased name
/// (`ExfatVfs::name_hash`), and may only set `no_fat_chain` when the
/// file's clusters really were placed contiguously.
#[allow(clippy::too_many_arguments)]
pub(super) fn compose_file_entry_set(
    name: &str,
    name_hash: u16,
    attributes: ExfatAttributes,
    first_cluster: u32,
    data_length: u64,
    no_fat_chain: bool,
    times: EntryTimes,
) -> Vec<u8> {
    // Freshly-created files are fully valid: ValidDataLength == DataLength.
    compose_file_entry_set_full(
        name,
        name_hash,
        attributes,
        first_cluster,
        data_length,
        data_length,
        no_fat_chain,
        times,
    )
}

/// Like [`compose_file_entry_set`] but with an independent
/// `valid_data_length`, as needed when relocating an entry (rename).
#[allow(clippy::too_many_arguments)]
pub(super) fn compose_file_entry_set_full(
    name: &str,
    name_hash: u16,
    attributes: ExfatAttributes,
    first_cluster: u32,
    valid_data_length: u64,
    data_length: u64,
    no_fat_chain: bool,
    times: EntryTimes,
) -> Vec<u8> {
    let name_u16: Vec<u16> = name.encode_utf16().collect();
    let name_entries = name_u16.len().div_ceil(15).max(1);
    let secondary_count = 1 + name_entries; // Stream + N FileName
    let total_entries = 1 + secondary_count;
    let mut set = vec![0u8; total_entries * EXFAT_ENTRY_SIZE];

    let secondary_count_u8 = u8::try_from(secondary_count)
        .expect("callers cap names at 255 UCS-2 units, so at most 18 entries");
    let mut file = ExfatFileEntry::new_zeroed(secondary_count_u8, attributes.bits());
    if let Some(dt) = times.created {
        let (ts, incr) = encode_datetime(dt);
        file.create_timestamp = ts;
        file.create_10ms = incr;
    }
    if let Some(dt) = times.modified {
        let (ts, incr) = encode_datetime(dt);
        file.modified_timestamp = ts;
        file.modified_10ms = incr;
    }
    if let Some(date) = times.accessed {
        file.accessed_timestamp = encode_date(date);
    }
    file.write_into(&mut set[..EXFAT_ENTRY_SIZE]);

    let name_len = u8::try_from(name_u16.len()).expect("callers cap names at 255 UCS-2 units");
    let mut stream = ExfatStreamExtension::new(
        name_len,
        first_cluster,
        valid_data_length,
        data_length,
        no_fat_chain,
    );
    stream.name_hash = name_hash;
    stream.write_into(&mut set[EXFAT_ENTRY_SIZE..EXFAT_ENTRY_SIZE * 2]);

    for (i, chunk) in name_u16.chunks(15).enumerate() {
        let off = EXFAT_ENTRY_SIZE * (2 + i);
        ExfatFileNameEntry::from_units(chunk).write_into(&mut set[off..off + EXFAT_ENTRY_SIZE]);
    }

    // Checksum field must be zero while hashing, then written back.
    set[0x02] = 0;
    set[0x03] = 0;
    let sum = entry_set_checksum(&set);
    set[0x02..0x04].copy_from_slice(&sum.to_le_bytes());
    set
}
