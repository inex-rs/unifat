//! ExFAT backend.
//!
//! ExFAT differs enough from FAT12/16/32 to warrant its own
//! [`ExfatVfs`] type: cluster usage is tracked by an allocation
//! bitmap (one bit per cluster), with FAT chains consulted only for
//! fragmented (`NoFatChain=0`) files; directory entries are fixed
//! 32-byte primary/secondary sets covered by a `SetChecksum`; and filenames
//! are stored inline as UCS-2 across File Name entries.

use alloc::vec;

use crate::error::{CorruptKind, FsError, FsResult};
use crate::name::NameEq;
use crate::options::FsOptions;
use embedded_io::{Read, Seek, SeekFrom, Write};

mod bitmap;
mod cluster_map;
mod compose;
mod create;
mod data_store;
mod dir_io;
mod dir_slot;
pub(crate) mod directory;
pub(crate) mod direntry;
mod fat_io;
mod open;
mod readdir;
mod resolve;
mod timestamp;
mod upcase;
mod vfs;

pub(crate) use cluster_map::ExfatClusterMap;
pub(crate) use data_store::ExfatDataStore;
pub(crate) use dir_slot::ExfatDirSlotWriter;
pub(crate) use directory::ExfatDirectory;
pub(crate) use readdir::ExfatReadDir;

/// ExFAT backend stream used by the public [`crate::File`] handle.
pub(crate) type ExfatStreamFile<'a, S> = crate::vfs::StreamFile<
    'a,
    <S as embedded_io::ErrorType>::Error,
    ExfatClusterMap<'a, S>,
    ExfatDirSlotWriter<'a, S>,
    ExfatDataStore<'a, S>,
>;

use crate::codec::exfat::boot::{EXFAT_BOOT_RECORD_SIZE, ExfatBootRecord};
use crate::codec::exfat::raw_entry::ExfatClusteredMetaEntry;
pub(crate) use direntry::ExfatAttributes;
pub(crate) use direntry::ExfatDirEntry;
pub(crate) use direntry::entry_set_checksum;
use upcase::load_upcase_table;

// On-disk constants live in the pure codec layer (re-export for submodules).
pub(crate) use crate::codec::exfat::consts::{
    END_OF_DIRECTORY, ENTRY_IN_USE, ENTRY_TYPE_ALLOCATION_BITMAP, ENTRY_TYPE_FILE,
    ENTRY_TYPE_UPCASE_TABLE, EXFAT_ENTRY_SIZE, FAT_BAD, FAT_EOC, STREAM_FLAG_ALLOCATION_POSSIBLE,
};

/// Location of a cluster-addressable blob on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClusterRange {
    pub first_cluster: u32,
    pub byte_length: u64,
}

/// `VolumeFlags` field offset in the VBR. Deliberately excluded from the
/// boot-region checksum by the spec so it can be flipped in place.
const VOLUME_FLAGS_OFFSET: u64 = 106;
/// `VolumeDirty` bit of `VolumeFlags`.
const VOLUME_DIRTY: u16 = 0x0002;
/// `PercentInUse` field offset in the VBR (checksum-excluded, like the
/// flags): 0xFF = "not available".
const PERCENT_IN_USE_OFFSET: u64 = 112;

/// A mounted ExFAT volume, exposing directory, file, and metadata APIs.
pub(crate) struct ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    cache: Option<crate::store::SectorCache<S>>,
    pub(super) boot: ExfatBootRecord,
    bitmap: ClusterRange,
    /// Mount-time name policy (upcase table for this volume).
    pub(crate) name_policy: crate::name::NamePolicy,
    /// Mount options: timestamp clock + modified-on-write gating.
    options: FsOptions,
    /// Writable opens are exclusive; read-only may be shared.
    pub(super) handles: crate::handles::HandleTable,
    /// `VolumeDirty` was already set when we mounted (unclean previous
    /// session) — never clear it; we haven't verified the volume.
    dirty_since_mount: bool,
    /// `VolumeDirty` set by this session's writes; cleared on flush.
    volume_dirty: core::cell::Cell<bool>,
}

