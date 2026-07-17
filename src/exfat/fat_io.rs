//! FAT table and cluster payload I/O for [`ExfatVfs`].

use embedded_io::{Read, Seek, Write};

use super::direntry::{FatEntry, classify_fat_entry};
use super::{ExfatVfs, cluster_to_byte_offset};
use crate::codec::exfat::raw_entry::ExfatFatEntryWord;
use crate::error::FsResult;

impl<S> ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Read one 4-byte FAT entry at `cluster`.
    pub(super) fn read_fat_entry(&self, cluster: u32) -> FsResult<FatEntry, S::Error> {
        let sector_bytes = self.boot.bytes_per_sector();
        let offset =
            u64::from(self.boot.fat_offset) * u64::from(sector_bytes) + u64::from(cluster) * 4;
        let mut buf = [0u8; 4];
        self.read_at(offset, &mut buf)?;
        let word = ExfatFatEntryWord::parse(&buf);
        Ok(classify_fat_entry(word.0, self.boot.cluster_count))
    }

    /// Next cluster in a chain. With `no_fat_chain` (Stream Extension
    /// flag bit 1) the allocation is contiguous — next is `current + 1`
    /// and the caller must track where the extent ends.
    pub(super) fn next_cluster(
        &self,
        current: u32,
        no_fat_chain: bool,
    ) -> FsResult<Option<u32>, S::Error> {
        if no_fat_chain {
            let next = current + 1;
            if next > self.boot.cluster_count + 1 {
                return Ok(None);
            }
            return Ok(Some(next));
        }
        match self.read_fat_entry(current)? {
            FatEntry::Next(n) => Ok(Some(n)),
            FatEntry::Eoc => Ok(None),
            FatEntry::Free | FatEntry::Bad | FatEntry::Invalid => Ok(None),
        }
    }

    /// Read one full cluster into `buf` (must be ≥ cluster size).
    pub(super) fn read_cluster_into(&self, cluster: u32, buf: &mut [u8]) -> FsResult<(), S::Error> {
        let cluster_bytes = self.boot.bytes_per_cluster() as usize;
        debug_assert!(buf.len() >= cluster_bytes);
        let sector_bytes = self.boot.bytes_per_sector();
        let offset = cluster_to_byte_offset(
            self.boot.cluster_heap_offset,
            cluster,
            sector_bytes,
            self.boot.bytes_per_cluster(),
        );
        self.read_at(offset, &mut buf[..cluster_bytes])
    }

    /// Write a raw FAT entry at `cluster` to `value`.
    pub(super) fn write_fat_entry(&self, cluster: u32, value: u32) -> FsResult<(), S::Error> {
        let sector_bytes = self.boot.bytes_per_sector();
        let offset =
            u64::from(self.boot.fat_offset) * u64::from(sector_bytes) + u64::from(cluster) * 4;
        self.write_at(offset, &ExfatFatEntryWord(value).encode())?;
        Ok(())
    }
}
