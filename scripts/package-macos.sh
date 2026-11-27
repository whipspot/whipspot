#!/bin/bash
# Builds dist/Whipspot.app (release, universal arm64 + x86_64 when both targets are installed).
# Ad-hoc signed only: no Developer ID, no notarisation. The app is distributed by lunepack
# (packed by .github/workflows/release.yml), not zipped here.
# Usage: bash scripts/package-macos.sh [--arch arm64|x86_64|universal] [--unsealed]
#   --unsealed  leave the bundle without a seal (no Contents/_CodeSignature; only the main
#               executable carries its own ad-hoc signature, made before it enters the bundle,
#               since codesign on a path inside a bundle seals the whole bundle) for
#               `lunepack pack`: it refuses a sealed bundle that lacks the
#               runtime it would add.
set -euo pipefail
cd "$(dirname "$0")/.."

arch=universal
unsealed=0
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) arch="${2:?--arch needs a value}"; shift 2 ;;
    --unsealed) unsealed=1; shift ;;
    *) echo "package-macos: unknown argument $1" >&2; exit 1 ;;
  esac
done
case "$arch" in
  arm64) targets=(aarch64-apple-darwin) ;;
  x86_64) targets=(x86_64-apple-darwin) ;;
  universal) targets=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  *) echo "package-macos: unknown arch $arch" >&2; exit 1 ;;
esac

version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/whipspot-app/Cargo.toml | head -1)
bins=()
for t in "${targets[@]}"; do
  rustup target add "$t" >/dev/null
  MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked -p whipspot-app --target "$t"
  bins+=("target/$t/release/whipspot-app")
done

app=dist/Whipspot.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
if [ "${#bins[@]}" -gt 1 ]; then
  lipo -create -output "$stage/Whipspot" "${bins[@]}"
else
  cp "${bins[0]}" "$stage/Whipspot"
fi
if [ "$unsealed" = 1 ]; then codesign --force --sign - "$stage/Whipspot"; fi
cp -p "$stage/Whipspot" "$app/Contents/MacOS/Whipspot"

# The app icon (scripts/make-icons.sh), and the PNG lunepack names as install.icon (release/lunepack.toml);
# both go in before the signature so the seal covers them.
cp assets/icon/whipspot.icns "$app/Contents/Resources/Whipspot.icns"
cp assets/icon/whipspot-256.png "$app/Contents/Resources/whipspot-256.png"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Whipspot</string>
  <key>CFBundleDisplayName</key><string>Whipspot</string>
  <key>CFBundleIdentifier</key><string>io.github.whipspot</string>
  <key>CFBundleExecutable</key><string>Whipspot</string>
  <key>CFBundleIconFile</key><string>Whipspot.icns</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

if [ "$unsealed" = 0 ]; then codesign --force --deep --sign - "$app"; fi
echo "$app"
