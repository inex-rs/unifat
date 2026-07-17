//! MBR (Master Boot Record) partitioning: [`PartitionTable`] reads the
//! table, and the [`Partition`] adapter slices a block device down to
//! one partition so a [`Volume`](crate::Volume) can mount it. Only the
//! four primary partitions are parsed (EBR chains aren't followed); for
//! other layouts, construct a [`Partition`] directly with a known offset.

use embedded_io::{Read, Seek, SeekFrom, Write};

use crate::error::{CorruptKind, FsError, FsResult};

/// MBR addresses are always in 512-byte LBA units, independent of the filesystem's sector size.
const LBA_SIZE: u64 = 512;

/// On-disk boot signature (`55 AA` at bytes 510..512, little-endian).
const MBR_SIGNATURE: u16 = 0xAA55;

/// Offset of the first partition entry; four 16-byte entries follow.
const PARTITION_TABLE_OFFSET: usize = 446;

/// On-disk 16-byte primary partition table entry.
#[derive(Debug, Clone, Copy)]
struct MbrPartitionRaw {
    status: u8,
    type_byte: u8,
    start_lba: u32,
    sector_count: u32,
}

impl MbrPartitionRaw {
    fn parse(b: &[u8]) -> Self {
        Self {
            status: b[0],
            // CHS address bytes 1..4 and 5..8 are ignored (LBA only).
            type_byte: b[4],
            start_lba: u32::from_le_bytes(b[8..12].try_into().expect("16-byte entry")),
            sector_count: u32::from_le_bytes(b[12..16].try_into().expect("16-byte entry")),
        }
    }
}

/// The kind of a partition, from its MBR type byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionKind {
    /// Unused table slot (type `0x00`).
    Empty,
    /// A FAT16 partition (`0x04`, `0x06`, `0x0E`).
    Fat16,
    /// A FAT32 partition (`0x0B`, `0x0C`).
    Fat32,
    /// An ExFAT partition (`0x07`, shared with NTFS/exFAT/IFS).
    ExFat,
    /// An extended partition (`0x05`, `0x0F`) — not followed.
    Extended,
    /// Any other type byte.
    Other(u8),
}

impl PartitionKind {
    fn from_byte(byte: u8) -> Self {
        match byte {
            0x00 => PartitionKind::Empty,
            0x04 | 0x06 | 0x0E => PartitionKind::Fat16,
            0x0B | 0x0C => PartitionKind::Fat32,
            0x07 => PartitionKind::ExFat,
            0x05 | 0x0F => PartitionKind::Extended,
            other => PartitionKind::Other(other),
        }
    }

    /// Whether this kind is mountable by this library (FAT16/FAT32/ExFAT).
    pub fn is_filesystem(&self) -> bool {
        matches!(self, Self::Fat16 | Self::Fat32 | Self::ExFat)
    }
}

/// One primary partition-table entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionEntry {
    /// The partition kind (from the type byte).
    pub kind: PartitionKind,
    /// Whether the bootable/active flag (`0x80`) is set.
    pub bootable: bool,
    /// First sector of the partition, in 512-byte LBA units.
    pub start_lba: u32,
    /// Partition length, in 512-byte sectors.
    pub sector_count: u32,
}

/// The four primary partitions parsed from an MBR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionTable {
    /// The four primary entries; `None` for an unused slot.
    pub partitions: [Option<PartitionEntry>; 4],
}

