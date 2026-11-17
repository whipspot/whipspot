#!/bin/bash
# Crate boundary rules. Run from anywhere; it works on the repo root. Prints one line per violation:
#   check-boundaries: <rule>: <file:line | crate -> dep>
# Exit 0 = clean, 1 = at least one violation (all listed), 2 = a needed tool (cargo, jq, grep) is
# missing, `cargo metadata` fails, or a scanned path is missing (fail closed, never 0 after an error).
# Works in Git Bash on Windows and macOS bash 3.2. Reads files only; changes nothing.
#
#   R1  whipspot-core has no dependency (any kind) on librespot*, keyring*, lune*, winit, wgpu, zip,
#       cpal, rodio, wasapi, reqwest, an image decoder (png, zune-*, jpeg-decoder, image, bmp, tinybmp),
#       an inflate crate (miniz_oxide, flate2) or any whipspot-* crate
#   R2  crates/whipspot-core/src names none of those crates in a path (`<crate>::`) or `use <crate>`
#   R3  the skin-format words (wsz, winamp, pledit, viscolor, text.bmp, region.txt, any case) appear
#       nowhere in the src or manifest of whipspot-core, whipspot-ui, whipspot-skin-fs and
#       whipspot-testkit (the format-compat crate whipspot-skin-winamp is the one place they live)
#   R4  whipspot-ui (when the crate exists) has R1's and R2's bans minus lune* and whipspot-core
#   R5  none of whipspot-spotify, -credentials, -audio-out, -net, -ui, -skin-fs, -skin-winamp depends
#       (normal or build) on another of them; whipspot-testkit's [dependencies] name no workspace
#       crate but whipspot-core
#   R6  whipspot-testkit is in no crate's normal or build dependencies (a dev-dependency is allowed)
#   R7  Cargo.lock names no package `rodio`
#   R8  `spotify` or `librespot` (any case) appears nowhere in crates/whipspot-ui/src or
#       crates/whipspot-testkit/src (the provider names stay in the adapters and the core's tests)
#
# Two overrides, read only by scripts/check-boundaries-selftest.sh; the real gate sets neither:
#   BOUNDARY_METADATA  a saved `cargo metadata --no-deps --format-version 1` JSON, read instead of cargo
#   BOUNDARY_ROOT      the tree that holds crates/ and Cargo.lock, instead of the repo root
# A variable that is set (even to an empty string) must name an existing file or directory, else exit 2.

# A failing jq (or any stage) must fail its pipeline, not hide behind the last stage's status.
set -o pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" || exit 2

for tool in cargo jq grep tr; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "check-boundaries: $tool is not installed or not on PATH." >&2
    exit 2
  fi
done

if [ -n "${BOUNDARY_ROOT+x}" ]; then
  if [ ! -d "$BOUNDARY_ROOT" ]; then
    echo "check-boundaries: BOUNDARY_ROOT is not a directory: $BOUNDARY_ROOT" >&2
    exit 2
  fi
  ROOT="$BOUNDARY_ROOT"
fi

# The override file is read before the cd below, so a relative path means the directory the script
# was started from.
have_meta=0
if [ -n "${BOUNDARY_METADATA+x}" ]; then
  if [ ! -f "$BOUNDARY_METADATA" ]; then
    echo "check-boundaries: BOUNDARY_METADATA is not a file: $BOUNDARY_METADATA" >&2
    exit 2
  fi
  META="$(cat -- "$BOUNDARY_METADATA")" || {
    echo "check-boundaries: cannot read BOUNDARY_METADATA." >&2
    exit 2
  }
  have_meta=1
fi

cd "$ROOT" || exit 2

violations=0
report() { # report <rule> <what>
  printf 'check-boundaries: %s: %s\n' "$1" "$2"
  violations=$((violations + 1))
}

if [ "$have_meta" -eq 0 ]; then
  META="$(cargo metadata --no-deps --format-version 1 2>/dev/null)" || {
    echo "check-boundaries: cargo metadata failed." >&2
    exit 2
  }
