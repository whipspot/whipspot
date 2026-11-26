#!/bin/bash
# Self-test of scripts/check-boundaries.sh (H16). Builds a temp fixture tree and a metadata JSON,
# runs the checker on the clean fixture (exit 0), then seeds one violation per rule R1-R8, each on
# its own copy, and checks exit 1 and that rule's line. Also checks that the two overrides fail
# closed (exit 2). Prints `selftest R<n>: pass` per rule; exits 1 on the first miss. Creates only
# its own temp dir, removed on every exit path. Run from anywhere; needs bash, mktemp and the
# checker's tools (cargo is not run: the fixture supplies the metadata).

# A failing stage must fail the pipeline, as in the checker.
set -o pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)" || exit 2
CHECK="$HERE/check-boundaries.sh"
if [ ! -f "$CHECK" ]; then
  echo "check-boundaries-selftest: $CHECK is missing." >&2
  exit 2
fi

TMP="$(mktemp -d)" || {
  echo "check-boundaries-selftest: mktemp failed." >&2
  exit 2
}
trap 'rm -rf "$TMP"' EXIT

# The workspace crates of the fixture, in the real names.
PKGS="whipspot-core whipspot-ui whipspot-testkit whipspot-skin-fs whipspot-skin-kit whipspot-spotify whipspot-local whipspot-ytmusic whipspot-update whipspot-credentials whipspot-audio-out whipspot-net whipspot-skin-winamp whipspot-skin-wmp whipspot-app"

# clean_deps <package>: its dependency objects in the cargo metadata shape (kind null = normal).
# The clean graph follows the real one: adapters depend on the core (and the testkit as dev only),
# the testkit on the core only, the app on everything (testkit as dev only).
clean_deps() {
  case "$1" in
    whipspot-core) ;;
    whipspot-ui) printf '%s' '{"name":"whipspot-core","kind":null},{"name":"lune-ui","kind":null}' ;;
    whipspot-testkit | whipspot-skin-kit) printf '%s' '{"name":"whipspot-core","kind":null}' ;;
    whipspot-spotify | whipspot-local | whipspot-ytmusic | whipspot-update | whipspot-net | whipspot-skin-fs)
      printf '%s' '{"name":"whipspot-core","kind":null},{"name":"whipspot-testkit","kind":"dev"}'
      ;;
    whipspot-skin-winamp | whipspot-skin-wmp)
      printf '%s' '{"name":"whipspot-core","kind":null},{"name":"whipspot-skin-kit","kind":null},{"name":"whipspot-testkit","kind":"dev"}'
      ;;
    whipspot-credentials | whipspot-audio-out)
      printf '%s' '{"name":"whipspot-core","kind":null}'
      ;;
    whipspot-app)
      printf '%s' '{"name":"whipspot-core","kind":null},{"name":"whipspot-spotify","kind":null},{"name":"whipspot-local","kind":null},{"name":"whipspot-ytmusic","kind":null},{"name":"whipspot-update","kind":null},{"name":"whipspot-credentials","kind":null},{"name":"whipspot-audio-out","kind":null},{"name":"whipspot-net","kind":null},{"name":"whipspot-ui","kind":null},{"name":"whipspot-skin-fs","kind":null},{"name":"whipspot-skin-winamp","kind":null},{"name":"whipspot-skin-wmp","kind":null},{"name":"whipspot-testkit","kind":"dev"}'
      ;;
  esac
}

# write_meta <file> <owner> <extra dependency object>: the clean metadata, plus one extra
# dependency on <owner> ("-" for none).
write_meta() {
  local file="$1" owner="$2" extra="$3" p deps sep=""
  {
    printf '%s' '{"packages":['
    for p in $PKGS; do
      deps="$(clean_deps "$p")"
      if [ "$p" = "$owner" ]; then deps="${deps:+$deps,}$extra"; fi
      printf '%s{"name":"%s","dependencies":[%s]}' "$sep" "$p" "$deps"
      sep=","
    done
    printf '%s\n' ']}'
  } >"$file"
}

# make_tree <root>: a clean source tree (one lib.rs per crate) and a lock file without rodio.
make_tree() {
  local root="$1" p
  for p in $PKGS; do
    mkdir -p "$root/crates/$p/src" || return 1
    printf '%s\n' "[package]" "name = \"$p\"" >"$root/crates/$p/Cargo.toml" || return 1
    printf '%s\n' '// fixture' 'pub fn marker() {}' >"$root/crates/$p/src/lib.rs" || return 1
  done
  printf '%s\n' 'version = 4' '' '[[package]]' 'name = "serde"' 'version = "1.0.0"' >"$root/Cargo.lock" || return 1
}

# new_case <name>: a fresh clean fixture in $TMP/<name>; sets D to its root.
new_case() {
  D="$TMP/$1"
  make_tree "$D" || exit 2
  write_meta "$D/metadata.json" - '' || exit 2
}

# run_check <root> <metadata>: runs the checker with both overrides; sets OUT and RC.
run_check() {
  OUT="$(BOUNDARY_ROOT="$1" BOUNDARY_METADATA="$2" bash "$CHECK" 2>&1)"
  RC=$?
}

# expect_rule <rule>: the last run must exit 1 and print that rule's line.
expect_rule() {
  if [ "$RC" -ne 1 ] || ! grep -q "^check-boundaries: $1: " <<<"$OUT"; then
    echo "selftest $1: FAIL (exit $RC, expected 1 and a $1 line)" >&2
    printf '%s\n' "$OUT" >&2
    exit 1
  fi
  echo "selftest $1: pass"
}