impl PartitionTable {
    /// Read and parse the MBR from the start of `storage`. Fails with
    /// [`FsError::Unsupported`] if the sector isn't an MBR (missing
    /// `55 AA` signature, invalid status bytes, or a bare filesystem
    /// VBR — mount those with [`Volume::mount`](crate::Volume::mount)).
    pub fn read<S: Read + Seek>(storage: &mut S) -> FsResult<Self, S::Error> {
        let mut sector = [0u8; 512];
        storage.seek(SeekFrom::Start(0))?;
        storage.read_exact(&mut sector).map_err(map_read_exact)?;

        if u16::from_le_bytes([sector[510], sector[511]]) != MBR_SIGNATURE {
            return Err(FsError::Unsupported);
        }
        // A filesystem VBR shares the `55 AA` signature — reject bare volumes.
        if looks_like_vbr(&sector) {
            return Err(FsError::Unsupported);
        }

        let mut partitions = [None; 4];
        for (i, slot) in partitions.iter_mut().enumerate() {
            let base = PARTITION_TABLE_OFFSET + i * 16;
            let raw = MbrPartitionRaw::parse(&sector[base..base + 16]);

            // Must be 0x00 (inactive) or 0x80 (active); anything else is likely VBR boot code.
            if raw.status != 0x00 && raw.status != 0x80 {
                return Err(FsError::Unsupported);
            }

            let kind = PartitionKind::from_byte(raw.type_byte);

            if kind == PartitionKind::Empty || raw.sector_count == 0 {
                continue;
            }

            *slot = Some(PartitionEntry {
                kind,
                bootable: raw.status == 0x80,
                start_lba: raw.start_lba,
                sector_count: raw.sector_count,
            });
        }

        Ok(PartitionTable { partitions })
    }

    /// Index of the first mountable filesystem partition, if any.
    pub fn first_filesystem(&self) -> Option<usize> {
        self.partitions
            .iter()
            .position(|p| p.is_some_and(|e| e.kind.is_filesystem()))
    }
}

/// I/O error for [`Partition`]: an inner-device failure, or an access that
/// ran past the end of the partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionError<E> {
    /// The underlying storage returned an error.
    Io(E),
    /// A write was attempted at or past the partition's end; writes that
    /// straddle the end are clamped to the remaining space instead. (Reads
    /// at the end simply return `Ok(0)` — EOF — per the `Read` contract.)
    OutOfBounds,
}

impl<E: embedded_io::Error> core::fmt::Display for PartitionError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PartitionError::Io(e) => write!(f, "partition I/O error: {e}"),
            PartitionError::OutOfBounds => f.write_str("write past the end of the partition"),
        }
    }
}

impl<E: embedded_io::Error> core::error::Error for PartitionError<E> {}

impl<E: embedded_io::Error> embedded_io::Error for PartitionError<E> {
    fn kind(&self) -> embedded_io::ErrorKind {
        match self {
            PartitionError::Io(e) => e.kind(),
            PartitionError::OutOfBounds => embedded_io::ErrorKind::InvalidInput,
        }
    }
}

/// Rewrap a volume-level error so its I/O variant carries [`PartitionError`].
pub(crate) fn wrap_fs_err<E: embedded_io::Error>(e: FsError<E>) -> FsError<PartitionError<E>> {
    match e {
        FsError::Io(io) => FsError::Io(PartitionError::Io(io)),
        FsError::NotFound => FsError::NotFound,
        FsError::AlreadyExists => FsError::AlreadyExists,
        FsError::NotADirectory => FsError::NotADirectory,
        FsError::IsADirectory => FsError::IsADirectory,
        FsError::DirectoryNotEmpty => FsError::DirectoryNotEmpty,
        FsError::FileLocked => FsError::FileLocked,
        FsError::StorageFull => FsError::StorageFull,
        FsError::RootDirectoryFull => FsError::RootDirectoryFull,
        FsError::InvalidInput => FsError::InvalidInput,
        FsError::FileTooLarge => FsError::FileTooLarge,
        FsError::ReadOnlyFile => FsError::ReadOnlyFile,
        FsError::PermissionDenied => FsError::PermissionDenied,
        FsError::Corrupt(k) => FsError::Corrupt(k),
        FsError::Unsupported => FsError::Unsupported,
    }
}

