#!/bin/bash
# Builds Whipspot.app (release, universal arm64 + x86_64 when both targets are installed) and
# zips it to dist/whipspot-<version>-macos-<arch>.zip. Ad-hoc signed only: no Developer ID,
# no notarisation. Usage: bash scripts/package-macos.sh [--arch arm64|x86_64|universal]
set -euo pipefail
cd "$(dirname "$0")/.."

arch=universal
if [ "${1:-}" = "--arch" ]; then arch="${2:?--arch needs a value}"; fi
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
mkdir -p "$app/Contents/MacOS"
if [ "${#bins[@]}" -gt 1 ]; then
  lipo -create -output "$app/Contents/MacOS/Whipspot" "${bins[@]}"
else
  cp "${bins[0]}" "$app/Contents/MacOS/Whipspot"
fi

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Whipspot</string>
  <key>CFBundleDisplayName</key><string>Whipspot</string>
  <key>CFBundleIdentifier</key><string>io.github.whipspot</string>
  <key>CFBundleExecutable</key><string>Whipspot</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$app"
zip="dist/whipspot-${version}-macos-${arch}.zip"
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "$zip"
