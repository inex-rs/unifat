//! Pure ExFAT Volume Boot Record layout (bytes ↔ struct).

use crate::codec::{FixedCodec, bytes, le_u16, le_u32, le_u64, put_u16, put_u32, put_u64};

/// Size of the fixed portion of the ExFAT VBR we decode; the rest of
/// the sector holds boot code / CRC fields a mount doesn't need.
pub(crate) const EXFAT_BOOT_RECORD_SIZE: usize = 120;

/// VolumeFlags (bytes 106–107) and PercentInUse (byte 112) are excluded
/// from the boot-region checksum so they can be updated in place.
const CHECKSUM_EXCLUDED: [usize; 3] = [106, 107, 112];

/// One step of the exFAT rolling `u32` checksum: rotate right by one,
/// then add the byte. Used for both the boot-region and up-case-table
/// checksums.
#[inline]
fn roll(checksum: u32, byte: u8) -> u32 {
    ((checksum & 1) << 31)
        .wrapping_add(checksum >> 1)
        .wrapping_add(u32::from(byte))
}

/// The exFAT Main Boot Checksum over the first 11 sectors of the boot
/// region (`region` must be exactly `11 * sector_bytes` long), skipping
/// the three mutable bytes. The 12th sector on disk is this value
/// repeated to fill the sector.
pub(crate) fn boot_region_checksum(region: &[u8]) -> u32 {
    region.iter().enumerate().fold(0u32, |cs, (i, &b)| {
        if CHECKSUM_EXCLUDED.contains(&i) {
            cs
        } else {
            roll(cs, b)
        }
    })
}

/// The exFAT up-case table checksum: the rolling `u32` over every byte
/// of the table's on-disk data (`DataLength` bytes).
pub(crate) fn upcase_table_checksum(table: &[u8]) -> u32 {
    table.iter().fold(0u32, |cs, &b| roll(cs, b))
}

/// The fixed head of an ExFAT Volume Boot Record.
#[derive(Debug, Clone)]
pub(crate) struct ExfatBootRecord {
    pub _dummy_jmp: [u8; 3],
    pub _oem_identifier: [u8; 8],
    pub _zeroed: [u8; 53],
    pub _partition_offset: u64,
    pub volume_len: u64,
    pub fat_offset: u32,
    pub fat_len: u32,
    pub cluster_heap_offset: u32,
    pub cluster_count: u32,
    pub root_dir_cluster: u32,
    pub partition_serial_num: u32,
    pub fs_revision: u16,
    pub flags: u16,
    pub sector_shift: u8,
    pub cluster_shift: u8,
    pub fat_count: u8,
    pub drive_select: u8,
    pub used_percentage: u8,
    pub _reserved: [u8; 7],
}

impl FixedCodec for ExfatBootRecord {
    const SIZE: usize = EXFAT_BOOT_RECORD_SIZE;

    fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < Self::SIZE {
            return None;
        }
        Some(Self {
            _dummy_jmp: bytes(b, 0),
            _oem_identifier: bytes(b, 3),
            _zeroed: bytes(b, 11),
            _partition_offset: le_u64(b, 64),
            volume_len: le_u64(b, 72),
            fat_offset: le_u32(b, 80),
            fat_len: le_u32(b, 84),
            cluster_heap_offset: le_u32(b, 88),
            cluster_count: le_u32(b, 92),
            root_dir_cluster: le_u32(b, 96),
            partition_serial_num: le_u32(b, 100),
            fs_revision: le_u16(b, 104),
            flags: le_u16(b, 106),
            sector_shift: b[108],
            cluster_shift: b[109],
            fat_count: b[110],
            drive_select: b[111],
            used_percentage: b[112],
            _reserved: bytes(b, 113),
        })
    }

    fn write_into(&self, out: &mut [u8]) {
        let b = &mut out[..Self::SIZE];
        b[0..3].copy_from_slice(&self._dummy_jmp);
        b[3..11].copy_from_slice(&self._oem_identifier);
        b[11..64].copy_from_slice(&self._zeroed);
        put_u64(b, 64, self._partition_offset);
        put_u64(b, 72, self.volume_len);
        put_u32(b, 80, self.fat_offset);
        put_u32(b, 84, self.fat_len);
        put_u32(b, 88, self.cluster_heap_offset);
        put_u32(b, 92, self.cluster_count);
        put_u32(b, 96, self.root_dir_cluster);
        put_u32(b, 100, self.partition_serial_num);
        put_u16(b, 104, self.fs_revision);
        put_u16(b, 106, self.flags);
        b[108] = self.sector_shift;
        b[109] = self.cluster_shift;
        b[110] = self.fat_count;
        b[111] = self.drive_select;
        b[112] = self.used_percentage;
        b[113..120].copy_from_slice(&self._reserved);
    }
}

