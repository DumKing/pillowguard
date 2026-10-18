#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
bash tools/package-macos.sh "${1:-release}"
ARCH=$(uname -m)
ditto -c -k --sequesterRsrc --keepParent 'dist/枕安.app' "dist/枕安-Mac-$ARCH.zip"
echo "已生成 dist/枕安-Mac-$ARCH.zip；解压后双击枕安.app"
