//! Pure entry-set composition helpers (no I/O beyond byte packing).

use alloc::vec::Vec;

use layline::Message;

use super::ExfatAttributes;
use super::timestamp::{EntryTimes, encode_date, encode_datetime};
use crate::codec::exfat::{entry_set::ExfatEntrySet, raw_entry::ExfatStreamExtension};

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
    let name_len = u8::try_from(name_u16.len()).expect("callers cap names at 255 UCS-2 units");
    let mut stream = ExfatStreamExtension::new(
        name_len,
        first_cluster,
        valid_data_length,
        data_length,
        no_fat_chain,
    );
    stream.name_hash = name_hash;

    let mut set = ExfatEntrySet::new(&name_u16, attributes.bits(), stream);
    if let Some(dt) = times.created {
        let (ts, incr) = encode_datetime(dt);
        set.create_timestamp = ts;
        set.create_10ms = incr;
    }
    if let Some(dt) = times.modified {
        let (ts, incr) = encode_datetime(dt);
        set.modified_timestamp = ts;
        set.modified_10ms = incr;
    }
    if let Some(date) = times.accessed {
        set.accessed_timestamp = encode_date(date);
    }
    // Encode stamps SecondaryCount and SetChecksum.
    set.encode()
}
