//! Unified file stream: payload I/O + [`ClusterMap`] + [`DirSlotWriter`].

use core::cmp;

use embedded_io::{Read, Seek, SeekFrom, Write};
use time::{Date, PrimitiveDateTime};

use crate::entry_times::EntryTimes;
use crate::error::FileError;
use crate::handles::HandleTable;
use crate::path::PathBuf;
use crate::time::Clock;
use crate::vfs::{ClusterMap, DataStore, DirSlotWriter, EntryPatch};

/// Open file stream shared by FAT and ExFAT backends.
///
/// Map, writer, and store all surface [`FileError<Io>`] so `Drop` can flush.
pub(crate) struct StreamFile<'a, Io, C, W, D>
where
    Io: embedded_io::Error,
    C: ClusterMap<Error = FileError<Io>>,
    W: DirSlotWriter<Error = FileError<Io>>,
    D: DataStore<Error = FileError<Io>>,
{
    store: D,
    map: C,
    slot_writer: W,
    pos: u64,
    len: u64,
    /// ExFAT `ValidDataLength` watermark (`== len` on FAT): bytes at
    /// `valid_len..len` have no on-disk data and read back as zeros;
    /// writes past it zero-fill the gap first.
    valid_len: u64,
    first_cluster: u32,
    /// Last cluster of the chain when known. Threaded back into
    /// [`ClusterMap::extend`] so appends link from the true tail without
    /// re-walking the chain; `None` forces the map to rediscover it.
    chain_tail: Option<u32>,
    current_cluster: Option<u32>,
    current_index: u32,
    lock_key: PathBuf,
    owns_lock: bool,
    writable: bool,
    dirty: bool,
    times: EntryTimes,
    attributes: crate::attrs::Attributes,
    handles: &'a HandleTable,
    update_times: bool,
    clock: &'a dyn Clock,
    fat_size_cap: bool,
    _io: core::marker::PhantomData<Io>,
}

/// Everything a backend supplies to open a [`StreamFile`], besides the
/// three trait implementations (store / map / slot writer). Grouping the scalar
/// state and volume borrows keeps the constructor to four parameters.
pub(crate) struct StreamInit<'a> {
    pub first_cluster: u32,
    /// Logical file size (`DataLength` on ExFAT).
    pub len: u64,
    /// Initialized-prefix watermark (`ValidDataLength`; `== len` on FAT).
    pub valid_len: u64,
    pub lock_key: PathBuf,
    pub times: EntryTimes,
    /// The entry's on-disk attribute flags (files are never directories).
    pub attributes: crate::attrs::Attributes,
    pub handles: &'a HandleTable,
    pub clock: &'a dyn Clock,
    pub writable: bool,
    /// Whether this handle currently owns its entry in the lock table.
    pub owns_lock: bool,
    /// Whether writes refresh the modified/accessed stamps.
    pub update_times: bool,
    /// Whether the format caps files at 4 GiB - 1 (FAT).
    pub fat_size_cap: bool,
}

