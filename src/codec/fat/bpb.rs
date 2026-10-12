//! Pure FAT boot sector / BPB / FSInfo layouts (bytes ↔ structs).
//!
//! No volume I/O — mount code in `crate::fat` drives decoding.

use crate::codec::fat::DIRENTRY_SIZE;
use crate::codec::fat::sector::MIN_SECTOR_SIZE;
use crate::codec::fat::types::{ClusterCount, FatType, SectorCount, SectorIndex};
use layline::Layout;

pub(crate) const BOOT_SIGNATURE: u8 = 0x29;
// The boot-sector signature is the bytes `55 AA` at offset 510..512.
// Decoded little-endian that is `0xAA55`, NOT `0x55AA` — getting this
// wrong once made signature validation a silent no-op.
pub(crate) const FAT_SIGNATURE: u16 = 0xAA55;

#[derive(Debug, Clone)]
pub(crate) struct FatBootRecord {
    pub bpb: FatBpb,
    pub ebr: Ebr,
    /// The boot-sector signature word (bytes 510..512).
    pub signature: u16,
}

impl FatBootRecord {
    #[inline]
    pub(crate) fn verify_signature(&self) -> bool {
        let boot_signature = match &self.ebr {
            Ebr::Fat16(ebr_fat16) => ebr_fat16.boot_signature,
            Ebr::Fat32(ebr_fat32, _) => ebr_fat32.boot_signature,
        };
        boot_signature == BOOT_SIGNATURE && self.signature == FAT_SIGNATURE
    }

    #[inline]
    /// Total sectors in the volume, including the VBR
    pub(crate) fn total_sectors(&self) -> SectorCount {
        if self.bpb.total_sectors_16 == 0 {
            self.bpb.total_sectors_32
        } else {
            self.bpb.total_sectors_16.into()
        }
    }

    #[inline]
    /// FAT size in sectors
    pub(crate) fn fat_sector_size(&self) -> u32 {
        match &self.ebr {
            Ebr::Fat16(_ebr_fat16) => self.bpb.table_size_16.into(),
            Ebr::Fat32(ebr_fat32, _) => ebr_fat32.table_size_32,
        }
    }

    #[inline]
    /// Sectors occupied by the root directory, rounded up (0 on FAT32).
    ///
    /// All arithmetic is overflow-safe on adversarial BPB fields: the
    /// byte count is widened to `u32` (`root_entry_count * 32` overflows
    /// `u16`), a zero `bytes_per_sector` yields 0 (rejected downstream),
    /// and the result saturates back into `u16`.
    pub(crate) fn root_dir_sectors(&self) -> u16 {
        let entry_bytes = u32::try_from(DIRENTRY_SIZE).expect("DIRENTRY_SIZE (32) fits u32");
        let bytes = u32::from(self.bpb.root_entry_count) * entry_bytes;
        let per_sector = u32::from(self.bpb.bytes_per_sector);
        if per_sector == 0 {
            return 0;
        }
        u16::try_from(bytes.div_ceil(per_sector)).unwrap_or(u16::MAX)
    }

    #[inline]
    /// The first sector in the File Allocation Table
    pub(crate) fn first_fat_sector(&self) -> u16 {
        self.bpb.reserved_sector_count
    }

    #[inline]
    /// The first sector of the root directory (returns the first data
    /// sector on FAT32). Saturating: adversarial `table_count` /
    /// `table_size` cannot overflow the `u32` sector index.
    pub(crate) fn first_root_dir_sector(&self) -> SectorIndex {
        SectorIndex::from(self.first_fat_sector()).saturating_add(
            SectorIndex::from(self.bpb.table_count).saturating_mul(self.fat_sector_size()),
        )
    }

    #[inline]
    /// The first data sector (that is, the first sector in which
    /// directories and files may be stored). Saturating.
    pub(crate) fn first_data_sector(&self) -> SectorIndex {
        self.first_root_dir_sector()
            .saturating_add(SectorIndex::from(self.root_dir_sectors()))
    }

    #[inline]
    /// The total number of data sectors (spec `DataSec`). Saturating: a
    /// `first_data_sector` past `total_sectors` (crafted BPB) yields 0
    /// rather than underflowing.
    pub(crate) fn total_data_sectors(&self) -> SectorCount {
        self.total_sectors()
            .saturating_sub(SectorCount::from(self.first_data_sector()))
    }

