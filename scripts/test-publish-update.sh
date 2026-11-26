#!/bin/bash
# Tests scripts/publish-update.sh against local folders standing in for the R2 buckets: bootstrap,
# 12 packs and publishes of a tiny fake app for this Mac's target, keep-10 pruning by lunepack's
# version precedence, the refusals, an injected upload failure and that no key or secret is printed.
# Builds the lunepack CLI and runtime from $LUNEPACK_SRC (default: lunepack-rs beside this checkout)
# offline. Throwaway keys only; no network. macOS only (the runtime must be a Mach-O of the host).
set -euo pipefail

REPO=$(cd "$(dirname "$0")/.." && pwd)
fail() { echo "test-publish-update: FAIL: $*" >&2; exit 1; }
ok() { echo "test-publish-update: ok: $*"; }

[ "$(uname -s)" = Darwin ] || fail "runs on macOS only"
case "$(uname -m)" in
  arm64) TARGET=macos-aarch64 ;;
  x86_64) TARGET=macos-x86_64 ;;
  *) fail "unknown host architecture" ;;
esac
command -v rclone >/dev/null || fail "rclone is not installed"
command -v jq >/dev/null || fail "jq is not installed"

# ---- the lunepack tools ------------------------------------------------------------------------
if [ -z "${LUNEPACK_SRC:-}" ]; then
  for c in "$REPO/../lunepack-rs" \
    "$(git -C "$REPO" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)/../../lunepack-rs"; do
    if [ -f "$c/crates/lunepack-cli/Cargo.toml" ]; then LUNEPACK_SRC=$(cd "$c" && pwd); break; fi
  done
fi
[ -n "${LUNEPACK_SRC:-}" ] && [ -f "$LUNEPACK_SRC/crates/lunepack-cli/Cargo.toml" ] ||
  fail "no lunepack-rs checkout found; set LUNEPACK_SRC"
pinned=$(sed -n 's/^lunepack = .*rev = "\([0-9a-f]*\)".*/\1/p' "$REPO/crates/whipspot-update/Cargo.toml" | head -1)
have=$(git -C "$LUNEPACK_SRC" rev-parse HEAD 2>/dev/null || echo unknown)
[ "$have" = "$pinned" ] || echo "test-publish-update: note: lunepack-rs is at $have, the app pins $pinned"
TOOLS="$REPO/target/lunepack-tools"
(cd "$LUNEPACK_SRC" && cargo build --quiet --offline --locked --release -p lunepack-cli -p lunepack-runtime --target-dir "$TOOLS")
BIN="$TOOLS/release"
[ -x "$BIN/lunepack" ] && [ -x "$BIN/lunepack-runtime" ] || fail "the lunepack build left no binaries"

# ---- a scratch world ---------------------------------------------------------------------------
T=$(mktemp -d "${TMPDIR:-/tmp}/whipspot-publish-test.XXXXXX")
if [ -n "${WHIPSPOT_TEST_KEEP:-}" ]; then echo "test-publish-update: keeping $T"; else trap 'rm -rf "$T"' EXIT; fi
FAKE="$T/repo" PUB="$T/bucket-public" PRIV="$T/bucket-private" KEYS="$T/keys"
mkdir -p "$FAKE/scripts" "$FAKE/release" "$FAKE/crates/whipspot-app" "$PUB" "$PRIV" "$KEYS" "$T/fakebin" "$T/apps"
chmod 700 "$KEYS"
cp "$REPO/scripts/publish-update.sh" "$FAKE/scripts/"
cp "$REPO/release/lunepack.toml" "$FAKE/release/"
mkdir -p "$FAKE/assets"
cp -R "$REPO/assets/icon" "$FAKE/assets/icon"
set_app_version() { printf '[package]\nname = "whipspot-app"\nversion = "%s"\nedition = "2024"\n' "$1" >"$FAKE/crates/whipspot-app/Cargo.toml"; }
set_app_version 0.0.0

