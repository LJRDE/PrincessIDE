#!/usr/bin/env bash
# PrincessIDE P8 — deb package builder.
#
# Builds a .deb package for the PrincessIDE desktop application.
#
# Strategy:
#   1. Build the Tauri desktop app in release mode.
#   2. Use Tauri's built-in deb bundler (configured in tauri.conf.json).
#
# The Tauri build produces a .deb under apps/desktop/src-tauri/target/release/bundle/deb/.
#
# Requirements:
#   - The full toolchain must be bootstrapped (bash scripts/bootstrap-toolchain.sh).
#   - Node.js + pnpm must be available for the frontend build.
#   - dpkg-deb must be available (standard on Debian/Ubuntu).
#
# Usage:
#   bash scripts/package-deb.sh [--no-build]
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

while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) DO_BUILD=0; shift ;;
        -h|--help)  sed -n '2,20p' "$0"; exit 0 ;;
        *)          echo "package-deb: unknown argument: $1" >&2; exit 2 ;;
    esac
done

log()  { printf '[package-deb] %s\n' "$*"; }
die()  { printf '[package-deb] ERROR: %s\n' "$*" >&2; exit 1; }

# ------------------------------------------------------------- preflight -----
command -v dpkg-deb >/dev/null 2>&1 || die "dpkg-deb not found (install dpkg)"
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

# ------------------------------------------------------- locate the .deb ----
DEB_DIR="$TAURI_DIR/target/release/bundle/deb"
if [ ! -d "$DEB_DIR" ]; then
    # Also check Tauri v2 output location
    DEB_DIR="$TAURI_DIR/target/release/bundle/deb"
fi

mkdir -p "$OUTDIR"

DEB_FILE=""
if [ -d "$DEB_DIR" ]; then
    DEB_FILE="$(ls "$DEB_DIR"/*.deb 2>/dev/null | head -n1)"
fi

if [ -n "$DEB_FILE" ] && [ -f "$DEB_FILE" ]; then
    cp "$DEB_FILE" "$OUTDIR/"
    DEB_NAME="$(basename "$DEB_FILE")"
    log "deb package created: $OUTDIR/$DEB_NAME"

    # Print package info
    log "package info:"
    dpkg-deb --info "$OUTDIR/$DEB_NAME" 2>/dev/null | head -20 || true
    log "package contents (first 20 entries):"
    dpkg-deb --contents "$OUTDIR/$DEB_NAME" 2>/dev/null | head -20 || true
else
    # Fallback: create a simple deb from the release binary
    log "Tauri deb not found, creating manual deb package"

    BINARY="$TAURI_DIR/target/release/princesside-desktop"
    if [ ! -f "$BINARY" ]; then
        BINARY="$TAURI_DIR/target/release/princesside"
    fi
    [ -f "$BINARY" ] || die "release binary not found; run without --no-build first"

    PKG_NAME="princesside"
    PKG_VERSION="0.1.0"
    PKG_DIR="/tmp/${PKG_NAME}_${PKG_VERSION}_amd64"

    rm -rf "$PKG_DIR"
    mkdir -p "$PKG_DIR/DEBIAN"
    mkdir -p "$PKG_DIR/usr/bin"
    mkdir -p "$PKG_DIR/usr/share/applications"
    mkdir -p "$PKG_DIR/usr/share/icons/hicolor/128x128/apps"

    # Control file
    cat >"$PKG_DIR/DEBIAN/control" <<EOF
Package: $PKG_NAME
Version: $PKG_VERSION
Section: devel
Priority: optional
Architecture: amd64
Depends: libgtk-3-0, libwebkit2gtk-4.1-0, libjavascriptcoregtk-4.1-0, libsoup-3.0-0
Maintainer: PrincessIDE Team
Description: PrincessIDE - IDE for x86_64 kernel development
 PrincessIDE is a desktop IDE designed for x86_64 kernel developers.
 It provides build, run, debug, binary analysis, page table visualization,
 and AI-assisted coding for bare-metal kernel projects.
EOF

    # Install binary
    cp "$BINARY" "$PKG_DIR/usr/bin/$PKG_NAME"
    chmod 755 "$PKG_DIR/usr/bin/$PKG_NAME"

    # Desktop file
    cat >"$PKG_DIR/usr/share/applications/$PKG_NAME.desktop" <<EOF
[Desktop Entry]
Name=PrincessIDE
Comment=IDE for x86_64 kernel development
Exec=/usr/bin/$PKG_NAME
Icon=$PKG_NAME
Type=Application
Categories=Development;IDE;
Terminal=false
EOF

    # Icon
    if [ -f "$ROOT/apps/desktop/src-tauri/icons/128x128.png" ]; then
        cp "$ROOT/apps/desktop/src-tauri/icons/128x128.png" \
           "$PKG_DIR/usr/share/icons/hicolor/128x128/apps/$PKG_NAME.png"
    fi

    # Build the .deb
    DEB_OUT="$OUTDIR/${PKG_NAME}_${PKG_VERSION}_amd64.deb"
    dpkg-deb --build "$PKG_DIR" "$DEB_OUT"

    log "deb package created: $DEB_OUT"
    log "package info:"
    dpkg-deb --info "$DEB_OUT" 2>/dev/null | head -20 || true
fi

log "done"
exit 0
