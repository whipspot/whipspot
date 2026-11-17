# Whipspot patch to cpal 0.16.0

Vendored from crates.io `cpal 0.16.0` (Apache-2.0, see `LICENSE`), wired in via
`[patch.crates-io]` in the workspace `Cargo.toml`. Removed from the upstream
tarball: `.github/`, `Cross.toml`, `Dockerfile`, `Cargo.lock`, `.cargo-ok`.

## Why

The macOS backend declares `let data_size = 0u32;` (immutable) and passes
`NonNull::from(&data_size)` to `AudioObjectGetPropertyDataSize` /
`AudioObjectGetPropertyData`, which write through it. That is undefined
behaviour. At opt-level >= 1 the compiler assumes `data_size` is still 0, so
`reserve_exact(data_size as usize)` allocates nothing and CoreAudio writes into
a dangling pointer: SIGSEGV in `Device::supported_configs` (seen in `spike
play` under `[profile.dev] opt-level = 1`).

## Diff (only these two files; no behaviour change)

- `src/host/coreaudio/macos/mod.rs`: every `let data_size = ...u32;` ->
  `let mut data_size = ...;` and every `NonNull::from(&data_size)` ->
  `NonNull::from(&mut data_size)`.
- `src/host/coreaudio/macos/enumerate.rs`: same change.

Verify: `diff -ru <registry>/cpal-0.16.0/src vendor/cpal/src`.

## Upstream

cpal 0.17.0 (checked 0.17.0, 0.17.3, 0.18.2 sources) already uses
`let mut data_size` + `&mut data_size` at the size-query sites that caused the
crash (`supported_configs`, `enumerate::audio_devices`). A few in/out sites
(e.g. `AudioClassID` query, default input/output device lookup) are still
immutable there but the value written equals the value passed in, so they are
benign in practice. Drop this vendor copy when the dependency can move to
cpal >= 0.17.
