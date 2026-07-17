//! Boot-sector sniffing shared by the mount dispatch and the FAT backend.

/// ExFAT OEM identifier at bytes 3..=10 of the Volume Boot Record. The
/// ExFAT BPB overlaps FAT's in problematic ways, so this ASCII magic is
/// the reliable up-front discriminator.
pub(crate) const EXFAT_OEM_IDENTIFIER: &[u8; 8] = b"EXFAT   ";

/// Whether `buf` (the first 11+ bytes of the VBR) carries the ExFAT OEM magic.
#[inline]
pub(crate) fn has_exfat_magic(buf: &[u8]) -> bool {
    buf.len() >= 11 && &buf[3..11] == EXFAT_OEM_IDENTIFIER
}
