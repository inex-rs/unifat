//! Byte-addressed, N-way, write-back sector cache.
//!
//! Both backends do all storage I/O through this type. Metadata access
//! (`read_at` / `write_at`) is cached with LRU write-back; bulk payload
//! access (`read_through` / `write_through`) bypasses the slots but
//! stays coherent by flushing / dropping any overlapping ones first.
//! All addressing is by absolute byte offset — there are no stream
//! position conventions, and dirty victims are written back
//! automatically on eviction.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::fmt;

use embedded_io::SeekFrom;

use crate::error::{CorruptKind, FsError, FsResult};
use crate::store::BlockDevice;

/// Number of cached sectors. Sized for the metadata working set of one
/// operation (a directory sector or two, a FAT/bitmap sector, the
/// boot/FSInfo sector) — payload I/O doesn't go through the slots.
const SLOTS: usize = 8;

/// `Slot::sector` value marking an empty slot.
const EMPTY: u64 = u64::MAX;

struct Slot {
    /// Logical sector held, or [`EMPTY`].
    sector: u64,
    dirty: bool,
    /// LRU stamp (monotonic use counter).
    stamp: u64,
    buf: Box<[u8]>,
}

/// Write-back cache owning the storage; see the module docs.
pub(crate) struct SectorCache<S> {
    storage: RefCell<S>,
    slots: RefCell<Vec<Slot>>,
    sector_size: u32,
    tick: Cell<u64>,
}

impl<S> fmt::Debug for SectorCache<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SectorCache")
            .field("sector_size", &self.sector_size)
            .field("slots", &SLOTS)
            .finish_non_exhaustive()
    }
}

