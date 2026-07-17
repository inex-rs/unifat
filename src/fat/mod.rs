mod cluster_map;
mod data_store;
mod dir_slot;
mod dir_slots;
mod directory;
mod direntry;
mod entry_io;
mod fat_types;
mod fs;
mod open;
mod sector_io;
mod sfn;
mod vfs;

pub(crate) use cluster_map::FatClusterMap;
pub(crate) use data_store::FatDataStore;
pub(crate) use dir_slot::FatDirSlotWriter;
pub(crate) use directory::FatDirectory;
pub(crate) use sfn::{as_sfn, gen_sfn, string_from_lfn};

pub(crate) use crate::codec::fat::bpb::*;
pub(crate) use crate::codec::fat::sector::*;
pub(crate) use crate::codec::fat::types::*;
pub(crate) use dir_slots::*;
pub(crate) use direntry::*;
pub(crate) use fat_types::*;
pub(crate) use fs::*;

/// FAT backend stream used by the public [`crate::File`] handle.
pub(crate) type FatStreamFile<'a, S> = crate::vfs::StreamFile<
    'a,
    <S as embedded_io::ErrorType>::Error,
    FatClusterMap<'a, S>,
    FatDirSlotWriter<'a, S>,
    FatDataStore<'a, S>,
>;
