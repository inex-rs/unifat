#![no_main]
//! Mount arbitrary bytes, then drive the write paths; must not panic, hang, or OOM.

use libfuzzer_sys::fuzz_target;
use unifat::io::{Seek, SeekFrom, Write};
use unifat::{MemBlockDevice, Volume};

const FILES: [&str; 3] = ["\\a.bin", "\\d\\c.bin", "\\d\\d.bin"];
const DIRS: [&str; 2] = ["\\d", "\\d\\e"];

fuzz_target!(|data: &[u8]| {
    let Ok(vol) = Volume::mount(MemBlockDevice::from_slice(data)) else {
        return;
    };
    for &d in &DIRS {
        let _ = vol.create_dir_all(d);
    }
    for &p in &FILES {
        let _ = vol.create(p);
        if let Ok(mut f) = vol.open_rw(p) {
            let _ = f.seek(SeekFrom::Start(4096));
            let _ = f.write_all(b"payload");
            let _ = f.set_len(1);
        }
        let _ = vol.set_modified(p, unifat::EPOCH);
    }
    let _ = vol.rename(FILES[0], FILES[1]);
    let _ = vol.remove_file(FILES[2]);
    let _ = vol.remove_dir_all(DIRS[0]);
    let _ = vol.flush();
});
