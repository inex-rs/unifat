//! [`ExfatClusterMap`]: per-open-stream cluster geometry (owns NoFatChain).

use embedded_io::{Read, Seek, Write};

use crate::error::FileError;
use crate::exfat::ExfatVfs;
use crate::vfs::{ClusterMap, ExtendResult, FreeTailResult};

use super::FAT_EOC;

/// Cluster map for one open ExFAT stream.
pub(crate) struct ExfatClusterMap<'a, S>
where
    S: Read + Write + Seek,
{
    vol: &'a ExfatVfs<S>,
    /// First cluster of this stream (0 if empty).
    first: u32,
    no_fat_chain: bool,
    allocated_len: u64,
    cluster_size: u32,
}

impl<'a, S> ExfatClusterMap<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn new(
        vol: &'a ExfatVfs<S>,
        first: u32,
        allocated_len: u64,
        no_fat_chain: bool,
    ) -> Self {
        Self {
            vol,
            first,
            no_fat_chain,
            allocated_len,
            cluster_size: vol.boot.bytes_per_cluster(),
        }
    }

    /// Walk to the chain's last cluster, bounded by the volume's cluster
    /// count so a corrupt cyclic chain errors instead of looping forever.
    fn walk_to_tail(&self, first: u32) -> Result<u32, FileError<S::Error>> {
        let mut c = first;
        let mut steps = 0u32;
        while let Some(n) = self.vol.next_cluster(c, false).map_err(FileError::from)? {
            steps += 1;
            if steps > self.vol.boot.cluster_count {
                return Err(FileError::Corrupt);
            }
            c = n;
        }
        Ok(c)
    }

    fn materialize_fat_chain(&mut self, first: u32, count: u32) -> Result<(), FileError<S::Error>> {
        if count == 0 {
            return Ok(());
        }
        for i in 0..count.saturating_sub(1) {
            self.vol
                .write_fat_entry(first + i, first + i + 1)
                .map_err(FileError::from)?;
        }
        self.vol
            .write_fat_entry(first + count - 1, FAT_EOC)
            .map_err(FileError::from)?;
        self.no_fat_chain = false;
        Ok(())
    }
}

