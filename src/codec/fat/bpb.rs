//! Pure FAT boot sector / BPB / FSInfo layouts (bytes ↔ structs).
//!
//! No volume I/O — mount code in `crate::fat` drives decoding.

use crate::codec::fat::DIRENTRY_SIZE;
use crate::codec::fat::sector::MIN_SECTOR_SIZE;
use crate::codec::fat::types::{ClusterCount, FatType, SectorCount, SectorIndex};
use crate::codec::{FixedCodec, bytes, le_u16, le_u32, put_u16, put_u32};

pub(crate) const BOOT_SIGNATURE: u8 = 0x29;
// The boot-sector signature is the bytes `55 AA` at offset 510..512.
// Decoded little-endian that is `0xAA55`, NOT `0x55AA` — getting this
// wrong once made signature validation a silent no-op.
pub(crate) const FAT_SIGNATURE: u16 = 0xAA55;

#[derive(Debug, Clone)]
pub(crate) struct FatBootRecord {
    pub bpb: FatBpb,
    pub ebr: Ebr,
}

impl FatBootRecord {
    #[inline]
    pub(crate) fn verify_signature(&self) -> bool {
        match &self.ebr {
            Ebr::Fat16(ebr_fat16) => {
                ebr_fat16.boot_signature == BOOT_SIGNATURE && ebr_fat16.signature == FAT_SIGNATURE
            }
            Ebr::Fat32(ebr_fat32, _) => {
                ebr_fat32.boot_signature == BOOT_SIGNATURE && ebr_fat32.signature == FAT_SIGNATURE
            }
        }
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

pub(crate) const FAT_BPB_SIZE: usize = 36;
#[derive(Debug, Clone)]
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

impl FixedCodec for FatBpb {
    const SIZE: usize = FAT_BPB_SIZE;

    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            _jmpboot: bytes(b, 0),
            _oem_identifier: bytes(b, 3),
            bytes_per_sector: le_u16(b, 11),
            sectors_per_cluster: b[13],
            reserved_sector_count: le_u16(b, 14),
            table_count: b[16],
            root_entry_count: le_u16(b, 17),
            total_sectors_16: le_u16(b, 19),
            _media_type: b[21],
            table_size_16: le_u16(b, 22),
            _sectors_per_track: le_u16(b, 24),
            _head_side_count: le_u16(b, 26),
            _hidden_sector_count: le_u32(b, 28),
            total_sectors_32: le_u32(b, 32),
        })
    }

    fn write_into(&self, out: &mut [u8]) {
        let b = &mut out[..Self::SIZE];
        b[0..3].copy_from_slice(&self._jmpboot);
        b[3..11].copy_from_slice(&self._oem_identifier);
        put_u16(b, 11, self.bytes_per_sector);
        b[13] = self.sectors_per_cluster;
        put_u16(b, 14, self.reserved_sector_count);
        b[16] = self.table_count;
        put_u16(b, 17, self.root_entry_count);
        put_u16(b, 19, self.total_sectors_16);
        b[21] = self._media_type;
        put_u16(b, 22, self.table_size_16);
        put_u16(b, 24, self._sectors_per_track);
        put_u16(b, 26, self._head_side_count);
        put_u32(b, 28, self._hidden_sector_count);
        put_u32(b, 32, self.total_sectors_32);
    }
}

pub(crate) const EBR_SIZE: usize = MIN_SECTOR_SIZE - FAT_BPB_SIZE;
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Ebr {
    Fat16(Fat16Ebr),
    Fat32(Fat32Ebr, Fat32FsInfo),
}

/// The spec's shared FAT12/16 EBR layout (FAT12 itself is rejected at
/// mount).
#[derive(Debug, Clone)]
pub(crate) struct Fat16Ebr {
    pub _drive_num: u8,
    pub _windows_nt_flags: u8,
    pub boot_signature: u8,
    pub volume_serial_num: u32,
    pub volume_label: [u8; 11],
    pub _system_identifier: [u8; 8],
    pub _boot_code: [u8; 448],
    pub signature: u16,
}

/// FAT32 BPB extended flags (`BPB_ExtFlags`).
///
/// Bit layout of the little-endian `u16`:
/// `active_fat` = bits 0–3, `mirroring_disabled` = bit 7, rest reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fat32ExtendedFlags {
    pub(crate) active_fat: u8,
    pub(crate) mirroring_disabled: bool,
    /// Every bit we don't interpret, preserved for round-trips.
    reserved: u16,
}

impl Fat32ExtendedFlags {
    const ACTIVE_FAT_MASK: u16 = 0x000F;
    const MIRRORING_DISABLED: u16 = 0x0080;

    fn decode(raw: u16) -> Self {
        Self {
            #[allow(clippy::cast_possible_truncation)] // masked to 4 bits
            active_fat: (raw & Self::ACTIVE_FAT_MASK) as u8,
            mirroring_disabled: raw & Self::MIRRORING_DISABLED != 0,
            reserved: raw & !(Self::ACTIVE_FAT_MASK | Self::MIRRORING_DISABLED),
        }
    }

    fn encode(self) -> u16 {
        u16::from(self.active_fat & 0x0F)
            | if self.mirroring_disabled {
                Self::MIRRORING_DISABLED
            } else {
                0
            }
            | self.reserved
    }
}

/// `BPB_FSVer`: minor at offset 42, major at 43 (Microsoft FAT32 layout).
#[derive(Debug, Clone)]
pub(crate) struct FatVersion {
    minor: u8,
    major: u8,
}