fi
# One line per dependency edge: "<package> <kind> <dependency>" (kind: normal, dev, build).
EDGES="$(printf '%s' "$META" | jq -r '.packages[] | .name as $p | .dependencies[] | "\($p) \(.kind // "normal") \(.name)"' | tr -d '\r')" || {
  echo "check-boundaries: jq failed to read the cargo metadata." >&2
  exit 2
}
PACKAGES="$(printf '%s' "$META" | jq -r '.packages[].name' | tr -d '\r')" || {
  echo "check-boundaries: jq failed to read the cargo metadata." >&2
  exit 2
}
# A here-string, not a pipe: with pipefail, `grep -q` exiting early could SIGPIPE the writer.
has_package() { grep -qx -- "$1" <<<"$PACKAGES"; }
if ! has_package whipspot-core; then
  echo "check-boundaries: whipspot-core is not in the cargo metadata." >&2
  exit 2
fi

# Dependency names banned in whipspot-core / whipspot-ui (by package name).
banned_in_core() {
  case "$1" in
    librespot* | keyring* | lune* | winit | wgpu | zip | miniz_oxide | flate2 | cpal | rodio | wasapi | reqwest | png | zune-* | jpeg-decoder | image | bmp | tinybmp | whipspot-*) return 0 ;;
  esac
  return 1
}
banned_in_ui() {
  case "$1" in
    whipspot-core) return 1 ;; # the views drive the core (D31)
    librespot* | keyring* | winit | wgpu | zip | miniz_oxide | flate2 | cpal | rodio | wasapi | reqwest | png | zune-* | jpeg-decoder | image | bmp | tinybmp | whipspot-*) return 0 ;;
  esac
  return 1
}
is_adapter() {
  case "$1" in
    whipspot-spotify | whipspot-local | whipspot-credentials | whipspot-audio-out | whipspot-net | whipspot-ytmusic | whipspot-ui | whipspot-skin-fs | whipspot-skin-winamp | whipspot-skin-wmp) return 0 ;;
  esac
  return 1
}

# Source patterns in Rust path form (hyphens are underscores). `whipspot_core` is core's own name.
# The decoder crates are named by their zune and jpeg-decoder paths only: a bare `image` or `png`
# would also match `lune_ui::image::` (the toolkit's own module), so those two are held by the
# manifest rule alone.
SRC_NAMES_CORE='librespot[A-Za-z0-9_]*|keyring[A-Za-z0-9_]*|lune[A-Za-z0-9_]*|winit|wgpu|zip|miniz_oxide|flate2|cpal|rodio|wasapi|reqwest|zune_jpeg|zune_core|jpeg_decoder|whipspot_(spotify|local|ytmusic|credentials|testkit|audio_out|net|ui|skin_fs|skin_kit|skin_winamp|skin_wmp)'
SRC_NAMES_UI='librespot[A-Za-z0-9_]*|keyring[A-Za-z0-9_]*|winit|wgpu|zip|miniz_oxide|flate2|cpal|rodio|wasapi|reqwest|zune_jpeg|zune_core|jpeg_decoder|whipspot_(spotify|local|ytmusic|credentials|testkit|audio_out|net|skin_fs|skin_kit|skin_winamp|skin_wmp)'
SKIN_WORDS='wsz|winamp|pledit|viscolor|text\.bmp|region\.txt|wmz|wms|wmp'

