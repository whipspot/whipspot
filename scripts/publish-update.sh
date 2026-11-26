#!/bin/bash
# Packs and publishes Whipspot updates with lunepack, to the R2 buckets or to local folders.
#
#   publish-update.sh bootstrap                    once: root and release keys, keys.json, state
#   publish-update.sh pack <target> <input> <ver>  one target's pack (+ delta from the previous
#                                                  release) into $WHIPSPOT_PACKED/<ver>/
#   publish-update.sh setup <target> <ver>         the installer (.exe or .dmg) from that pack
#   publish-update.sh publish <ver>                add <ver> to the stable feed, keep the newest 10
#
# Environment:
#   WHIPSPOT_R2_PUBLIC    public bucket: an rclone path or a local folder (default whipspotr2:izzat-updates),
#                         or the public feed's base URL (https://updates.izzat.dev/), which is read-only
#                         and needs no key: pack and setup read it that way in CI, so the jobs that sign
#                         hold no R2 key (publish and bootstrap refuse a URL)
#   WHIPSPOT_R2_PRIVATE   private bucket, the publisher state (default whipspotr2:izzat-updates-publisher)
#   R2_ACCOUNT_ID, R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY
#                         define the rclone remote `whipspotr2:` (S3 on R2) in the environment only,
#                         used only when a bucket the command touches starts with `whipspotr2:` (pack
#                         and setup touch the public bucket only, publish and bootstrap both)
#   LUNEPACK_RELEASE_KEY  the release key (PKCS#8 PEM), or WHIPSPOT_RELEASE_KEY_FILE naming its file
#   WHIPSPOT_ROOT_KEY     bootstrap only: path of the new root key file (outside the repository)
#   LUNEPACK              the lunepack CLI (default `lunepack` on PATH)
#   LUNEPACK_BIN_DIR      pack: holds lunepack-runtime, lunepack-ui and (Windows) lunepack-stub.exe
#   WHIPSPOT_PACKED       pack output and publish input (default dist/update)
#   WHIPSPOT_DIST         setup output (default dist)
#
# Every bucket path this script builds starts with `whipspot/` (the buckets are shared with other
# apps). Nothing here prints a key, an environment value or a secret; never run it under `set -x`.
set -euo pipefail
set +x

REPO=$(cd "$(dirname "$0")/.." && pwd)
CONFIG="$REPO/release/lunepack.toml"
WORK="$REPO/release/work"
MACOS_CONFIG="$REPO/release/.lunepack-macos.toml"
PREFIX=whipspot
CHANNEL=stable
KEEP=10
PUBLIC=${WHIPSPOT_R2_PUBLIC:-whipspotr2:izzat-updates}
PRIVATE=${WHIPSPOT_R2_PRIVATE:-whipspotr2:izzat-updates-publisher}
LUNEPACK=${LUNEPACK:-lunepack}
PACKED=${WHIPSPOT_PACKED:-$REPO/dist/update}
DIST=${WHIPSPOT_DIST:-$REPO/dist}

die() { echo "publish-update: $*" >&2; exit 1; }
say() { echo "publish-update: $*"; }