#[derive(Debug, Clone)]
pub(crate) struct Fat32Ebr {
    pub table_size_32: u32,
    pub extended_flags: Fat32ExtendedFlags,
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
    pub _boot_code: [u8; 420],
    pub signature: u16,
}

pub(crate) const FSINFO_SIZE: usize = 512;
const FSINFO_LEAD_SIGNATURE: u32 = 0x41615252;
const FSINFO_MID_SIGNATURE: u32 = 0x61417272;
const FSINFO_TRAIL_SIGNATURE: u32 = 0xAA550000;
#[derive(Debug, Clone)]
pub(crate) struct Fat32FsInfo {
    pub lead_signature: u32,
    pub _reserved1: [u8; 480],
    pub mid_signature: u32,
    pub free_cluster_count: u32,
    pub first_free_cluster: u32,
    pub _reserved2: [u8; 12],
    pub trail_signature: u32,
}

impl Fat32FsInfo {
    pub(crate) fn verify_signature(&self) -> bool {
        self.lead_signature == FSINFO_LEAD_SIGNATURE
            && self.mid_signature == FSINFO_MID_SIGNATURE
            && self.trail_signature == FSINFO_TRAIL_SIGNATURE
    }

    /// A valid FSInfo with both hint fields "unknown" (`u32::MAX`).
    /// Used when the on-disk sector is corrupt: FSInfo is advisory, so a
    /// scribbled copy must not fail the mount — and syncing this back
    /// repairs it.
    pub(crate) fn unknown() -> Self {
        Self {
            lead_signature: FSINFO_LEAD_SIGNATURE,
            _reserved1: [0; 480],
            mid_signature: FSINFO_MID_SIGNATURE,
            free_cluster_count: u32::MAX,
            first_free_cluster: u32::MAX,
            _reserved2: [0; 12],
            trail_signature: FSINFO_TRAIL_SIGNATURE,
        }
    }
}

impl FixedCodec for Fat16Ebr {
    const SIZE: usize = EBR_SIZE;

    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            _drive_num: b[0],
            _windows_nt_flags: b[1],
            boot_signature: b[2],
            volume_serial_num: le_u32(b, 3),
            volume_label: bytes(b, 7),
            _system_identifier: bytes(b, 18),
            _boot_code: bytes(b, 26),
            signature: le_u16(b, 474),
        })
    }

    fn write_into(&self, out: &mut [u8]) {
        let b = &mut out[..Self::SIZE];
        b[0] = self._drive_num;
        b[1] = self._windows_nt_flags;
        b[2] = self.boot_signature;
        put_u32(b, 3, self.volume_serial_num);
        b[7..18].copy_from_slice(&self.volume_label);
        b[18..26].copy_from_slice(&self._system_identifier);
        b[26..474].copy_from_slice(&self._boot_code);
        put_u16(b, 474, self.signature);
    }
}

impl FixedCodec for Fat32Ebr {
    const SIZE: usize = EBR_SIZE;

    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            table_size_32: le_u32(b, 0),
            extended_flags: Fat32ExtendedFlags::decode(le_u16(b, 4)),
            fat_version: FatVersion {
                minor: b[6],
                major: b[7],
            },
            root_cluster: le_u32(b, 8),
            fat_info: le_u16(b, 12),
            backup_boot_sector: le_u16(b, 14),
            _reserved: bytes(b, 16),
            _drive_num: b[28],
            _windows_nt_flags: b[29],
            boot_signature: b[30],
            volume_serial_num: le_u32(b, 31),
            volume_label: bytes(b, 35),
            _system_ident: bytes(b, 46),
            _boot_code: bytes(b, 54),
            signature: le_u16(b, 474),
        })
    }

    fn write_into(&self, out: &mut [u8]) {
        let b = &mut out[..Self::SIZE];
        put_u32(b, 0, self.table_size_32);
        put_u16(b, 4, self.extended_flags.encode());
        b[6] = self.fat_version.minor;
        b[7] = self.fat_version.major;
        put_u32(b, 8, self.root_cluster);
        put_u16(b, 12, self.fat_info);
        put_u16(b, 14, self.backup_boot_sector);
        b[16..28].copy_from_slice(&self._reserved);
        b[28] = self._drive_num;
        b[29] = self._windows_nt_flags;
        b[30] = self.boot_signature;
        put_u32(b, 31, self.volume_serial_num);
        b[35..46].copy_from_slice(&self.volume_label);
        b[46..54].copy_from_slice(&self._system_ident);
        b[54..474].copy_from_slice(&self._boot_code);
        put_u16(b, 474, self.signature);
    }
}

impl FixedCodec for Fat32FsInfo {
    const SIZE: usize = FSINFO_SIZE;

    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            lead_signature: le_u32(b, 0),
            _reserved1: bytes(b, 4),
            mid_signature: le_u32(b, 484),
            free_cluster_count: le_u32(b, 488),
            first_free_cluster: le_u32(b, 492),
            _reserved2: bytes(b, 496),
            trail_signature: le_u32(b, 508),
        })
    }

    fn write_into(&self, out: &mut [u8]) {
        let b = &mut out[..Self::SIZE];
        put_u32(b, 0, self.lead_signature);
        b[4..484].copy_from_slice(&self._reserved1);
        put_u32(b, 484, self.mid_signature);
        put_u32(b, 488, self.free_cluster_count);
        put_u32(b, 492, self.first_free_cluster);
        b[496..508].copy_from_slice(&self._reserved2);
        put_u32(b, 508, self.trail_signature);
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
                    _boot_code: [0; 448],
                    signature: FAT_SIGNATURE,
                }),
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
                    _boot_code: [0; 448],
                    signature: FAT_SIGNATURE,
                }),
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
