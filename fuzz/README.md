# unifat fuzz targets

Coverage-guided fuzzing via [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz)
(libFuzzer). Requires a nightly toolchain and `cargo install cargo-fuzz`.

| target  | drives                                                              |
|---------|--------------------------------------------------------------------|
| `mount` | `Volume::mount` + full tree walk on arbitrary bytes (read parsers) |
| `ops`   | arbitrary op stream (create/write/truncate/rename/lock/…) on a real image |
| `write` | mount arbitrary bytes, then write paths (open_rw/create/remove/rename/truncate) — exercises code that trusts on-disk cluster/length geometry |
| `codec` | byte-in round-trip of the pure codec layer (`--features __fuzz`)   |

```sh
cargo +nightly fuzz run mount
cargo +nightly fuzz run ops
cargo +nightly fuzz run write   # seed corpus with fixtures first (see below)
cargo +nightly fuzz run codec
```

`mount` and `write` find far more by mutating a *mountable* image than
noise, so seed their corpora from the fixtures:

```sh
mkdir -p corpus/write && cp ../tests/fixtures/*.img corpus/write/
```

The `codec` target reaches the private codec layer through the crate's
`__fuzz` feature (`#[doc(hidden) pub mod fuzz]`) — unstable, not public API.
Seed the corpus with the images under `../tests/fixtures/` for a fast start.