impl<S> ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Consume the volume and yield the underlying storage back,
    /// flushing (and clearing `VolumeDirty`) first; errors during this
    /// best-effort shutdown are swallowed, matching `FatVfs::into_inner`.
    pub(crate) fn into_inner(mut self) -> S {
        let _ = self.flush();
        let cache = self.cache.take().expect("cache present until into_inner");
        cache.into_inner()
    }

    /// The live sector cache (present until `into_inner` takes it).
    fn cache(&self) -> &crate::store::SectorCache<S> {
        self.cache.as_ref().expect("cache present until into_inner")
    }

    /// Cached read of an arbitrary byte range.
    pub(crate) fn read_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<(), S::Error> {
        self.cache().read_at(offset, buf)
    }

    /// Uncached bulk read for file payload (coherent with the cache).
    pub(super) fn read_through_at(&self, offset: u64, buf: &mut [u8]) -> FsResult<(), S::Error> {
        self.cache().read_through(offset, buf)
    }

    /// Case-insensitive filename comparison using the volume name policy.
    pub(crate) fn names_equal(&self, a: &str, b: &str) -> bool {
        self.name_policy.names_equal(a, b)
    }

    /// exFAT `NameHash` of `name`, up-cased through this volume's table.
    /// Windows uses the hash to pre-filter lookups; every written entry
    /// set must carry it.
    pub(super) fn name_hash(&self, name: &str) -> u16 {
        crate::codec::exfat::name_hash(name.encode_utf16().map(|u| self.name_policy.upcase_unit(u)))
    }

    /// Mount an ExFAT volume. The storage cursor must start at byte 0
    /// of the volume (the Volume Boot Record sector).
    pub(crate) fn mount(mut storage: S, options: FsOptions) -> FsResult<Self, S::Error> {
        // Sectors can be up to 4 KiB via `sector_shift`; read the maximum,
        // looping over legal short reads.
        let mut vbr = [0u8; 4096];
        let n = crate::store::read_full(&mut storage, &mut vbr)?;
        if n < EXFAT_BOOT_RECORD_SIZE {
            return Err(FsError::Corrupt(CorruptKind::Other));
        }
        if !crate::boot::has_exfat_magic(&vbr) {
            return Err(FsError::Unsupported);
        }
        use crate::codec::FixedCodec;
        let boot = ExfatBootRecord::parse(&vbr[..EXFAT_BOOT_RECORD_SIZE])
            .ok_or(FsError::Corrupt(CorruptKind::Codec))?;
        // Reject adversarial out-of-range shifts before any size arithmetic.
        if !boot.shifts_valid() {
            log_error!("ExFAT VBR has out-of-range sector/cluster shifts");
            return Err(FsError::Unsupported);
        }

        let cluster_bytes = boot.bytes_per_cluster();
        let sector_bytes = boot.bytes_per_sector();

        // Verify the Main Boot Checksum over the first 11 sectors: the
        // 12th sector holds that `u32` repeated. A mismatch means a
        // corrupt or tampered boot region.
        {
            let region_len = 11 * sector_bytes as usize;
            let mut region = vec![0u8; 12 * sector_bytes as usize];
            storage.seek(SeekFrom::Start(0))?;
            storage.read_exact(&mut region).map_err(map_read_exact)?;
            let calc = crate::codec::exfat::boot::boot_region_checksum(&region[..region_len]);
            let stored = u32::from_le_bytes(
                region[region_len..region_len + 4]
                    .try_into()
                    .expect("4 bytes"),
            );
            if calc != stored {
                log_error!("ExFAT Main Boot Checksum mismatch");
                return Err(FsError::Corrupt(CorruptKind::BootSector));
            }
        }

        let mut bitmap: Option<ClusterRange> = None;
        let mut upcase: Option<ClusterRange> = None;
        let mut upcase_checksum: u32 = 0;
        // The spec puts the Allocation Bitmap and Up-case Table entries
        // anywhere in the root directory — walk its whole FAT chain, not
        // just the first cluster. The chain read is raw (pre-cache);
        // `cluster_count` bounds a corrupt cyclic chain.
        let mut root_cluster = Some(boot.root_dir_cluster);
        let mut walked = 0u32;
        let mut root_buf = vec![0u8; cluster_bytes as usize];
        'chain: while let Some(cluster) = root_cluster {
            if walked > boot.cluster_count || cluster < 2 {
                return Err(FsError::Corrupt(CorruptKind::ClusterChain));
            }
            walked += 1;
            let offset = cluster_to_byte_offset(
                boot.cluster_heap_offset,
                cluster,
                sector_bytes,
                cluster_bytes,
            );
            storage.seek(SeekFrom::Start(offset))?;
            storage.read_exact(&mut root_buf).map_err(map_read_exact)?;

            let mut off = 0usize;
            while off + EXFAT_ENTRY_SIZE <= root_buf.len() {
                let entry = &root_buf[off..off + EXFAT_ENTRY_SIZE];
                let ty = entry[0];
                if ty == END_OF_DIRECTORY {
                    break 'chain;
                }
                match ty {
                    ENTRY_TYPE_ALLOCATION_BITMAP | ENTRY_TYPE_UPCASE_TABLE => {
                        if let Some(meta) = ExfatClusteredMetaEntry::parse(entry) {
                            let range = ClusterRange {
                                first_cluster: meta.first_cluster,
                                byte_length: meta.data_length,
                            };
                            if ty == ENTRY_TYPE_ALLOCATION_BITMAP {
                                bitmap.get_or_insert(range);
                            } else if upcase.is_none() {
                                upcase = Some(range);
                                upcase_checksum = meta.table_checksum;
                            }
                        }
                    }
                    _ => {}
                }
                if bitmap.is_some() && upcase.is_some() {
                    break 'chain;
                }
                off += EXFAT_ENTRY_SIZE;
            }

            // Follow the root's FAT chain (raw read; the cache doesn't
            // exist yet).
            let fat_pos =
                u64::from(boot.fat_offset) * u64::from(sector_bytes) + u64::from(cluster) * 4;
            let mut cell = [0u8; 4];
            storage.seek(SeekFrom::Start(fat_pos))?;
            storage.read_exact(&mut cell).map_err(map_read_exact)?;
            let next = u32::from_le_bytes(cell);
            root_cluster = (next >= 2 && next <= boot.cluster_count + 1).then_some(next);
        }

        let bitmap = bitmap.ok_or(FsError::Corrupt(CorruptKind::Other))?;
        let upcase = upcase.ok_or(FsError::Corrupt(CorruptKind::Other))?;

        // R10: the bitmap's location comes from an un-checksummed root
        // entry; validate it before every allocator write trusts it.
        let last_cluster = boot.cluster_count + 1;
        let bitmap_clusters = u32::try_from(bitmap.byte_length.div_ceil(u64::from(cluster_bytes)))
            .unwrap_or(u32::MAX);
        let bitmap_ok = bitmap.first_cluster >= 2
            && bitmap.first_cluster <= last_cluster
            && bitmap.byte_length >= u64::from(boot.cluster_count.div_ceil(8))
            && u64::from(bitmap.first_cluster) + u64::from(bitmap_clusters) - 1
                <= u64::from(last_cluster);
        if !bitmap_ok {
            log_error!("ExFAT allocation-bitmap entry has out-of-range geometry");
            return Err(FsError::Corrupt(CorruptKind::Other));
        }
        // Same for the up-case table's run before it is read.
        let upcase_clusters = u32::try_from(upcase.byte_length.div_ceil(u64::from(cluster_bytes)))
            .unwrap_or(u32::MAX);
        let upcase_ok = upcase.first_cluster >= 2
            && upcase.first_cluster <= last_cluster
            && upcase.byte_length > 0
            && u64::from(upcase.first_cluster) + u64::from(upcase_clusters) - 1
                <= u64::from(last_cluster);
        if !upcase_ok {
            log_error!("ExFAT up-case table entry has out-of-range geometry");
            return Err(FsError::Corrupt(CorruptKind::Other));
        }

        let upcase_lookup = load_upcase_table(&mut storage, &boot, upcase, upcase_checksum)?;

        let dirty_since_mount = boot.flags & VOLUME_DIRTY != 0;
        let sector_size = boot.bytes_per_sector();
        Ok(Self {
            cache: Some(crate::store::SectorCache::new(storage, sector_size)),
            boot,
            bitmap,
            name_policy: crate::name::NamePolicy::upcase(upcase_lookup),
            options,
            handles: crate::handles::HandleTable::new(),
            dirty_since_mount,
            volume_dirty: core::cell::Cell::new(false),
        })
    }

    /// Register a read-only open. Fails if a RW handle is outstanding.
    pub(super) fn lock_ro(&self, path: &crate::path::Path) -> FsResult<(), S::Error> {
        self.handles.lock_ro(path)
    }

    /// Register a read-write open. Exclusive.
    pub(super) fn lock_rw(&self, path: &crate::path::Path) -> FsResult<(), S::Error> {
        self.handles.lock_rw(path)
    }

    /// Stat a path without building a full directory iterator.
    pub(crate) fn lookup(
        &self,
        path: &crate::path::Path,
    ) -> FsResult<crate::dir::Metadata, S::Error> {
        use crate::dir::Metadata;

        let normalized = path.normalize();
        if normalized.file_name().is_none() {
            return Ok(Metadata::root());
        }
        let (_, entry) = self.resolve_entry(path)?;
        Ok(Metadata::from_exfat(&entry))
    }
}