impl<S> ClusterMap for ExfatClusterMap<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FileError<S::Error>;

    fn next(&self, cluster: u32) -> Result<Option<u32>, FileError<S::Error>> {
        if self.no_fat_chain {
            if self.first < 2 || self.allocated_len == 0 || cluster < self.first {
                return Ok(None);
            }
            let cs = u64::from(self.cluster_size);
            // Saturate: a count past `u32::MAX` only occurs on a corrupt
            // volume (spec caps cluster indices at u32); reads stay bounded
            // by `len` regardless.
            let allocated_clusters =
                u32::try_from(self.allocated_len.div_ceil(cs)).unwrap_or(u32::MAX);
            let index = cluster - self.first;
            if index + 1 < allocated_clusters {
                return Ok(Some(cluster + 1));
            }
            return Ok(None);
        }
        self.vol
            .next_cluster(cluster, false)
            .map_err(FileError::from)
    }

    fn extend(
        &mut self,
        first: Option<u32>,
        tail: Option<u32>,
        needed_allocated_len: u64,
    ) -> Result<ExtendResult, FileError<S::Error>> {
        let cs = u64::from(self.cluster_size);
        let mut first_out = first.filter(|&c| c >= 2).unwrap_or(0);
        let mut tail_out = tail.filter(|&c| c >= 2).unwrap_or(first_out);
        let mut seeded_first = false;

        // Empty file: allocate first cluster.
        if first_out < 2 {
            let new = self
                .vol
                .find_free_cluster()
                .map_err(FileError::from)?
                .ok_or(FileError::StorageFull)?;
            self.vol
                .mark_cluster_allocated(new)
                .map_err(FileError::from)?;
            self.vol
                .write_fat_entry(new, FAT_EOC)
                .map_err(FileError::from)?;
            first_out = new;
            tail_out = new;
            self.first = new;
            self.no_fat_chain = false;
            self.allocated_len = cs;
            seeded_first = true;
        }

        // Growing an existing FAT-chained file with an unknown tail: walk to
        // the true end of the chain before linking. Linking from `first`
        // would overwrite its FAT cell and orphan every cluster after it.
        if !seeded_first
            && !self.no_fat_chain
            && first_out >= 2
            && tail.filter(|&c| c >= 2).is_none()
            && self.allocated_len < needed_allocated_len
        {
            tail_out = self.walk_to_tail(first_out)?;
        }

        while self.allocated_len < needed_allocated_len {
            // Materialize the contiguous run before FAT-linking growth.
            if self.no_fat_chain {
                // Saturate: see `next` — counts past `u32::MAX` mean corruption.
                let count = u32::try_from(self.allocated_len.div_ceil(cs)).unwrap_or(u32::MAX);
                self.materialize_fat_chain(first_out, count)?;
                tail_out = self.walk_to_tail(first_out)?;
            }

            let new = self
                .vol
                .find_free_cluster()
                .map_err(FileError::from)?
                .ok_or(FileError::StorageFull)?;
            self.vol
                .mark_cluster_allocated(new)
                .map_err(FileError::from)?;
            self.vol
                .write_fat_entry(tail_out, new)
                .map_err(FileError::from)?;
            self.vol
                .write_fat_entry(new, FAT_EOC)
                .map_err(FileError::from)?;
            tail_out = new;
            self.no_fat_chain = false;
            self.allocated_len += cs;
        }

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
        let cs = u64::from(self.cluster_size);
        if first < 2 {
            self.allocated_len = 0;
            return Ok(FreeTailResult { first_cluster: 0 });
        }

        // Saturate: see `next` — counts past `u32::MAX` mean corruption.
        let old_count = u32::try_from(self.allocated_len.div_ceil(cs)).unwrap_or(u32::MAX);
        let keep = u32::try_from(keep_len.div_ceil(cs)).unwrap_or(u32::MAX);

        if old_count <= keep {
            return Ok(FreeTailResult {
                first_cluster: first,
            });
        }

        if keep == 0 {
            self.vol
                .free_cluster_chain(first, old_count, self.no_fat_chain)
                .map_err(FileError::from)?;
            self.allocated_len = 0;
            // Clear our own flag too: flush_entry reads it from the map,
            // and a persisted NoFatChain=1 with FirstCluster=0 is out of
            // spec (flagged by chkdsk-class tools).
            self.no_fat_chain = false;
            return Ok(FreeTailResult { first_cluster: 0 });
        }

        if self.no_fat_chain {
            self.vol
                .free_cluster_chain(first + keep, old_count - keep, true)
                .map_err(FileError::from)?;
            self.allocated_len = u64::from(keep) * cs;
            return Ok(FreeTailResult {
                first_cluster: first,
            });
        }

        // Chained: walk keep-1 steps, EOC last kept, free rest.
        let mut cur = first;
        for _ in 0..(keep - 1) {
            match self.vol.next_cluster(cur, false).map_err(FileError::from)? {
                Some(n) => cur = n,
                None => break,
            }
        }
        let tail = self.vol.next_cluster(cur, false).map_err(FileError::from)?;
        self.vol
            .write_fat_entry(cur, FAT_EOC)
            .map_err(FileError::from)?;
        if let Some(first_free) = tail {
            self.vol
                .free_cluster_chain(first_free, old_count - keep, false)
                .map_err(FileError::from)?;
        }
        self.allocated_len = u64::from(keep) * cs;
        Ok(FreeTailResult {
            first_cluster: first,
        })
    }

    fn cluster_size(&self) -> u32 {
        self.cluster_size
    }

    fn cluster_to_offset(&self, cluster: u32) -> u64 {
        super::cluster_to_byte_offset(
            self.vol.boot.cluster_heap_offset,
            cluster,
            self.vol.boot.bytes_per_sector(),
            self.vol.boot.bytes_per_cluster(),
        )
    }

    fn no_fat_chain(&self) -> bool {
        self.no_fat_chain
    }

    fn allocated_len(&self) -> u64 {
        self.allocated_len
    }
}