impl<'a, Io, C, W, D> StreamFile<'a, Io, C, W, D>
where
    Io: embedded_io::Error,
    C: ClusterMap<Error = FileError<Io>>,
    W: DirSlotWriter<Error = FileError<Io>>,
    D: DataStore<Error = FileError<Io>>,
{
    pub(crate) fn new(store: D, map: C, slot_writer: W, init: StreamInit<'a>) -> Self {
        Self {
            store,
            map,
            slot_writer,
            pos: 0,
            len: init.len,
            valid_len: init.valid_len.min(init.len),
            first_cluster: init.first_cluster,
            chain_tail: None,
            current_cluster: (init.first_cluster >= 2).then_some(init.first_cluster),
            current_index: 0,
            lock_key: init.lock_key,
            owns_lock: init.owns_lock,
            writable: init.writable,
            dirty: false,
            times: init.times,
            attributes: init.attributes,
            handles: init.handles,
            update_times: init.update_times,
            clock: init.clock,
            fat_size_cap: init.fat_size_cap,
            _io: core::marker::PhantomData,
        }
    }

    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Current metadata for this open file: live length and timestamps
    /// (which may have advanced through this handle) plus the entry's
    /// attribute flags. An open file is never a directory.
    pub(crate) fn metadata(&self) -> crate::dir::Metadata {
        crate::dir::Metadata {
            len: self.len,
            is_dir: false,
            attributes: self.attributes,
            created: self.times.created,
            modified: self.times.modified,
            accessed: self.times.accessed,
        }
    }

    pub(crate) fn set_owns_lock(&mut self, owns: bool) {
        self.owns_lock = owns;
    }

    fn seek_to_cluster(&mut self, nth: u32) -> Result<(), FileError<Io>> {
        if self.first_cluster < 2 {
            self.current_cluster = None;
            return Ok(());
        }
        let (mut cur, mut idx) = match self.current_cluster {
            Some(c) if self.current_index <= nth => (c, self.current_index),
            _ => (self.first_cluster, 0),
        };
        while idx < nth {
            match self.map.next(cur)? {
                Some(n) => {
                    cur = n;
                    idx += 1;
                }
                None => {
                    self.current_cluster = None;
                    return Ok(());
                }
            }
        }
        self.current_cluster = Some(cur);
        self.current_index = idx;
        Ok(())
    }

    /// Bytes reachable in one contiguous device access starting at the
    /// current cluster (ordinal `nth`, `avail` bytes left in it).
    ///
    /// Extends across chain links whose next cluster is physically
    /// adjacent (`cluster + 1` — `cluster_to_offset` is affine, so
    /// adjacent clusters are byte-contiguous) until `wanted` bytes are
    /// covered or the chain fragments. ExFAT `NoFatChain` runs coalesce
    /// fully; FAT chains coalesce as far as they happen to be sequential.
    /// Leaves the walk cache on the last cluster of the span, which never
    /// overshoots the next operation's ordinal.
    fn contiguous_span(
        &mut self,
        first: u32,
        nth: u32,
        mut avail: u64,
        wanted: u64,
    ) -> Result<u64, FileError<Io>> {
        let cs = u64::from(self.map.cluster_size());
        let mut last = first;
        let mut idx = nth;
        while avail < wanted {
            match self.map.next(last)? {
                Some(n) if n == last + 1 => {
                    last = n;
                    idx += 1;
                    avail += cs;
                }
                _ => break,
            }
        }
        self.current_cluster = Some(last);
        self.current_index = idx;
        Ok(avail)
    }

    fn ensure_allocation(&mut self, needed_end: u64) -> Result<(), FileError<Io>> {
        if needed_end <= self.map.allocated_len() && self.first_cluster >= 2 {
            return Ok(());
        }
        let first = (self.first_cluster >= 2).then_some(self.first_cluster);
        let res = self.map.extend(first, self.chain_tail, needed_end)?;
        self.first_cluster = res.first_cluster;
        self.chain_tail = (res.tail >= 2).then_some(res.tail);
        self.current_cluster = None;
        self.current_index = 0;
        self.dirty = true;
        Ok(())
    }

    /// Zero the byte range `[from, to)` of the stream's clusters.
    ///
    /// FAT/ExFAT have no sparse files: a write past EOF materializes the
    /// gap, and freshly allocated clusters still hold whatever bytes the
    /// previous owner left there. Without this, seek-past-EOF + write
    /// exposes deleted-file contents through the gap.
    fn zero_range(&mut self, mut from: u64, to: u64) -> Result<(), FileError<Io>> {
        const ZEROS: [u8; 512] = [0; 512];
        let cs = u64::from(self.map.cluster_size());
        while from < to {
            #[allow(clippy::cast_possible_truncation)] // cluster ordinal bounded by u32 chain
            let nth = (from / cs) as u32;
            let in_cluster = from % cs;
            self.seek_to_cluster(nth)?;
            let cluster = self.current_cluster.ok_or(FileError::Corrupt)?;
            let base = self.map.cluster_to_offset(cluster);
            let n = (cs - in_cluster).min(to - from).min(ZEROS.len() as u64);
            let chunk = usize::try_from(n).unwrap_or(ZEROS.len());
            self.store.write_at(base + in_cluster, &ZEROS[..chunk])?;
            from += n;
        }
        Ok(())
    }

    fn touch_modified(&mut self) {
        if self.update_times {
            let now = self.clock.now();
            self.times.modified = Some(now);
            self.times.accessed = Some(now.date());
            self.dirty = true;
        }
    }

    fn touch_accessed(&mut self) {
        if self.writable && self.update_times {
            let now = self.clock.now();
            self.times.accessed = Some(now.date());
            self.dirty = true;
        }
    }

    fn flush_entry(&mut self) -> Result<(), FileError<Io>> {
        if !self.dirty || !self.writable {
            return Ok(());
        }
        let patch = EntryPatch {
            size: Some(self.len),
            valid_size: Some(self.valid_len),
            first_cluster: Some(self.first_cluster),
            no_fat_chain: Some(self.map.no_fat_chain()),
            times: Some(self.times),
            attrs: None,
        };
        self.slot_writer.write_patch(patch)?;
        self.dirty = false;
        Ok(())
    }

    /// Resize the file to `size` bytes. Shrinking frees the tail
    /// clusters; growing allocates, and the new range reads as zeros
    /// (zero-filled on disk for FAT, tracked via `ValidDataLength` on
    /// ExFAT). The cursor is left unchanged.
    pub(crate) fn set_len(&mut self, size: u64) -> Result<(), FileError<Io>> {
        if !self.writable {
            return Err(FileError::ReadOnly);
        }
        match size.cmp(&self.len) {
            cmp::Ordering::Equal => Ok(()),
            cmp::Ordering::Less => {
                if self.first_cluster >= 2 {
                    let res = self.map.free_tail(self.first_cluster, size)?;
                    self.first_cluster = res.first_cluster;
                }
                self.len = size;
                self.valid_len = self.valid_len.min(size);
                self.chain_tail = None;
                self.current_cluster = None;
                self.current_index = 0;
                self.dirty = true;
                Ok(())
            }
            cmp::Ordering::Greater => {
                if self.fat_size_cap && size > u64::from(u32::MAX) {
                    return Err(FileError::FileTooLarge);
                }
                self.ensure_allocation(size)?;
                if self.fat_size_cap {
                    // FAT has no ValidDataLength concept: the on-disk
                    // size IS the readable range, so the extension must
                    // be zeroed or it would expose stale cluster data.
                    self.zero_range(self.valid_len, size)?;
                    self.valid_len = size;
                }
                self.len = size;
                self.dirty = true;
                Ok(())
            }
        }
    }

    pub(crate) fn set_created(&mut self, when: PrimitiveDateTime) -> Result<(), FileError<Io>> {
        if !self.writable {
            return Err(FileError::ReadOnly);
        }
        self.times.created = Some(when);
        self.dirty = true;
        Ok(())
    }

    pub(crate) fn set_modified(&mut self, when: PrimitiveDateTime) -> Result<(), FileError<Io>> {
        if !self.writable {
            return Err(FileError::ReadOnly);
        }
        self.times.modified = Some(when);
        self.dirty = true;
        Ok(())
    }

    pub(crate) fn set_accessed(&mut self, when: Date) -> Result<(), FileError<Io>> {
        if !self.writable {
            return Err(FileError::ReadOnly);
        }
        self.times.accessed = Some(when);
        self.dirty = true;
        Ok(())
    }
}