# Luneterm's folders share the buckets: they must come out byte for byte the same.
mkdir -p "$PUB/luneterm/1.0.0" "$PRIV/luneterm"
echo luneterm-feed >"$PUB/luneterm/feed-stable.json"
echo luneterm-keys >"$PUB/luneterm/keys.json"
head -c 4096 /dev/urandom >"$PUB/luneterm/1.0.0/macos-aarch64.pack"
echo luneterm-state >"$PRIV/luneterm/lunepack-state.json"
echo bucket-root-file >"$PUB/index.html"
snap() { (cd "$1" && find . -type f -print0 | sort -z | xargs -0 shasum -a 256); }
snap_dir() { if [ -d "$1" ]; then snap "$1"; fi; }
SIB_PUB=$(snap_dir "$PUB/luneterm"; shasum -a 256 "$PUB/index.html" | cut -d' ' -f1)
SIB_PRIV=$(snap_dir "$PRIV/luneterm")
check_siblings() {
  [ "$(snap_dir "$PUB/luneterm"; shasum -a 256 "$PUB/index.html" | cut -d' ' -f1)" = "$SIB_PUB" ] || fail "the public bucket's other folders changed ($1)"
  [ "$(snap_dir "$PRIV/luneterm")" = "$SIB_PRIV" ] || fail "the private bucket's other folders changed ($1)"
}

# rclone stand-in: the real one, except that a run can make one upload fail, and that the R2
# remote never reaches the network (it records whether the env remote carried the secret).
cat >"$T/fakebin/rclone" <<EOF
#!/bin/bash
for a in "\$@"; do
  case "\$a" in whipspotr2:*)
    if [ "\${RCLONE_CONFIG_WHIPSPOTR2_SECRET_ACCESS_KEY:-}" = "\${R2_SECRET_ACCESS_KEY:-unset}" ]; then touch "$T/r2-remote-defined"; fi
    echo "fake rclone: no network in tests" >&2; exit 1 ;;
  esac
done
if [ -n "\${FAKE_RCLONE_FAIL_OP:-}" ]; then
  op=no dst=no
  for a in "\$@"; do
    [ "\$a" = "\$FAKE_RCLONE_FAIL_OP" ] && op=yes
    case "\$a" in *"/\$FAKE_RCLONE_FAIL_DST") dst=yes ;; esac
  done
  if [ \$op = yes ] && [ \$dst = yes ]; then echo "fake rclone: injected failure" >&2; exit 1; fi
fi
exec "$(command -v rclone)" "\$@"
EOF
chmod +x "$T/fakebin/rclone"

FAKE_SECRET="fake-r2-secret-$(head -c 12 /dev/urandom | xxd -p)"
export PATH="$T/fakebin:$PATH"
export LUNEPACK="$BIN/lunepack" LUNEPACK_BIN_DIR="$BIN"
export WHIPSPOT_R2_PUBLIC="$PUB" WHIPSPOT_R2_PRIVATE="$PRIV" WHIPSPOT_PACKED="$T/packed"
export WHIPSPOT_ROOT_KEY="$KEYS/root.key" WHIPSPOT_RELEASE_KEY_FILE="$KEYS/release.key"
export R2_ACCOUNT_ID=fakeaccount R2_ACCESS_KEY_ID=fakeid R2_SECRET_ACCESS_KEY="$FAKE_SECRET"
unset LUNEPACK_RELEASE_KEY
LOG="$T/all-output.log"
: >"$LOG"
run() { bash "$FAKE/scripts/publish-update.sh" "$@" >>"$LOG" 2>&1; }
refuse() { # <what> <reason text> <args...>: must exit non-zero for that reason and change no bucket file
  local what=$1 reason=$2 before_pub before_priv mark
  shift 2
  before_pub=$(snap "$PUB") before_priv=$(snap "$PRIV")
  mark=$(wc -l <"$LOG")
  if run "$@"; then fail "$what: it did not refuse"; fi
  tail -n +"$((mark + 1))" "$LOG" | grep -qF -- "$reason" || fail "$what: refused, but not with \"$reason\""
  [ "$(snap "$PUB")" = "$before_pub" ] && [ "$(snap "$PRIV")" = "$before_priv" ] || fail "$what: a bucket changed"
  [ ! -e "$FAKE/release/work" ] || fail "$what: release/work was left behind"
  ok "refused: $what"
}