/// A block device sliced down to a single partition: rebases all access
/// to the partition's byte range and clamps reads/writes to its length.
pub struct Partition<S> {
    inner: S,
    /// Byte offset of the partition within `inner`.
    base: u64,
    len: u64,
    /// Current position, relative to the partition (not `inner`).
    pos: u64,
}

impl<S> Partition<S> {
    /// Slice `inner` to `sector_count` 512-byte sectors starting at `start_lba`.
    pub fn new(inner: S, start_lba: u32, sector_count: u32) -> Self {
        Partition {
            inner,
            base: u64::from(start_lba) * LBA_SIZE,
            len: u64::from(sector_count) * LBA_SIZE,
            pos: 0,
        }
    }

    /// Slice `inner` to the region described by `entry`.
    pub fn from_entry(inner: S, entry: &PartitionEntry) -> Self {
        Self::new(inner, entry.start_lba, entry.sector_count)
    }

    /// The partition's length in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the partition is zero-length.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Consume the adapter and return the underlying device.
    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S> core::fmt::Debug for Partition<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Partition")
            .field("base", &self.base)
            .field("len", &self.len)
            .finish()
    }
}

impl<S: embedded_io::ErrorType> embedded_io::ErrorType for Partition<S> {
    type Error = PartitionError<S::Error>;
}

impl<S: Read + Seek> Read for Partition<S> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let remaining = self.len.saturating_sub(self.pos);
        if remaining == 0 || buf.is_empty() {
            return Ok(0);
        }
        let cap = usize::try_from(remaining).unwrap_or(usize::MAX);
        let want = buf.len().min(cap);
        self.inner
            .seek(SeekFrom::Start(self.base + self.pos))
            .map_err(PartitionError::Io)?;
        let n = self
            .inner
            .read(&mut buf[..want])
            .map_err(PartitionError::Io)?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<S: Write + Seek> Write for Partition<S> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        let remaining = self.len.saturating_sub(self.pos);
        if remaining == 0 {
            // Never `Ok(0)` for a non-empty buffer — `write_all` panics on it.
            return Err(PartitionError::OutOfBounds);
        }
        let cap = usize::try_from(remaining).unwrap_or(usize::MAX);
        let want = buf.len().min(cap);
        self.inner
            .seek(SeekFrom::Start(self.base + self.pos))
            .map_err(PartitionError::Io)?;
        let n = self.inner.write(&buf[..want]).map_err(PartitionError::Io)?;
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush().map_err(PartitionError::Io)
    }
}

impl<S: Seek> Seek for Partition<S> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        self.pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::Current(d) => {
                if d >= 0 {
                    self.pos.saturating_add(d.unsigned_abs())
                } else {
                    self.pos.saturating_sub(d.unsigned_abs())
                }
            }
            SeekFrom::End(d) => {
                if d >= 0 {
                    self.len.saturating_add(d.unsigned_abs())
                } else {
                    self.len.saturating_sub(d.unsigned_abs())
                }
            }
        };
        Ok(self.pos)
    }
}

/// Whether sector 0 is a filesystem boot record rather than an MBR (both
/// carry `55 AA`): the ExFAT OEM magic, or a FAT jump instruction plus a
/// plausible BPB `bytes_per_sector` — a combo MBR boot code never matches.
fn looks_like_vbr(sector: &[u8; 512]) -> bool {
    if crate::boot::has_exfat_magic(sector) {
        return true;
    }
    let jump = sector[0] == 0xEB || sector[0] == 0xE9;
    let bytes_per_sector = u16::from_le_bytes([sector[11], sector[12]]);
    jump && matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096)
}

fn map_read_exact<E: embedded_io::Error>(e: embedded_io::ReadExactError<E>) -> FsError<E> {
    match e {
        embedded_io::ReadExactError::UnexpectedEof => FsError::Corrupt(CorruptKind::Other),
        embedded_io::ReadExactError::Other(inner) => FsError::Io(inner),
    }
}