impl<Io, C, W, D> embedded_io::ErrorType for StreamFile<'_, Io, C, W, D>
where
    Io: embedded_io::Error,
    C: ClusterMap<Error = FileError<Io>>,
    W: DirSlotWriter<Error = FileError<Io>>,
    D: DataStore<Error = FileError<Io>>,
{
    type Error = FileError<Io>;
}

impl<Io, C, W, D> Read for StreamFile<'_, Io, C, W, D>
where
    Io: embedded_io::Error,
    C: ClusterMap<Error = FileError<Io>>,
    W: DirSlotWriter<Error = FileError<Io>>,
    D: DataStore<Error = FileError<Io>>,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        // Bytes at `valid_len..len` have no on-disk data (ExFAT
        // ValidDataLength < DataLength, e.g. Windows preallocation) and
        // must read back as zeros, never as stale cluster contents.
        if self.pos >= self.valid_len {
            let n =
                usize::try_from((self.len - self.pos).min(buf.len() as u64)).unwrap_or(buf.len());
            buf[..n].fill(0);
            self.pos += n as u64;
            self.touch_accessed();
            return Ok(n);
        }
        let cs = u64::from(self.map.cluster_size());
        #[allow(clippy::cast_possible_truncation)]
        let nth = (self.pos / cs) as u32;
        let in_cluster = self.pos % cs;
        self.seek_to_cluster(nth)?;
        // The chain ending before the valid watermark is corruption —
        // surface it instead of a silent short read.
        let cluster = self.current_cluster.ok_or(FileError::Corrupt)?;
        let base = self.map.cluster_to_offset(cluster);
        let valid_remaining = self.valid_len - self.pos;
        let wanted = (buf.len() as u64).min(valid_remaining);
        let span = self.contiguous_span(cluster, nth, cs - in_cluster, wanted)?;
        let to_read = wanted.min(span);
        let n = usize::try_from(to_read).unwrap_or(buf.len());
        self.store.read_at(base + in_cluster, &mut buf[..n])?;
        self.pos += n as u64;
        self.touch_accessed();
        Ok(n)
    }
}

