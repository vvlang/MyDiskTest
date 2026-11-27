#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
APP_NAME="MyDiskTest"
DIST_DIR="${ROOT_DIR}/dist"
STAGE_DIR="/tmp/mydisktest_macos_stage_$$"
STAGE_APP="${STAGE_DIR}/${APP_NAME}.app"
ZIP_PATH="${DIST_DIR}/${APP_NAME}-macOS.zip"

trap 'rm -rf "${STAGE_DIR}"' EXIT

cd "${ROOT_DIR}"
echo "==> Building release binary for macOS..."
cargo build --release --offline 2>/dev/null || cargo build --release

echo "==> Staging ${APP_NAME}.app on local APFS (${STAGE_DIR})..."
rm -rf "${STAGE_DIR}"
mkdir -p "${STAGE_APP}/Contents/MacOS"
mkdir -p "${STAGE_APP}/Contents/Resources"

cp "${ROOT_DIR}/target/release/mydisktest" "${STAGE_APP}/Contents/MacOS/${APP_NAME}"
chmod 755 "${STAGE_APP}/Contents/MacOS/${APP_NAME}"
if [[ -f "${ROOT_DIR}/assets/AppIcon.icns" ]]; then
    cp "${ROOT_DIR}/assets/AppIcon.icns" "${STAGE_APP}/Contents/Resources/AppIcon.icns"
fi

cat > "${STAGE_APP}/Contents/Info.plist" << 'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>zh_CN</string>
    <key>CFBundleExecutable</key>
    <string>MyDiskTest</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIdentifier</key>
    <string>com.mydisktest.next</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>MyDiskTest</string>
    <key>CFBundleDisplayName</key>
    <string>MyDiskTest Next</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>1.4.0</string>
    <key>CFBundleVersion</key>
    <string>9</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSRemovableVolumesUsageDescription</key>
    <string>MyDiskTest 需要访问可移动存储设备以进行扩容检测、坏块扫描和读写速度基准测试。</string>
</dict>
</plist>
EOF

echo "==> Signing ${STAGE_APP} (ad-hoc bundle signature)..."
xattr -cr "${STAGE_APP}" 2>/dev/null || true
codesign --force --deep --sign - "${STAGE_APP}"
chmod 755 "${STAGE_APP}/Contents/MacOS/${APP_NAME}"
codesign --verify --deep --strict "${STAGE_APP}"

mkdir -p "${DIST_DIR}"

echo "==> Creating signed ZIP archive (${ZIP_PATH})..."
rm -f "${ZIP_PATH}"
ditto -c -k --sequesterRsrc --keepParent "${STAGE_APP}" "${ZIP_PATH}"

echo "==> Copying signed bundle to ${DIST_DIR}/${APP_NAME}.app..."
rm -rf "${DIST_DIR}/${APP_NAME}.app"
cp -R "${STAGE_APP}" "${DIST_DIR}/${APP_NAME}.app"
chmod 755 "${DIST_DIR}/${APP_NAME}.app/Contents/MacOS/${APP_NAME}" 2>/dev/null || true

echo "==> Done!"
echo "  ZIP: ${ZIP_PATH}"
echo "  APP: ${DIST_DIR}/${APP_NAME}.app"
