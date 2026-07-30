#!/bin/sh
# make-ferail-icon.sh — cut the macOS app-icon assets from the source artwork.
#
# Input : ferail-macos-source.png  (the raw brushed-steel + rust artwork; may be a
#         full-bleed square OR an already-rounded tile on transparency — both work,
#         because we trim + re-square + re-round from scratch here).
# Output: (written next to this script, in resources/)
#   ferail-macos.png    macOS-spec 1024 master (transparent squircle); also the
#                         image bytes the Dock icon / About dialog load at runtime
#                         (see src/app_icon.rs).
#   ferail-macos.icns   multi-resolution icon for the .app bundle (CFBundleIconFile).
#
# macOS 11+ (Big Sur) app-icon grid — baked into the art because, unlike iOS, macOS does
# NOT auto-mask app icons. Get this wrong and the tile renders oversized and overflows the
# Dock selection box (which is exactly what the old hand-cut tile did — 887x846, off-centre):
#   canvas 1024x1024 · tile (rounded body) 824x824 (80.5%) · 100px margin each side ·
#   CONTINUOUS (squircle) corners at RAD=278 (Apple's nominal radius is 185.4 = 0.225 x 824;
#   we cut ~50% rounder on purpose — picked by eye, the spec value read too square here).
#   Unlike the macaron generator this was adapted from, we do NOT approximate the corner with a
#   circular arc: a circular arc is tangent-continuous but has a curvature *jump* where it joins
#   the straight edge, which reads as a subtle kink at Dock sizes. Instead we cut Apple's
#   continuous corner via figma corner-smoothing (s=0.6): the curvature eases into the edges so
#   the round blends seamlessly into the sides. The straight edges break at the same 185.4px
#   point as the circular version, so the tile footprint (and thus the margins) is unchanged.
#   Refs: Apple HIG "App icons"; Apple Developer Forums thread 670578; figma corner smoothing.
#
# Two ImageMagick traps this script works around (both cost real time to rediscover):
#   - a Gray-colorspace mask silently collapses the DstIn result to grayscale and drops
#     the rust's colour -> force the mask to sRGB first.
#   - chaining -extent after -composite drops the pixels -> pad in a separate magick call.
#
# Windows/Linux keep their own art (ferail.ico / ferail.png); this script is macOS only.
#
# Requires: ImageMagick (magick), iconutil (macOS). Procedure adapted from
# aros-aarch64/hosted/cocoametal/make-macaron-icon.sh.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="${1:-$HERE/ferail-macos-source.png}"
[ -f "$SRC" ] || { echo "source image not found: $SRC (pass one as \$1)" >&2; exit 1; }
command -v magick   >/dev/null 2>&1 || { echo "need ImageMagick (magick)" >&2; exit 1; }
command -v iconutil >/dev/null 2>&1 || { echo "need iconutil (macOS)" >&2; exit 1; }

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
T=824; SS=4; TS=$((T*SS))        # tile side, supersample factor, supersampled canvas
RAD=278                          # corner radius / edge-break point. Apple's nominal spec is
                                 # 185.4 (0.225 x 824), but that read too square on this art;
                                 # +50% was picked by eye from a 185/213/241/278 comparison.
SMOOTH=0.6                       # figma corner-smoothing (Apple's continuous-corner value)
G=40                             # overscan margin cropped off each side (see step 2)

# figma corner-smoothing: emit ONE quadrant of the squircle (its top-right corner), as an SVG
# path in supersampled space. Only this single corner is hand-written — the full mask is then
# assembled by flip/flop mirroring, so all four corners are identical BY CONSTRUCTION (an
# earlier version hand-mirrored the path for each corner and got each one subtly wrong).
# Geometry is the figma-squircle top-right corner verbatim; s=SMOOTH, and the base radius is
# RAD/(1+s) so the straight edges break at exactly RAD (same footprint as a circular cut).
squircle_quadrant_path() {
  awk -v W="$1" -v s="$SMOOTH" -v RAD="$2" '
    function rad(x){return x*PI/180}
    function tn(x){return sin(x)/cos(x)}
    BEGIN{
      PI=atan2(0,-1); r=RAD/(1+s);
      p=(1+s)*r; arc=90*(1-s); al=sin(rad(arc/2))*r*sqrt(2);
      alpha=(90-arc)/2; beta=45*s; p3=r*tn(rad(beta/2));
      c=p3*cos(rad(alpha)); d=c*tn(rad(alpha)); b=(p-al-c-d)/3; a=2*b;
      printf "M 0 0 L %.4f 0 ", W-p;
      printf "c %.4f 0 %.4f 0 %.4f %.4f ", a, a+b, a+b+c, d;
      printf "a %.4f %.4f 0 0 1 %.4f %.4f ", r, r, al, al;
      printf "c %.4f %.4f %.4f %.4f %.4f %.4f ", d, c, d, b+c, d, a+b+c;
      printf "L %.4f %.4f L 0 %.4f Z", W, W, W;
    }'
}

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

