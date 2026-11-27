# Releasing updates

Whipspot updates itself with [lunepack](https://github.com/lune-rs/lunepack). `lunepack.toml` here
holds the app id, the root public key, the feed and the `[publish]` table (names of environment
variables, never values); every installed copy carries the first three, so changing any of them
strands existing installs. It never holds a key.

Storage (Cloudflare R2, shared with Luneterm, which owns `luneterm/` in both buckets):

- `izzat-updates`, public at `https://updates.izzat.dev/`: Whipspot's release directory under
  `whipspot/` (`keys.json`, `feed-stable.json`, one folder per version).
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
  `publish push --dry-run` and `publish push`. Keeps the 10 newest versions in the `stable` feed.
- `release`: a draft GitHub release with the two installers.

`.github/workflows/refresh.yml`:

- weekly `refresh` (environment `release-refresh`, main only): re-signs the feed when it expires
  within 30 days; does nothing while nothing is published.
- `drop` (by hand from a `v*` tag, with `drop_version`; environment `release-push`): takes one release
  out of the feed.
- `feed_check` (by hand): reads `keys.json` from an Ubuntu, a macOS and a Windows runner and runs
  `publish fetch` on Ubuntu; no secret. Run it before the first tag.

No job that holds a secret compiles anything, and each runs `lunepack` only after its hash check.

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

A tag ruleset limiting who may create `v*` tags, and a protected `main`, are recommended.

## Every release

1. Set the version in `crates/whipspot-app/Cargo.toml` (the workflow refuses a tag that differs), commit,
   push.
2. `git tag -a vX.Y.Z -m "Whipspot X.Y.Z"` and push the tag. Approve `release-push` when the pack jobs
   are done.
3. Check the result without any credential:
   `lunepack publish fetch --release-dir <empty dir> --version 999999.0.0` checks the signatures and the
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
