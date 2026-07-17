#![no_main]
//! Mount arbitrary bytes and walk the tree; read-side parsers must not panic.

use core::cell::Cell;
use libfuzzer_sys::fuzz_target;
use unifat::{MemBlockDevice, Volume};

// Cap total work: a crafted image can cross-link dirs into an unbounded tree.
const OP_BUDGET: u32 = 20_000;

fn walk(vol: &Volume<MemBlockDevice>, path: &str, depth: u32, budget: &Cell<u32>) {
    if depth > 16 || budget.get() == 0 {
        return;
    }
    let Ok(rd) = vol.read_dir(path) else {
        return;
    };
    for entry in rd.take(4096) {
        if budget.get() == 0 {
            return;
        }
        budget.set(budget.get() - 1);
        let Ok(entry) = entry else { continue };
        let child = entry.path().as_str().to_string();
        let meta = entry.metadata();
        if meta.is_dir() {
            walk(vol, &child, depth + 1, budget);
        } else {
            let _ = vol.read(&child);
        }
        let _ = (meta.created(), meta.modified(), meta.accessed(), meta.len());
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(vol) = Volume::mount(MemBlockDevice::from_slice(data)) else {
        return;
    };
    walk(&vol, "\\", 0, &Cell::new(OP_BUDGET));
});
