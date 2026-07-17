//! Path-pure FAT directory access (no ambient cwd).
//!
//! A [`FatDirectory`] is an ephemeral view of one directory identified by a
//! [`FatDir`] region. It is created at the start of a VFS method and dropped
//! before return — never stored on [`FatVfs`]. All read and write operations
//! take an explicit directory; the volume does not track a cwd.

use alloc::string::String;
use alloc::vec::Vec;

use embedded_io::{Read, Seek, Write};

use crate::codec::FixedCodec;
use crate::dir::Metadata;
use crate::error::{CorruptKind, FsError, FsResult};
use crate::name::NameEq;
use crate::path::{Path, PathBuf, WindowsComponent, path_consts};
use crate::vfs::{Directory, EntryPatch, NewEntry};

use super::direntry::{DIRENTRY_SIZE, MinProperties, RawAttributes, RawDirEntry};
use super::{Ebr, FatDir, FatSlotIter, FatVfs, RawProperties};

/// Ephemeral path-pure directory handle.
#[derive(Debug)]
pub(crate) struct FatDirectory<'a, S>
where
    S: Read + Write + Seek,
{
    fs: &'a FatVfs<S>,
    anchor: FatDir,
    /// Normalized absolute path of this directory (for building child paths).
    path: PathBuf,
}

