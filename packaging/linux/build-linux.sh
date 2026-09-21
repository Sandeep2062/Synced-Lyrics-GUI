#!/usr/bin/env bash
set -euo pipefail

# Synced Lyrics GUI - Linux Package Builder (.deb, .AppImage, and portable .tar.gz)
VERSION="${1:-1.0.0}"
TARGET_DIR="${2:-target/release}"
OUTPUT_DIR="${3:-dist}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

echo "Building Linux packages for Synced Lyrics v${VERSION}..."
mkdir -p "${OUTPUT_DIR}"

BUILD_DIR="build/linux"
rm -rf "${BUILD_DIR}"
mkdir -p "${BUILD_DIR}"

BINARY_PATH="${TARGET_DIR}/lyrics-desktop"
if [[ ! -f "${BINARY_PATH}" ]]; then
    echo "Error: Binary not found at ${BINARY_PATH}" >&2
    exit 1
fi

# ---------------------------------------------------------------------------
# 1. Portable .tar.gz
# ---------------------------------------------------------------------------
echo "Packaging portable tar.gz..."
PORTABLE_STAGE="${BUILD_DIR}/synced-lyrics-${VERSION}-linux-x86_64"
mkdir -p "${PORTABLE_STAGE}"
cp "${BINARY_PATH}" "${PORTABLE_STAGE}/synced-lyrics"
chmod +x "${PORTABLE_STAGE}/synced-lyrics"
cp packaging/linux/synced-lyrics.desktop "${PORTABLE_STAGE}/"
cp assets/icon.png "${PORTABLE_STAGE}/synced-lyrics.png"
cp LICENSE "${PORTABLE_STAGE}/"
cp README.md "${PORTABLE_STAGE}/"

# Add portable runner script
cat > "${PORTABLE_STAGE}/run.sh" <<'EOF'
#!/usr/bin/env bash
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export SYNCED_LYRICS_PORTABLE=1
exec "${HERE}/synced-lyrics" "$@"
EOF
chmod +x "${PORTABLE_STAGE}/run.sh"

tar -czf "${OUTPUT_DIR}/Synced-Lyrics-linux-x86_64-portable.tar.gz" -C "${BUILD_DIR}" "synced-lyrics-${VERSION}-linux-x86_64"

# ---------------------------------------------------------------------------
# 2. Debian / Ubuntu .deb package
# ---------------------------------------------------------------------------
echo "Packaging Debian package (.deb)..."
DEB_STAGE="${BUILD_DIR}/synced-lyrics_${VERSION}_amd64"
mkdir -p "${DEB_STAGE}/DEBIAN"
mkdir -p "${DEB_STAGE}/usr/bin"
mkdir -p "${DEB_STAGE}/usr/share/applications"
mkdir -p "${DEB_STAGE}/usr/share/icons/hicolor/512x512/apps"
mkdir -p "${DEB_STAGE}/usr/share/doc/synced-lyrics"

cp "${BINARY_PATH}" "${DEB_STAGE}/usr/bin/synced-lyrics"
chmod +x "${DEB_STAGE}/usr/bin/synced-lyrics"

cp packaging/linux/synced-lyrics.desktop "${DEB_STAGE}/usr/share/applications/"
cp assets/icon.png "${DEB_STAGE}/usr/share/icons/hicolor/512x512/apps/synced-lyrics.png"
cp LICENSE "${DEB_STAGE}/usr/share/doc/synced-lyrics/copyright"
cp README.md "${DEB_STAGE}/usr/share/doc/synced-lyrics/"

cat > "${DEB_STAGE}/DEBIAN/control" <<EOF
Package: synced-lyrics
Version: ${VERSION}
Section: sound
Priority: optional
Architecture: amd64
Maintainer: Synced Lyrics GUI contributors <https://github.com/Sandeep2062/Synced-Lyrics-GUI>
Depends: libc6, libasound2
Description: Blazing-fast desktop application for synchronized LRC lyrics
 A lightweight, high-performance native Slint/Rust desktop application for
 managing, searching, and downloading synchronized and plain-text lyrics for
 local music libraries.
EOF

dpkg-deb --build --root-owner-group "${DEB_STAGE}" "${OUTPUT_DIR}/Synced-Lyrics-linux-amd64.deb"

# ---------------------------------------------------------------------------
# 3. Universal .AppImage
# ---------------------------------------------------------------------------
echo "Packaging AppImage..."
APP_DIR="${BUILD_DIR}/AppDir"
mkdir -p "${APP_DIR}/usr/bin"
mkdir -p "${APP_DIR}/usr/share/applications"
mkdir -p "${APP_DIR}/usr/share/icons/hicolor/512x512/apps"

cp "${BINARY_PATH}" "${APP_DIR}/usr/bin/lyrics-desktop"
chmod +x "${APP_DIR}/usr/bin/lyrics-desktop"
cp packaging/linux/synced-lyrics.desktop "${APP_DIR}/synced-lyrics.desktop"
cp packaging/linux/synced-lyrics.desktop "${APP_DIR}/usr/share/applications/synced-lyrics.desktop"
cp assets/icon.png "${APP_DIR}/synced-lyrics.png"
cp assets/icon.png "${APP_DIR}/usr/share/icons/hicolor/512x512/apps/synced-lyrics.png"

# AppRun launcher script
cat > "${APP_DIR}/AppRun" <<'EOF'
#!/usr/bin/env bash
HERE="$(dirname "$(readlink -f "${0}")")"
export PATH="${HERE}/usr/bin:${PATH}"
export LD_LIBRARY_PATH="${HERE}/usr/lib:${LD_LIBRARY_PATH:-}"
exec "${HERE}/usr/bin/lyrics-desktop" "$@"
EOF
chmod +x "${APP_DIR}/AppRun"

# Download appimagetool if not available
if ! command -v appimagetool &>/dev/null; then
    echo "Downloading appimagetool..."
    curl -fsSL -o "${BUILD_DIR}/appimagetool" "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage" || true
    if [[ -f "${BUILD_DIR}/appimagetool" ]]; then
        chmod +x "${BUILD_DIR}/appimagetool"
    fi
fi

if command -v appimagetool &>/dev/null; then
    ARCH=x86_64 appimagetool "${APP_DIR}" "${OUTPUT_DIR}/Synced-Lyrics-linux-x86_64.AppImage"
elif [[ -x "${BUILD_DIR}/appimagetool" ]]; then
    ARCH=x86_64 "${BUILD_DIR}/appimagetool" --appimage-extract-and-run "${APP_DIR}" "${OUTPUT_DIR}/Synced-Lyrics-linux-x86_64.AppImage" || true
fi

echo "Linux packaging complete in ${OUTPUT_DIR}:"
ls -la "${OUTPUT_DIR}"
