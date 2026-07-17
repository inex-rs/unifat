//! Format-agnostic VFS: path prep, directory/stream abstractions.
//!
//! Concrete backends are `crate::fat::FatVfs` and `crate::exfat::ExfatVfs`.

mod backend;
mod cluster_map;
mod data_store;
mod dir_slot;
mod directory;
mod engine;
mod path_ops;
mod stream;

pub(crate) use backend::{OpenFlags, VfsBackend};
pub(crate) use cluster_map::{ClusterMap, ExtendResult, FreeTailResult};
pub(crate) use data_store::DataStore;
pub(crate) use dir_slot::DirSlotWriter;
pub(crate) use directory::{Directory, EntryPatch, NewEntry};
pub(crate) use engine::PathEngine;
pub(crate) use path_ops::prepare_path;
pub(crate) use stream::{StreamFile, StreamInit};
