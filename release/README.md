# Releasing updates

Whipspot updates itself with [lunepack](https://github.com/lune-rs/lunepack). `lunepack.toml` here
holds the app id, the root public key and the feed; every installed copy carries them, so changing
any of them strands existing installs. It never holds a key.

Storage (Cloudflare R2, shared with Luneterm, which owns `luneterm/` in both buckets):

- `izzat-updates`, public at `https://updates.izzat.dev/`: Whipspot's release directory under
  `whipspot/` (`keys.json`, `feed-stable.json`, one folder per version).
- `izzat-updates-publisher`, private: `whipspot/lunepack-state.json`, the publisher state lunepack
  uses to refuse a replayed older document. Never rebuild it from the public bucket.

`scripts/publish-update.sh` does everything below, the same way locally and in CI. It touches only
paths under `whipspot/`, uses rclone with a remote defined from the environment (no rclone config
file), and prints no key or secret. Every step needs the lunepack CLI (`LUNEPACK`, default `lunepack`
on PATH).

## Once: bootstrap the keys

On your Mac, with the R2 credentials in the environment (`R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`,
`R2_SECRET_ACCESS_KEY`):

```
WHIPSPOT_ROOT_KEY=<offline path>/whipspot-root.key \
WHIPSPOT_RELEASE_KEY_FILE=<private path>/whipspot-release.key \
bash scripts/publish-update.sh bootstrap
```

It refuses when `whipspot/keys.json` or the state file already exists, or when a key file exists. It
generates both keys (outside the repository), writes and uploads `keys.json` and the state file, and
puts the root public key into `release/lunepack.toml`. Then:

1. commit `release/lunepack.toml`;
2. `gh secret set LUNEPACK_RELEASE_KEY < <private path>/whipspot-release.key`, plus `R2_ACCOUNT_ID`,
   `R2_ACCESS_KEY_ID` and `R2_SECRET_ACCESS_KEY`;
3. move the root key offline (it signs only key changes; `../lunepack-rs/docs/KEY-CEREMONY.md`).

The release key's window is two years from bootstrap: renew it with `lunepack keys set` and the root
key before then.

## Every release

A `v*` tag runs `.github/workflows/release.yml`, which calls the script. By hand, with the version in
`crates/whipspot-app/Cargo.toml` (the script refuses any other):

```
export LUNEPACK_BIN_DIR=<dir with lunepack-runtime, lunepack-ui, lunepack-stub.exe>
export WHIPSPOT_RELEASE_KEY_FILE=<private path>/whipspot-release.key   # or LUNEPACK_RELEASE_KEY
bash scripts/publish-update.sh pack macos-universal dist/Whipspot.app <version>
bash scripts/publish-update.sh setup macos-universal <version>         # dist/*.dmg
bash scripts/publish-update.sh publish <version>
```

`pack` fetches the previous release's pack of the target for a delta and writes
`dist/update/<version>/`.
In CI the jobs that sign (`pack`, `setup`) set `WHIPSPOT_R2_PUBLIC=https://updates.izzat.dev/`: they
read the public feed over https and hold no R2 key; only the `publish` job has the write keys (and
the release key), and no job that compiles has any secret. `publish` fetches the state and the retained releases, adds the version to
the `stable` feed, drops all but the 10 newest (by lunepack's version precedence, read from the
verified feed), verifies, uploads packs, then manifests, then keys, then the feed, and only then
deletes the dropped version folders and saves the state. A failure before the feed upload deletes
nothing; re-run `publish` with the same version. If a delete fails after the feed is live, the
folder is no longer listed by any feed; remove it by hand.

`bash scripts/test-publish-update.sh` runs all of this against local folders with throwaway keys.

## Recovery

- `release/work exists`: a run was interrupted; remove `release/work/` once no run is going.
- To withdraw a release, publish a feed without it (`lunepack feed refresh --drop`, lunepack's
  `docs/CLI.md`); the script does not do this.
