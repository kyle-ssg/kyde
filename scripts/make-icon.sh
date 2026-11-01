#!/usr/bin/env bash
# Generate the macOS app icon from a square source, shaped to Apple's icon grid:
# the art sits in an 824×824 rounded "squircle" centered on a 1024 canvas with
# transparent margins (corner radius 185px). Big Sur+ does NOT auto-round app
# icons — a full-bleed square renders as a hard square in the Dock, unlike native
# apps. This bakes the rounded shape + margin so it sits right next to them.
#
#   ./scripts/make-icon.sh [source.png]   # default source: assets/logo-square.png
#
# Outputs (overwrites):
#   assets/logo.png      — 1024 rounded master (runtime Dock icon + window icon + README)
#   assets/logo-dev.png  — the same, upside down: the runtime icon of a local build
#   assets/AppIcon.icns  — bundled .app Dock/Finder icon (all sizes)
#
# Windows (assets/AppIcon.ico) is intentionally left full-bleed — Windows doesn't
# round icons, so a margin there would just shrink it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

SRC="${1:-assets/logo-square.png}"
[[ -f "$SRC" ]] || { echo "source not found: $SRC" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Apple macOS icon grid (1024 canvas): 824 body, 100 margin each side, r=185.
BODY=824
RADIUS=185

# 1. Scale art to the body size, 2. clip it to a rounded rect (DstIn keeps the art
#    only where the rrect is opaque), 3. center on a transparent 1024 canvas.
magick "$SRC" -resize ${BODY}x${BODY}! "$TMP/art.png"
magick "$TMP/art.png" \
  \( -size ${BODY}x${BODY} xc:none \
     -draw "roundrectangle 0,0,$((BODY-1)),$((BODY-1)),$RADIUS,$RADIUS" \) \
  -alpha set -compose DstIn -composite "$TMP/round.png"
magick -size 1024x1024 xc:none "$TMP/round.png" -gravity center -compose over -composite "$TMP/icon1024.png"

cp "$TMP/icon1024.png" assets/logo.png
magick "$TMP/icon1024.png" -rotate 180 assets/logo-dev.png

# Build the iconset (every size LaunchServices wants) and pack it into .icns.
ISET="$TMP/Kyde.iconset"
mkdir -p "$ISET"
gen() { sips -z "$1" "$1" "$TMP/icon1024.png" --out "$ISET/$2" >/dev/null; }
gen 16   icon_16x16.png
gen 32   icon_16x16@2x.png
gen 32   icon_32x32.png
gen 64   icon_32x32@2x.png
gen 128  icon_128x128.png
gen 256  icon_128x128@2x.png
gen 256  icon_256x256.png
gen 512  icon_256x256@2x.png
gen 512  icon_512x512.png
cp "$TMP/icon1024.png" "$ISET/icon_512x512@2x.png"
iconutil -c icns "$ISET" -o assets/AppIcon.icns

echo "wrote assets/logo.png (1024 rounded) + assets/logo-dev.png + assets/AppIcon.icns"
