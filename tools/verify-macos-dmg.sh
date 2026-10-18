#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
DMG=${1:?请指定 DMG 文件}
ATTACH=$(mktemp "${TMPDIR:-/private/tmp}/shouwang-mount.XXXXXX")
MOUNT=""
cleanup() {
  if [ -n "$MOUNT" ]; then hdiutil detach "$MOUNT" >/dev/null; fi
  rm -f "$ATTACH"
}
trap cleanup EXIT
hdiutil verify "$DMG"
hdiutil attach -readonly -nobrowse -plist "$DMG" > "$ATTACH"
MOUNT=$(python3 - "$ATTACH" <<'PY'
import plistlib,sys
with open(sys.argv[1],'rb') as f: p=plistlib.load(f)
print(next(x['mount-point'] for x in p['system-entities'] if 'mount-point' in x))
PY
)
APP="$MOUNT/枕安.app"
test "$(readlink "$MOUNT/Applications")" = /Applications
test -f "$MOUNT/安装说明.txt"
test -f "$APP/Contents/Resources/models/yolov8n-pose.onnx"
codesign --verify --deep --strict "$APP"
# Run from the image, so self-test must find bundled resources without checkout.
(cd "$MOUNT" && env -u LAOREN_HOME "$APP/Contents/MacOS/laoren" --self-test)
"$APP/Contents/Resources/bin/ffmpeg" -version > /dev/null
python3 - "$APP" <<'PY'
from pathlib import Path
import sys,plistlib
app=Path(sys.argv[1])
files={str(p.relative_to(app)) for p in app.rglob('*') if p.is_file()}
expected={'Contents/Resources/PillowGuard.icns','Contents/Info.plist','Contents/MacOS/laoren','Contents/Resources/bin/ffmpeg','Contents/Resources/models/yolov8n-pose.onnx','Contents/Resources/FFmpeg-许可说明.txt','Contents/_CodeSignature/CodeResources'}
with (app/'Contents/Info.plist').open('rb') as f: info=plistlib.load(f)
assert info['CFBundleIconFile']=='PillowGuard.icns'
assert (app/'Contents/Resources/PillowGuard.icns').read_bytes()[:4]==b'icns'
assert files==expected, f'Unexpected bundle files: {files ^ expected}'
print('PASS：安装镜像、应用签名、内置推理模型、视频组件和纯程序文件检查通过')
PY
