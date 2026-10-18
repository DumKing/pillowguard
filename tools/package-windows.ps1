param([ValidateSet('debug','release')][string]$Profile='release',[Parameter(Mandatory=$true)][string]$FfmpegPath)
$ErrorActionPreference='Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$TargetRoot=if($env:CARGO_TARGET_DIR){$env:CARGO_TARGET_DIR}else{'target'}
$Exe=Join-Path $TargetRoot "$Profile/laoren.exe"
if(!(Test-Path $Exe)){throw '先构建桌面程序：cargo build --release --features desktop --locked'}
if(!(Test-Path $FfmpegPath)){throw '请提供 Windows 版本 ffmpeg.exe'}
$FfmpegPath=(Resolve-Path $FfmpegPath).Path
$Destination='dist/PillowGuard-Windows'
New-Item -ItemType Directory -Force "$Destination/bin","$Destination/models" | Out-Null
Copy-Item $Exe "$Destination/PillowGuard.exe" -Force
Copy-Item $FfmpegPath "$Destination/bin/ffmpeg.exe" -Force
Copy-Item 'models/yolov8n-pose.onnx' "$Destination/models/yolov8n-pose.onnx" -Force
@'
@echo off
cd /d "%~dp0"
start "" PillowGuard.exe
'@ | Set-Content "$Destination/Start.cmd" -Encoding ascii
@'
枕安 Windows 便携版
解压整个文件夹，双击 PillowGuard.exe。不要单独移动 exe，保留 bin 与 models。
首次运行需允许摄像头访问。使用系统 WebView2；缺少该组件时需安装微软 WebView2 Runtime。
配置保存在 %LOCALAPPDATA%\Shouwang。默认仅本机可访问，不需要 Python。
Windows 本机摄像头默认使用首个 DirectShow 视频设备，也支持网络摄像头与视频回放。
这是本机开发打包脚本的产物；正式分发前需要在 Windows 实机验收与签名。
'@ | Set-Content "$Destination/使用说明.txt" -Encoding utf8
Compress-Archive -Path "$Destination/*" -DestinationPath 'dist/PillowGuard-Windows.zip' -Force
Write-Host '已生成 dist/PillowGuard-Windows.zip'
