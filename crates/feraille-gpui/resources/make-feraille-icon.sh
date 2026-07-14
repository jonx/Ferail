#!/bin/sh
# make-feraille-icon.sh — cut the macOS app-icon assets from the source artwork.
#
# Input : feraille-macos-source.png  (the raw brushed-steel + rust artwork; may be a
#         full-bleed square OR an already-rounded tile on transparency — both work,
#         because we trim + re-square + re-round from scratch here).
# Output: (written next to this script, in resources/)
#   feraille-macos.png    macOS-spec 1024 master (transparent squircle); also the
#                         image bytes the Dock icon / About dialog load at runtime
#                         (see src/app_icon.rs).
#   feraille-macos.icns   multi-resolution icon for the .app bundle (CFBundleIconFile).
#
# macOS 11+ (Big Sur) app-icon grid — baked into the art because, unlike iOS, macOS does
# NOT auto-mask app icons. Get this wrong and the tile renders oversized and overflows the
# Dock selection box (which is exactly what the old hand-cut tile did — 887x846, off-centre):
#   canvas 1024x1024 · tile (rounded body) 824x824 (80.5%) · 100px margin each side ·
#   corner radius 185.4px (= 0.225 x 824), continuous/squircle corners (we approximate
#   with a circular arc — indistinguishable at Dock sizes).
#   Refs: Apple HIG "App icons"; Apple Developer Forums thread 670578.
#
# Two ImageMagick traps this script works around (both cost real time to rediscover):
#   - a Gray-colorspace mask silently collapses the DstIn result to grayscale and drops
#     the rust's colour -> force the mask to sRGB first.
#   - chaining -extent after -composite drops the pixels -> pad in a separate magick call.
#
# Windows/Linux keep their own art (feraille.ico / feraille.png); this script is macOS only.
#
# Requires: ImageMagick (magick), iconutil (macOS). Procedure adapted from
# aros-aarch64/hosted/cocoametal/make-macaron-icon.sh.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${1:-$HERE/feraille-macos-source.png}"
[ -f "$SRC" ] || { echo "source image not found: $SRC (pass one as \$1)" >&2; exit 1; }
command -v magick   >/dev/null 2>&1 || { echo "need ImageMagick (magick)" >&2; exit 1; }
command -v iconutil >/dev/null 2>&1 || { echo "need iconutil (macOS)" >&2; exit 1; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
T=824; SS=4; RSS=741.6            # tile side, supersample, radius*SS (185.4 * 4)
CROPL=9                          # source columns shaved off the LEFT edge (see step 2)

# 1. If the source has an opaque white background (a fresh full-bleed render), lift it to
#    transparency with a connected flood-fill from the four corners — this preserves any
#    near-white highlight a global "-transparent white" would eat. On our already-transparent
#    tile the corners aren't white, so this is a harmless no-op. Then trim the frame.
W="$(magick identify -format '%w' "$SRC")"; H="$(magick identify -format '%h' "$SRC")"
magick "$SRC" -alpha set -fill none -fuzz 9% \
  -floodfill +0+0 white -floodfill "+$((W-1))+0" white \
  -floodfill "+0+$((H-1))" white -floodfill "+$((W-1))+$((H-1))" white \
  -channel A -morphology Erode Disk:1 +channel \
  -trim +repage "$TMP/tile.png"

# 2. Shave CROPL columns off the LEFT of the source, then square to the 824 tile. The source
#    has a baked-in specular highlight down its LEFT edge (a ~4px near-white band) that reads
#    as a stray white column against the Dock; dropping the leftmost columns lands the tile
#    edge on the mid-tone metal instead. We crop in source space and keep the real right edge
#    (smooth metal — no highlight, and no replication smear). A 5% aspect nudge on a texture is
#    invisible and keeps the full composition (the rust corner still reaches the tile edge).
SW="$(magick identify -format '%w' "$TMP/tile.png")"; SH="$(magick identify -format '%h' "$TMP/tile.png")"
magick "$TMP/tile.png" -crop "$((SW-CROPL))x${SH}+${CROPL}+0" +repage \
  -resize "${T}x${T}!" "$TMP/art.png"

# 3. Anti-aliased circular rounded-rect mask at radius 185.4 (drawn 4x, downsampled).
magick -size "$((T*SS))x$((T*SS))" xc:none -fill white \
  -draw "roundRectangle 0,0 $((T*SS-1)),$((T*SS-1)) $RSS,$RSS" \
  -filter Lanczos -resize "${T}x${T}" "$TMP/mask.png"

# 4. Clip the art to the tile (mask forced to sRGB — see header note). Apple's squircle is
#    equal-or-rounder than the old hand-cut corner, so this only ever removes corner pixels;
#    it never has to invent any.
magick "$TMP/art.png" \( "$TMP/mask.png" -colorspace sRGB \) \
  -compose DstIn -composite "$TMP/tile824.png"

# 5. Centre the tile on a 1024 canvas with 100px transparent margins (separate call).
magick "$TMP/tile824.png" -background none -gravity center -extent 1024x1024 \
  "$HERE/feraille-macos.png"

# 6. .icns — every size derived from the 1024 master.
IS="$TMP/feraille.iconset"; mkdir -p "$IS"
for pair in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" "128 128x128" \
            "256 128x128@2x" "256 256x256" "512 256x256@2x" "512 512x512" "1024 512x512@2x"; do
  set -- $pair
  magick "$HERE/feraille-macos.png" -resize "${1}x${1}" -filter Lanczos "$IS/icon_${2}.png"
done
iconutil -c icns "$IS" -o "$HERE/feraille-macos.icns"

echo ">> wrote feraille-macos.png, feraille-macos.icns in $HERE"
echo ">> master: $(magick "$HERE/feraille-macos.png" -format '%@ on %wx%h' info:)  (expect 824x824+100+100 on 1024x1024)"