# 2. Square the art with a G-px OVERSCAN: resize to (T+2G) then centre-crop back to T. Two
#    problems solved at once, both caused by the source being an already-cut tile rather than
#    full-bleed art:
#    - its own old rounded corners (hand-cut, slightly asymmetric) would otherwise sit inside
#      the new mask and bleed through DstIn, making the four cut corners differ;
#    - it has a baked-in specular highlight down its LEFT edge (~4px near-white band) that
#      read as a stray white column against the Dock.
#    Overscanning pushes the old cut edge fully OUTSIDE the new mask (G=40 verified: DstIn
#    alpha is then pixel-identical to the mask alpha), so every visible pixel is genuine
#    texture and the mask alone defines the silhouette. A ~5% texture zoom is invisible and
#    the rust corner still reaches the tile edge.
magick "$TMP/tile.png" -resize "$((T+2*G))x$((T+2*G))!" \
  -gravity center -crop "${T}x${T}+0+0" +repage "$TMP/art.png"

# 3. Anti-aliased continuous-corner (squircle) mask: draw one quadrant at SSx supersample,
#    mirror it into the four corners, then downsample. Interior opaque white, exterior
#    transparent, so the shape lives in the alpha channel that step 4's DstIn samples.
Q=$((TS/2))                       # quadrant side in supersampled space
magick -size "${Q}x${Q}" xc:none -fill white \
  -draw "path '$(squircle_quadrant_path "$Q" "$(awk -v r="$RAD" -v ss="$SS" 'BEGIN{printf "%.4f", r*ss}')")'" \
  "$TMP/quad.png"
magick \( "$TMP/quad.png" -flop \) "$TMP/quad.png" +append "$TMP/tophalf.png"
magick "$TMP/tophalf.png" \( "$TMP/tophalf.png" -flip \) -append \
  -filter Lanczos -resize "${T}x${T}" "$TMP/mask.png"

# 4. Clip the art to the tile (mask forced to sRGB — see header note). Apple's squircle is
#    equal-or-rounder than the old hand-cut corner, so this only ever removes corner pixels;
#    it never has to invent any.
magick "$TMP/art.png" \( "$TMP/mask.png" -colorspace sRGB \) \
  -compose DstIn -composite "$TMP/tile824.png"

# 5. Centre the tile on a 1024 canvas with 100px transparent margins (separate call).
magick "$TMP/tile824.png" -background none -gravity center -extent 1024x1024 \
  "$HERE/ferail-macos.png"

# 6. .icns — every size derived from the 1024 master.
IS="$TMP/ferail.iconset"; mkdir -p "$IS"
for pair in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" "128 128x128" \
            "256 128x128@2x" "256 256x256" "512 256x256@2x" "512 512x512" "1024 512x512@2x"; do
  set -- $pair
  magick "$HERE/ferail-macos.png" -resize "${1}x${1}" -filter Lanczos "$IS/icon_${2}.png"
done
iconutil -c icns "$IS" -o "$HERE/ferail-macos.icns"

echo ">> wrote ferail-macos.png, ferail-macos.icns in $HERE"
echo ">> master: $(magick "$HERE/ferail-macos.png" -format '%@ on %wx%h' info:)  (expect 824x824+100+100 on 1024x1024)"

# Self-check: the four cut corners of the SHIPPED master must be identical (mirror-compare
# its alpha — checking the mask alone is not enough, since the art's own alpha could still
# bleed through DstIn if the overscan ever stops clearing the old cut).
magick "$HERE/ferail-macos.png" -alpha extract "$TMP/MA.png"
magick "$TMP/MA.png" -crop "260x260+100+100" +repage "$TMP/m_tl.png"
magick "$TMP/MA.png" -crop "260x260+$((1024-100-260))+100" +repage -flop "$TMP/m_tr.png"
magick "$TMP/MA.png" -crop "260x260+100+$((1024-100-260))" +repage -flip "$TMP/m_bl.png"
magick "$TMP/MA.png" -crop "260x260+$((1024-100-260))+$((1024-100-260))" +repage -flip -flop "$TMP/m_br.png"
FAIL=0
for c in m_tr m_bl m_br; do
  D="$(magick "$TMP/m_tl.png" "$TMP/$c.png" -compose difference -composite -threshold 10% -format '%[fx:round(mean*w*h)]' info:)"
  echo ">> corner check $c vs m_tl: ${D} differing px (expect 0)"
  [ "$D" = "0" ] || FAIL=1
done
[ "$FAIL" = "0" ] || { echo ">> CORNER CHECK FAILED — the four corners are not identical" >&2; exit 1; }
