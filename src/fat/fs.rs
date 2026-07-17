use super::*;

use crate::options::FsOptions;
use crate::{error::*, path::*};

use core::{
    cell::{Cell, RefCell},
    ops,
};

use crate::codec::FixedCodec;
use embedded_io::*;

/// Version-independent geometry (sector size, cluster counts, …) computed
/// once at mount and reused for every access.
#[derive(Debug)]
pub(crate) struct FsProperties {
    pub(crate) sector_size: u16,
    pub(crate) cluster_size: u32,
    pub(crate) sec_per_clus: u8,
    pub(crate) total_sectors: SectorCount,
    /// Highest valid data-cluster index (`CountofClusters + 1`).
    pub(crate) max_cluster: ClusterCount,
    pub(crate) fat_table_count: u8,
    pub(crate) fat_sector_size: u32,
    pub(crate) first_fat_sector: u16,
    pub(crate) first_root_dir_sector: SectorIndex,
    pub(crate) first_data_sector: SectorIndex,
}

impl From<&FatBootRecord> for FsProperties {
    fn from(boot_record_fat: &FatBootRecord) -> Self {
        let sector_size = boot_record_fat.bpb.bytes_per_sector;
        let cluster_size =
            u32::from(boot_record_fat.bpb.sectors_per_cluster) * u32::from(sector_size);
        let sec_per_clus = boot_record_fat.bpb.sectors_per_cluster;
        let total_sectors = boot_record_fat.total_sectors();
        let max_cluster = boot_record_fat.max_cluster();
        let fat_table_count = boot_record_fat.bpb.table_count;
        let fat_sector_size = boot_record_fat.fat_sector_size();
        let first_fat_sector = boot_record_fat.first_fat_sector();
        let first_root_dir_sector = boot_record_fat.first_root_dir_sector();
        let first_data_sector = boot_record_fat.first_data_sector();

        FsProperties {
            sector_size,
            cluster_size,
            sec_per_clus,
            fat_table_count,
            fat_sector_size,
            first_fat_sector,
            total_sectors,
            max_cluster,
            first_root_dir_sector,
            first_data_sector,
        }
    }
}

