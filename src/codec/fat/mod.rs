//! Pure FAT on-disk layouts (bytes ↔ structs). No volume state, no paths.
//!
//! - [`bpb`]: boot sector, BPB, EBR, FSInfo
//! - [`dir_entry`] / [`sfn_layout`] / [`lfn`] / [`entry_time`]: directory records
//! - [`types`] / [`sector`]: index aliases and sector-size bounds

/// Size in bytes of every FAT directory slot (SFN and LFN alike).
pub(crate) const DIRENTRY_SIZE: usize = 32;

pub(crate) mod bpb;
pub(crate) mod dir_entry;
pub(crate) mod entry_time;
pub(crate) mod fat_entry;
pub(crate) mod lfn;
pub(crate) mod sector;
pub(crate) mod sfn_layout;
pub(crate) mod types;