# The public bucket as a URL: read through rclone's http backend (no listing, no key). Only https,
# or http on 127.0.0.1 for the tests; nothing else can reach a connection string.
PUBLIC_HTTP=0
case "$PUBLIC" in
  http://* | https://*)
    [[ "$PUBLIC" =~ ^(https://[A-Za-z0-9.-]+(:[0-9]+)?|http://127\.0\.0\.1:[0-9]+)/?$ ]] ||
      die "WHIPSPOT_R2_PUBLIC is a URL that is not a plain https host (or http://127.0.0.1:<port>)"
    PUBLIC=":http,url='${BASH_REMATCH[1]}/':"
    PUBLIC_HTTP=1
    ;;
esac

# ---- checks -------------------------------------------------------------------------------------

# A version as lunepack and a bucket folder name accept it: no slash, no dot-only name.
check_version_text() {
  [[ "$1" =~ ^[0-9A-Za-z][0-9A-Za-z.+-]*$ ]] || die "not a version: refused"
}

app_version() {
  sed -n 's/^version = "\(.*\)"$/\1/p' "$REPO/crates/whipspot-app/Cargo.toml" | head -1
}

check_app_version() {
  local want
  want=$(app_version)
  [ -n "$want" ] || die "no version in crates/whipspot-app/Cargo.toml"
  [ "$1" = "$want" ] || die "version $1 is not crates/whipspot-app/Cargo.toml's $want"
}

# Fails on a placeholder anywhere outside comments (the root key until bootstrap, a feed).
check_no_placeholder() {
  [ -f "$CONFIG" ] || die "no release/lunepack.toml"
  if grep -v '^[[:space:]]*#' "$CONFIG" | grep -q -i 'placeholder'; then
    die "release/lunepack.toml still holds a placeholder (root key or feed); run bootstrap first"
  fi
}

check_target() {
  case "$1" in
    windows-x86_64 | macos-universal | macos-aarch64 | macos-x86_64) ;;
    *) die "unknown target $1" ;;
  esac
}

need_release_key() {
  if [ -z "${LUNEPACK_RELEASE_KEY:-}" ]; then
    [ -n "${WHIPSPOT_RELEASE_KEY_FILE:-}" ] || die "set LUNEPACK_RELEASE_KEY or WHIPSPOT_RELEASE_KEY_FILE"
    [ -f "$WHIPSPOT_RELEASE_KEY_FILE" ] || die "WHIPSPOT_RELEASE_KEY_FILE does not name a file"
    LUNEPACK_RELEASE_KEY=$(<"$WHIPSPOT_RELEASE_KEY_FILE")
  fi
  export LUNEPACK_RELEASE_KEY
}

# ---- bucket paths and rclone -------------------------------------------------------------------

# The one way to name a bucket object: <bucket>/<rel>, where <rel> is `whipspot/` or under it.
# Sets BP rather than printing it, so a refusal ends the script instead of a command substitution.
bucket_ref() {
  local bucket=$1 rel=$2
  case "$rel" in
    "$PREFIX"/*) ;;
    *) die "refused a bucket path outside $PREFIX/" ;;
  esac
  case "$rel" in
    *..* | *//* | */./* | *[[:space:]]* | *\\*) die "refused a bucket path that is not plain" ;;
  esac
  BP="${bucket%/}/$rel"
}

# setup_rclone <pub|priv>...: the buckets the command touches. The R2 remote and its keys are
# required only when one of those is an R2 path.
setup_rclone() {
  local used="" b
  for b in "$@"; do
    case "$b" in
      pub) used+=" $PUBLIC" ;;
      priv) used+=" $PRIVATE" ;;
      *) die "setup_rclone: unknown bucket $b" ;;
    esac
  done
  case "$used" in
    *whipspotr2:*) ;;
    *) return 0 ;;
  esac
  [ -n "${R2_ACCOUNT_ID:-}" ] && [ -n "${R2_ACCESS_KEY_ID:-}" ] && [ -n "${R2_SECRET_ACCESS_KEY:-}" ] ||
    die "set R2_ACCOUNT_ID, R2_ACCESS_KEY_ID and R2_SECRET_ACCESS_KEY"
  export RCLONE_CONFIG_WHIPSPOTR2_TYPE=s3
  export RCLONE_CONFIG_WHIPSPOTR2_PROVIDER=Cloudflare
  export RCLONE_CONFIG_WHIPSPOTR2_ENDPOINT="https://${R2_ACCOUNT_ID}.r2.cloudflarestorage.com"
  export RCLONE_CONFIG_WHIPSPOTR2_ACCESS_KEY_ID="$R2_ACCESS_KEY_ID"
  export RCLONE_CONFIG_WHIPSPOTR2_SECRET_ACCESS_KEY="$R2_SECRET_ACCESS_KEY"
  export RCLONE_CONFIG_WHIPSPOTR2_NO_CHECK_BUCKET=true
}

# rclone, with every bucket path written as @pub:<rel> or @priv:<rel> and checked by bucket_ref.
# A purge takes exactly one release folder, @pub:whipspot/<version>.
rc() {
  local -a args=()
  local a refs=0
  for a in "$@"; do
    case "$a" in
      "") die "refused an empty rclone argument" ;;
      @pub:*) bucket_ref "$PUBLIC" "${a#@pub:}"; args+=("$BP"); refs=$((refs + 1)) ;;
      @priv:*) bucket_ref "$PRIVATE" "${a#@priv:}"; args+=("$BP"); refs=$((refs + 1)) ;;
      *) args+=("$a") ;;
    esac
  done
  [ "$refs" -ge 1 ] || die "refused an rclone call without a bucket path"
  if [ "$1" = purge ]; then
    [ $# = 2 ] && [[ "$2" =~ ^@pub:$PREFIX/[0-9A-Za-z][0-9A-Za-z.+-]*$ ]] || die "refused a purge that is not one release folder"
  fi
  rclone -q --stats 0 "${args[@]}"
}

# 0 when <name> exists in the folder <rel> of <bucket> (@pub or @priv), 1 when it does not, and
# dies when that cannot be told.
bucket_has() { # <@pub|@priv> <rel folder> <name>
  local out status=0
  out=$(rc lsf --max-depth 1 "$1:$2/" 2>/dev/null) || status=$?
  case "$status" in
    0) grep -qxF -e "$3" -e "$3/" <<<"$out" ;;
    3) return 1 ;; # directory not found
    *) die "cannot list a bucket (rclone exit $status)" ;;
  esac
}

fetch_file() { # <@pub|@priv> <rel> <local file>
  rc copyto "$1:$2" "$3"
}

# Fetches @pub:<rel> to <local file>: 0 when fetched, 1 when the server has no such file; anything
# else (a refused connection, a 403, a 5xx) is not "absent" and ends the script.
fetch_optional() { # <rel> <local file>
  local status=0
  rc copyto "@pub:$1" "$2" 2>/dev/null || status=$?
  case "$status" in
    0) return 0 ;;
    3 | 4) return 1 ;;
    *) die "cannot read the public bucket (rclone exit $status)" ;;
  esac
}

# keys.json, root-*.json and the channel's feed into $WORK/rel (the feed may be absent).
fetch_trust_docs() {
  mkdir -p "$WORK/rel"
  if [ "$PUBLIC_HTTP" = 1 ]; then
    # A URL has no listing: the documents are fetched by name. The root documents are root-1.json,
    # root-2.json, ... (a root rotation adds one); the first missing one ends the run.
    local n=1
    fetch_optional "$PREFIX/keys.json" "$WORK/rel/keys.json" ||
      die "no $PREFIX/keys.json at the public feed; run bootstrap first"
    fetch_optional "$PREFIX/feed-$CHANNEL.json" "$WORK/rel/feed-$CHANNEL.json" || true
    while [ "$n" -le 64 ] && fetch_optional "$PREFIX/root-$n.json" "$WORK/rel/root-$n.json"; do n=$((n + 1)); done
    return 0
  fi
  rc copy "@pub:$PREFIX/keys.json" "$WORK/rel" 2>/dev/null ||
    die "no $PREFIX/keys.json in the public bucket; run bootstrap first"
  rc copy "@pub:$PREFIX/" "$WORK/rel" --max-depth 1 \
    --include "/root-*.json" --include "/feed-$CHANNEL.json"
}

# ---- work directory -----------------------------------------------------------------------------

lock_work() {
  mkdir "$WORK" 2>/dev/null || die "release/work exists: another run is going, or a failed run left it (remove it by hand)"
  trap 'rm -rf "$WORK" "$MACOS_CONFIG"' EXIT
}

# The icon file of the packed tree: a PNG at the root for Windows, inside the bundle for macOS.
icon_rel_for() {
  case "$1" in
    windows-*) printf '%s' "whipspot-256.png" ;;
    macos-*) printf '%s' "Contents/Resources/whipspot-256.png" ;;
  esac
}

# The config for a target: a macOS bundle's entry is Contents/MacOS/Whipspot and its icon sits in
# Contents/Resources.
config_for() {
  case "$1" in
    windows-*) printf '%s' "$CONFIG" ;;
    macos-*)
      local n
      n=$(grep -c '^entry = "whipspot.exe"$' "$CONFIG" || true)
      [ "$n" = 1 ] || die "release/lunepack.toml has no single [install] entry line to adapt for macOS"
      n=$(grep -c '^icon = "whipspot-256.png"$' "$CONFIG" || true)
      [ "$n" = 1 ] || die "release/lunepack.toml has no single [install] icon line to adapt for macOS"
      sed -e 's|^entry = "whipspot.exe"$|entry = "Contents/MacOS/Whipspot"|' \
        -e "s|^icon = \"whipspot-256.png\"\$|icon = \"$(icon_rel_for "$1")\"|" "$CONFIG" >"$MACOS_CONFIG"
      printf '%s' "$MACOS_CONFIG"
      ;;
  esac
}

# The tree to pack: the input itself when it holds the install icon, else a copy of it with the
# icon added (the input is never changed). Prints the tree's path.
tree_with_icon() {
  local target=$1 input=$2 rel src="$REPO/assets/icon/whipspot-256.png"
  rel=$(icon_rel_for "$target")
  if [ -f "$input/$rel" ] && [ ! -L "$input/$rel" ]; then printf '%s' "$input"; return; fi
  [ ! -e "$input/$rel" ] || die "the install icon in the input is not a regular file"
  [ -f "$src" ] && [ ! -L "$src" ] || die "assets/icon/whipspot-256.png is missing (scripts/make-icons.sh makes it)"
  mkdir -p "$WORK/tree"
  cp -Rp "$input/." "$WORK/tree/"
  mkdir -p "$WORK/tree/$(dirname "$rel")"
  cp "$src" "$WORK/tree/$rel"
  printf '%s' "$WORK/tree"
}

lp() { "$LUNEPACK" "$@"; }

# The versions of the feed in $WORK/rel, verified by lunepack, in the feed's order (lunepack's
# version precedence, lowest first). Prints nothing when there is no feed. Fails closed.
feed_versions() {
  local out count listed
  [ -f "$WORK/rel/feed-$CHANNEL.json" ] || return 0
  out=$(lp inspect "$WORK/rel" --channel "$CHANNEL" --config "$CONFIG") || die "the release directory does not verify"
  count=$(sed -n "s/^feed $CHANNEL (feed-$CHANNEL.json): .*, \([0-9][0-9]*\) release(s)\$/\1/p" <<<"$out")
  [ -n "$count" ] || die "lunepack inspect printed no $CHANNEL feed"
  listed=$(sed -n 's/^  release \([^:]*\): .*$/\1/p' <<<"$out")
  [ "$(grep -c . <<<"$listed" || true)" = "$count" ] || die "lunepack inspect's release lines do not match its count"
  while IFS= read -r v; do
    [ -n "$v" ] || continue
    check_version_text "$v"
    printf '%s\n' "$v"
  done <<<"$listed"
}

# The versions a feed file names, unverified: only to choose what to download (lunepack verifies
# everything before it is used).
feed_file_versions() { # <feed file> [<target>]
  [ -f "$1" ] || return 0
  local payload
  payload=$(jq -r '.payload' "$1") || return 1
  base64 -d <<<"$payload" | jq -r --arg t "${2:-}" \
    '.releases[] | select($t == "" or .targets[$t] != null) | .version'
}

# ---- bootstrap ----------------------------------------------------------------------------------

cmd_bootstrap() {
  local root_key=${WHIPSPOT_ROOT_KEY:-} rel_key=${WHIPSPOT_RELEASE_KEY_FILE:-} f app_id root_pub rel_pub
  [ "$PUBLIC_HTTP" = 0 ] || die "bootstrap writes the public bucket: it cannot use a URL"
  [ -n "$root_key" ] || die "set WHIPSPOT_ROOT_KEY to the path of the new root key file"
  [ -n "$rel_key" ] || die "set WHIPSPOT_RELEASE_KEY_FILE to the path of the new release key file"
  for f in "$root_key" "$rel_key"; do
    [ ! -e "$f" ] || die "a key file already exists at a given path; bootstrap never overwrites one"
    [ -d "$(dirname "$f")" ] || die "the directory of a key file does not exist"
    case "$(cd "$(dirname "$f")" && pwd -P)/" in
      "$(cd "$REPO" && pwd -P)"/*) die "key files must live outside the repository" ;;
    esac
  done
  grep -q '^    "ROOT-KEY-PLACEHOLDER-run-publish-update-bootstrap",$' "$CONFIG" ||
    die "release/lunepack.toml holds no root key placeholder: already bootstrapped"
  app_id=$(sed -n 's/^app_id = "\([a-z0-9._-]*\)"$/\1/p' "$CONFIG")
  [ -n "$app_id" ] || die "no app_id in release/lunepack.toml"
  setup_rclone pub priv
  if bucket_has @pub "$PREFIX" keys.json; then die "$PREFIX/keys.json already exists in the public bucket"; fi
  if bucket_has @priv "$PREFIX" lunepack-state.json; then die "$PREFIX/lunepack-state.json already exists in the private bucket"; fi
  lock_work
  mkdir -p "$WORK/cfg" "$WORK/rel"

  root_pub=$(lp keys gen "$root_key" | sed -n 's/^public: \([0-9a-f]\{64\}\)$/\1/p')
  rel_pub=$(lp keys gen "$rel_key" | sed -n 's/^public: \([0-9a-f]\{64\}\)$/\1/p')
  [ -n "$root_pub" ] && [ -n "$rel_pub" ] || die "lunepack keys gen printed no public key"
  lp init --app-id "$app_id" --root-key "$root_pub" --config "$WORK/cfg/lunepack.toml" >/dev/null
  lp keys set --release-dir "$WORK/rel" --release-key "$rel_pub" --sign "file:$root_key" \
    --config "$WORK/cfg/lunepack.toml"
  lp verify "$WORK/rel" --config "$WORK/cfg/lunepack.toml"

  rc copyto "$WORK/rel/keys.json" "@pub:$PREFIX/keys.json"
  rc copyto "$WORK/cfg/lunepack-state.json" "@priv:$PREFIX/lunepack-state.json"

  sed "s|^    \"ROOT-KEY-PLACEHOLDER-run-publish-update-bootstrap\",\$|    \"$root_pub\",|" "$CONFIG" >"$WORK/lunepack.toml.new"
  mv "$WORK/lunepack.toml.new" "$CONFIG"
  say "bootstrapped: keys.json and the publisher state are uploaded; release/lunepack.toml has the root key."
  say "next: commit release/lunepack.toml; store the release key file's contents as the GitHub secret"
  say "      LUNEPACK_RELEASE_KEY (gh secret set LUNEPACK_RELEASE_KEY < <file>); keep the root key offline."
}

# ---- pack ---------------------------------------------------------------------------------------

cmd_pack() {
  [ $# = 3 ] || die "usage: pack <target> <input> <version>"
  local target=$1 input=$2 version=$3 cfg prev ext="" out
  check_target "$target"
  check_version_text "$version"
  check_app_version "$version"
  check_no_placeholder
  [ -d "$input" ] || die "the input is not a directory"
  [ -n "${LUNEPACK_BIN_DIR:-}" ] || die "set LUNEPACK_BIN_DIR"
  out="$PACKED/$version"
  [ ! -e "$out/$target.manifest.json" ] && [ ! -e "$out/$target.pack" ] || die "$version/$target is already packed"
  need_release_key
  setup_rclone pub
  lock_work
  fetch_trust_docs
  cfg=$(config_for "$target")
  input=$(tree_with_icon "$target" "$input")

  # The previous release of this target, for a delta (D7: from the previous version only).
  prev=$(feed_file_versions "$WORK/rel/feed-$CHANNEL.json" "$target") || die "cannot read the $CHANNEL feed"
  prev=$(tail -n 1 <<<"$prev")
  if [ -n "$prev" ]; then
    check_version_text "$prev"
    [ "$prev" != "$version" ] || die "$version is already in the $CHANNEL feed"
    mkdir -p "$WORK/rel/$prev"
    fetch_file @pub "$PREFIX/$prev/$target.pack" "$WORK/rel/$prev/$target.pack"
    fetch_file @pub "$PREFIX/$prev/$target.manifest.json" "$WORK/rel/$prev/$target.manifest.json"
  fi

  case "$target" in windows-*) ext=.exe ;; esac
  local args=(pack --input "$input" --version "$version" --target "$target" --release-dir "$WORK/rel"
    --sign release --deltas 1 --runtime "$LUNEPACK_BIN_DIR/lunepack-runtime$ext" --config "$cfg")
  [ ! -f "$LUNEPACK_BIN_DIR/lunepack-ui$ext" ] || args+=(--ui "$LUNEPACK_BIN_DIR/lunepack-ui$ext")
  case "$target" in windows-*) args+=(--stub "$LUNEPACK_BIN_DIR/lunepack-stub.exe") ;; esac
  lp "${args[@]}"

  mkdir -p "$out"
  cp "$WORK/rel/$version/$target".* "$out/"
  say "packed $version/$target into $out"
}

# ---- setup --------------------------------------------------------------------------------------

cmd_setup() {
  [ $# = 2 ] || die "usage: setup <target> <version>"
  local target=$1 version=$2 cfg file
  check_target "$target"
  check_version_text "$version"
  check_app_version "$version"
  check_no_placeholder
  [ -f "$PACKED/$version/$target.manifest.json" ] || die "$version/$target is not packed"
  setup_rclone pub
  lock_work
  fetch_trust_docs
  cfg=$(config_for "$target")
  mkdir -p "$WORK/rel/$version" "$DIST"
  cp "$PACKED/$version/$target".* "$WORK/rel/$version/"
  case "$target" in
    windows-*) file="$DIST/whipspot-$version-$target-setup.exe" ;;
    macos-*) file="$DIST/whipspot-$version-$target.dmg" ;;
  esac
  lp setup "$WORK/rel" --version "$version" --target "$target" --out "$file" --config "$cfg"
  say "installer: $file"
}

# ---- publish ------------------------------------------------------------------------------------

cmd_publish() {
  [ $# = 1 ] || die "usage: publish <version>"
  local version=$1 v
  check_version_text "$version"
  check_app_version "$version"
  check_no_placeholder
  ls "$PACKED/$version/"*.manifest.json >/dev/null 2>&1 || die "nothing packed for $version"
  [ "$PUBLIC_HTTP" = 0 ] || die "publish writes the public bucket: it cannot use a URL"
  need_release_key
  setup_rclone pub priv
  lock_work

  # The publisher state: never rebuilt from the public bucket.
  fetch_file @priv "$PREFIX/lunepack-state.json" "$WORK/lunepack-state.json" 2>/dev/null ||
    die "no publisher state in the private bucket; run bootstrap first"
  fetch_trust_docs
  local -a drop=() kept=() after=() final=()
  local listed
  # Download the releases the feed names (unverified, only to choose), then verify them all.
  listed=$(feed_file_versions "$WORK/rel/feed-$CHANNEL.json") || die "cannot read the $CHANNEL feed"
  for v in $listed; do
    check_version_text "$v"
    [ "$v" != "$version" ] || die "$version is already in the $CHANNEL feed"
    mkdir -p "$WORK/rel/$v"
    rc copy "@pub:$PREFIX/$v" "$WORK/rel/$v"
  done
  listed=$(feed_versions) || exit 1
  for v in $listed; do
    [ "$v" != "$version" ] || die "$version is already in the $CHANNEL feed"
  done

  [ ! -e "$WORK/rel/$version" ] || die "$version is already in the release directory"
  mkdir -p "$WORK/rel/$version"
  cp "$PACKED/$version/"* "$WORK/rel/$version/"
  lp feed add --release-dir "$WORK/rel" --channel "$CHANNEL" --version "$version" --sign release --config "$CONFIG"

  # The drop list comes from the verified feed lunepack just wrote, lowest precedence first.
  listed=$(feed_versions) || exit 1
  for v in $listed; do after+=("$v"); done
  local n=${#after[@]} i
  for ((i = 0; i < n; i++)); do
    if ((i < n - KEEP)); then drop+=("${after[$i]}"); else kept+=("${after[$i]}"); fi
  done
  for v in ${drop[@]+"${drop[@]}"}; do
    [ "$v" != "$version" ] || die "$version would be dropped at once: $KEEP newer releases exist"
  done
  if ((${#drop[@]} > 0)); then
    local -a dargs=()
    for v in "${drop[@]}"; do dargs+=(--drop "$v"); done
    lp feed refresh --release-dir "$WORK/rel" --channel "$CHANNEL" --sign release "${dargs[@]}" --config "$CONFIG"
  fi
  lp verify "$WORK/rel" --config "$CONFIG" >/dev/null || die "the new release directory does not verify"
  listed=$(feed_versions) || exit 1
  for v in $listed; do final+=("$v"); done
  [ "${final[*]}" = "${kept[*]}" ] || die "the refreshed feed does not list the kept releases"

  # Upload in lunepack's order: packs and deltas, manifests, root and keys, the feed last.
  rc copy "$WORK/rel/$version" "@pub:$PREFIX/$version" --include "*.pack" --include "*.delta"
  rc copy "$WORK/rel/$version" "@pub:$PREFIX/$version" --include "*.manifest.json"
  rc copy "$WORK/rel" "@pub:$PREFIX/" --max-depth 1 --include "/root-*.json"
  rc copyto "$WORK/rel/keys.json" "@pub:$PREFIX/keys.json"
  rc copyto "$WORK/rel/feed-$CHANNEL.json" "@pub:$PREFIX/feed-$CHANNEL.json"
  say "published $version; the $CHANNEL feed lists ${#final[@]} release(s)"

  # Only now, with the new feed live, the dropped releases' folders go.
  local failed=0 k
  for v in ${drop[@]+"${drop[@]}"}; do
    check_version_text "$v"
    for k in "${final[@]}"; do [ "$v" != "$k" ] || die "refused to delete $v: the feed lists it"; done
    if rc purge "@pub:$PREFIX/$v"; then
      say "deleted $PREFIX/$v/"
    else
      echo "publish-update: could not delete $PREFIX/$v/ (no feed lists it; remove it by hand)" >&2
      failed=1
    fi
  done
  [ "$failed" = 0 ] || die "publish done but some dropped folders remain; the publisher state was not saved (the next publish accepts the newer feed)"
  rc copyto "$WORK/lunepack-state.json" "@priv:$PREFIX/lunepack-state.json"
  say "saved the publisher state"
}

# ---- main ---------------------------------------------------------------------------------------

cmd=${1:-}
[ $# -gt 0 ] && shift
case "$cmd" in
  bootstrap) [ $# = 0 ] || die "usage: bootstrap"; cmd_bootstrap ;;
  pack) cmd_pack "$@" ;;
  setup) cmd_setup "$@" ;;
  publish) cmd_publish "$@" ;;
  *) die "usage: publish-update.sh bootstrap | pack <target> <input> <version> | setup <target> <version> | publish <version>" ;;
esac
