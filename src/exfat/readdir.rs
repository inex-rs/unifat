//! [`ExfatReadDir`]: directory iteration over the concatenated cluster
//! stream, so entry sets that straddle cluster boundaries decode like any
//! other (Windows writes such sets in multi-cluster directories).

use embedded_io::{Read, Seek, Write};

use super::ExfatVfs;
use super::direntry::{DirEntryDecoder, ExfatDirEntry, SetLoc};
use crate::error::FsResult;

/// Iterator yielding [`ExfatDirEntry`]s from a directory. The whole
/// extent is read up front (directories are small); decoding then runs
/// over one flat buffer, immune to cluster-boundary straddling.
pub(crate) struct ExfatReadDir<'v, S>
where
    S: Read + Write + Seek,
{
    vol: &'v ExfatVfs<S>,
    decoder: DirEntryDecoder,
    /// Walk parameters recorded into each yielded entry's [`SetLoc`].
    dir_first_cluster: u32,
    dir_no_fat_chain: bool,
    dir_max_clusters: u32,
}

impl<'v, S> ExfatReadDir<'v, S>
where
    S: Read + Write + Seek,
{
    pub(super) fn new(
        vol: &'v ExfatVfs<S>,
        first_cluster: u32,
        no_fat_chain: bool,
        max_clusters: u32,
    ) -> FsResult<Self, S::Error> {
        let (_, buf) = vol.read_dir_stream(first_cluster, no_fat_chain, max_clusters)?;
        Ok(Self {
            vol,
            decoder: DirEntryDecoder::new(buf),
            dir_first_cluster: first_cluster,
            dir_no_fat_chain: no_fat_chain,
            dir_max_clusters: max_clusters,
        })
    }
}

impl<S> Iterator for ExfatReadDir<'_, S>
where
    S: Read + Write + Seek,
{
    type Item = ExfatDirEntry;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let mut entry = self.decoder.next()?;
            // The decoder stops just past the set it returned; back up
            // by the set length to get this entry's logical offset.
            let set_start = self
                .decoder
                .pos
                .saturating_sub(entry.entry_set_len as usize);
            entry.loc = Some(SetLoc {
                dir_first_cluster: self.dir_first_cluster,
                dir_no_fat_chain: self.dir_no_fat_chain,
                dir_max_clusters: self.dir_max_clusters,
                logical: set_start as u64,
                len: entry.entry_set_len,
            });
            // Reject entries whose cluster/length fields don't fit the
            // volume geometry, so no consumer trusts a crafted
            // `first_cluster` / `data_length` (skip like a bad set).
            if !self.vol.boot.entry_geometry_valid(
                entry.first_cluster,
                entry.data_length,
                entry.valid_data_length,
                entry.no_fat_chain,
            ) {
                continue;
            }
            return Some(entry);
        }
    }
}
