//! Up-case table load (mount-time).

use alloc::vec;
use alloc::vec::Vec;

use embedded_io::{Read, Seek, SeekFrom, Write};

use super::{ClusterRange, cluster_to_byte_offset};
use crate::codec::exfat::boot::ExfatBootRecord;
use crate::error::{CorruptKind, FsError, FsResult};

/// Load the on-disk upcase table as a **sparse**, codepoint-sorted list
/// of non-identity mappings (a flat 64K-entry table would cost 128 KiB
/// of RAM per mounted volume; real tables have ~2.6K non-identity
/// entries). Handles both uncompressed (sequential u16 mappings) and
/// compressed (`0xFFFF` marker + run-length identity skip) forms.
pub(super) fn load_upcase_table<S: Read + Write + Seek>(
    storage: &mut S,
    boot: &ExfatBootRecord,
    upcase: ClusterRange,
    expected_checksum: u32,
) -> FsResult<Vec<(u16, u16)>, S::Error> {
    let mut lookup: Vec<(u16, u16)> = Vec::new();
    if upcase.first_cluster < 2 || upcase.byte_length == 0 {
        return Ok(lookup);
    }
    let cluster_bytes = u64::from(boot.bytes_per_cluster());
    let total = upcase.byte_length;
    if total > 256 * 1024 {
        // Spec max is 128 KiB (64K u16) + compression overhead.
        return Err(FsError::Corrupt(CorruptKind::Other));
    }
    let total = usize::try_from(total).expect("upcase table is capped at 256 KiB");
    let mut bytes = vec![0u8; total];
    // The spec allocates the upcase table contiguously; walk sequential
    // clusters rather than the FAT.
    let mut written = 0usize;
    let mut cluster = upcase.first_cluster;
    while written < total {
        let offset = cluster_to_byte_offset(
            boot.cluster_heap_offset,
            cluster,
            boot.bytes_per_sector(),
            boot.bytes_per_cluster(),
        );
        let to_read = core::cmp::min(
            usize::try_from(cluster_bytes).expect("bytes per cluster fits usize (from a u32)"),
            total - written,
        );
        storage.seek(SeekFrom::Start(offset))?;
        if let Err(e) = storage.read_exact(&mut bytes[written..written + to_read]) {
            return Err(match e {
                embedded_io::ReadExactError::UnexpectedEof => FsError::Corrupt(CorruptKind::Other),
                embedded_io::ReadExactError::Other(inner) => FsError::Io(inner),
            });
        }
        written += to_read;
        cluster += 1;
    }

    // Verify the table's `TableChecksum` before trusting its mappings.
    if crate::codec::exfat::boot::upcase_table_checksum(&bytes) != expected_checksum {
        log_error!("ExFAT up-case TableChecksum mismatch");
        return Err(FsError::Corrupt(CorruptKind::Other));
    }

    // Each u16 is either a literal mapping for the next codepoint or a
    // 0xFFFF marker followed by a count of identity entries to skip.
    // Codepoints only ascend, so the sparse list comes out sorted.
    let mut idx: usize = 0;
    let mut codepoint: u32 = 0;
    while idx + 2 <= bytes.len() && codepoint < 0x10000 {
        let unit = u16::from_le_bytes([bytes[idx], bytes[idx + 1]]);
        idx += 2;
        if unit == 0xFFFF && idx + 2 <= bytes.len() {
            let skip = u32::from(u16::from_le_bytes([bytes[idx], bytes[idx + 1]]));
            idx += 2;
            codepoint = codepoint.saturating_add(skip);
        } else {
            let cp = u16::try_from(codepoint).expect("codepoint < 0x10000");
            if unit != cp {
                // A table remapping a path separator (either direction)
                // would let a crafted-but-checksum-valid image corrupt
                // folded lock keys and defeat the rename-ancestor guard.
                const SEPARATORS: [u16; 2] = [b'/' as u16, b'\\' as u16];
                if SEPARATORS.contains(&cp) || SEPARATORS.contains(&unit) {
                    log_error!("ExFAT up-case table remaps a path separator");
                    return Err(FsError::Corrupt(CorruptKind::Other));
                }
                lookup.push((cp, unit));
            }
            codepoint += 1;
        }
    }
    Ok(lookup)
}