/// A mounted FAT volume: the medium, its boot record, and cached geometry.
#[derive(Debug)]
pub(crate) struct FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Sector cache (owns storage; N-way LRU write-back).
    /// `None` only transiently inside [`Self::into_inner`], so `Drop`
    /// can skip the second unmount without any unsafe field extraction.
    cache: Option<crate::store::SectorCache<S>>,
    pub(crate) fsinfo_modified: Cell<bool>,

    pub(crate) options: FsOptions,

    /// Name equality for this volume (always ASCII case-fold for classic FAT).
    pub(crate) name_policy: crate::name::NamePolicy,

    pub(crate) boot_record: RefCell<FatBootRecord>,
    /// Cached copy of `boot_record.fat_type()` (deeply nested to derive)
    pub(crate) fat_type: FatType,
    pub(crate) props: FsProperties,
    /// Search hint: the first free cluster is at or after this index
    pub(crate) first_free_cluster: RefCell<ClusterIndex>,

    // Two live handles to one file clobber each other's offset/cluster
    // bookkeeping. Writable opens are exclusive; read-only may be shared.
    pub(crate) handles: crate::handles::HandleTable,
}

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// The live sector cache (present for the volume's whole life; taken
    /// only inside [`Self::into_inner`], after which nothing runs but a
    /// guarded `Drop`).
    #[inline]
    pub(crate) fn sector_cache(&self) -> &crate::store::SectorCache<S> {
        self.cache
            .as_ref()
            .expect("sector cache present until into_inner")
    }
}

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Case-folded handle-table key (names resolve case-insensitively).
    pub(crate) fn lock_key(&self, path: &Path) -> crate::path::PathBuf {
        self.name_policy.fold_path(path)
    }

    /// Register a read-only open. Fails if a RW handle is outstanding.
    pub(crate) fn lock_ro(&self, path: &Path) -> FsResult<(), S::Error> {
        self.handles.lock_ro(&self.lock_key(path))
    }

    /// Register a read-write open. Exclusive: fails if any other handle exists.
    pub(crate) fn lock_rw(&self, path: &Path) -> FsResult<(), S::Error> {
        self.handles.lock_rw(&self.lock_key(path))
    }
}

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Which on-disk [`Format`](crate::Format) this volume uses — always a
    /// FAT variant (`ExfatVfs` reports ExFAT separately).
    pub(crate) fn format(&self) -> crate::Format {
        match self.fat_type {
            FatType::FAT16 => crate::Format::Fat16,
            FatType::FAT32 => crate::Format::Fat32,
        }
    }
}

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Create a [`FatVfs`] from a storage object
    ///
    /// Fails if the storage is too small to hold a FAT filesystem
    pub(crate) fn new(mut storage: S, options: FsOptions) -> FsResult<Self, S::Error> {
        // the sector size is unknown yet — read the largest possible sector
        let mut buffer = [0u8; MAX_SECTOR_SIZE];

        // Loop over short reads: `Read::read` may return fewer bytes than
        // requested even mid-medium (e.g. one device block per call).
        let bytes_read = crate::store::read_full(&mut storage, &mut buffer)?;

        if bytes_read < MIN_SECTOR_SIZE {
            return Err(FsError::Corrupt(CorruptKind::Other));
        }

        // An ExFAT VBR sets `bytes_per_sector` to 0 (it uses `sector_shift`),
        // so sniff the OEM magic at bytes 3..=10 instead. `Volume::mount` does
        // this too — this guard is for callers constructing a `FatVfs` directly.
        if crate::boot::has_exfat_magic(&buffer) {
            return Err(FsError::Unsupported);
        }

        let bpb =
            FatBpb::parse(&buffer[..FAT_BPB_SIZE]).ok_or(FsError::Corrupt(CorruptKind::Codec))?;

        // Reject a bogus sector size before it is used to index `buffer`
        // (a `bytes_per_sector` > MAX_SECTOR_SIZE would slice out of bounds).
        if !crate::codec::fat::sector::is_valid_sector_size(bpb.bytes_per_sector) {
            return Err(FsError::Unsupported);
        }

        let ebr = if bpb.table_size_16 == 0 {
            let ebr_fat32 = Fat32Ebr::parse(&buffer[FAT_BPB_SIZE..FAT_BPB_SIZE + EBR_SIZE])
                .ok_or(FsError::Corrupt(CorruptKind::Codec))?;

            // With mirroring disabled the ActiveFat index selects the one
            // live copy; an index past the FAT count is inconsistent.
            if ebr_fat32.extended_flags.mirroring_disabled
                && ebr_fat32.extended_flags.active_fat >= bpb.table_count
            {
                log_error!("FAT32 ActiveFat index is out of range");
                return Err(FsError::Corrupt(CorruptKind::BootSector));
            }

            storage.seek(SeekFrom::Start(
                u64::from(ebr_fat32.fat_info) * u64::from(bpb.bytes_per_sector),
            ))?;
            storage.read_exact(&mut buffer[..usize::from(bpb.bytes_per_sector)])?;
            let fsinfo = Fat32FsInfo::parse(&buffer[..usize::from(bpb.bytes_per_sector)])
                .filter(Fat32FsInfo::verify_signature)
                .unwrap_or_else(|| {
                    // FSInfo is advisory — never fail the mount over it.
                    // Unknown hints; the next sync writes a repaired copy.
                    log_error!("FAT32 FSInfo invalid; continuing with unknown hints");
                    Fat32FsInfo::unknown()
                });

            Ebr::Fat32(ebr_fat32, fsinfo)
        } else {
            Ebr::Fat16(
                Fat16Ebr::parse(&buffer[FAT_BPB_SIZE..FAT_BPB_SIZE + EBR_SIZE])
                    .ok_or(FsError::Corrupt(CorruptKind::Codec))?,
            )
        };

        let boot_record = FatBootRecord { bpb, ebr };

        // FAT12 (sub-4085-cluster) volumes are intentionally unsupported
        let Some(fat_type) = boot_record.fat_type() else {
            log_error!("volume is FAT12 (or too small); unsupported");
            return Err(FsError::Unsupported);
        };
        log_info!("The FAT type of the filesystem is {fat_type:?}");

        if !boot_record.verify_signature() {
            log_error!("FAT boot record has invalid signature(s)");
            return Err(FsError::Corrupt(CorruptKind::BootSector));
        }

        let props = FsProperties::from(&boot_record);

        let sector_size = props.sector_size;
        // A medium that cannot even produce one BPB-declared sector of
        // boot data cannot be a valid volume.
        if usize::from(sector_size) > bytes_read {
            return Err(FsError::Corrupt(CorruptKind::BootSector));
        }
        let fs = Self {
            cache: Some(crate::store::SectorCache::new(
                storage,
                u32::from(sector_size),
            )),
            fsinfo_modified: false.into(),
            options,
            name_policy: crate::name::NamePolicy::ascii(),
            boot_record: boot_record.into(),
            fat_type,
            props,
            first_free_cluster: RESERVED_FAT_ENTRIES.into(),
            handles: crate::handles::HandleTable::new(),
        };

        Ok(fs)
    }
}

impl<S> FatVfs<S>
where
    S: Read + Write + Seek,
{
    /// Device-order write barrier: push every cached metadata write to the
    /// device now, so writes issued after this cannot land before it.
    pub(crate) fn metadata_barrier(&self) -> FsResult<(), S::Error> {
        self.sector_cache().flush()?;
        self.sector_cache().flush_device()?;
        Ok(())
    }

    /// Sync any pending changes back to the storage medium; use it to catch
    /// IO errors that [`Drop`] would swallow
    pub(crate) fn unmount(&self) -> FsResult<(), S::Error> {
        self.sync_fsinfo()?;
        self.metadata_barrier()
    }

    /// Unmount and return the underlying storage handle. Flushes pending
    /// sector-buffer and FSInfo writes first; flush errors during this
    /// best-effort shutdown are swallowed, matching
    /// `ExfatVfs::into_inner`.
    pub(crate) fn into_inner(mut self) -> S {
        let _ = self.unmount();
        let cache = self.cache.take().expect("into_inner consumes self once");
        // `self` now drops normally; `Drop` sees the empty slot and skips.
        cache.into_inner()
    }
}

impl<S> ops::Drop for FatVfs<S>
where
    S: Read + Write + Seek,
{
    fn drop(&mut self) {
        // Skip when `into_inner` already unmounted and took the cache.
        if self.cache.is_some() {
            let _ = self.unmount();
        }
    }
}