impl<Io, C, W, D> Write for StreamFile<'_, Io, C, W, D>
where
    Io: embedded_io::Error,
    C: ClusterMap<Error = FileError<Io>>,
    W: DirSlotWriter<Error = FileError<Io>>,
    D: DataStore<Error = FileError<Io>>,
{
    fn write(&mut self, mut buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.writable {
            return Err(FileError::ReadOnly);
        }
        if self.fat_size_cap {
            let max = u64::from(u32::MAX);
            // The embedded-io contract forbids `Ok(0)` for a non-empty
            // buffer (`write_all` panics on it), so a cursor at or past the
            // cap must error rather than accept nothing.
            if self.pos >= max {
                return Err(FileError::FileTooLarge);
            }
            if self.pos.saturating_add(buf.len() as u64) > max {
                let allow = usize::try_from(max - self.pos).unwrap_or(usize::MAX);
                buf = &buf[..allow.min(buf.len())];
            }
        }

        let needed_end = self
            .pos
            .checked_add(buf.len() as u64)
            .ok_or(FileError::FileTooLarge)?;
        self.ensure_allocation(needed_end)?;
        // Writing past the valid watermark: zero the gap first (bytes at
        // `valid_len..pos` have no on-disk data). `len`/`valid_len` are
        // only advanced by the data write below, so a failed write leaves
        // the file unchanged.
        if self.pos > self.valid_len {
            self.zero_range(self.valid_len, self.pos)?;
        }

        let cs = u64::from(self.map.cluster_size());
        #[allow(clippy::cast_possible_truncation)]
        let nth = (self.pos / cs) as u32;
        let in_cluster = self.pos % cs;
        self.seek_to_cluster(nth)?;
        let cluster = self.current_cluster.ok_or(FileError::Corrupt)?;
        let base = self.map.cluster_to_offset(cluster);
        let span = self.contiguous_span(cluster, nth, cs - in_cluster, buf.len() as u64)?;
        // `span` may exceed `usize` on 32-bit targets; saturate — the
        // `min(buf.len())` bounds it to the real request.
        let to_write = cmp::min(buf.len(), usize::try_from(span).unwrap_or(usize::MAX));
        self.store.write_at(base + in_cluster, &buf[..to_write])?;
        self.pos += to_write as u64;
        if self.pos > self.valid_len {
            self.valid_len = self.pos;
        }
        if self.pos > self.len {
            self.len = self.pos;
        }
        self.touch_modified();
        self.dirty = true;
        Ok(to_write)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        // Chain before entry: the FAT/bitmap writes backing this file must
        // be on the device before the directory entry that references them
        // — a crash in between leaks clusters instead of leaving an entry
        // pointing at unlinked space.
        self.store.flush()?;
        self.flush_entry()?;
        self.store.flush()
    }
}

impl<Io, C, W, D> Seek for StreamFile<'_, Io, C, W, D>
where
    Io: embedded_io::Error,
    C: ClusterMap<Error = FileError<Io>>,
    W: DirSlotWriter<Error = FileError<Io>>,
    D: DataStore<Error = FileError<Io>>,
{
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        // `unsigned_abs` (not `-d as u64`): negating `i64::MIN` overflows.
        // Seeking before byte 0 is an error per the embedded-io contract.
        self.pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => {
                if d >= 0 {
                    self.pos.saturating_add(d.unsigned_abs())
                } else {
                    self.pos
                        .checked_sub(d.unsigned_abs())
                        .ok_or(FileError::InvalidSeek)?
                }
            }
            SeekFrom::End(d) => {
                if d >= 0 {
                    self.len.saturating_add(d.unsigned_abs())
                } else {
                    self.len
                        .checked_sub(d.unsigned_abs())
                        .ok_or(FileError::InvalidSeek)?
                }
            }
        };
        Ok(self.pos)
    }
}

impl<Io, C, W, D> Drop for StreamFile<'_, Io, C, W, D>
where
    Io: embedded_io::Error,
    C: ClusterMap<Error = FileError<Io>>,
    W: DirSlotWriter<Error = FileError<Io>>,
    D: DataStore<Error = FileError<Io>>,
{
    fn drop(&mut self) {
        if self.writable && self.dirty {
            // Same chain-before-entry ordering as `Write::flush`; errors
            // are unreportable here (documented on `Volume::flush`).
            let _ = Write::flush(self);
        }
        if self.owns_lock {
            if self.writable {
                self.handles.release_rw(&self.lock_key);
            } else {
                self.handles.release_ro(&self.lock_key);
            }
            self.owns_lock = false;
        }
    }
}

impl<Io, C, W, D> core::fmt::Debug for StreamFile<'_, Io, C, W, D>
where
    Io: embedded_io::Error,
    C: ClusterMap<Error = FileError<Io>>,
    W: DirSlotWriter<Error = FileError<Io>>,
    D: DataStore<Error = FileError<Io>>,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StreamFile")
            .field("pos", &self.pos)
            .field("len", &self.len)
            .field("first_cluster", &self.first_cluster)
            .field("writable", &self.writable)
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}
