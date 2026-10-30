#!/usr/bin/env bash
# Builds Ferrufus-x86_64.AppImage from release binaries.
#
# Usage: packaging/build-appimage.sh [OUTPUT_DIR]
#
# Expects target/release/{ferrufus,ferrufus-cli} (cargo build --release).
# Downloads appimagetool and the AppImage runtime at pinned versions and
# verifies their SHA-256 before use; FUSE is not needed.
set -euo pipefail

APPIMAGETOOL_VERSION=1.9.1
APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
RUNTIME_VERSION=20251108
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$(realpath -m "${1:-$repo/dist}")"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fetch() { # url sha256 destination
    local url="$1" sum="$2" dest="$3"
    if [ -n "${FERRUFUS_TOOLS_DIR:-}" ] && [ -f "$FERRUFUS_TOOLS_DIR/$(basename "$dest")" ]; then
        cp "$FERRUFUS_TOOLS_DIR/$(basename "$dest")" "$dest"
    else
        curl --fail --silent --show-error --location --proto '=https' --tlsv1.2 -o "$dest" "$url"
    fi
    echo "$sum  $dest" | sha256sum --check --strict --quiet
}

fetch "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-x86_64.AppImage" \
    "$APPIMAGETOOL_SHA256" "$work/appimagetool-x86_64.AppImage"
fetch "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VERSION/runtime-x86_64" \
    "$RUNTIME_SHA256" "$work/runtime-x86_64"
chmod +x "$work/appimagetool-x86_64.AppImage"

appdir="$work/Ferrufus.AppDir"
install -Dm755 "$repo/target/release/ferrufus" "$appdir/usr/bin/ferrufus"
install -Dm755 "$repo/target/release/ferrufus-cli" "$appdir/usr/bin/ferrufus-cli"
install -Dm755 "$repo/packaging/appimage/AppRun" "$appdir/AppRun"
install -Dm644 "$repo/packaging/appimage/ferrufus.desktop" "$appdir/ferrufus.desktop"
install -Dm644 "$repo/packaging/appimage/ferrufus.desktop" "$appdir/usr/share/applications/ferrufus.desktop"
install -Dm644 "$repo/packaging/appimage/ferrufus.png" "$appdir/ferrufus.png"
install -Dm644 "$repo/packaging/appimage/ferrufus.png" "$appdir/usr/share/icons/hicolor/256x256/apps/ferrufus.png"
install -Dm644 "$repo/packaging/appimage/ferrufus.svg" "$appdir/usr/share/icons/hicolor/scalable/apps/ferrufus.svg"
ln -s ferrufus.png "$appdir/.DirIcon"
install -Dm644 "$repo/LICENSE" "$appdir/usr/share/doc/ferrufus/LICENSE"
install -Dm644 "$repo/NOTICE.md" "$appdir/usr/share/doc/ferrufus/NOTICE.md"

mkdir -p "$out"
ARCH=x86_64 "$work/appimagetool-x86_64.AppImage" --appimage-extract-and-run \
    --no-appstream --runtime-file "$work/runtime-x86_64" \
    "$appdir" "$out/Ferrufus-x86_64.AppImage"
chmod +x "$out/Ferrufus-x86_64.AppImage"
echo "Built $out/Ferrufus-x86_64.AppImage"
