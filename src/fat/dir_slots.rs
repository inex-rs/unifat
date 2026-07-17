//! Flat-offset directory slot addressing for [`FatVfs`].
//!
//! A FAT directory is a run of 32-byte slots living either in the fixed
//! FAT12/16 root region or in a cluster chain (FAT32 root, any sub-directory).
//! A [`SlotPos`] is just the slot's absolute partition byte offset plus its
//! owning cluster (for chain-following) — the same flat-offset model the
//! ExFAT backend uses, rather than a `(unit, index)` pair. Slot I/O goes
//! through the sector cache via inherent `FatVfs` methods.

use super::*;
use crate::FsResult;
use crate::codec::fat::lfn::{LAST_AND_UNUSED_ENTRY, UNUSED_ENTRY};
use embedded_io::{Read, Seek, Write};

/// The region a directory occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FatDir {
    /// FAT12/16 fixed root region (a contiguous sector run, no cluster chain).
    FixedRoot,
    /// A cluster chain: FAT32 root, or any sub-directory, keyed by first cluster.
    Clusters(ClusterIndex),
}

/// The on-disk position of one 32-byte directory slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SlotPos {
    /// Absolute byte offset of the slot on the partition.
    pub(crate) offset: u64,
    /// The cluster the slot lives in, or `None` in the fixed root.
    cluster: Option<ClusterIndex>,
}

impl SlotPos {
    #[inline]
    pub(crate) fn cluster(self) -> Option<ClusterIndex> {
        self.cluster
    }
}

/// A contiguous set of directory slots (the LFN entries + SFN of one entry).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SlotChain {
    pub(crate) first: SlotPos,
    pub(crate) len: EntryCount,
}

/// First-byte markers of a directory slot.
pub(crate) const SLOT_END: u8 = LAST_AND_UNUSED_ENTRY; // 0x00 — this and all following are free
pub(crate) const SLOT_DELETED: u8 = UNUSED_ENTRY; // 0xE5 — free, but scanning continues

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// The first slot of directory `dir`.
    pub(crate) fn dir_first_slot(&self, dir: FatDir) -> SlotPos {
        match dir {
            FatDir::FixedRoot => SlotPos {
                offset: self.sector_byte_offset(self.props.first_root_dir_sector),
                cluster: None,
            },
            FatDir::Clusters(first) => self.cluster_first_slot(first),
        }
    }

    /// The first slot of cluster `cluster`.
    fn cluster_first_slot(&self, cluster: ClusterIndex) -> SlotPos {
        SlotPos {
            offset: self.sector_byte_offset(self.cluster_first_sector(cluster)),
            cluster: Some(cluster),
        }
    }

    /// Read the 32 bytes of the slot at `pos`.
    pub(crate) fn read_slot(&self, pos: SlotPos) -> FsResult<[u8; DIRENTRY_SIZE], S::Error> {
        let mut bytes = [0u8; DIRENTRY_SIZE];
        self.read_bytes(pos.offset, &mut bytes)?;
        Ok(bytes)
    }

    /// Overwrite the slot at `pos`.
    pub(crate) fn write_slot(
        &self,
        pos: SlotPos,
        bytes: &[u8; DIRENTRY_SIZE],
    ) -> FsResult<(), S::Error> {
        self.write_bytes(pos.offset, bytes)
    }

    /// Mark the slot at `pos` free. `end` writes the `0x00` terminator (this
    /// and every later slot are free); otherwise `0xE5` (deleted, keep scanning).
    pub(crate) fn free_slot(&self, pos: SlotPos, end: bool) -> FsResult<(), S::Error> {
        self.write_bytes(pos.offset, &[if end { SLOT_END } else { SLOT_DELETED }])
    }

    /// The slot after `pos`, crossing sector and cluster boundaries; `None` at
    /// the end of the region (fixed-root end, or end-of-chain).
    pub(crate) fn next_slot(&self, pos: SlotPos) -> FsResult<Option<SlotPos>, S::Error> {
        let next = pos.offset + DIRENTRY_SIZE as u64;
        match pos.cluster {
            None => {
                let end = self.sector_byte_offset(self.props.first_root_dir_sector)
                    + u64::from(self.root_region_bytes());
                Ok((next < end).then_some(SlotPos {
                    offset: next,
                    cluster: None,
                }))
            }
            Some(cluster) => {
                let cluster_start = self.sector_byte_offset(self.cluster_first_sector(cluster));
                let cluster_end = cluster_start + u64::from(self.props.cluster_size);
                if next < cluster_end {
                    return Ok(Some(SlotPos {
                        offset: next,
                        cluster: Some(cluster),
                    }));
                }
                Ok(match self.next_cluster(cluster)? {
                    Some(n) if n <= self.props.max_cluster => Some(self.cluster_first_slot(n)),
                    _ => None,
                })
            }
        }
    }

    /// The slot `n` positions after `pos` (`n == 0` returns `pos`).
    pub(crate) fn nth_slot(
        &self,
        pos: SlotPos,
        n: EntryCount,
    ) -> FsResult<Option<SlotPos>, S::Error> {
        let mut cur = pos;
        for _ in 0..n {
            match self.next_slot(cur)? {
                Some(next) => cur = next,
                None => return Ok(None),
            }
        }
        Ok(Some(cur))
    }

    /// Byte length of the fixed FAT16 root directory region.
    fn root_region_bytes(&self) -> u32 {
        u32::from(self.boot_record.borrow().root_dir_sectors()) * u32::from(self.props.sector_size)
    }
}
