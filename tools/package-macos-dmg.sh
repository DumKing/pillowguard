#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
PROFILE=${1:-release}
ARCH=$(uname -m)
VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
STAGE=$(mktemp -d "${TMPDIR:-/private/tmp}/pillowguard-installer.XXXXXX")
trap 'rm -rf "$STAGE"' EXIT
bash tools/package-macos.sh "$PROFILE" "$STAGE/枕安.app"
ln -s /Applications "$STAGE/Applications"
cat > "$STAGE/安装说明.txt" <<'TEXT'
枕安 · 老人起身监护

安装：将「枕安.app」拖到「Applications」文件夹，然后从应用程序启动。
更新：先退出旧版枕安，再替换应用。配置与记录保存在用户目录，不在应用包内。

这是 Intel Mac 版本，要求 macOS 12 或更高版本；尚未进行 Apple 开发者签名和公证。
内置 Rust 程序、识别模型和 FFmpeg，无需安装 Python、Rust 或 Home Assistant 服务端。
如使用小爱音箱播报，需要连接你已部署的 Home Assistant。

首次使用摄像头和局域网设备时，请允许摄像头及本地网络访问。
通知设置、密钥和历史记录保存在 ~/Library/Application Support/枕安，卸载应用不会自动删除这些数据。
安装包不包含开发者的摄像头地址、令牌、推送密钥或监测记录。

画面可配置 3～12 顶点的床位多边形。当前仅启用坐起提醒，床沿与地面区域只保存备用。
TEXT
OUT="dist/枕安-$VERSION-Mac-$ARCH.dmg"
mkdir -p dist
hdiutil create -volname "枕安 $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$OUT"
hdiutil verify "$OUT"
(cd dist && LC_ALL=C LC_CTYPE=C LANG=C shasum -a 256 "$(basename "$OUT")" > "$(basename "$OUT").sha256")
echo "已生成 $OUT"
