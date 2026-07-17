//! Allocation-bitmap helpers for [`ExfatVfs`].

use embedded_io::{Read, Seek, Write};

use super::direntry::FatEntry;
use super::{ExfatVfs, cluster_to_byte_offset};
use crate::error::{CorruptKind, FsError, FsResult};

/// Window size for streaming allocation-bitmap scans, so a multi-TB
/// volume's bitmap (tens of MB) is never loaded whole.
const BITMAP_WINDOW: usize = 512;

impl<S> ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Byte offset of the bitmap's first byte. The bitmap is a single
    /// contiguous run (as every real formatter lays it out).
    fn bitmap_base(&self) -> u64 {
        cluster_to_byte_offset(
            self.boot.cluster_heap_offset,
            self.bitmap.first_cluster,
            self.boot.bytes_per_sector(),
            self.boot.bytes_per_cluster(),
        )
    }

    /// Bitmap bytes actually covering the cluster heap — one bit per
    /// cluster (`2..=cluster_count+1`), capped at the on-disk length.
    fn bitmap_scan_bytes(&self) -> u64 {
        u64::from(self.boot.cluster_count)
            .div_ceil(8)
            .min(self.bitmap.byte_length)
    }

    /// Disk byte offset and bit position of `cluster`'s bitmap bit.
    fn bitmap_byte_offset_for_cluster(&self, cluster: u32) -> Option<(u64, u8)> {
        if cluster < 2 {
            return None;
        }
        let bit_idx = cluster - 2;
        let byte_in_bitmap = u64::from(bit_idx / 8);
        if byte_in_bitmap >= self.bitmap.byte_length {
            return None;
        }
        let bit_in_byte = (bit_idx % 8) as u8;
        Some((self.bitmap_base() + byte_in_bitmap, bit_in_byte))
    }

    /// Set the bitmap bit for `cluster` to mark it allocated.
    pub(super) fn mark_cluster_allocated(&self, cluster: u32) -> FsResult<(), S::Error> {
        let (byte_off, bit) = self
            .bitmap_byte_offset_for_cluster(cluster)
            .ok_or(FsError::Unsupported)?;
        let mut b = [0u8; 1];
        self.read_at(byte_off, &mut b)?;
        b[0] |= 1 << bit;
        self.write_at(byte_off, &b)?;
        Ok(())
    }

    /// Scan the bitmap for the first free cluster, in bounded windows so
    /// memory stays flat on multi-TB volumes. `None` = volume full.
    pub(super) fn find_free_cluster(&self) -> FsResult<Option<u32>, S::Error> {
        let base = self.bitmap_base();
        let scan_bytes = self.bitmap_scan_bytes();
        let last_cluster = u64::from(self.boot.cluster_count) + 1;

        let mut window = [0u8; BITMAP_WINDOW];
        let mut byte_idx: u64 = 0;
        while byte_idx < scan_bytes {
            let chunk = usize::try_from((scan_bytes - byte_idx).min(BITMAP_WINDOW as u64))
                .unwrap_or(BITMAP_WINDOW);
            self.read_at(base + byte_idx, &mut window[..chunk])?;
            for (i, &b) in window[..chunk].iter().enumerate() {
                if b == 0xFF {
                    continue;
                }
                for bit in 0..8u32 {
                    if (b >> bit) & 1 != 0 {
                        continue;
                    }
                    let cluster_idx = (byte_idx + i as u64) * 8 + u64::from(bit);
                    let cluster_u64 = 2 + cluster_idx;
                    if cluster_u64 > last_cluster {
                        return Ok(None);
                    }
                    return Ok(Some(
                        u32::try_from(cluster_u64).expect("bounded by cluster_count + 1 (a u32)"),
                    ));
                }
            }
            byte_idx += chunk as u64;
        }
        Ok(None)
    }

    /// Scan the bitmap (in bounded windows) for a run of `count`
    /// consecutive free clusters; `None` when no such run exists.
    pub(super) fn find_free_contiguous_run(&self, count: u32) -> FsResult<Option<u32>, S::Error> {
        if count == 0 {
            return Ok(None);
        }
        let base = self.bitmap_base();
        let scan_bytes = self.bitmap_scan_bytes();
        let last_cluster = u64::from(self.boot.cluster_count) + 1;

        let mut window = [0u8; BITMAP_WINDOW];
        let mut run_start: Option<u32> = None;
        let mut run_len: u32 = 0;
        let mut byte_idx: u64 = 0;
        while byte_idx < scan_bytes {
            let chunk = usize::try_from((scan_bytes - byte_idx).min(BITMAP_WINDOW as u64))
                .unwrap_or(BITMAP_WINDOW);
            self.read_at(base + byte_idx, &mut window[..chunk])?;
            for (i, &b) in window[..chunk].iter().enumerate() {
                for bit in 0..8u32 {
                    let cluster_u64 = 2 + (byte_idx + i as u64) * 8 + u64::from(bit);
                    if cluster_u64 > last_cluster {
                        return Ok(None);
                    }
                    let cluster =
                        u32::try_from(cluster_u64).expect("bounded by cluster_count + 1 (a u32)");
                    if (b >> bit) & 1 == 0 {
                        if run_start.is_none() {
                            run_start = Some(cluster);
                            run_len = 1;
                        } else {
                            run_len += 1;
                        }
                        if run_len == count {
                            return Ok(run_start);
                        }
                    } else {
                        run_start = None;
                        run_len = 0;
                    }
                }
            }
            byte_idx += chunk as u64;
        }
        Ok(None)
    }

    /// Mark `count` clusters starting at `first` as allocated.
    pub(super) fn mark_cluster_range_allocated(
        &self,
        first: u32,
        count: u32,
    ) -> FsResult<(), S::Error> {
        for i in 0..count {
            self.mark_cluster_allocated(first + i)?;
        }
        Ok(())
    }

    /// Clear the bitmap bit for `cluster`, marking it free. Idempotent.
    fn mark_cluster_free(&self, cluster: u32) -> FsResult<(), S::Error> {
        let (byte_off, bit) = self
            .bitmap_byte_offset_for_cluster(cluster)
            .ok_or(FsError::Unsupported)?;
        let mut b = [0u8; 1];
        self.read_at(byte_off, &mut b)?;
        b[0] &= !(1 << bit);
        self.write_at(byte_off, &b)?;
        Ok(())
    }

    /// Free every cluster reachable from `first_cluster` — a contiguous
    /// run of `count` clusters (NoFatChain) or a FAT-linked chain.
    pub(super) fn free_cluster_chain(
        &self,
        first_cluster: u32,
        count: u32,
        no_fat_chain: bool,
    ) -> FsResult<(), S::Error> {
        if no_fat_chain {
            for i in 0..count {
                self.mark_cluster_free(first_cluster + i)?;
            }
        } else {
            // Cap the walk at `cluster_count` so a malformed FAT cycle
            // still terminates.
            let mut cur = first_cluster;
            let mut visited = 0u32;
            let cap = self.boot.cluster_count;
            while visited <= cap {
                let next = self.read_fat_entry(cur)?;
                // A BAD marker means `cur` itself is a bad cluster that a
                // corrupt chain led us to: leave it allocated (the spec
                // keeps bad clusters out of the free pool) and stop.
                if matches!(next, FatEntry::Bad) {
                    return Ok(());
                }
                self.mark_cluster_free(cur)?;
                self.write_fat_entry(cur, 0)?;
                match next {
                    FatEntry::Next(n) => {
                        cur = n;
                        visited += 1;
                    }
                    _ => return Ok(()),
                }
            }
            return Err(FsError::Corrupt(CorruptKind::ClusterChain));
        }
        Ok(())
    }
}