impl ExfatBootRecord {
    /// Guard against adversarial VBR shift fields. The spec fixes
    /// `BytesPerSectorShift` in `9..=12` (512 B – 4 KiB sectors) and caps
    /// `sector_shift + cluster_shift` at 25. Out-of-range values would
    /// overflow the `u8` addition, shift past a target integer's width,
    /// or (a sub-512-byte sector) undersize boot-region buffers — so a
    /// bogus VBR is treated as an unsupported filesystem instead.
    #[inline]
    pub(crate) fn shifts_valid(&self) -> bool {
        (9..=12).contains(&self.sector_shift)
            && u16::from(self.sector_shift) + u16::from(self.cluster_shift) <= 25
    }

    /// Bytes per sector — `2^sector_shift`. Checked shift: an
    /// unvalidated adversarial `sector_shift` yields 0 (rejected by
    /// downstream length checks) instead of panicking.
    #[inline]
    pub(crate) fn bytes_per_sector(&self) -> u32 {
        1u32.checked_shl(u32::from(self.sector_shift)).unwrap_or(0)
    }

    /// Bytes per cluster — `2^(sector_shift + cluster_shift)`, with the
    /// same widened, checked-shift hardening as [`Self::bytes_per_sector`].
    #[inline]
    pub(crate) fn bytes_per_cluster(&self) -> u32 {
        let total_shift = u32::from(self.sector_shift) + u32::from(self.cluster_shift);
        1u32.checked_shl(total_shift).unwrap_or(0)
    }

