# unifat - Unified File Allocation Table

A `no_std`-friendly **FAT16/32 + ExFAT** filesystem driver with one unified,
`std::fs`-flavoured API. Mount any `embedded_io::{Read, Write, Seek}` storage —
the boot sector is auto-detected, so your code never branches on FAT vs ExFAT.

Modern media only (SD cards, USB, embedded flash): FAT12 and OEM-codepage 8.3
names are intentionally unsupported.

```rust
use unifat::Volume;
use embedded_io::{Read, Write};

let vol = Volume::mount(storage)?;            // FAT16/32 or ExFAT, auto-detected
vol.create_dir_all("saves/slot1")?;

let mut file = vol.create("saves/slot1/game.sav")?;
file.write_all(&save_data)?;                  // flushed on drop; no lost writes

for entry in vol.read_dir("saves/slot1")? {
    let entry = entry?;
    println!("{} — {} bytes", entry.file_name(), entry.metadata().len());
}
```

A raw SD card has an MBR, so mount it by partition instead — the `Volume` is
used identically:

```rust
let vol = Volume::mount_first_partition(sd_card)?;
```

Everything hangs off [`Volume`] (mount, `read_dir`, `metadata`, `open`/`open_rw`,
`create`, `read`/`write`, `create_dir[_all]`, `remove_*`, `rename`,
`set_attributes`, `set_created`/`set_modified`/`set_accessed`, `flush`,
`into_storage`) and [`File`], which implements `embedded_io::{Read, Write, Seek}`
plus `set_len`, and persists metadata on `flush()` and `Drop`. Bridge into
`std::io` with [`embedded-io-adapters`](https://crates.io/crates/embedded-io-adapters).

FAT and ExFAT support the same operations (listing, chained + contiguous reads,
streaming writes, sub-directories, recursive delete, timestamps, attributes,
cross-directory rename, single-writer handle safety). Writes refresh the
modification stamp by default (`FsOptions::with_auto_timestamps(false)` opts
out). Case-insensitive matching is ASCII-fold on FAT and upcase-table-aware
(BMP) on ExFAT.

## Features

- `std` (default) — enables `time/local-offset`; turn off for `#![no_std]`
  (needs a global allocator).
- `logging` — forwards tracing via the `log` crate.

## Limitations

- No FAT12 (sub-4085-cluster volumes are `FsError::Unsupported`).
- `Volume` requires `Read + Write + Seek`; wrap a read-only medium in an adapter
  whose `Write` errors.
- ExFAT allocation bitmap must be contiguous (no TexFAT).
- MBR primary partitions only — no GPT/EBR; slice with [`Partition`] for other
  layouts.
- 8.3 short names are ASCII only; long file names (UCS-2) work in any language.

## License

[MPL-2.0](LICENSE).

[`Volume`]: https://docs.rs/unifat/latest/unifat/struct.Volume.html
[`File`]: https://docs.rs/unifat/latest/unifat/struct.File.html
[`Partition`]: https://docs.rs/unifat/latest/unifat/struct.Partition.html
