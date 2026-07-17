//! Pure ExFAT on-disk layouts (bytes ↔ structs). No volume state, no paths.
//!
//! Import concrete items from submodules, e.g. [`boot::ExfatBootRecord`],
//! [`consts::EXFAT_ENTRY_SIZE`], [`raw_entry::ExfatFileEntry`].

pub(crate) mod boot;
pub(crate) mod consts;
pub(crate) mod raw_entry;

/// exFAT `NameHash` (spec §7.6.5): rotate-right-by-one + add over each byte
/// of the **up-cased** UTF-16LE file name. Callers must up-case the units
/// through the volume's up-case table before hashing.
pub(crate) fn name_hash(upcased_units: impl Iterator<Item = u16>) -> u16 {
    let mut hash: u16 = 0;
    for unit in upcased_units {
        for byte in unit.to_le_bytes() {
            hash = hash.rotate_right(1).wrapping_add(u16::from(byte));
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::name_hash;

    #[test]
    fn name_hash_known_vectors() {
        // Hand-computed from the spec algorithm:
        // "A" (0x0041): 0x41 -> 0x0041; 0x00 -> rotr(0x0041) = 0x8020.
        assert_eq!(name_hash([0x0041u16].into_iter()), 0x8020);
        // "AB": continue with 0x42 -> rotr(0x8020)+0x42 = 0x4052;
        // 0x00 -> rotr(0x4052) = 0x2029.
        assert_eq!(name_hash([0x0041u16, 0x0042].into_iter()), 0x2029);
        // Hash is over upcased units, so equal inputs hash equal.
        assert_eq!(
            name_hash("FILE.TXT".encode_utf16()),
            name_hash("FILE.TXT".encode_utf16()),
        );
        assert_eq!(name_hash(core::iter::empty()), 0);
    }
}