    /// Whether a directory entry's cluster/length fields are consistent
    /// with this volume's cluster heap. Applied at the decode boundary so
    /// no downstream code trusts an out-of-range `first_cluster` /
    /// `data_length` for allocation, freeing, or I/O (a crafted image
    /// could otherwise drive an unbounded FAT-write loop or overflow).
    ///
    /// Valid data-cluster indices are `2..=cluster_count + 1`. An entry
    /// with no allocation (`first_cluster == 0`) must carry no data; a
    /// referenced run must fit within `cluster_count` clusters; and a
    /// contiguous (`NoFatChain`) run must lie entirely within the heap.
    pub(crate) fn entry_geometry_valid(
        &self,
        first_cluster: u32,
        data_length: u64,
        valid_data_length: u64,
        no_fat_chain: bool,
    ) -> bool {
        if valid_data_length > data_length {
            return false;
        }
        if first_cluster == 0 {
            return data_length == 0;
        }
        let last_cluster = self.cluster_count.saturating_add(1);
        if first_cluster < 2 || first_cluster > last_cluster {
            return false;
        }
        let cluster_bytes = u64::from(self.bytes_per_cluster());
        if cluster_bytes == 0 {
            return false;
        }
        let clusters = data_length.div_ceil(cluster_bytes);
        if clusters > u64::from(self.cluster_count) {
            return false;
        }
        if no_fat_chain && clusters > 0 {
            let last_used = u64::from(first_cluster) + clusters - 1;
            if last_used > u64::from(last_cluster) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exfat_with_shifts(sector_shift: u8, cluster_shift: u8) -> ExfatBootRecord {
        ExfatBootRecord {
            _dummy_jmp: [0; 3],
            _oem_identifier: *crate::boot::EXFAT_OEM_IDENTIFIER,
            _zeroed: [0; 53],
            _partition_offset: 0,
            volume_len: 0,
            fat_offset: 0,
            fat_len: 0,
            cluster_heap_offset: 0,
            cluster_count: 0,
            root_dir_cluster: 0,
            partition_serial_num: 0,
            fs_revision: 0,
            flags: 0,
            sector_shift,
            cluster_shift,
            fat_count: 1,
            drive_select: 0,
            used_percentage: 0,
            _reserved: [0; 7],
        }
    }

    #[test]
    fn valid_shifts_accepted() {
        // Typical 512 B/sector, 8 sectors/cluster (4 KiB clusters).
        let br = exfat_with_shifts(9, 3);
        assert!(br.shifts_valid());
        assert_eq!(br.bytes_per_sector(), 512);
        assert_eq!(br.bytes_per_cluster(), 4096);
    }

    /// A 4 KiB-cluster volume with 100 data clusters (indices 2..=101).
    fn exfat_with_clusters(cluster_count: u32) -> ExfatBootRecord {
        let mut br = exfat_with_shifts(9, 3); // 512 B/sector, 4 KiB/cluster
        br.cluster_count = cluster_count;
        br
    }

    #[test]
    fn entry_geometry_accepts_valid_and_rejects_crafted() {
        let br = exfat_with_clusters(100); // valid clusters 2..=101, 4 KiB each
        let cl = 4096;

        // Valid: empty file, in-range single cluster, and the last cluster.
        assert!(br.entry_geometry_valid(0, 0, 0, false));
        assert!(br.entry_geometry_valid(2, cl, cl, true));
        assert!(br.entry_geometry_valid(101, cl, 0, true));
        assert!(br.entry_geometry_valid(2, 100 * cl, 100 * cl, true)); // fills heap

        // Rejected: allocation with no cluster, or data with no allocation.
        assert!(!br.entry_geometry_valid(0, 1, 0, false));
        // Rejected: cluster index below the heap or past its end.
        assert!(!br.entry_geometry_valid(1, cl, cl, false));
        assert!(!br.entry_geometry_valid(102, cl, cl, false));
        // Rejected: more clusters than the volume has.
        assert!(!br.entry_geometry_valid(2, 101 * cl, 101 * cl, false));
        // Rejected: contiguous run spilling past the heap end.
        assert!(!br.entry_geometry_valid(100, 3 * cl, 3 * cl, true));
        // Rejected: valid_data_length exceeds data_length.
        assert!(!br.entry_geometry_valid(2, cl, cl + 1, false));
        // A FAT-chained file whose data_length fits is fine even near the end.
        assert!(br.entry_geometry_valid(100, 2 * cl, 2 * cl, false));
    }

    #[test]
    fn entry_geometry_never_panics_on_extremes() {
        // Zero-cluster and max-cluster volumes must not overflow/underflow.
        assert!(exfat_with_clusters(0).entry_geometry_valid(0, 0, 0, false));
        assert!(!exfat_with_clusters(0).entry_geometry_valid(2, 1, 1, true));
        let max = exfat_with_clusters(u32::MAX);
        let _ = max.entry_geometry_valid(u32::MAX, u64::MAX, u64::MAX, true);
        let _ = max.entry_geometry_valid(2, u64::MAX, 0, false);
    }

    #[test]
    fn sub_512_byte_sector_rejected() {
        // BytesPerSectorShift below 9 (sub-512-byte sector) is out of
        // spec; accepting it undersized the boot-region buffer and
        // panicked on the checksum slice (fuzz-found).
        for shift in 0..9u8 {
            assert!(!exfat_with_shifts(shift, 0).shifts_valid(), "shift {shift}");
        }
        assert!(exfat_with_shifts(9, 0).shifts_valid());
    }

    #[test]
    fn out_of_range_shifts_rejected_not_panicked() {
        // sector_shift past the spec max (12).
        let a = exfat_with_shifts(40, 0);
        assert!(!a.shifts_valid());
        // sector_shift + cluster_shift past the spec cap (25).
        let b = exfat_with_shifts(12, 20);
        assert!(!b.shifts_valid());
        // Adversarial values that would overflow a `u8` addition.
        let c = exfat_with_shifts(200, 200);
        assert!(!c.shifts_valid());

        // The byte-size helpers must not panic on any of these; an
        // out-of-width shift saturates to 0.
        assert_eq!(a.bytes_per_sector(), 0);
        assert_eq!(a.bytes_per_cluster(), 0);
        assert_eq!(b.bytes_per_sector(), 4096);
        assert_eq!(b.bytes_per_cluster(), 0);
        assert_eq!(c.bytes_per_sector(), 0);
        assert_eq!(c.bytes_per_cluster(), 0);
    }
}
