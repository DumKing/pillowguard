#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
ICONSET=$(mktemp -d "${TMPDIR:-/private/tmp}/pillowguard-icons.XXXXXX")/PillowGuard.iconset
mkdir -p "$ICONSET"
trap 'rm -rf "$(dirname "$ICONSET")"' EXIT
for SIZE in 16 32 128 256 512; do
  sips -z "$SIZE" "$SIZE" assets/pillowguard-icon-v1.png --out "$ICONSET/icon_${SIZE}x${SIZE}.png" >/dev/null
  DOUBLE=$((SIZE * 2))
  sips -z "$DOUBLE" "$DOUBLE" assets/pillowguard-icon-v1.png --out "$ICONSET/icon_${SIZE}x${SIZE}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o assets/PillowGuard.icns
cp "$ICONSET/icon_128x128@2x.png" assets/pillowguard-icon-256.png
cp "$ICONSET/icon_32x32.png" assets/pillowguard-icon-32.png
cp "$ICONSET/icon_512x512@2x.png" assets/pillowguard-icon-1024.png

python3 - <<'ICONPY'
from pathlib import Path
import struct
png=Path('assets/pillowguard-icon-256.png').read_bytes()
Path('assets/PillowGuard.ico').write_bytes(struct.pack('<HHH',0,1,1)+struct.pack('<BBBBHHII',0,0,0,0,1,32,len(png),22)+png)
ICONPY
