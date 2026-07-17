//! FAT directory (de)serialization: reading entry sets into
//! [`RawProperties`] and composing new LFN + SFN slot runs.

use super::*;

#[cfg(not(feature = "std"))]
use alloc::{string::String, vec::Vec};

use crate::FsResult;
use crate::fat::*;

use crate::codec::FixedCodec;
use embedded_io::*;

const LAST_LFN_ENTRY_MASK: u8 = 0x40;
const LONG_ENTRY_TYPE: u8 = 0;

pub(crate) use crate::codec::fat::lfn::{CHARS_PER_LFN_ENTRY, LFN_CHAR_LIMIT, LfnEntry};

/// Cap on slots examined in one listing — a crafted cyclic cluster chain
/// would otherwise loop forever. No real directory approaches 2^20 slots.
const MAX_DIR_SLOTS: u32 = 1 << 20;

/// Build the on-disk slot run for one entry: the LFN slots (highest order
/// first, per spec) followed by the SFN slot. A name that fits 8.3 with no
/// case conflict yields the SFN slot only.
pub(crate) fn compose_entry(props: &MinProperties) -> Vec<[u8; DIRENTRY_SIZE]> {
    let mut sfn_bytes = [0u8; DIRENTRY_SIZE];
    RawDirEntry::from(props.clone()).write_into(&mut sfn_bytes);

    let needs_lfn = !crate::fat::as_sfn(&props.name).is_some_and(|sfn| sfn == props.sfn);
    if !needs_lfn {
        return alloc::vec![sfn_bytes];
    }

    let checksum = props.sfn.gen_checksum();
    let units: Vec<u16> = props.name.encode_utf16().collect();
    let groups = units.chunks(CHARS_PER_LFN_ENTRY);
    let group_count = groups.len();

    // Emit LFN slots highest-order-first so they precede the SFN on disk.
    let mut slots = Vec::with_capacity(group_count + 1);
    for (i, group) in groups.enumerate().rev() {
        let order = u8::try_from(i + 1).unwrap_or(u8::MAX);
        let is_last = i + 1 == group_count;
        let mut bytes = [0u8; DIRENTRY_SIZE];
        lfn_slot(group, order, is_last, checksum).write_into(&mut bytes);
        slots.push(bytes);
    }
    slots.push(sfn_bytes);
    slots
}

/// Assemble one Long-File-Name entry from up to 13 UCS-2 units. Per
/// spec a short final run is NUL-terminated and then 0xFFFF-padded
/// (strict readers and chkdsk flag zero padding).
fn lfn_slot(units: &[u16], order: u8, is_last: bool, checksum: u8) -> LfnEntry {
    let mut chars = [0xFFu8; CHARS_PER_LFN_ENTRY * 2];
    for (i, &unit) in units.iter().enumerate() {
        chars[i * 2..i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
    }
    if units.len() < CHARS_PER_LFN_ENTRY {
        chars[units.len() * 2] = 0;
        chars[units.len() * 2 + 1] = 0;
    }
    LfnEntry {
        order: if is_last {
            order | LAST_LFN_ENTRY_MASK
        } else {
            order
        },
        first_chars: chars[..10].try_into().unwrap(),
        _lfn_attribute: RawAttributes::LFN.bits(),
        _long_entry_type: LONG_ENTRY_TYPE,
        checksum,
        mid_chars: chars[10..22].try_into().unwrap(),
        _zeroed: [0, 0],
        last_chars: chars[22..].try_into().unwrap(),
    }
}

/// Streaming reader over a directory's slots, coalescing LFN + SFN sets into
/// [`RawProperties`]. `.`/`..` and volume-label entries are surfaced/skipped
/// exactly as the volume stores them.
#[derive(Debug)]
pub(crate) struct FatSlotIter<'a, S>
where
    S: Read + Write + Seek,
{
    fs: &'a FatVfs<S>,
    /// Next slot to read, or `None` once exhausted.
    cursor: Option<SlotPos>,
    /// Long-name pieces gathered so far, in on-disk (reverse) order.
    lfn_pieces: Vec<String>,
    lfn_checksum: Option<u8>,
    /// Expected ordinal of the NEXT LFN slot (sets count down to 1);
    /// 0 = no set in progress.
    next_ord: u8,
    /// Start + length of the set currently being assembled.
    set_start: Option<SlotPos>,
    set_len: EntryCount,
    slots_seen: u32,
}

/// Outcome of folding one LFN slot into the pending set.
enum LfnStep {
    /// The slot continued the current set in sequence.
    Continued,
    /// The slot carried the last-entry flag: a new set starts here.
    StartedNew,
    /// The slot is orphaned/malformed; the pending set is garbage.
    Orphan,
}

impl<'a, S> FatSlotIter<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn new(fs: &'a FatVfs<S>, dir: FatDir) -> Self {
        Self {
            fs,
            cursor: Some(fs.dir_first_slot(dir)),
            lfn_pieces: Vec::with_capacity(LFN_CHAR_LIMIT.div_ceil(CHARS_PER_LFN_ENTRY)),
            lfn_checksum: None,
            next_ord: 0,
            set_start: None,
            set_len: 0,
            slots_seen: 0,
        }
    }

    /// Drop any half-assembled long-name state.
    fn reset_set(&mut self) {
        self.lfn_pieces.clear();
        self.lfn_checksum = None;
        self.next_ord = 0;
        self.set_start = None;
        self.set_len = 0;
    }
}

