#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
MODEL='models/yolov8n-pose.onnx'
EXPECTED='04f6d2416266f2aba6c5ba8b26de33ed9eba3279f972ac02e33f9f1366547586'
checksum() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d ' ' -f 1
  else LC_ALL=C LC_CTYPE=C LANG=C shasum -a 256 "$1" | cut -d ' ' -f 1; fi
}
if [ -f "$MODEL" ]; then
  test "$(checksum "$MODEL")" = "$EXPECTED" || { echo '已有模型校验不匹配，未覆盖，请检查来源。' >&2; exit 1; }
  echo '模型已存在，SHA-256 校验通过。'
  exit 0
fi
mkdir -p models
TEMP=$(mktemp models/.download.XXXXXX)
trap 'rm -f "$TEMP"' EXIT
curl -fL --retry 2 'https://huggingface.co/Xenova/yolov8-pose-onnx/resolve/main/yolov8n-pose.onnx' -o "$TEMP"
test "$(checksum "$TEMP")" = "$EXPECTED" || { echo '模型下载校验失败，未安装。' >&2; exit 1; }
mv "$TEMP" "$MODEL"
echo '模型下载完成，SHA-256 校验通过。'