# Clean fixture: exit 0.
new_case clean
run_check "$D" "$D/metadata.json"
if [ "$RC" -ne 0 ]; then
  echo "selftest clean fixture: FAIL (exit $RC, expected 0)" >&2
  printf '%s\n' "$OUT" >&2
  exit 1
fi
echo "selftest clean fixture: ok"

# Overrides fail closed: a missing root or a missing metadata file is exit 2.
run_check "$TMP/no-such-root" "$D/metadata.json"
if [ "$RC" -ne 2 ]; then
  echo "selftest overrides: FAIL (missing BOUNDARY_ROOT exit $RC, expected 2)" >&2
  exit 1
fi
run_check "$D" "$TMP/no-such-metadata.json"
if [ "$RC" -ne 2 ]; then
  echo "selftest overrides: FAIL (missing BOUNDARY_METADATA exit $RC, expected 2)" >&2
  exit 1
fi
echo "selftest overrides fail closed: ok"

# R1: the core depends on rodio.
new_case R1
write_meta "$D/metadata.json" whipspot-core '{"name":"rodio","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R1

# R2: the core's source names librespot in a use.
new_case R2
printf '%s\n' 'use librespot::Session;' >>"$D/crates/whipspot-core/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R2

# R3: the core's source names a skin format.
new_case R3
printf '%s\n' '// a Winamp reference' >>"$D/crates/whipspot-core/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R3

# R4: the UI depends on wgpu.
new_case R4
write_meta "$D/metadata.json" whipspot-ui '{"name":"wgpu","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R4

# R5: an adapter depends on another adapter.
new_case R5
write_meta "$D/metadata.json" whipspot-spotify '{"name":"whipspot-net","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R5

# R6: the testkit is a normal dependency of the app.
new_case R6
write_meta "$D/metadata.json" whipspot-app '{"name":"whipspot-testkit","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R6

# R3: the skin kit's source names a skin format.
new_case R3-kit
printf '%s
' '// a Winamp reference' >>"$D/crates/whipspot-skin-kit/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R3

# R5: the skin kit depends on an adapter.
new_case R5-kit
write_meta "$D/metadata.json" whipspot-skin-kit '{"name":"whipspot-skin-winamp","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R5

# R2: the core's source names the local adapter.
new_case R2-local
printf '%s
' 'use whipspot_local::Scanner;' >>"$D/crates/whipspot-core/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R2

# R4: the UI's source names the local adapter.
new_case R4-local
printf '%s
' 'use whipspot_local::Scanner;' >>"$D/crates/whipspot-ui/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R4

# R5: the local adapter depends on another adapter.
new_case R5-local
write_meta "$D/metadata.json" whipspot-local '{"name":"whipspot-net","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R5

# R5: another adapter depends on the local adapter.
new_case R5-local-in
write_meta "$D/metadata.json" whipspot-spotify '{"name":"whipspot-local","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R5

# R2: the core's source names the YouTube Music adapter.
new_case R2-ytmusic
printf '%s
' 'use whipspot_ytmusic::Provider;' >>"$D/crates/whipspot-core/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R2

# R4: the UI's source names the YouTube Music adapter.
new_case R4-ytmusic
printf '%s
' 'use whipspot_ytmusic::Provider;' >>"$D/crates/whipspot-ui/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R4

# R5: the YouTube Music adapter depends on another adapter.
new_case R5-ytmusic
write_meta "$D/metadata.json" whipspot-ytmusic '{"name":"whipspot-net","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R5

# R8: the UI source names YouTube, the adapter or its tool.
for word in YouTube ytmusic yt-dlp; do
  new_case "R8-$word"
  printf '%s
' "// $word session" >>"$D/crates/whipspot-ui/src/lib.rs"
  run_check "$D" "$D/metadata.json"
  expect_rule R8
done

# R8: the testkit source names YouTube.
new_case R8-testkit-youtube
printf '%s
' '// YouTube fake' >>"$D/crates/whipspot-testkit/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R8

# R1: the core depends on lunepack.
new_case R1-lunepack
write_meta "$D/metadata.json" whipspot-core '{"name":"lunepack","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R1

# R2: the core's source names lunepack.
new_case R2-lunepack
printf '%s\n' 'use lunepack::Updater;' >>"$D/crates/whipspot-core/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R2

# R2: the core's source names the update adapter.
new_case R2-update
printf '%s\n' 'use whipspot_update::LunepackUpdater;' >>"$D/crates/whipspot-core/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R2

# R4: the UI depends on lunepack.
new_case R4-lunepack
write_meta "$D/metadata.json" whipspot-ui '{"name":"lunepack","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R4

# R4: the UI's source names lunepack.
new_case R4-lunepack-src
printf '%s\n' 'use lunepack::Updater;' >>"$D/crates/whipspot-ui/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R4

# R5: the update adapter depends on another adapter.
new_case R5-update
write_meta "$D/metadata.json" whipspot-update '{"name":"whipspot-net","kind":null}'
run_check "$D" "$D/metadata.json"
expect_rule R5

# R7: the lock file names rodio.
new_case R7
printf '%s\n' '' '[[package]]' 'name = "rodio"' 'version = "0.20.0"' >>"$D/Cargo.lock"
run_check "$D" "$D/metadata.json"
expect_rule R7

# R8: the UI source names Spotify.
new_case R8
printf '%s\n' '// Spotify session' >>"$D/crates/whipspot-ui/src/lib.rs"
run_check "$D" "$D/metadata.json"
expect_rule R8

echo "check-boundaries-selftest: all eight rules are checked"
exit 0