impl<'a, S> FatDirectory<'a, S>
where
    S: Read + Write + Seek,
{
    /// Root directory of the volume.
    pub(crate) fn root(fs: &'a FatVfs<S>) -> Self {
        let anchor = match &fs.boot_record.borrow().ebr {
            Ebr::Fat16(_) => FatDir::FixedRoot,
            Ebr::Fat32(ebr, _) => FatDir::Clusters(ebr.root_cluster),
        };
        Self {
            fs,
            anchor,
            path: PathBuf::from(path_consts::SEPARATOR_STR),
        }
    }

    #[inline]
    pub(crate) fn anchor(&self) -> FatDir {
        self.anchor
    }

    #[inline]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Resolve `path` (absolute or relative-from-root after normalize) to a
    /// directory by walking from the root.
    pub(crate) fn open_path(fs: &'a FatVfs<S>, path: &Path) -> FsResult<Self, S::Error> {
        if !path.is_valid() {
            return Err(FsError::InvalidInput);
        }
        let normalized = path.normalize();
        let mut dir = Self::root(fs);
        for component in normalized.components() {
            match component {
                WindowsComponent::RootDir => {
                    dir = Self::root(fs);
                }
                WindowsComponent::CurDir => {}
                WindowsComponent::ParentDir => {
                    // Clamp at root: resolve parent by walking path string.
                    if let Some(parent) = dir.path.parent() {
                        dir = Self::open_path(fs, parent)?;
                    }
                }
                WindowsComponent::Normal(name) => {
                    let entry = dir.lookup(name)?.ok_or(FsError::NotFound)?;
                    if !entry.is_dir {
                        return Err(FsError::NotADirectory);
                    }
                    let child_path = dir.path.join(name);
                    dir = Self {
                        fs,
                        anchor: FatDir::Clusters(entry.data_cluster),
                        path: child_path,
                    };
                }
            }
        }
        Ok(dir)
    }

    /// Scan this directory for `name` using the volume name policy.
    pub(crate) fn lookup(&self, name: &str) -> FsResult<Option<RawProperties>, S::Error> {
        self.lookup_with(name, &self.fs.name_policy)
    }

    /// Lookup with an explicit name-equality policy.
    pub(crate) fn lookup_with(
        &self,
        name: &str,
        eq: &dyn NameEq,
    ) -> FsResult<Option<RawProperties>, S::Error> {
        for entry in self.iter_raw() {
            let entry = entry?;
            if eq.names_equal(&entry.name, name) {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    /// Internal entry iterator (includes `.` / `..`).
    pub(crate) fn iter_raw(&self) -> FatSlotIter<'a, S> {
        FatSlotIter::new(self.fs, self.anchor)
    }

    fn metadata_of(raw: &RawProperties) -> Metadata {
        Metadata {
            len: u64::from(raw.file_size),
            is_dir: raw.is_dir,
            attributes: raw.attributes.into(),
            created: raw.created,
            modified: Some(raw.modified),
            accessed: raw.accessed,
        }
    }
}

impl<S> Directory for FatDirectory<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FsError<S::Error>;
    type EntryRef = RawProperties;

    fn lookup(
        &self,
        name: &str,
        eq: &dyn NameEq,
    ) -> Result<Option<(Self::EntryRef, Metadata)>, Self::Error> {
        Ok(self.lookup_with(name, eq)?.map(|raw| {
            let meta = Self::metadata_of(&raw);
            (raw, meta)
        }))
    }

    fn list_entries(&self) -> Result<Vec<(Self::EntryRef, String, Metadata)>, Self::Error> {
        let mut out = Vec::new();
        for entry in self.iter_raw() {
            let entry = entry?;
            if entry.name == path_consts::CURRENT_DIR_STR
                || entry.name == path_consts::PARENT_DIR_STR
            {
                continue;
            }
            let name = entry.name.clone();
            let meta = Self::metadata_of(&entry);
            out.push((entry, name, meta));
        }
        Ok(out)
    }

    fn insert(&mut self, entry: NewEntry) -> Result<Self::EntryRef, Self::Error> {
        if self.lookup(&entry.name)?.is_some() {
            return Err(FsError::AlreadyExists);
        }

        // ExFAT-only fields are intentionally ignored on classic FAT.
        let _ = (entry.no_fat_chain, entry.allocated_size);

        let sfn = crate::fat::gen_sfn(&entry.name, self)?;
        let now = entry.times.modified_or_epoch();
        let mut user_attrs = entry.attrs;
        if !entry.is_dir {
            // New files get the archive bit (Windows convention).
            user_attrs.archive = true;
        }
        let attrs = RawAttributes::from_attributes(user_attrs, entry.is_dir);

        // A fresh directory allocates and seeds its `.`/`..` cluster here
        // (after the duplicate check, so a failure can't leak it).
        let owns_cluster = entry.is_dir && entry.first_cluster == 0;
        let data_cluster = if owns_cluster {
            self.fs.create_dir_cluster(self.anchor, now)?
        } else {
            entry.first_cluster
        };

        // Allocate-before-link on the device: a new directory's cluster
        // (chain + zeroed `.`/`..` seed) must be durable before the entry
        // that points at it becomes visible.
        if owns_cluster {
            self.fs.metadata_barrier()?;
        }

        let size = u32::try_from(entry.size).unwrap_or(u32::MAX);
        let fat32 = self.fs.fat_type == crate::fat::FatType::FAT32;
        let props = MinProperties {
            name: entry.name.into(),
            sfn,
            attributes: attrs,
            created: entry.times.created,
            modified: now,
            accessed: entry.times.accessed,
            file_size: size,
            data_cluster,
            nt_res: 0,
            ea_handle: (!fat32).then_some(0),
        };
        match self.fs.insert_entry(&props, self.anchor) {
            Ok(chain) => Ok(RawProperties::from_chain(props, chain)),
            Err(e) => {
                if owns_cluster {
                    // Don't leak the freshly seeded directory cluster when
                    // the insert itself fails (e.g. no free slots).
                    let _ = self.fs.free_cluster_chain(data_cluster);
                }
                Err(e)
            }
        }
    }

    fn remove(&mut self, entry: &Self::EntryRef) -> Result<(), Self::Error> {
        self.fs.remove_entry_set(&entry.chain)
    }

    fn free_data(&mut self, entry: &Self::EntryRef) -> Result<(), Self::Error> {
        if entry.data_cluster >= 2 {
            self.fs.free_cluster_chain(entry.data_cluster)?;
        }
        Ok(())
    }

    fn link(&mut self, entry: &Self::EntryRef, new_name: &str) -> Result<(), Self::Error> {
        use alloc::boxed::Box;
        let now = self.fs.options.clock.now();

        // Insert the new link FIRST: the insert is the fallible step
        // (RootDirectoryFull / StorageFull / gen_sfn exhaustion), and
        // rewriting the moved directory's `..` before it would leave a
        // failed rename with `..` pointing at the destination parent.
        // A move preserves the entry's original timestamps.
        self.insert(NewEntry {
            name: String::from(new_name),
            attrs: entry.attributes.into(),
            is_dir: entry.is_dir,
            first_cluster: entry.data_cluster,
            size: u64::from(entry.file_size),
            times: crate::entry_times::EntryTimes {
                created: entry.created,
                modified: Some(entry.modified),
                accessed: entry.accessed,
            },
            no_fat_chain: None,
            allocated_size: None,
        })?;

        if entry.is_dir {
            // Repoint the moved directory's `..` (physical slot 1 per
            // spec) at this directory — after verifying the slot really
            // holds the dotdot entry, so a non-compliant volume can't
            // trick us into overwriting a real entry.
            let dotdot = self
                .fs
                .nth_slot(
                    self.fs
                        .dir_first_slot(super::FatDir::Clusters(entry.data_cluster)),
                    1,
                )?
                .ok_or(FsError::Corrupt(CorruptKind::DirEntry))?;
            let existing = self.fs.read_slot(dotdot)?;
            let is_dotdot =
                RawDirEntry::parse(&existing[..]).is_some_and(|e| e.sfn == super::PARENT_DIR_SFN);
            if !is_dotdot {
                return Err(FsError::Corrupt(CorruptKind::DirEntry));
            }
            let fat32 = self.fs.fat_type == crate::fat::FatType::FAT32;
            let parent_entry = MinProperties {
                name: Box::from(path_consts::PARENT_DIR_STR),
                sfn: super::PARENT_DIR_SFN,
                attributes: RawAttributes::DIRECTORY,
                created: Some(now),
                modified: now,
                accessed: Some(now.date()),
                file_size: 0,
                data_cluster: self.fs.parent_link_cluster(self.anchor),
                nt_res: 0,
                ea_handle: (!fat32).then_some(0),
            };
            let mut bytes = [0u8; DIRENTRY_SIZE];
            RawDirEntry::from(parent_entry).write_into(&mut bytes);
            self.fs.write_slot(dotdot, &bytes)?;
        }
        Ok(())
    }

    fn update(&mut self, entry: Self::EntryRef, patch: EntryPatch) -> Result<(), Self::Error> {
        let mut props = MinProperties::from(entry.clone());
        self.fs
            .patch_sfn_record(entry.chain, &mut props, entry.is_dir, patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FsError;
    use crate::fat::{
        CURRENT_DIR_SFN, DIRENTRY_SIZE, MinProperties, RawAttributes, RawDirEntry, SLOT_DELETED,
        SLOT_END,
    };
    use crate::options::FsOptions;
    use crate::path::path_consts;
    use crate::store::MemBlockDevice;
    use alloc::boxed::Box;
    use alloc::format;
    use time::macros::datetime;

    const FAT16: &[u8] = include_bytes!("../../tests/fixtures/fat16-3m.img");
    const FAT16_MSC: &[u8] = include_bytes!("../../tests/fixtures/fat16-msc-8m.img");

    fn mount_msc() -> FatVfs<MemBlockDevice> {
        FatVfs::new(MemBlockDevice::from_slice(FAT16_MSC), FsOptions::default())
            .expect("mount fat16-msc")
    }

    fn mount_fat16() -> FatVfs<MemBlockDevice> {
        FatVfs::new(MemBlockDevice::from_slice(FAT16), FsOptions::default())
            .expect("mount fat16-3m")
    }

    /// Stamp every fixed-root slot as used, then create must yield `RootDirectoryFull`.
    #[test]
    fn root_directory_full_after_filling_slots() {
        let fs = mount_fat16();
        let root = FatDirectory::root(&fs);
        assert_eq!(
            root.anchor(),
            FatDir::FixedRoot,
            "fixture must be FAT16 fixed root"
        );

        let now = datetime!(2020-01-01 0:00);
        let mut pos = fs.dir_first_slot(FatDir::FixedRoot);
        let mut n = 0u32;
        loop {
            // Stamp every free / end slot with a dummy used SFN.
            if matches!(fs.read_slot(pos).expect("read")[0], SLOT_DELETED | SLOT_END) {
                let props = MinProperties {
                    name: Box::from(format!("F{n:05}")),
                    sfn: CURRENT_DIR_SFN, // any non-free first byte; uniqueness not required
                    attributes: RawAttributes::ARCHIVE,
                    created: Some(now),
                    modified: now,
                    accessed: Some(now.date()),
                    file_size: 0,
                    data_cluster: 0,
                    nt_res: 0,
                    ea_handle: Some(0),
                };
                let mut bytes = [0u8; DIRENTRY_SIZE];
                RawDirEntry::from(props).write_into(&mut bytes);
                // Ensure the first byte is a printable SFN char, not 0x00/0xE5.
                bytes[0] = b'A'.wrapping_add((n % 26) as u8);
                fs.write_slot(pos, &bytes).expect("stamp slot");
                n += 1;
            }

            match fs.next_slot(pos).expect("next") {
                Some(next) => pos = next,
                None => break,
            }
        }
        assert!(n > 0, "should have filled some free root slots");

        let err = crate::vfs::PathEngine::create_empty_file(&fs, Path::new("/ZZZ.TXT"))
            .expect_err("root must be full");
        assert!(
            matches!(err, FsError::RootDirectoryFull),
            "expected RootDirectoryFull, got {err:?}"
        );
    }

    /// Directory trait veneer: lookup + list_entries on a path-pure handle.
    #[test]
    fn directory_trait_lookup_and_iter() {
        use crate::vfs::Directory;

        let fs = mount_msc();
        crate::vfs::PathEngine::create_dir(&fs, Path::new("/trait_dir")).expect("mkdir");
        crate::vfs::PathEngine::create_empty_file(&fs, Path::new("/trait_dir/a.txt"))
            .expect("file");
        {
            let mut f = fs.get_rw_file("/trait_dir/a.txt").expect("open rw");
            use embedded_io::Write;
            f.write_all(b"hi").expect("write");
        }

        let dir = FatDirectory::open_path(&fs, Path::new("/trait_dir")).expect("open");
        let (raw, meta) = Directory::lookup(&dir, "a.txt", &fs.name_policy)
            .expect("lookup")
            .expect("found");
        assert!(!meta.is_dir());
        assert_eq!(raw.name, "a.txt");

        let entries = Directory::list_entries(&dir).expect("iter");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, "a.txt");

        // Directory::update rewrites SFN size / times without a StreamFile handle.
        let mut dir = FatDirectory::open_path(&fs, Path::new("/trait_dir")).expect("reopen");
        let (raw, _) = Directory::lookup(&dir, "a.txt", &fs.name_policy)
            .expect("lookup")
            .expect("found");
        Directory::update(
            &mut dir,
            raw,
            crate::vfs::EntryPatch {
                size: Some(2),
                ..Default::default()
            },
        )
        .expect("update size");
        let (_, meta) = Directory::lookup(&dir, "a.txt", &fs.name_policy)
            .expect("lookup after")
            .expect("found");
        assert_eq!(meta.len, 2);
    }

    /// After a cross-directory rename, the moved directory's `..` entry must
    /// point at the new parent cluster (0 when the new parent is the FAT16 root).
    /// Nested file LFNs must also survive the move.
    #[test]
    fn rename_dir_rewrites_dotdot_cluster() {
        use embedded_io::Write;

        let fs = mount_msc();
        use crate::vfs::PathEngine;
        PathEngine::create_dir(&fs, Path::new("/src")).expect("src");
        PathEngine::create_dir(&fs, Path::new("/src/moved")).expect("src/moved");
        PathEngine::create_dir(&fs, Path::new("/dst")).expect("dst");
        PathEngine::create_empty_file(&fs, Path::new("/src/moved/payload.bin"))
            .expect("nested file");
        {
            let mut f = fs.get_rw_file("/src/moved/payload.bin").expect("open rw");
            f.write_all(b"nested").expect("write");
        }

        // Parent of /src/moved is /src (cluster-backed). Capture that cluster.
        let src_dir = FatDirectory::open_path(&fs, Path::new("/src")).expect("open src");
        let src_cluster = match src_dir.anchor() {
            FatDir::Clusters(c) => c,
            FatDir::FixedRoot => panic!("/src should not be fixed root"),
        };

        // Before rename, `..` of moved should point at src.
        let moved = FatDirectory::open_path(&fs, Path::new("/src/moved")).expect("open moved");
        let mut found_parent = None;
        for entry in moved.iter_raw() {
            let entry = entry.expect("entry");
            if entry.name == path_consts::PARENT_DIR_STR {
                found_parent = Some(entry.data_cluster);
                break;
            }
        }
        assert_eq!(
            found_parent,
            Some(src_cluster),
            "`..` should point at /src before rename"
        );

        PathEngine::rename(&fs, Path::new("/src/moved"), Path::new("/dst/moved"))
            .expect("rename into /dst");

        let dst_dir = FatDirectory::open_path(&fs, Path::new("/dst")).expect("open dst");
        let dst_cluster = match dst_dir.anchor() {
            FatDir::Clusters(c) => c,
            FatDir::FixedRoot => panic!("/dst should not be fixed root"),
        };

        let moved = FatDirectory::open_path(&fs, Path::new("/dst/moved")).expect("open after");
        let mut found_parent = None;
        for entry in moved.iter_raw() {
            let entry = entry.expect("entry");
            if entry.name == path_consts::PARENT_DIR_STR {
                found_parent = Some(entry.data_cluster);
                break;
            }
        }
        assert_eq!(
            found_parent,
            Some(dst_cluster),
            "`..` must be rewritten to /dst after cross-dir rename"
        );

        // Nested LFN entry must still resolve by long name.
        let nested = moved
            .lookup("payload.bin")
            .expect("lookup")
            .expect("nested file present");
        assert_eq!(nested.name, "payload.bin");

        // Rename onto FAT16 fixed root → parent cluster must be 0.
        PathEngine::rename(&fs, Path::new("/dst/moved"), Path::new("/at_root"))
            .expect("rename to root parent");
        let at_root = FatDirectory::open_path(&fs, Path::new("/at_root")).expect("open at_root");
        let mut found_parent = None;
        for entry in at_root.iter_raw() {
            let entry = entry.expect("entry");
            if entry.name == path_consts::PARENT_DIR_STR {
                found_parent = Some(entry.data_cluster);
                break;
            }
        }
        assert_eq!(
            found_parent,
            Some(0),
            "`..` cluster must be 0 when parent is FAT16 fixed root"
        );
    }
}
