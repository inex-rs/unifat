//! FAT directory-entry write-back for open streams.

use alloc::boxed::Box;
use embedded_io::{Read, Seek, Write};

use crate::error::FileError;
use crate::fat::{FatVfs, MinProperties, RawAttributes, SlotChain};
use crate::vfs::{DirSlotWriter, EntryPatch};

/// Writes the SFN record (last of an LFN+SFN set) for one open FAT file.
pub(crate) struct FatDirSlotWriter<'a, S>
where
    S: Read + Write + Seek,
{
    fs: &'a FatVfs<S>,
    chain: SlotChain,
    is_dir: bool,
    props: MinProperties,
}

impl<'a, S> FatDirSlotWriter<'a, S>
where
    S: Read + Write + Seek,
{
    pub(crate) fn from_props(fs: &'a FatVfs<S>, props: &crate::fat::Properties) -> Self {
        Self {
            fs,
            chain: props.chain,
            is_dir: props.is_dir,
            props: MinProperties {
                name: Box::from(props.path.file_name().expect("file path has a name")),
                sfn: props.sfn,
                attributes: RawAttributes::from_attributes(props.attributes, props.is_dir),
                created: props.created,
                modified: props.modified,
                accessed: props.accessed,
                file_size: props.file_size,
                data_cluster: props.data_cluster,
                nt_res: props.nt_res,
                ea_handle: props.ea_handle,
            },
        }
    }
}

impl<S> DirSlotWriter for FatDirSlotWriter<'_, S>
where
    S: Read + Write + Seek,
{
    type Error = FileError<S::Error>;

    fn write_patch(&mut self, patch: EntryPatch) -> Result<(), FileError<S::Error>> {
        self.fs
            .patch_sfn_record(self.chain, &mut self.props, self.is_dir, patch)
            .map_err(FileError::from)
    }
}