make_app() { # <version>: a tiny unsigned app bundle whose files change per version
  local d="$T/apps/$1/Whipspot.app"
  mkdir -p "$d/Contents/MacOS" "$d/Contents/Resources"
  printf '<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleExecutable</key><string>Whipspot</string><key>CFBundleShortVersionString</key><string>%s</string></dict></plist>\n' "$1" >"$d/Contents/Info.plist"
  { echo '#!/bin/sh'; echo "echo whipspot $1"; head -c 65536 /dev/zero | tr '\0' 'w'; echo; } >"$d/Contents/MacOS/Whipspot"
  chmod 755 "$d/Contents/MacOS/Whipspot"
  printf 'data for %s\n' "$1" >"$d/Contents/Resources/data.txt"
  printf '%s' "$d"
}
pack_and_publish() { # <version>
  set_app_version "$1"
  run pack "$TARGET" "$(make_app "$1")" "$1" || fail "pack $1"
  run publish "$1" || fail "publish $1"
}
feed_list() { # the verified feed of the public bucket, as lunepack sees it
  "$BIN/lunepack" inspect "$PUB/whipspot" --channel stable --config "$FAKE/release/lunepack.toml" |
    sed -n 's/^  release \([^:]*\): .*$/\1/p' | tr '\n' ' ' | sed 's/ $//'
}

# ---- refusals before bootstrap -----------------------------------------------------------------
set_app_version 0.2.1
refuse "placeholder root key (pack)" "still holds a placeholder" pack "$TARGET" "$(make_app 0.2.1)" 0.2.1
refuse "placeholder root key (publish)" "still holds a placeholder" publish 0.2.1

# ---- bootstrap ---------------------------------------------------------------------------------
run bootstrap || fail "bootstrap"
[ -f "$PUB/whipspot/keys.json" ] && [ -f "$PRIV/whipspot/lunepack-state.json" ] || fail "bootstrap uploaded nothing"
grep -q 'PLACEHOLDER' "$FAKE/release/lunepack.toml" && fail "bootstrap left the placeholder"
ok "bootstrap"
cp "$KEYS/root.key" "$KEYS/root.key.kept"
mv "$KEYS/root.key" "$KEYS/root.key.moved" && mv "$KEYS/release.key" "$KEYS/release.key.moved"
cp "$FAKE/release/lunepack.toml" "$T/config.bootstrapped"
# A second bootstrap must refuse because keys.json exists (the config is put back to a placeholder
# and the key paths are free, so the remote check is what refuses).
cp "$REPO/release/lunepack.toml" "$FAKE/release/lunepack.toml"
refuse "bootstrap with keys.json already published" "keys.json already exists" bootstrap
cp "$T/config.bootstrapped" "$FAKE/release/lunepack.toml"
mv "$KEYS/root.key.moved" "$KEYS/root.key" && mv "$KEYS/release.key.moved" "$KEYS/release.key"
check_siblings bootstrap

# ---- 12 publishes, with an injected failure at the 12th -------------------------------------------
VERSIONS=(0.2.1 0.2.2 0.2.3 0.2.4 0.2.5 0.2.6 0.2.7 0.2.8 0.2.10-rc.1 0.2.9 0.2.10 0.2.11)
for v in "${VERSIONS[@]:0:3}"; do pack_and_publish "$v"; done
ok "three publishes: $(feed_list)"