impl<S> Iterator for FatSlotIter<'_, S>
where
    S: Read + Write + Seek,
{
    type Item = FsResult<RawProperties, S::Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            self.cursor?;
            match self.step() {
                Ok(Some(props)) => return Some(Ok(props)),
                Ok(None) => continue,
                Err(e) => {
                    self.cursor = None;
                    return Some(Err(e));
                }
            }
        }
    }
}

impl<S> core::iter::FusedIterator for FatSlotIter<'_, S> where S: Read + Write + Seek {}

impl<S> FatSlotIter<'_, S>
where
    S: Read + Write + Seek,
{
    /// Process one slot. Returns `Ok(Some)` when a full entry completes,
    /// `Ok(None)` when the slot was folded/skipped (the cursor has advanced or
    /// hit the end), and propagates any read error.
    fn step(&mut self) -> FsResult<Option<RawProperties>, S::Error> {
        let Some(pos) = self.cursor else {
            return Ok(None);
        };
        if self.slots_seen >= MAX_DIR_SLOTS {
            self.cursor = None;
            return Ok(None);
        }
        self.slots_seen += 1;

        let slot = self.fs.read_slot(pos)?;
        match slot[0] {
            SLOT_END => {
                self.cursor = None;
                return Ok(None);
            }
            SLOT_DELETED => {
                self.reset_set();
                self.cursor = self.fs.next_slot(pos)?;
                return Ok(None);
            }
            _ => {}
        }

        let Some(entry) = RawDirEntry::parse(&slot[..]) else {
            self.cursor = None;
            return Ok(None);
        };

        // Track the on-disk extent of the set being built.
        self.set_start.get_or_insert(pos);
        self.set_len += 1;

        if entry.attributes.bits() == RawAttributes::LFN.bits() {
            match self.consume_lfn(&slot) {
                LfnStep::Continued => {}
                // A last-flagged slot begins a fresh set at THIS slot,
                // discarding any orphaned partial before it (Windows
                // restarts the same way).
                LfnStep::StartedNew => {
                    self.set_start = Some(pos);
                    self.set_len = 1;
                }
                LfnStep::Orphan => self.reset_set(),
            }
            self.cursor = self.fs.next_slot(pos)?;
            return Ok(None);
        }

        if entry.attributes.contains(RawAttributes::VOLUME_ID) {
            self.reset_set();
            self.cursor = self.fs.next_slot(pos)?;
            return Ok(None);
        }

        let name = self.take_name(&entry);
        let created = entry.created.into();
        let accessed = entry.accessed.into();
        // Zero / undecodable write stamps are common in the wild (cameras,
        // MCU firmware). Surface the entry with the FAT epoch instead of
        // hiding it — a skipped entry is unopenable AND invisible to the
        // duplicate-SFN check, so `gen_sfn` could mint its short name twice.
        let modified = entry.modified.try_into().unwrap_or(crate::time::EPOCH);

        let chain = SlotChain {
            first: self.set_start.unwrap_or(pos),
            len: self.set_len,
        };
        self.reset_set();
        self.cursor = self.fs.next_slot(pos)?;

        // Bytes 20-21 are the cluster-high word only on FAT32; on
        // FAT12/16 they hold the OS/2/NT EA handle (nonzero on real
        // media) and must not be OR'd into the start cluster.
        let fat32 = self.fs.fat_type == crate::fat::FatType::FAT32;
        let data_cluster = if fat32 {
            (ClusterIndex::from(entry.cluster_high) << (ClusterIndex::BITS / 2))
                + ClusterIndex::from(entry.cluster_low)
        } else {
            ClusterIndex::from(entry.cluster_low)
        };
        Ok(Some(RawProperties {
            name,
            sfn: entry.sfn,
            is_dir: entry.attributes.contains(RawAttributes::DIRECTORY),
            attributes: entry.attributes,
            created,
            modified,
            accessed,
            file_size: entry.file_size,
            data_cluster,
            nt_res: entry._reserved[0],
            ea_handle: (!fat32).then_some(entry.cluster_high),
            chain,
        }))
    }

    /// Fold an LFN slot into the pending long name, validating the spec's
    /// set structure: a set opens with the `0x40`-flagged slot carrying
    /// ordinal N (1..=20), counts down contiguously to 1, and every slot
    /// shares one checksum. Violations orphan the pending set — Windows
    /// falls back to the 8.3 name for such runs rather than surfacing a
    /// garbled partial long name.
    fn consume_lfn(&mut self, slot: &[u8; DIRENTRY_SIZE]) -> LfnStep {
        let Some(lfn) = LfnEntry::parse(&slot[..]) else {
            return LfnStep::Orphan;
        };
        if !lfn.verify_signature() {
            return LfnStep::Orphan;
        }
        let ord = lfn.order & !LAST_LFN_ENTRY_MASK;
        let is_last = lfn.order & LAST_LFN_ENTRY_MASK != 0;
        // 20 slots × 13 units = 260 ≥ the 255-unit name cap.
        const MAX_LFN_SLOTS: u8 = 20;

        if is_last {
            if ord == 0 || ord > MAX_LFN_SLOTS {
                self.reset_set();
                return LfnStep::Orphan;
            }
            // A new set begins here, discarding any orphaned partial.
            self.lfn_pieces.clear();
            self.lfn_checksum = Some(lfn.checksum);
            self.next_ord = ord;
        } else {
            // Continuation: must be in-sequence and checksum-consistent.
            if self.next_ord < 2 || ord != self.next_ord - 1 {
                self.reset_set();
                return LfnStep::Orphan;
            }
            if self.lfn_checksum != Some(lfn.checksum) {
                self.reset_set();
                return LfnStep::Orphan;
            }
            self.next_ord = ord;
        }

        if let Ok(piece) = crate::fat::string_from_lfn(&lfn.utf16_units()) {
            self.lfn_pieces.push(piece);
        }
        if is_last {
            LfnStep::StartedNew
        } else {
            LfnStep::Continued
        }
    }

    /// The entry's name: the reassembled long name if the set is complete
    /// (counted down to ordinal 1) and its checksum matches the SFN, else
    /// the decoded 8.3 short name.
    fn take_name(&mut self, entry: &RawDirEntry) -> String {
        if !self.lfn_pieces.is_empty()
            && self.next_ord == 1
            && self.lfn_checksum == Some(entry.sfn.gen_checksum())
        {
            // Pieces were gathered highest-order-first; reverse to read order.
            self.lfn_pieces.iter().rev().cloned().collect()
        } else {
            entry.sfn.decode()
        }
    }
}
