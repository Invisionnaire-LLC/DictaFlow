#!/usr/bin/env bash
# Build a full-bleed 1024×1024 app icon from a Figma export or Downloads PNG.
#
# Usage:
#   ./scripts/prepare-app-icon.sh ~/Downloads/dictaFlow\ app\ icon\ 3.png
#   ICON_SCALE=0.78 ICON_OFFSET_Y=90 ./scripts/prepare-app-icon.sh path/to/icon.png
#
# ICON_SCALE   — artwork size vs canvas (default 0.80)
# ICON_OFFSET_Y — pixels from bottom edge (default 20)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="${1:?usage: prepare-app-icon.sh <source-png>}"
OUT="$ROOT/src-tauri/icons/app-icon.png"
SCALE="${ICON_SCALE:-0.80}"
OFFSET_Y="${ICON_OFFSET_Y:-20}"

if ! command -v magick >/dev/null; then
  echo "ImageMagick (magick) is required."
  exit 1
fi

SIZE=$(python3 -c "print(int(1024 * float('$SCALE')))")
BG=$(magick "$SRC" -format "%[pixel:p{2,2}]" info:)

magick -size 1024x1024 "xc:$BG" \
  \( "$SRC" -fuzz 4% -trim +repage -filter Lanczos -resize "${SIZE}x${SIZE}" \) \
  -gravity south -geometry "+0+${OFFSET_Y}" -composite \
  -alpha off -type TrueColor \
  "$OUT"

echo "Wrote $OUT (scale=$SCALE, offset_y=${OFFSET_Y}px)"
echo "Note: menu bar tray icon (icons/tray.png) is unchanged — edit that file separately."
echo "Regenerate bundle icons: cd src-tauri && cargo tauri icon icons/app-icon.png -o icons"
