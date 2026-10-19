#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")/.."
PROFILE=${1:-debug}
APP_BUNDLE=${2:-dist/枕安.app}
APP="$APP_BUNDLE/Contents"
mkdir -p "$APP/MacOS" "$APP/Resources/bin" "$APP/Resources/models"
# Remove generated files from previous package names when reusing a staging bundle.
rm -f "$APP/MacOS/laoren" "$APP/MacOS/枕安" "$APP/MacOS/zhenan" "$APP/MacOS/pillowguard" "$APP/Resources/zhenan.icns"
# Keep the internal executable ASCII for macOS WebKit bundle initialization.
cp "${CARGO_TARGET_DIR:-target}/$PROFILE/pillowguard" "$APP/MacOS/pillowguard"
lipo bin/ffmpeg -thin "$(uname -m)" -output "$APP/Resources/bin/ffmpeg"
bin/ffmpeg -L > "$APP/Resources/FFmpeg-许可说明.txt" 2>&1
cp assets/PillowGuard.icns "$APP/Resources/PillowGuard.icns"
cp models/yolov8n-pose.onnx models/head-detector.onnx "$APP/Resources/models/"
cp assets/HeadDetector-LICENSE.txt "$APP/Resources/HeadDetector-LICENSE.txt"
cat > "$APP/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>CFBundleExecutable</key><string>pillowguard</string><key>CFBundleIdentifier</key><string>com.laoren.shouwang</string><key>CFBundleIconFile</key><string>PillowGuard.icns</string><key>CFBundleName</key><string>枕安</string><key>CFBundleDisplayName</key><string>枕安</string><key>CFBundlePackageType</key><string>APPL</string><key>CFBundleShortVersionString</key><string>0.3.10</string><key>CFBundleVersion</key><string>310</string><key>LSMinimumSystemVersion</key><string>12.0</string><key>NSHighResolutionCapable</key><true/><key>NSLocalNetworkUsageDescription</key><string>连接家庭局域网中的 Home Assistant、音箱和摄像头，用于本地监护与告警。</string><key>NSCameraUsageDescription</key><string>读取摄像头进行本地起身检测</string><key>NSAppTransportSecurity</key><dict><key>NSAllowsLocalNetworking</key><true/></dict></dict></plist>
PLIST
chmod +x "$APP/MacOS/pillowguard" "$APP/Resources/bin/ffmpeg"
codesign --force --sign - --identifier com.laoren.shouwang.ffmpeg "$APP/Resources/bin/ffmpeg"
codesign --force --sign - --identifier com.laoren.shouwang "$APP_BUNDLE"
codesign --verify --deep --strict "$APP_BUNDLE"
echo "已生成 $APP_BUNDLE（本机临时签名，未公证）"