impl<S> SectorCache<S>
where
    S: BlockDevice,
{
    /// Build an empty cache over `storage` with `sector_size`-byte slots.
    pub(crate) fn new(storage: S, sector_size: u32) -> Self {
        let slots = (0..SLOTS)
            .map(|_| Slot {
                sector: EMPTY,
                dirty: false,
                stamp: 0,
                buf: alloc::vec![0u8; sector_size as usize].into_boxed_slice(),
            })
            .collect();
        Self {
            storage: RefCell::new(storage),
            slots: RefCell::new(slots),
            sector_size,
            tick: Cell::new(0),
        }
    }

    fn next_stamp(&self) -> u64 {
        let t = self.tick.get() + 1;
        self.tick.set(t);
        t
    }

    /// Write `slot` back to the device (no-op when clean).
    fn write_back(&self, slot: &mut Slot) -> FsResult<(), S::Error> {
        if !slot.dirty || slot.sector == EMPTY {
            slot.dirty = false;
            return Ok(());
        }
        let mut storage = self.storage.borrow_mut();
        storage.seek(SeekFrom::Start(slot.sector * u64::from(self.sector_size)))?;
        storage.write_all(&slot.buf)?;
        slot.dirty = false;
        Ok(())
    }

    /// Index of the slot holding `sector`, loading it on a miss (the LRU
    /// victim is written back first). `fill_only` skips the device read
    /// for a full-sector overwrite.
    fn slot_index(
        &self,
        slots: &mut [Slot],
        sector: u64,
        fill_only: bool,
    ) -> FsResult<usize, S::Error> {
        if let Some(i) = slots.iter().position(|s| s.sector == sector) {
            slots[i].stamp = self.next_stamp();
            return Ok(i);
        }
        // Evict the least-recently-used slot.
        let i = (0..slots.len())
            .min_by_key(|&i| slots[i].stamp)
            .expect("SLOTS > 0");
        self.write_back(&mut slots[i])?;
        slots[i].sector = EMPTY;
        if !fill_only {
            let mut storage = self.storage.borrow_mut();
            storage.seek(SeekFrom::Start(sector * u64::from(self.sector_size)))?;
            storage.read_exact(&mut slots[i].buf).map_err(|e| match e {
                embedded_io::ReadExactError::UnexpectedEof => FsError::Corrupt(CorruptKind::Other),
                embedded_io::ReadExactError::Other(inner) => FsError::Io(inner),
            })?;
        }
        slots[i].sector = sector;
        slots[i].dirty = false;
        slots[i].stamp = self.next_stamp();
        Ok(i)
    }

    /// Cached read of an arbitrary byte range.
    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<(), S::Error> {
        let ss = u64::from(self.sector_size);
        let mut slots = self.slots.borrow_mut();
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let sector = pos / ss;
            #[allow(clippy::cast_possible_truncation)] // remainder < sector size
            let within = (pos % ss) as usize;
            let n = (self.sector_size as usize - within).min(buf.len() - done);
            let i = self.slot_index(&mut slots, sector, false)?;
            buf[done..done + n].copy_from_slice(&slots[i].buf[within..within + n]);
            done += n;
        }
        Ok(())
    }

    /// Cached write-back write of an arbitrary byte range.
    pub(crate) fn write_at(&self, offset: u64, buf: &[u8]) -> FsResult<(), S::Error> {
        let ss = u64::from(self.sector_size);
        let mut slots = self.slots.borrow_mut();
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            let sector = pos / ss;
            #[allow(clippy::cast_possible_truncation)] // remainder < sector size
            let within = (pos % ss) as usize;
            let n = (self.sector_size as usize - within).min(buf.len() - done);
            // A full-sector overwrite needs no device read on a miss.
            let full = within == 0 && n == self.sector_size as usize;
            let i = self.slot_index(&mut slots, sector, full)?;
            slots[i].buf[within..within + n].copy_from_slice(&buf[done..done + n]);
            slots[i].dirty = true;
            done += n;
        }
        Ok(())
    }

    /// Write back every slot overlapping `[offset, offset+len)` so a
    /// direct device access sees fresh bytes. When `invalidate` is set,
    /// the slots are also emptied — a direct *write* must not leave a
    /// stale cached copy to shadow it; a direct *read* only needs the
    /// flush.
    fn writeback_overlap(
        &self,
        offset: u64,
        len: usize,
        invalidate: bool,
    ) -> FsResult<(), S::Error> {
        if len == 0 {
            return Ok(());
        }
        let ss = u64::from(self.sector_size);
        let (first, last) = (offset / ss, (offset + len as u64 - 1) / ss);
        let mut slots = self.slots.borrow_mut();
        for slot in slots.iter_mut() {
            if slot.sector != EMPTY && slot.sector >= first && slot.sector <= last {
                self.write_back(slot)?;
                if invalidate {
                    slot.sector = EMPTY;
                }
            }
        }
        Ok(())
    }

    /// Uncached bulk read (file payload); coherent with cached writes.
    pub(crate) fn read_through(&self, offset: u64, buf: &mut [u8]) -> FsResult<(), S::Error> {
        self.writeback_overlap(offset, buf.len(), false)?;
        let mut storage = self.storage.borrow_mut();
        storage.seek(SeekFrom::Start(offset))?;
        storage.read_exact(buf).map_err(|e| match e {
            embedded_io::ReadExactError::UnexpectedEof => FsError::Corrupt(CorruptKind::Other),
            embedded_io::ReadExactError::Other(inner) => FsError::Io(inner),
        })
    }

    /// Uncached bulk write (file payload); coherent with cached reads.
    pub(crate) fn write_through(&self, offset: u64, buf: &[u8]) -> FsResult<(), S::Error> {
        self.writeback_overlap(offset, buf.len(), true)?;
        let mut storage = self.storage.borrow_mut();
        storage.seek(SeekFrom::Start(offset))?;
        storage.write_all(buf).map_err(FsError::Io)
    }

    /// Write every dirty slot back to the device (contents stay cached).
    pub(crate) fn flush(&self) -> FsResult<(), S::Error> {
        let mut slots = self.slots.borrow_mut();
        for slot in slots.iter_mut() {
            self.write_back(slot)?;
        }
        Ok(())
    }

    /// Device-level flush (call after [`Self::flush`]).
    pub(crate) fn flush_device(&self) -> Result<(), S::Error> {
        self.storage.borrow_mut().flush()
    }

    /// Consume the cache without flushing. Callers flush first.
    pub(crate) fn into_inner(self) -> S {
        self.storage.into_inner()
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // test patterns
mod tests {
    use super::*;
    use crate::store::MemBlockDevice;

    fn image(len: usize) -> MemBlockDevice {
        MemBlockDevice::fixed((0..len).map(|i| (i % 251) as u8).collect())
    }

    #[test]
    fn cached_read_write_roundtrip_across_sectors() {
        let cache = SectorCache::new(image(4096), 512);
        // Straddle a sector boundary.
        let mut got = [0u8; 64];
        cache.read_at(512 - 32, &mut got).unwrap();
        let want: Vec<u8> = (480..544).map(|i| (i % 251) as u8).collect();
        assert_eq!(&got[..], &want[..]);

        cache.write_at(512 - 32, &[0xAA; 64]).unwrap();
        let mut back = [0u8; 64];
        cache.read_at(512 - 32, &mut back).unwrap();
        assert_eq!(back, [0xAA; 64]);

        // Not yet necessarily on the device; flush persists it.
        cache.flush().unwrap();
        let dev = cache.into_inner();
        assert_eq!(&dev.as_slice()[480..544], &[0xAA; 64]);
    }

    #[test]
    fn eviction_writes_back_dirty_victims() {
        let cache = SectorCache::new(image(64 * 512), 512);
        // Dirty more sectors than there are slots.
        for s in 0..(SLOTS as u64 + 4) {
            cache.write_at(s * 512, &[s as u8; 8]).unwrap();
        }
        // Every write must survive eviction churn.
        for s in 0..(SLOTS as u64 + 4) {
            let mut b = [0u8; 8];
            cache.read_at(s * 512, &mut b).unwrap();
            assert_eq!(b, [s as u8; 8], "sector {s}");
        }
        cache.flush().unwrap();
        let dev = cache.into_inner();
        for s in 0..(SLOTS as u64 + 4) {
            assert_eq!(
                &dev.as_slice()[(s as usize) * 512..(s as usize) * 512 + 8],
                &[s as u8; 8],
                "sector {s} on device"
            );
        }
    }

    #[test]
    fn through_paths_stay_coherent_with_slots() {
        let cache = SectorCache::new(image(8192), 512);
        // Dirty a cached sector, then read it via the through-path: the
        // device must have been synced first.
        cache.write_at(1024, &[0x5A; 512]).unwrap();
        let mut got = [0u8; 512];
        cache.read_through(1024, &mut got).unwrap();
        assert_eq!(got, [0x5A; 512]);

        // A direct write must not be shadowed by a stale cached copy.
        cache.write_through(1024, &[0x77; 512]).unwrap();
        let mut back = [0u8; 512];
        cache.read_at(1024, &mut back).unwrap();
        assert_eq!(back, [0x77; 512]);
    }
}