/// Byte offset of `cluster` within the volume. Cluster numbering starts
/// at 2 (0 and 1 are reserved), so the heap's first cluster is index 2.
pub(super) fn cluster_to_byte_offset(
    cluster_heap_offset_sectors: u32,
    cluster: u32,
    sector_bytes: u32,
    cluster_bytes: u32,
) -> u64 {
    let heap_offset = u64::from(cluster_heap_offset_sectors) * u64::from(sector_bytes);
    heap_offset + u64::from(cluster.saturating_sub(2)) * u64::from(cluster_bytes)
}

fn map_read_exact<E>(e: embedded_io::ReadExactError<E>) -> FsError<E>
where
    E: embedded_io::Error,
{
    match e {
        embedded_io::ReadExactError::UnexpectedEof => FsError::Corrupt(CorruptKind::Other),
        embedded_io::ReadExactError::Other(inner) => FsError::Io(inner),
    }
}

impl<S> ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Cached write of an arbitrary byte range. Raises `VolumeDirty`
    /// before the first write of a session so an interruption leaves the
    /// volume marked for verification.
    pub(crate) fn write_at(&self, offset: u64, buf: &[u8]) -> FsResult<(), S::Error> {
        self.ensure_volume_dirty()?;
        self.cache().write_at(offset, buf)
    }

    /// Uncached bulk write for file payload (coherent with the cache).
    pub(super) fn write_through_at(&self, offset: u64, buf: &[u8]) -> FsResult<(), S::Error> {
        self.ensure_volume_dirty()?;
        self.cache().write_through(offset, buf)
    }

    /// Store `flags` into the VBR `VolumeFlags` field (spec-excluded from
    /// the boot checksum precisely so it can be updated in place).
    fn write_volume_flags(&self, flags: u16) -> FsResult<(), S::Error> {
        self.cache()
            .write_at(VOLUME_FLAGS_OFFSET, &flags.to_le_bytes())
    }

    /// Set `VolumeDirty` on disk once per dirty window. No-op when the
    /// volume mounted dirty (it is already flagged for verification).
    fn ensure_volume_dirty(&self) -> FsResult<(), S::Error> {
        if self.dirty_since_mount || self.volume_dirty.get() {
            return Ok(());
        }
        // Mark before the write that needs it; a failure here aborts the op.
        self.write_volume_flags(self.boot.flags | VOLUME_DIRTY)?;
        // The formatter's PercentInUse goes stale the moment we allocate;
        // 0xFF is the spec's "not available" marker (checksum-excluded).
        self.cache().write_at(PERCENT_IN_USE_OFFSET, &[0xFF])?;
        // The flag must be ON THE DEVICE before the write it guards —
        // payload goes write-through, so a cached-only flag would let
        // data land while the volume is still marked clean.
        self.sync()?;
        self.volume_dirty.set(true);
        Ok(())
    }

    /// Clear `VolumeDirty` after everything is durable. Never clears a
    /// flag that was already set at mount — this session didn't verify
    /// the volume, so the unclean marker must survive.
    fn clear_volume_dirty(&self) -> FsResult<(), S::Error> {
        if self.dirty_since_mount || !self.volume_dirty.get() {
            return Ok(());
        }
        self.write_volume_flags(self.boot.flags & !VOLUME_DIRTY)?;
        self.volume_dirty.set(false);
        Ok(())
    }

    /// Flush pending metadata within an operation. Leaves `VolumeDirty`
    /// raised; only [`Self::flush`] (the user-facing flush) lowers it.
    pub(crate) fn sync(&self) -> FsResult<(), S::Error> {
        self.cache().flush()?;
        self.cache().flush_device().map_err(FsError::Io)
    }

    /// User-facing flush: make everything durable, then lower
    /// `VolumeDirty` and flush again so the clean marker persists.
    pub(crate) fn flush(&self) -> FsResult<(), S::Error> {
        self.sync()?;
        self.clear_volume_dirty()?;
        self.sync()
    }
}

impl<S> Drop for ExfatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Best-effort flush so a dropped-without-flush volume is not left
    /// permanently `VolumeDirty` (this driver never clears a dirty flag
    /// it inherits, so Windows would demand a scan forever). Errors are
    /// swallowed — use [`Self::flush`]/`into_inner` to observe them.
    fn drop(&mut self) {
        if self.cache.is_some() {
            let _ = self.flush();
        }
    }
}
