//! # unifat
//!
//! A `no_std`-friendly FAT16/32 + ExFAT filesystem driver for Rust,
//! behind one unified, `std::fs`-flavoured API.
//!
//! [`Volume::mount`] sniffs the boot sector of any [`embedded_io::Read`] +
//! `Write` + `Seek` storage, so callers never branch on FAT vs ExFAT.
//! [`Volume`] exposes the usual path operations (open, create, read_dir,
//! metadata, create_dir_all, remove, rename); [`File`] implements the
//! embedded-io I/O traits, persisting metadata on flush and on drop.
//! Bridge into [`std::io`] with the
//! [`embedded-io-adapters`](https://crates.io/crates/embedded-io-adapters)
//! crate on hosted platforms.
//!
//! ## Example
//!
//! ```
//! use unifat::io::Write; // re-exported `embedded_io`
//! use unifat::{MemBlockDevice, Volume};
//!
//! # fn main() -> Result<(), unifat::FsError<unifat::MemError>> {
//! // Any embedded_io::{Read, Write, Seek} storage works; a bundled test
//! // image keeps this example self-contained and runnable.
//! let image = include_bytes!("../tests/fixtures/exfat-1m.img");
//! let vol = Volume::mount(MemBlockDevice::from_slice(image))?; // FAT or ExFAT
//!
//! vol.create_dir_all("saves/slot1")?;
//! let mut file = vol.create("saves/slot1/game.sav")?;
//! file.write_all(b"hello")?;
//! drop(file); // metadata persists on drop (or call `file.flush()`)
//!
//! for entry in vol.read_dir("saves/slot1")? {
//!     let entry = entry?;
//!     println!("{} ({} bytes)", entry.file_name(), entry.metadata().len());
//! }
//! # Ok(())
//! # }
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![deny(deprecated)]
#![deny(macro_use_extern_crate)]
#![deny(private_bounds)]
#![deny(private_interfaces)]
#![deny(unsafe_op_in_unsafe_fn)]
#![warn(missing_copy_implementations)]
#![deny(missing_debug_implementations)]
#![deny(missing_docs)]
#![warn(non_ascii_idents)]
#![warn(trivial_numeric_casts)]
#![warn(single_use_lifetimes)]
#![warn(unused_import_braces)]
#![warn(unused_lifetimes)]
#![warn(clippy::absurd_extreme_comparisons)]
#![warn(clippy::derive_partial_eq_without_eq)]
#![warn(clippy::cast_lossless)]
#![warn(clippy::cast_possible_truncation)]
#![warn(clippy::cast_possible_wrap)]
#![warn(clippy::cast_precision_loss)]
#![warn(clippy::cast_sign_loss)]
#![warn(clippy::redundant_clone)]

extern crate alloc;

#[macro_use]
mod log_macros;

mod attrs;
mod boot;
mod codec;
mod dir;
mod entry_times;
mod error;
mod exfat;
mod fat;
mod file;
#[cfg(feature = "__fuzz")]
#[doc(hidden)]
pub mod fuzz;
mod handles;
mod mbr;
mod name;
mod options;
mod path;
mod store;
mod time;
mod vfs;
mod volume;

pub use embedded_io as io;

pub use attrs::Attributes;
pub use dir::{DirEntry, Metadata, ReadDir};
pub use error::{CorruptKind, FileError, FsError, FsResult};
pub use file::File;
pub use mbr::{Partition, PartitionEntry, PartitionError, PartitionKind, PartitionTable};
pub use options::FsOptions;
pub use path::{Ancestors, Components, Path, PathBuf, WindowsComponent};
pub use store::{MemBlockDevice, MemError};
pub use time::{Clock, DefaultClock, EPOCH};
pub use volume::{Format, Volume};