# Refusals on a live release directory.
set_app_version 0.2.4
refuse "version != crates/whipspot-app/Cargo.toml (publish)" "is not crates/whipspot-app/Cargo.toml" publish 0.2.5
refuse "version != crates/whipspot-app/Cargo.toml (pack)" "is not crates/whipspot-app/Cargo.toml" pack "$TARGET" "$(make_app 0.2.5)" 0.2.5
set_app_version 0.2.3
mkdir -p "$T/packed-dup" && cp -R "$T/packed/0.2.3" "$T/packed-dup/"
WHIPSPOT_PACKED="$T/packed-dup" refuse "version already in the feed" "already in the stable feed" publish 0.2.3
set_app_version 0.2.4
run pack "$TARGET" "$(make_app 0.2.4)" 0.2.4 || fail "pack 0.2.4"
sed 's|^feed = .*|feed = "https://updates.izzat.dev/PLACEHOLDER/"|' "$T/config.bootstrapped" >"$FAKE/release/lunepack.toml"
refuse "placeholder feed" "still holds a placeholder" publish 0.2.4
sed 's|^    "[0-9a-f]\{64\}",$|    "ROOT-KEY-PLACEHOLDER-run-publish-update-bootstrap",|' "$T/config.bootstrapped" >"$FAKE/release/lunepack.toml"
grep -q PLACEHOLDER "$FAKE/release/lunepack.toml" || fail "the test could not plant a root placeholder"
refuse "placeholder root key" "still holds a placeholder" publish 0.2.4
cp "$T/config.bootstrapped" "$FAKE/release/lunepack.toml"
mv "$PRIV/whipspot/lunepack-state.json" "$T/state.aside"
refuse "private state file missing" "no publisher state" publish 0.2.4
mv "$T/state.aside" "$PRIV/whipspot/lunepack-state.json"

# An upload failing before the feed is uploaded: nothing deleted, feed and state unchanged.
inject() { # <op> <destination suffix> <version>
  local before_pub before_state
  before_pub=$(snap "$PUB/whipspot") before_state=$(shasum -a 256 <"$PRIV/whipspot/lunepack-state.json")
  local mark
  mark=$(wc -l <"$LOG")
  if FAKE_RCLONE_FAIL_OP=$1 FAKE_RCLONE_FAIL_DST="whipspot/$2" run publish "$3"; then fail "publish $3 survived an injected $1 failure"; fi
  tail -n +"$((mark + 1))" "$LOG" | grep -qF "injected failure" || fail "publish $3 failed before the injected $1 failure"
  # Every file that was there is still there, unchanged (new version files may have been added).
  local after_pub
  after_pub=$(snap "$PUB/whipspot")
  while IFS= read -r line; do
    grep -qxF -- "$line" <<<"$after_pub" || fail "injected $1 failure: a file was deleted or changed"
  done <<<"$before_pub"
  [ "$(shasum -a 256 <"$PRIV/whipspot/lunepack-state.json")" = "$before_state" ] || fail "injected failure: the state was saved"
  ok "injected $1 failure on $2 deleted nothing"
}
inject copy "0.2.4" 0.2.4
run publish 0.2.4 || fail "publish 0.2.4 after a failed attempt"
for v in "${VERSIONS[@]:4:7}"; do pack_and_publish "$v"; done
ok "eleven publishes: $(feed_list)"
[ "$(feed_list)" = "0.2.2 0.2.3 0.2.4 0.2.5 0.2.6 0.2.7 0.2.8 0.2.9 0.2.10-rc.1 0.2.10" ] || fail "the feed after 11 publishes is not the 10 newest by precedence"
[ ! -e "$PUB/whipspot/0.2.1" ] || fail "0.2.1 was not deleted after the 11th publish"

