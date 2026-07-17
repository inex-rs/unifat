//! [`FatClusterMap`]: FAT-chain cluster geometry for one open stream.

use core::num;

use embedded_io::{Read, Seek, Write};

use crate::error::{FileError, FsError};
use crate::fat::{FatVfs, FatWrite};
use crate::vfs::{ClusterMap, ExtendResult, FreeTailResult};

/// Cluster map for classic FAT (always chained; never NoFatChain).
pub(crate) struct FatClusterMap<'a, S>
where
    S: Read + Write + Seek,
{
    fs: &'a FatVfs<S>,
    allocated_len: u64,
}

impl<'a, S> FatClusterMap<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn new(fs: &'a FatVfs<S>, file_size: u32, first_cluster: u32) -> Self {
        let cs = u64::from(fs.cluster_size());
        let allocated_len = if first_cluster < 2 {
            0
        } else if file_size == 0 {
            cs
        } else {
            u64::from(file_size).next_multiple_of(cs)
        };
        Self { fs, allocated_len }
    }

    /// Walk to the chain's last cluster, bounded by the volume's cluster
    /// count so a corrupt cyclic chain errors instead of looping forever.
    fn walk_to_tail(&self, first: u32) -> Result<u32, FileError<S::Error>> {
        let mut c = first;
        let mut steps = 0u32;
        while let Some(n) = self.next(c)? {
            steps += 1;
            if steps > self.fs.props.max_cluster {
                return Err(FileError::Corrupt);
            }
            c = n;
        }
        Ok(c)
    }
}

impl<S> ClusterMap for FatClusterMap<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FileError<S::Error>;

    fn next(&self, cluster: u32) -> Result<Option<u32>, FileError<S::Error>> {
        self.fs.next_cluster(cluster).map_err(FileError::from)
    }

    fn extend(
        &mut self,
        first: Option<u32>,
        tail: Option<u32>,
        needed_allocated_len: u64,
    ) -> Result<ExtendResult, FileError<S::Error>> {
        let cs = u64::from(self.fs.cluster_size());
        let first_out = first.filter(|&c| c >= 2).unwrap_or(0);

        if needed_allocated_len <= self.allocated_len && first_out >= 2 {
            let t = match tail {
                Some(t) => t,
                None => self.walk_to_tail(first_out)?,
            };
            return Ok(ExtendResult {
                first_cluster: first_out,
                tail: t,
            });
        }

        // Link new clusters from the true chain tail. When the caller has no
        // cached tail, walk to the end of the chain — linking from `first`
        // would overwrite its FAT cell and orphan every cluster after it.
        let link_from = match tail.filter(|&c| c >= 2) {
            Some(t) => Some(t),
            None => match first.filter(|&c| c >= 2) {
                Some(f) => Some(self.walk_to_tail(f)?),
                None => None,
            },
        };
        // If empty (no first), allocate enough for needed_allocated_len from 0.
        let bytes_needed = if link_from.is_none() {
            needed_allocated_len.max(cs)
        } else {
            needed_allocated_len.saturating_sub(self.allocated_len)
        };

        let clusters_to_allocate = bytes_needed.div_ceil(cs).max(1);
        let n = u32::try_from(clusters_to_allocate).unwrap_or(u32::MAX);
        let n = num::NonZero::new(n).ok_or(FileError::Corrupt)?;

        let new = self
            .fs
            .allocate_clusters(n, link_from)
            .map_err(|e| match e {
                FsError::StorageFull => FileError::StorageFull,
                other => FileError::from(other),
            })?;

        let first_out = if first_out >= 2 { first_out } else { new[0] };
        let tail_out = *new.last().expect("n >= 1");

        self.allocated_len = if link_from.is_some() {
            self.allocated_len + u64::from(n.get()) * cs
        } else {
            u64::from(n.get()) * cs
        };

        Ok(ExtendResult {
            first_cluster: first_out,
            tail: tail_out,
        })
    }

    fn free_tail(
        &mut self,
        first: u32,
        keep_len: u64,
    ) -> Result<FreeTailResult, FileError<S::Error>> {
        let cs = u64::from(self.fs.cluster_size());
        if first < 2 {
            self.allocated_len = 0;
            return Ok(FreeTailResult { first_cluster: 0 });
        }

        // FAT empty files keep one cluster (matches FatFile::truncate).
        // Saturate: FAT32 cluster counts are < 2^28, so a count past
        // `u32::MAX` only occurs on nonsense input; the walk below is
        // bounded by the real chain end regardless.
        let keep = if keep_len == 0 {
            1u32
        } else {
            u32::try_from(keep_len.div_ceil(cs)).unwrap_or(u32::MAX)
        };

        let mut cur = first;
        for _ in 0..keep.saturating_sub(1) {
            match self.next(cur)? {
                Some(n) => cur = n,
                None => break,
            }
        }
        let mut next = self.next(cur)?;
        self.fs
            .write_fat(cur, FatWrite::End)
            .map_err(FileError::from)?;
        while let Some(c) = next {
            next = self.next(c)?;
            self.fs
                .write_fat(c, FatWrite::Free)
                .map_err(FileError::from)?;
        }

        self.allocated_len = u64::from(keep) * cs;
        Ok(FreeTailResult {
            first_cluster: first,
        })
    }

    fn cluster_size(&self) -> u32 {
        self.fs.cluster_size()
    }

    fn cluster_to_offset(&self, cluster: u32) -> u64 {
        let sector = self.fs.cluster_first_sector(cluster);
        self.fs.sector_byte_offset(sector)
    }

    fn no_fat_chain(&self) -> bool {
        false
    }

    fn allocated_len(&self) -> u64 {
        self.allocated_len
    }
}
