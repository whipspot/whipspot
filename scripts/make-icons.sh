#!/bin/bash
# Regenerates the app icon files from the two SVG masters in assets/icon/ (macOS only; needs
# rsvg-convert, magick and iconutil). whipspot.svg is the master for 48 px and up,
# whipspot-small.svg for 16, 24 and 32 px. Output, committed:
#   assets/icon/whipspot.icns   16 to 512 at @1x and @2x (ten images)
#   assets/icon/whipspot.ico    16, 24, 32 (small master); 48, 64, 128, 256 (full master)
#   assets/icon/whipspot-256.png  lunepack's install.icon
# Usage: bash scripts/make-icons.sh
set -euo pipefail
cd "$(dirname "$0")/.."

for tool in rsvg-convert magick iconutil; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "make-icons: '$tool' is not installed (needs rsvg-convert, magick and iconutil; macOS only)" >&2
    exit 1
  }
done

dir=assets/icon
full=$dir/whipspot.svg
small=$dir/whipspot-small.svg
[ -f "$full" ] && [ -f "$small" ] || { echo "make-icons: the SVG masters are missing from $dir" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# One PNG of `px` pixels: the small master up to 32 px, the full one above.
render() {
  local px=$1 out=$2 src=$full
  if [ "$px" -le 32 ]; then src=$small; fi
  rsvg-convert --width "$px" --height "$px" --format png --output "$out" "$src"
}

# .ico: seven sizes in a fixed order. -strip and the date defines keep the bytes reproducible.
pngs=()
for px in 16 24 32 48 64 128 256; do
  render "$px" "$work/ico-$px.png"
  pngs+=("$work/ico-$px.png")
done
magick "${pngs[@]}" -strip -define png:exclude-chunk=date,time "$dir/whipspot.ico"

# lunepack install.icon: a square PNG of at most 256 px.
render 256 "$dir/whipspot-256.png"
magick "$dir/whipspot-256.png" -strip -define png:exclude-chunk=date,time "$work/p.png"
cp "$work/p.png" "$dir/whipspot-256.png"

# .icns: the ten iconset images.
set_dir="$work/whipspot.iconset"
mkdir -p "$set_dir"
for base in 16 32 128 256 512; do
  render "$base" "$set_dir/icon_${base}x${base}.png"
  render $((base * 2)) "$set_dir/icon_${base}x${base}@2x.png"
done
iconutil -c icns -o "$dir/whipspot.icns" "$set_dir"

echo "make-icons: wrote $dir/whipspot.icns, whipspot.ico and whipspot-256.png"
