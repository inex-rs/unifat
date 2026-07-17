//! Sector size bounds for classic FAT volumes.

/// The minimum size (in bytes) a sector is allowed to have
pub(crate) const MIN_SECTOR_SIZE: usize = 512;
/// The maximum size (in bytes) a sector is allowed to have
pub(crate) const MAX_SECTOR_SIZE: usize = 4096;

/// Whether `bytes_per_sector` from a BPB is a supported FAT sector size:
/// a power of two within `[MIN_SECTOR_SIZE, MAX_SECTOR_SIZE]` (512, 1024,
/// 2048, or 4096). Mount rejects anything else *before* using the value
/// to index the sector buffer — a crafted BPB must not slice out of bounds.
#[inline]
pub(crate) const fn is_valid_sector_size(bytes_per_sector: u16) -> bool {
    let n = bytes_per_sector as usize;
    n >= MIN_SECTOR_SIZE && n <= MAX_SECTOR_SIZE && n.is_power_of_two()
}

#[cfg(test)]
mod tests {
    use super::is_valid_sector_size;

    #[test]
    fn only_supported_power_of_two_sizes_pass() {
        for ok in [512, 1024, 2048, 4096] {
            assert!(is_valid_sector_size(ok), "{ok} should be valid");
        }
        // Zero (ExFAT VBR), non-powers-of-two, and out-of-range all reject.
        for bad in [0, 256, 513, 1000, 3072, 8192, u16::MAX] {
            assert!(!is_valid_sector_size(bad), "{bad} should be rejected");
        }
    }
}
