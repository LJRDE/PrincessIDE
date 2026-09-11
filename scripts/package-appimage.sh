#!/usr/bin/env bash
# PrincessIDE P8 — AppImage package builder.
#
# Builds an AppImage for the PrincessIDE desktop application.
#
# Strategy:
#   1. Build the Tauri desktop app in release mode.
#   2. Package the binary + runtime dependencies into an AppDir.
#   3. Use appimagetool to create the final .AppImage.
#
# Requirements:
#   - The full toolchain must be bootstrapped (bash scripts/bootstrap-toolchain.sh).
#   - Node.js + pnpm must be available for the frontend build.
#   - FUSE must be available for appimagetool (or use APPIMAGE_EXTRACT_AND_RUN=1).
#
# Usage:
#   bash scripts/package-appimage.sh [--no-build]
#
# Options:
#   --no-build    Skip the Tauri build; use existing release artifacts.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=env.sh
source "$SCRIPT_DIR/env.sh"

DO_BUILD=1
OUTDIR="$ROOT/dist"
TAURI_DIR="$ROOT/apps/desktop/src-tauri"
APPIMAGE_TOOL_URL="https://github.com/AppImage/AppImageKit/releases/download/continuous/appimagetool-x86_64.AppImage"

while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) DO_BUILD=0; shift ;;
        -h|--help)  sed -n '2,22p' "$0"; exit 0 ;;
        *)          echo "package-appimage: unknown argument: $1" >&2; exit 2 ;;
    esac
done

log()  { printf '[package-appimage] %s\n' "$*"; }
die()  { printf '[package-appimage] ERROR: %s\n' "$*" >&2; exit 1; }

# ------------------------------------------------------------- preflight -----
[ -d "$TAURI_DIR" ] || die "Tauri directory not found: $TAURI_DIR"

# ------------------------------------------------------------ build ----------
if [ "$DO_BUILD" = "1" ]; then
    log "building Tauri desktop app (release mode)"

    # Build frontend
    cd "$ROOT/apps/desktop"
    if [ -f "package.json" ]; then
        log "building frontend"
        pnpm install --frozen-lockfile 2>/dev/null || pnpm install
        pnpm build
    else
        log "no package.json found, creating minimal frontend dist"
        mkdir -p dist
        echo '<html><body><h1>PrincessIDE</h1></body></html>' > dist/index.html
    fi

    # Build Tauri (release)
    cd "$TAURI_DIR"
    CARGO_BUILD_JOBS=2 cargo build --release 2>&1 | tail -20

    log "Tauri build complete"
fi

# ------------------------------------------------------ locate the binary ----
BINARY=""
for candidate in \
    "$TAURI_DIR/target/release/princesside-desktop" \
    "$TAURI_DIR/target/release/princesside"; do
    if [ -f "$candidate" ]; then
        BINARY="$candidate"
        break
    fi
done
[ -n "$BINARY" ] || die "release binary not found; run without --no-build first"
log "binary: $BINARY"

# -------------------------------------------------------- appimagetool ------
TOOL_DIR="$ROOT/.scratch/appimage"
mkdir -p "$TOOL_DIR"
APPIMAGE_TOOL="$TOOL_DIR/appimagetool"

if [ ! -x "$APPIMAGE_TOOL" ]; then
    log "downloading appimagetool"
    curl -fsSL -o "$APPIMAGE_TOOL" "$APPIMAGE_TOOL_URL" \
        || die "could not download appimagetool"
    chmod +x "$APPIMAGE_TOOL"
fi

# ---------------------------------------------------------- AppDir ----------
APPDIR="$TOOL_DIR/AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin"
mkdir -p "$APPDIR/usr/lib"
mkdir -p "$APPDIR/usr/share/applications"
mkdir -p "$APPDIR/usr/share/icons/hicolor/128x128/apps"

# Copy binary
cp "$BINARY" "$APPDIR/usr/bin/princesside"
chmod 755 "$APPDIR/usr/bin/princesside"

# Copy icon
if [ -f "$ROOT/apps/desktop/src-tauri/icons/128x128.png" ]; then
    cp "$ROOT/apps/desktop/src-tauri/icons/128x128.png" \
       "$APPDIR/usr/share/icons/hicolor/128x128/apps/princesside.png"
    cp "$ROOT/apps/desktop/src-tauri/icons/128x128.png" \
       "$APPDIR/princesside.png"
fi

# Desktop file
cat >"$APPDIR/usr/share/applications/princesside.desktop" <<EOF
[Desktop Entry]
Name=PrincessIDE
Comment=IDE for x86_64 kernel development
Exec=princesside
Icon=princesside
Type=Application
Categories=Development;IDE;
Terminal=false
EOF

cp "$APPDIR/usr/share/applications/princesside.desktop" "$APPDIR/princesside.desktop"

# AppRun (symlink to binary)
ln -sf usr/bin/princesside "$APPDIR/AppRun"

# Copy shared libraries that Tauri needs
# This is a simplified approach; for production, use linuxdeploy
log "collecting shared libraries"
LIBS=(
    "libwebkit2gtk-4.1.so"
    "libjavascriptcoregtk-4.1.so"
    "libgtk-3.so"
    "libgdk-3.so"
    "libglib-2.0.so"
    "libgobject-2.0.so"
    "libsoup-3.0.so"
    "libgio-2.0.so"
    "libcairo.so"
    "libpango-1.0.so"
    "libgdk_pixbuf-2.0.so"
    "libatk-1.0.so"
)

for lib_pattern in "${LIBS[@]}"; do
    for lib_path in /usr/lib/x86_64-linux-gnu/${lib_pattern}* \
                    /lib/x86_64-linux-gnu/${lib_pattern}*; do
        if [ -f "$lib_path" ] && [ ! -L "$lib_path" ]; then
            cp -L "$lib_path" "$APPDIR/usr/lib/" 2>/dev/null || true
        fi
    done
done

# Also copy symlinks
for lib_pattern in "${LIBS[@]}"; do
    for lib_path in /usr/lib/x86_64-linux-gnu/${lib_pattern}*; do
        if [ -L "$lib_path" ]; then
            cp -P "$lib_path" "$APPDIR/usr/lib/" 2>/dev/null || true
        fi
    done
done

# -------------------------------------------------------- build AppImage ----
mkdir -p "$OUTDIR"
VERSION="0.1.0"
APPIMAGE_OUT="$OUTDIR/PrincessIDE-${VERSION}-x86_64.AppImage"

log "building AppImage"
# APPIMAGE_EXTRACT_AND_RUN=1 allows running without FUSE
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 \
    "$APPIMAGE_TOOL" "$APPDIR" "$APPIMAGE_OUT" 2>&1 | tail -5

if [ -f "$APPIMAGE_OUT" ]; then
    chmod +x "$APPIMAGE_OUT"
    SIZE="$(du -h "$APPIMAGE_OUT" | cut -f1)"
    log "AppImage created: $APPIMAGE_OUT ($SIZE)"
else
    die "AppImage creation failed"
fi

log "done"
exit 0
