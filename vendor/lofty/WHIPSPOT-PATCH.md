# Whipspot patch to lofty 0.25.4

Vendored from crates.io `lofty 0.25.4` (MIT OR Apache-2.0, per its `Cargo.toml`; the
crates.io tarball ships no licence files), wired in via `[patch.crates-io]` in the workspace
`Cargo.toml`. Left out of the registry tarball: `.cargo-ok`, `Cargo.lock`. The first commit
that added this directory is the unmodified copy; the patch is the next commit.

## Why

lofty inflates an ID3v2 frame that carries the compression flag through
`flate2::read::ZlibDecoder::new(reader)` (`src/id3/v2/frame/read.rs`, `handle_compression`).
`take(size)` bounds only the compressed input, and the content parsers `read_to_end` the
decoder with no cap (`allocation_limit` is not applied there), so a few hundred KB of deflated
zeros in any audio file with an ID3v2 tag (MP3, or a WAV/AIFF ID3 chunk) allocate about 2000
times the file size. whipspot-local reads tags of every file in the user's music folders and
again on each embedded-art fetch, so one hostile file exhausted memory at every start (security
review S1, MD167). Upstream main is the same as of 2026-11-15.

The `id3v2_compression_support` feature cannot simply be turned off: 0.25.4's off branch names
`Id3v2Error`/`Id3v2ErrorKind` without importing them and does not compile; and an error there
fails the whole tag rather than one frame.

## Diff (one file)

- `src/id3/v2/frame/read.rs`, in `ParsedFrame::read`, right after
  `let mut reader = reader.take(u64::from(size));` (line 144): when `flags.compression` is set,
  the frame's raw bytes are consumed (`io::copy` into `io::sink`, bounded by that `take`) and
  `ParsedFrame::Skip` is returned, with a fixed-text `log::warn!`. The compressed branches below
  are now unreachable and left as they were (smallest diff); `flate2` stays a dependency, unused.

Verify: `diff -ru <registry>/lofty-0.25.4/src vendor/lofty/src`.

Test: `cargo test -p whipspot-local compressed` (a 64 MiB bomb in a ~420 KB MP3 built by
`whipspot_testkit::audio_files::id3_bomb_mp3`: the frame is not built, the TIT2 after it is
read, the scan indexes the file; it fails on the unpatched copy with `["GEOB", "TIT2"]`).

## Upgrading

On a lofty upgrade, check whether upstream caps the inflater (for example
`take(min(data_length_indicator, allocation_limit))`) or refuses compressed frames. If not,
copy the new version here unmodified in one commit, re-apply the block above in
`ParsedFrame::read` before the flag `match` in a second, and run the test. Drop the vendor copy
and the `[patch.crates-io]` line once upstream bounds it; keep the test.

## Licence

lofty is offered under MIT OR Apache-2.0. Whipspot redistributes this copy under
Apache-2.0; its text is in `LICENSE-APACHE` here.