# src_rule <rule> <names> <dir>: crate names (case-sensitive) as a path or `use` in Rust source.
# grep exits 2 on an error (a missing path); that stops the script with exit 2.
src_rule() {
  local rule="$1" names="$2" dir="$3" out rc=0 hit pattern
  pattern="(^|[^A-Za-z0-9_])(${names})::|(^|[^A-Za-z0-9_])use[[:space:]]+(${names})([^A-Za-z0-9_]|\$)"
  out="$(grep -rnIE --include='*.rs' -e "$pattern" "$dir" 2>&1)" || rc=$?
  if [ "$rc" -gt 1 ]; then
    echo "check-boundaries: grep failed in $rule: $out" >&2
    exit 2
  fi
  if [ "$rc" -eq 0 ]; then
    while IFS= read -r hit; do
      hit="$(printf '%s' "$hit" | tr -d '\r')"
      [ -n "$hit" ] && report "$rule" "$(printf '%s' "$hit" | cut -d: -f1,2)"
    done <<EOF
$out
EOF
  fi
}
skin_rule() { # skin_rule <crate dir>
  local dir="$1" out rc=0 hit
  out="$(grep -rnIiE --include='*.rs' --include='Cargo.toml' -e "$SKIN_WORDS" "$dir/src" "$dir/Cargo.toml" 2>&1)" || rc=$?
  if [ "$rc" -gt 1 ]; then
    echo "check-boundaries: grep failed in R3: $out" >&2
    exit 2
  fi
  if [ "$rc" -eq 0 ]; then
    while IFS= read -r hit; do
      hit="$(printf '%s' "$hit" | tr -d '\r')"
      [ -n "$hit" ] && report R3 "$(printf '%s' "$hit" | cut -d: -f1,2)"
    done <<EOF
$out
EOF
  fi
}
# R8: provider names (any case) in the UI and testkit sources, all text files there. The YouTube
# Music adapter's names are held back the same way (D9): youtube, ytmusic, yt-dlp (and yt_dlp).
r8_scan() { # r8_scan <dir>
  local dir="$1" out rc=0 hit
  out="$(grep -rnIiE -e 'spotify|librespot|youtube|ytmusic|yt-dlp|yt_dlp' "$dir" 2>&1)" || rc=$?
  if [ "$rc" -gt 1 ]; then
    echo "check-boundaries: grep failed in R8: $out" >&2
    exit 2
  fi
  if [ "$rc" -eq 0 ]; then
    while IFS= read -r hit; do
      hit="$(printf '%s' "$hit" | tr -d '\r')"
      [ -n "$hit" ] && report R8 "$(printf '%s' "$hit" | cut -d: -f1,2)"
    done <<EOF
$out
EOF
  fi
}
# R7: the lock file names no rodio package, at any version.
r7_lock() {
  local out rc=0 hit
  out="$(grep -nE '^name = "rodio"' Cargo.lock 2>&1)" || rc=$?
  if [ "$rc" -gt 1 ]; then
    echo "check-boundaries: grep failed in R7: $out" >&2
    exit 2
  fi
  if [ "$rc" -eq 0 ]; then
    while IFS= read -r hit; do
      hit="$(printf '%s' "$hit" | tr -d '\r')"
      [ -n "$hit" ] && report R7 "Cargo.lock:$(printf '%s' "$hit" | cut -d: -f1)"
    done <<EOF
$out
EOF
  fi
}

# Edge rules (R1, R4, R5, R6).
while read -r pkg kind dep; do
  [ -n "$pkg" ] || continue
  case "$pkg" in
    whipspot-core)
      if banned_in_core "$dep"; then report R1 "$pkg -> $dep ($kind)"; fi
      ;;
    whipspot-ui)
      if banned_in_ui "$dep"; then report R4 "$pkg -> $dep ($kind)"; fi
      ;;
    whipspot-testkit)
      if [ "$kind" = normal ]; then
        case "$dep" in
          whipspot-core) ;;
          whipspot-*) report R5 "$pkg -> $dep ($kind)" ;;
        esac
      fi
      ;;
    whipspot-skin-kit)
      if [ "$kind" = normal ]; then
        case "$dep" in
          whipspot-core) ;;
          whipspot-*) report R5 "$pkg -> $dep ($kind)" ;;
        esac
      fi
      ;;
  esac
  if [ "$dep" = whipspot-testkit ] && [ "$kind" != dev ]; then report R6 "$pkg -> $dep ($kind)"; fi
  if is_adapter "$pkg" && [ "$kind" != dev ] && is_adapter "$dep" && [ "$pkg" != "$dep" ]; then
    report R5 "$pkg -> $dep ($kind)"
  fi
done <<EOF
$EDGES
EOF

# Source rules (R2, R3, R4).
src_rule R2 "$SRC_NAMES_CORE" crates/whipspot-core/src
skin_rule crates/whipspot-core
if [ -d crates/whipspot-ui ]; then
  src_rule R4 "$SRC_NAMES_UI" crates/whipspot-ui/src
  skin_rule crates/whipspot-ui
fi
# The skins-folder source, the shared skin kit and the fakes name no skin format either (D7, MD100).
for dir in crates/whipspot-skin-fs crates/whipspot-skin-kit crates/whipspot-testkit; do
  if [ -d "$dir" ]; then
    skin_rule "$dir"
  fi
done

# Lock file and provider-name rules (R7, R8).
r7_lock
for dir in crates/whipspot-ui/src crates/whipspot-testkit/src; do
  r8_scan "$dir"
done

if [ "$violations" -gt 0 ]; then
  exit 1
fi
exit 0
