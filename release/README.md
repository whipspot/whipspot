# Releasing updates

Whipspot updates itself with [lunepack](https://github.com/lune-rs/lunepack). `lunepack.toml` here
holds the app id, the root public key, the feed and the `[publish]` table (names of environment
variables, never values); every installed copy carries the first three, so changing any of them
strands existing installs. It never holds a key.

Storage (Cloudflare R2, shared with Luneterm, which owns `luneterm/` in both buckets):

- `izzat-updates`, public at `https://updates.izzat.dev/`: Whipspot's release directory under
  `whipspot/` (`keys.json`, `feed-stable.json`, `feed-beta.json`, one folder per version).
- `izzat-updates-publisher`, private: `whipspot/lunepack-state.json`, the publisher state lunepack
  uses to refuse a replayed older document, and the pending-deletes record. Never rebuild it from the
  public bucket.

Publishing is `lunepack publish` (fetch, pack, setup, push, refresh, drop), run only by the GitHub
workflows below. lunepack's own guide is `docs/PUBLISHING.md` in its repository (the job split, rerun
and recovery, retention, taking a release out, changing the key set); this file names only what is
specific to Whipspot.

## The workflows

`.github/workflows/release.yml`, on a `v*` tag (a manual run only builds):

- `tools-macos`, `tools-windows`: build the lunepack CLI, runtime, UI and stub from the commit
  `Cargo.lock` pins; no secret; each outputs the SHA-256 of the CLI and of the bundled binaries.
- `build-macos`, `build-windows`: build the app; no secret.
- `pack-macos`, `pack-windows` (environment `release-pack`): check the tools against those hashes,
  then `publish fetch` (the previous release for the delta, read from the public feed over https),
  `pack`, `setup`. They hold the release key and no R2 key.
- `push` (environment `release-push`, the owner approves): checks the CLI hash, then
  `publish push --dry-run` and `publish push`. Keeps the 10 newest versions in each feed.
- `release`: a draft GitHub release with the two installers (a prerelease for a beta release).

`.github/workflows/refresh.yml`:

- weekly `refresh` (environment `release-refresh`, main only): re-signs each feed that expires
  within 30 days; does nothing while nothing is published.
- `drop` (by hand from a `v*` tag, with `drop_version`; environment `release-push`): takes one release
  out of the feed.
- `feed_check` (by hand): reads `keys.json` from an Ubuntu, a macOS and a Windows runner and runs
  `publish fetch` on Ubuntu; no secret. Run it before the first tag.

No job that holds a secret compiles anything, and each runs `lunepack` only after checking that its tools directory holds exactly the expected files and that their hashes match.

## GitHub environments and secrets

Environment secrets only, no repository secrets (a repository secret reaches any branch):

| Environment | Limited to | Secrets |
| --- | --- | --- |
| `release-pack` | tags `v*` | `LUNEPACK_RELEASE_KEY` |
| `release-push` | tags `v*`, you as required reviewer | `LUNEPACK_RELEASE_KEY`, `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY` (read and write on both buckets) |
| `release-refresh` | `main` | `LUNEPACK_RELEASE_KEY`, `R2_ACCOUNT_ID`, `R2_REFRESH_PUBLIC_ACCESS_KEY_ID`, `R2_REFRESH_PUBLIC_SECRET_ACCESS_KEY` (read and write, `izzat-updates` only), `R2_REFRESH_PRIVATE_ACCESS_KEY_ID`, `R2_REFRESH_PRIVATE_SECRET_ACCESS_KEY` (read only, `izzat-updates-publisher` only) |

The refresh tokens must not be one token with write access to both buckets: a refresh that could write
the private bucket could lower the state file's record. The workflows map these names onto the
variables `[publish]` in `lunepack.toml` reads.

A tag ruleset (`v*`: creation, update and deletion, repository admins bypass) and a `main` ruleset
(no deletion, no force push) are in place.

## Channels

Two channels, `stable` and `beta`, the standard every lunepack app of ours follows:

| Tag | Branch | Listed in | Packed for |
| --- | --- | --- | --- |
| `vX.Y.Z` | `main` | `stable` and `beta` | `stable` |
| `vX.Y.Z-rc.N` | `main` or `develop` | `beta` only | `beta` |

The workflow refuses any other tag, and a tag on any other branch. In both cases the version in
`crates/whipspot-app/Cargo.toml` is `X.Y.Z` (an rc leads to that version). Stable copies never see an rc:
the stable feed never lists one. A later stable `vX.Y.Z` is listed in beta too, so beta copies move to it.

An installed copy follows the channel its setup came from: the app records it in `settings.json`
(`update_channel`) at its first start and at the first start after a setup, and keeps it through updates
(a beta copy updated to a stable release stays on beta). To switch, uninstall and run the other channel's
setup.

## Every release

1. Set the version in `crates/whipspot-app/Cargo.toml` to `X.Y.Z` (the workflow refuses a tag whose
   `X.Y.Z` differs), commit, push. For a beta, do it on `develop` (or `main`) and wait for CI.
2. `git tag -a vX.Y.Z -m "Whipspot X.Y.Z"` (or `vX.Y.Z-rc.N` for a beta) and push the tag. Approve
   `release-push` when the pack jobs are done. Then publish the draft release.
3. Check the result without any credential:
   `lunepack publish fetch --config release/lunepack.toml --release-dir <empty dir> --version 999999.0.0` checks the signatures and the
   chain of what is published (it does not check expiry). The owner sees each feed's expiry with
   `lunepack publish plan --config release/lunepack.toml` and the store credentials in the environment.

A version is published once: never reuse one, never move a tag. A failed run is re-run as it is; see
lunepack's `docs/PUBLISHING.md`, "Rerun and recovery".

GitHub pauses scheduled workflows in a public repository after 60 days without repository activity
(GitHub's documented behaviour). The feed lives 90 days, so after a quiet stretch check the Actions tab
and re-enable `refresh.yml`.

## History

The keys were made and the stores adopted on 2026-11-26 by the first bootstrap (root key offline with
the owner, release key in `LUNEPACK_RELEASE_KEY`; its window is two years, so renew it with
`lunepack keys set` and the root key before then). Key changes go through `lunepack publish keys`, the
owner's command, never an agent's and never a workflow's.