set_app_version 0.2.11
run pack "$TARGET" "$(make_app 0.2.11)" 0.2.11 || fail "pack 0.2.11"
inject copyto "feed-stable.json" 0.2.11
[ -d "$PUB/whipspot/0.2.2" ] || fail "a failed publish deleted 0.2.2"
before_state=$(shasum -a 256 <"$PRIV/whipspot/lunepack-state.json")
run publish 0.2.11 || fail "publish 0.2.11"
[ "$(shasum -a 256 <"$PRIV/whipspot/lunepack-state.json")" != "$before_state" ] || fail "a successful publish did not save the state"

# ---- the end state -----------------------------------------------------------------------------
WANT="0.2.3 0.2.4 0.2.5 0.2.6 0.2.7 0.2.8 0.2.9 0.2.10-rc.1 0.2.10 0.2.11"
[ "$(feed_list)" = "$WANT" ] || fail "the feed does not list exactly the 10 newest by precedence"
read -r -a want_arr <<<"$WANT"
want_listing=$( (printf '%s/\n' "${want_arr[@]}"; echo feed-stable.json; echo keys.json) | sort)
have_listing=$(cd "$PUB/whipspot" && for f in *; do if [ -d "$f" ]; then echo "$f/"; else echo "$f"; fi; done | grep -v '^root-[0-9]*\.json$' | sort)
[ "$have_listing" = "$want_listing" ] || fail "the public folder does not hold exactly the 10 releases and the documents"
verify_out=$("$BIN/lunepack" verify "$PUB/whipspot" --config "$FAKE/release/lunepack.toml") || fail "lunepack verify rejects the public folder"
grep -q ': unreferenced' <<<"$verify_out" && fail "the public folder holds unreferenced manifests"
grep -q '\.delta' <<<"$verify_out" || echo "test-publish-update: note: no delta was written (the fake app may be too small to patch)"
[ "$(ls "$PRIV")" = "luneterm"$'\n'"whipspot" ] || fail "the private bucket holds more than the two prefixes"
[ "$(ls "$PRIV/whipspot")" = lunepack-state.json ] || fail "the private prefix holds more than the state"
check_siblings "12 publishes"
ok "12 publishes: the feed and the folder hold exactly the 10 newest; other folders untouched"

# ---- the R2 path: the env remote is defined, nothing reaches the network ------------------------
set_app_version 0.2.12
run pack "$TARGET" "$(make_app 0.2.12)" 0.2.12 || fail "pack 0.2.12"
WHIPSPOT_R2_PUBLIC=whipspotr2:izzat-updates WHIPSPOT_R2_PRIVATE=whipspotr2:izzat-updates-publisher \
  refuse "an unreachable R2 remote" "no publisher state" publish 0.2.12
[ -e "$T/r2-remote-defined" ] || fail "the rclone R2 remote was not defined from the environment"

# ---- nothing secret was printed ----------------------------------------------------------------
check_absent() { # <what> <needle file>: the needle is in the file (control) and not in the log
  local needle
  needle=$(sed -n '2p' "$2")
  [ ${#needle} -ge 20 ] || fail "$1: the control has no needle"
  grep -qF -- "$needle" "$2" || fail "$1: the control grep does not find the needle"
  if grep -qF -- "$needle" "$LOG"; then fail "$1 appears in the script's output"; fi
}
check_absent "the release key" "$KEYS/release.key"
check_absent "the root key" "$KEYS/root.key.kept"
printf 'control\n%s\n' "$FAKE_SECRET" >"$T/secret.control"
check_absent "the R2 secret" "$T/secret.control"
if grep -q 'PRIVATE KEY' "$LOG"; then fail "a PEM marker appears in the script's output"; fi
ok "no key, secret or PEM in $(wc -l <"$LOG" | tr -d ' ') lines of output"

shellcheck_ok=skipped
if command -v shellcheck >/dev/null; then
  shellcheck "$REPO/scripts/publish-update.sh" "$REPO/scripts/test-publish-update.sh" || fail "shellcheck"
  shellcheck_ok=clean
fi
echo "test-publish-update: PASS (target $TARGET, shellcheck $shellcheck_ok)"
