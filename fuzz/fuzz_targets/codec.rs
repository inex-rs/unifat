#![no_main]
//! Byte-in round-trip fuzzing of the pure codec layer (selector byte + payload).

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&sel, payload)) = data.split_first() else {
        return;
    };
    match sel % 5 {
        0 => unifat::fuzz::fat_bpb(payload),
        1 => unifat::fuzz::fat_dir_entry(payload),
        2 => unifat::fuzz::fat_lfn(payload),
        3 => unifat::fuzz::exfat_boot(payload),
        _ => unifat::fuzz::exfat_entries(payload),
    }
});
