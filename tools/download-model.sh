#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
checksum() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d ' ' -f 1
  else LC_ALL=C LC_CTYPE=C LANG=C shasum -a 256 "$1" | cut -d ' ' -f 1; fi
}
verify_existing() {
  if [ ! -f "$1" ]; then return 1; fi
  if [ "$(checksum "$1")" != "$2" ]; then echo "已有模型校验失败：$1" >&2; exit 1; fi
  echo "模型校验通过：$1"
}
mkdir -p models
MODEL_TMP=''
ARCHIVE_TMP=''
trap 'rm -f "$MODEL_TMP" "$ARCHIVE_TMP"' EXIT
if ! verify_existing models/yolov8n-pose.onnx 04f6d2416266f2aba6c5ba8b26de33ed9eba3279f972ac02e33f9f1366547586; then
  MODEL_TMP=$(mktemp models/.download.XXXXXX)
  curl -fL --retry 2 'https://huggingface.co/Xenova/yolov8-pose-onnx/resolve/main/yolov8n-pose.onnx' -o "$MODEL_TMP"
  test "$(checksum "$MODEL_TMP")" = 04f6d2416266f2aba6c5ba8b26de33ed9eba3279f972ac02e33f9f1366547586
  mv "$MODEL_TMP" models/yolov8n-pose.onnx
fi
if ! verify_existing models/head-detector.onnx f995ac5315917f54654b4ca47f9e574be7bdb636775f289fb8b82c25be040233; then
  MODEL_TMP=$(mktemp models/.download.XXXXXX)
  ARCHIVE_TMP=$(mktemp models/.archive.XXXXXX)
  echo '首次读取作者的头部模型归档（约 253 MB），仅保存其中约 3.9 MB 的模型。'
  curl -fL --retry 2 'https://s3.ap-northeast-2.wasabisys.com/pinto-model-zoo/426_YOLOX-Body-Head-Hand/resources_n.tar.gz' -o "$ARCHIVE_TMP"
  tar -xzOf "$ARCHIVE_TMP" yolox_n_body_head_hand_post_0461_0.4428_1x3x256x320.onnx > "$MODEL_TMP"
  test "$(checksum "$MODEL_TMP")" = f995ac5315917f54654b4ca47f9e574be7bdb636775f289fb8b82c25be040233
  mv "$MODEL_TMP" models/head-detector.onnx
fi
