#!/usr/bin/env bash
set -euo pipefail

# Synced Lyrics GUI - macOS Bundle and DMG Builder
VERSION="${1:-1.0.0}"
TARGET_DIR="${2:-target/release}"
OUTPUT_DIR="${3:-dist}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

echo "Building macOS application bundle for Synced Lyrics v${VERSION}..."

APP_NAME="Synced Lyrics"
BUNDLE_DIR="build/macos/${APP_NAME}.app"
CONTENTS_DIR="${BUNDLE_DIR}/Contents"
MACOS_DIR="${CONTENTS_DIR}/MacOS"
RESOURCES_DIR="${CONTENTS_DIR}/Resources"

rm -rf "build/macos"
mkdir -p "${MACOS_DIR}" "${RESOURCES_DIR}" "${OUTPUT_DIR}"

# 1. Copy Binary
cp "${TARGET_DIR}/lyrics-desktop" "${MACOS_DIR}/lyrics-desktop"
chmod +x "${MACOS_DIR}/lyrics-desktop"

# 2. Generate icon.icns using built-in sips and iconutil
if [[ -f "assets/icon.png" ]]; then
    echo "Generating AppIcon.icns from assets/icon.png..."
    ICONSET="build/macos/AppIcon.iconset"
    mkdir -p "${ICONSET}"
    sips -z 16 16     assets/icon.png --out "${ICONSET}/icon_16x16.png" > /dev/null
    sips -z 32 32     assets/icon.png --out "${ICONSET}/icon_16x16@2x.png" > /dev/null
    sips -z 32 32     assets/icon.png --out "${ICONSET}/icon_32x32.png" > /dev/null
    sips -z 64 64     assets/icon.png --out "${ICONSET}/icon_32x32@2x.png" > /dev/null
    sips -z 128 128   assets/icon.png --out "${ICONSET}/icon_128x128.png" > /dev/null
    sips -z 256 256   assets/icon.png --out "${ICONSET}/icon_128x128@2x.png" > /dev/null
    sips -z 256 256   assets/icon.png --out "${ICONSET}/icon_256x256.png" > /dev/null
    sips -z 512 512   assets/icon.png --out "${ICONSET}/icon_256x256@2x.png" > /dev/null
    sips -z 512 512   assets/icon.png --out "${ICONSET}/icon_512x512.png" > /dev/null
    sips -z 1024 1024 assets/icon.png --out "${ICONSET}/icon_512x512@2x.png" > /dev/null
    iconutil -c icns "${ICONSET}" -o "${RESOURCES_DIR}/AppIcon.icns"
    rm -rf "${ICONSET}"
fi

# 3. Configure Info.plist with version
sed -e "s/1.0.0/${VERSION}/g" packaging/macos/Info.plist > "${CONTENTS_DIR}/Info.plist"

# 4. Create Portable tar.gz
echo "Creating portable archive..."
tar -czf "${OUTPUT_DIR}/Synced-Lyrics-macos-portable.tar.gz" -C "build/macos" "${APP_NAME}.app"

# 5. Create DMG Installer
echo "Creating drag-and-drop DMG installer..."
DMG_STAGE="build/macos/dmg_stage"
mkdir -p "${DMG_STAGE}"
cp -R "${BUNDLE_DIR}" "${DMG_STAGE}/"
ln -s /Applications "${DMG_STAGE}/Applications"

hdiutil create \
    -volname "Synced Lyrics" \
    -srcfolder "${DMG_STAGE}" \
    -ov \
    -format UDZO \
    "${OUTPUT_DIR}/Synced-Lyrics-macos.dmg"

echo "Successfully built macOS packages in ${OUTPUT_DIR}:"
echo "  - ${OUTPUT_DIR}/Synced-Lyrics-macos.dmg (Installer)"
echo "  - ${OUTPUT_DIR}/Synced-Lyrics-macos-portable.tar.gz (Portable)"