    #[inline]
    /// The spec's `CountofClusters` — the number of data clusters. This
    /// drives FAT-type classification; the highest *valid cluster index*
    /// is `CountofClusters + 1` ([`Self::max_cluster`]), since numbering
    /// starts at 2. A zero `sectors_per_cluster` (invalid) yields 0 —
    /// classified as FAT12 and rejected at mount.
    pub(crate) fn total_clusters(&self) -> ClusterCount {
        let per_cluster = ClusterCount::from(self.bpb.sectors_per_cluster);
        if per_cluster == 0 {
            return 0;
        }
        self.total_data_sectors() / per_cluster
    }

    #[inline]
    /// Highest valid data-cluster index (`CountofClusters + 1`): cluster
    /// numbering starts at 2, so a volume with N clusters spans `2..=N+1`.
    pub(crate) fn max_cluster(&self) -> ClusterCount {
        self.total_clusters().saturating_add(1)
    }

    #[inline]
    /// The FAT type, by the spec's cluster-count thresholds. `None` for
    /// FAT12 (< 4085 clusters, unsupported) or a zero `bytes_per_sector`
    /// (an ExFAT VBR, which the mount path catches earlier).
    pub(crate) fn fat_type(&self) -> Option<FatType> {
        if self.bpb.bytes_per_sector == 0 {
            return None;
        }
        match self.total_clusters() {
            0..4085 => None, // FAT12 — unsupported
            4085..65525 => Some(FatType::FAT16),
            _ => Some(FatType::FAT32),
        }
    }
}

pub(crate) const FAT_BPB_SIZE: usize = <FatBpb as Layout>::WIRE_BYTES;
#[derive(Layout, Debug, Clone)]
#[layout(bytes = 36, endian = le)]
pub(crate) struct FatBpb {
    pub _jmpboot: [u8; 3],
    pub _oem_identifier: [u8; 8],
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sector_count: u16,
    pub table_count: u8,
    pub root_entry_count: u16,
    // If this is 0, check `total_sectors_32`
    pub total_sectors_16: u16,
    pub _media_type: u8,
    pub table_size_16: u16,
    pub _sectors_per_track: u16,
    pub _head_side_count: u16,
    pub _hidden_sector_count: u32,
    pub total_sectors_32: u32,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Ebr {
    Fat16(Fat16Ebr),
    Fat32(Fat32Ebr, Fat32FsInfo),
}

// The EBR records cover only the extended BPB fields after [`FatBpb`];
// the boot code up to the signature is opaque and never decoded (it
// would also be ~450 bytes held per mounted volume).

/// Byte offset of the boot-sector signature word.
pub(crate) const SIGNATURE_OFFSET: usize = MIN_SECTOR_SIZE - 2;

/// The boot-sector signature word (`55 AA` at bytes 510..512).
#[derive(Layout, Debug, Clone, Copy)]
#[layout(bytes = 2, endian = le)]
pub(crate) struct BootSectorSignature {
    pub signature: u16,
}

/// The spec's shared FAT12/16 extended BPB (bytes 36..62; FAT12 itself
/// is rejected at mount).
#[derive(Layout, Debug, Clone)]
#[layout(bytes = 26, endian = le)]
pub(crate) struct Fat16Ebr {
    pub _drive_num: u8,
    pub _windows_nt_flags: u8,
    pub boot_signature: u8,
    pub volume_serial_num: u32,
    pub volume_label: [u8; 11],
    pub _system_identifier: [u8; 8],
}

/// FAT32 BPB extended flags (`BPB_ExtFlags`): `active_fat` = bits 0–3,
/// `mirroring_disabled` = bit 7, the rest reserved (preserved on
/// round-trips).
#[derive(Layout, Debug, Clone, Copy, PartialEq, Eq)]
#[layout(bits = 16, endian = le)]
pub(crate) struct Fat32ExtendedFlags {
    #[bits(4)]
    pub(crate) active_fat: u8,
    #[bits(3)]
    _reserved_low: u8,
    #[bits(1)]
    pub(crate) mirroring_disabled: bool,
    #[bits(8)]
    _reserved_high: u8,
}

/// `BPB_FSVer`: minor at offset 42, major at 43 (Microsoft FAT32 layout).
#[derive(Layout, Debug, Clone, PartialEq)]
#[layout(bytes = 2)]
pub(crate) struct FatVersion {
    minor: u8,
    major: u8,
}

/// The FAT32 extended BPB (bytes 36..90).
#[derive(Layout, Debug, Clone)]
#[layout(bytes = 54, endian = le)]
pub(crate) struct Fat32Ebr {
    pub table_size_32: u32,
    #[bytes(2)]
    pub extended_flags: Fat32ExtendedFlags,
    #[bytes(2)]
    pub fat_version: FatVersion,
    pub root_cluster: u32,
    pub fat_info: u16,
    pub backup_boot_sector: u16,
    pub _reserved: [u8; 12],
    pub _drive_num: u8,
    pub _windows_nt_flags: u8,
    pub boot_signature: u8,
    pub volume_serial_num: u32,
    pub volume_label: [u8; 11],
    pub _system_ident: [u8; 8],
}

/// FAT32 FSInfo lead signature (bytes 0..4 of the FSInfo sector).
#[derive(Layout, Debug, Clone, Copy)]
#[layout(bytes = 4, endian = le)]
pub(crate) struct Fat32FsInfoLead {
    #[magic(0x41615252u32)]
    pub lead_signature: u32,
}

impl Fat32FsInfoLead {
    pub(crate) const VALID: Self = Self {
        lead_signature: 0x41615252,
    };
}

/// Byte offset of [`Fat32FsInfo`] within the FSInfo sector. The 480
/// reserved bytes before it are never decoded or rewritten.
pub(crate) const FSINFO_OFFSET: usize = 484;

/// FAT32 FSInfo body (bytes 484..512): the free-cluster hints between the
/// structure and trail signatures. The signatures are `#[magic]`: decode
/// fails on a scribbled sector, and encode always writes them.
#[derive(Layout, Debug, Clone)]
#[layout(bytes = 28, endian = le)]
pub(crate) struct Fat32FsInfo {
    #[magic(0x61417272u32)]
    pub mid_signature: u32,
    pub free_cluster_count: u32,
    pub first_free_cluster: u32,
    pub _reserved2: [u8; 12],
    #[magic(0xAA550000u32)]
    pub trail_signature: u32,
}

impl Fat32FsInfo {
    /// A valid FSInfo with both hint fields "unknown" (`u32::MAX`).
    /// Used when the on-disk sector is corrupt: FSInfo is advisory, so a
    /// scribbled copy must not fail the mount — and syncing this back
    /// repairs it.
    pub(crate) fn unknown() -> Self {
        Self {
            mid_signature: 0x61417272,
            free_cluster_count: u32::MAX,
            first_free_cluster: u32::MAX,
            _reserved2: [0; 12],
            trail_signature: 0xAA550000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The on-disk boot-sector signature is the bytes `55 AA`, which
    /// decode little-endian into `0xAA55`. A regression here (e.g. back
    /// to `0x55AA`) silently disables signature validation.
    #[test]
    fn fat_signature_constant_is_little_endian_decoded() {
        assert_eq!(FAT_SIGNATURE, 0xAA55);
        assert_eq!(
            u16::from_le_bytes([0x55, 0xAA]),
            FAT_SIGNATURE,
            "FAT_SIGNATURE must equal the LE decode of the on-disk `55 AA`",
        );
    }

    /// `CountofClusters` must follow the spec exactly: `DataSec / SPC`
    /// with `DataSec = TotSec - FirstDataSec` (no phantom `+1` sector).
    /// A one-off inflates the count when `DataSec % SPC == SPC - 1`,
    /// which (a) made the last valid cluster unreachable and (b)
    /// misclassified volumes at the FAT12/16/32 thresholds — a 65,524-
    /// cluster FAT16 volume read as FAT32 means 16-bit FAT cells decoded
    /// as 32-bit garbage, with writes enabled.
    #[test]
    fn cluster_count_is_spec_exact_at_type_boundaries() {
        fn boot32(sectors_per_cluster: u8, total_sectors_32: u32) -> FatBootRecord {
            FatBootRecord {
                bpb: FatBpb {
                    _jmpboot: [0; 3],
                    _oem_identifier: [0; 8],
                    bytes_per_sector: 512,
                    sectors_per_cluster,
                    reserved_sector_count: 1,
                    table_count: 2,
                    root_entry_count: 32, // 2 root-dir sectors
                    total_sectors_16: 0,
                    _media_type: 0,
                    table_size_16: 16,
                    _sectors_per_track: 0,
                    _head_side_count: 0,
                    _hidden_sector_count: 0,
                    total_sectors_32,
                },
                ebr: Ebr::Fat16(Fat16Ebr {
                    _drive_num: 0,
                    _windows_nt_flags: 0,
                    boot_signature: BOOT_SIGNATURE,
                    volume_serial_num: 0,
                    volume_label: [0; 11],
                    _system_identifier: [0; 8],
                }),
                signature: FAT_SIGNATURE,
            }
        }
        // FirstDataSec = 1 (reserved) + 2*16 (FATs) + 2 (root) = 35.
        let first_data = 35u32;

        // SPC=2, DataSec = 2*10_000 + 1: the old `+1` inflated this to
        // 10_001 clusters. Spec: 10_000, max valid cluster index 10_001.
        let br = boot32(2, first_data + 2 * 10_000 + 1);
        assert_eq!(br.total_clusters(), 10_000);
        assert_eq!(br.max_cluster(), 10_001);

        // FAT16/FAT32 threshold: exactly 65,524 clusters is FAT16; the
        // inflated count read 65,525 → FAT32.
        let br = boot32(2, first_data + 2 * 65_524 + 1);
        assert_eq!(br.total_clusters(), 65_524);
        assert_eq!(br.fat_type(), Some(FatType::FAT16));

        // FAT12/FAT16 threshold: 4,084 clusters is FAT12 → rejected; the
        // inflated count read 4,085 → mounted as FAT16.
        let br = boot32(2, first_data + 2 * 4_084 + 1);
        assert_eq!(br.total_clusters(), 4_084);
        assert_eq!(br.fat_type(), None, "FAT12 must be rejected");
    }

    /// A crafted BPB with maxed-out geometry fields must not panic when a
    /// mount computes cluster counts (fuzz-found: overflow in
    /// `total_clusters` / `first_data_sector`, div-by-zero on a zero
    /// `bytes_per_sector` / `sectors_per_cluster`). Every helper must
    /// return a bounded value instead of overflowing.
    #[test]
    fn adversarial_bpb_geometry_never_panics() {
        fn boot(
            bytes_per_sector: u16,
            sectors_per_cluster: u8,
            root_entry_count: u16,
            table_count: u8,
            table_size_16: u16,
            reserved_sector_count: u16,
            total_sectors_16: u16,
        ) -> FatBootRecord {
            FatBootRecord {
                bpb: FatBpb {
                    _jmpboot: [0; 3],
                    _oem_identifier: [0; 8],
                    bytes_per_sector,
                    sectors_per_cluster,
                    reserved_sector_count,
                    table_count,
                    root_entry_count,
                    total_sectors_16,
                    _media_type: 0,
                    table_size_16,
                    _sectors_per_track: 0,
                    _head_side_count: 0,
                    _hidden_sector_count: 0,
                    total_sectors_32: 0,
                },
                ebr: Ebr::Fat16(Fat16Ebr {
                    _drive_num: 0,
                    _windows_nt_flags: 0,
                    boot_signature: 0,
                    volume_serial_num: 0,
                    volume_label: [0; 11],
                    _system_identifier: [0; 8],
                }),
                signature: FAT_SIGNATURE,
            }
        }

        for br in [
            // All fields maxed — every multiply/add would overflow `u32`.
            boot(
                u16::MAX,
                u8::MAX,
                u16::MAX,
                u8::MAX,
                u16::MAX,
                u16::MAX,
                u16::MAX,
            ),
            // Zero divisors on both division paths.
            boot(0, 0, u16::MAX, 1, 1, 1, 1),
            // first_data_sector far past total_sectors → sub would underflow.
            boot(512, 1, u16::MAX, u8::MAX, u16::MAX, u16::MAX, 1),
        ] {
            // None of these may panic; values are just bounded.
            let _ = br.root_dir_sectors();
            let _ = br.first_data_sector();
            let _ = br.total_data_sectors();
            let _ = br.total_clusters();
            let _ = br.fat_type();
        }
    }
}
