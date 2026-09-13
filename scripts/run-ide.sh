#!/usr/bin/env bash
# PrincessIDE — launch the desktop shell.
#
# The README's manual path is three commands in the right order
# (`source scripts/env.sh`, `pnpm install`, `pnpm -C apps/desktop tauri dev`).
# Getting the order wrong, or skipping one, produces failures that look like
# bugs in the app rather than a missing prerequisite — so this script does the
# prerequisites itself and, when something is genuinely absent, says which
# command fixes it instead of letting Tauri fail somewhere deeper.
#
# Modes:
#   --tauri     (default) real Tauri window: vite dev server + Rust shell.
#               This is the actual IDE; it needs a display.
#   --browser   vite dev server only, then open the URL yourself.  The shell
#               renders and the bundled event fixtures replay, but there is no
#               engine behind it: IPC calls fail explicitly and the native
#               directory/file dialogs are absent (the panels degrade to their
#               text inputs).  Useful for checking layout without a display.
#   --check     run the preflight checks and exit without launching anything.
#
# Works in a clean shell: it locates and sources scripts/env.sh itself.
#
# Usage:  bash scripts/run-ide.sh [--tauri|--browser|--check]
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DESKTOP="$ROOT/apps/desktop"

# vite.config.ts pins these (strictPort: true) and tauri.conf.json's devUrl
# must agree with it, so the pair is duplicated here on purpose: if they ever
# disagree, this script is where it should be noticed.
VITE_PORT=1420
DEV_URL="http://127.0.0.1:$VITE_PORT"

MODE=tauri
while [ $# -gt 0 ]; do
    case "$1" in
        --tauri)   MODE=tauri; shift ;;
        --browser) MODE=browser; shift ;;
        --check)   MODE=check; shift ;;
        -h|--help) sed -n '2,23p' "$0"; exit 0 ;;
        *) echo "run-ide: unknown argument: $1" >&2; exit 2 ;;
    esac
done

log()  { printf '[run-ide] %s\n' "$*"; }
warn() { printf '[run-ide][warn] %s\n' "$*" >&2; }
die()  { printf '[run-ide][error] %s\n' "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------
PROBLEMS=0

problem() {
    PROBLEMS=$((PROBLEMS + 1))
    printf '[run-ide][error] %s\n' "$*" >&2
}

log "workspace: $ROOT"

# 1) Toolchain on PATH.  env.sh is idempotent and puts .toolchain/bin first, so
#    sourcing it here is exactly what the README asks for, minus the chance of
#    forgetting it.
# shellcheck source=env.sh
source "$SCRIPT_DIR/env.sh"

for tool in node pnpm; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        problem "$tool is not on PATH"
        warn "  install it, then re-run.  node/pnpm are host tools, not part of .toolchain/."
    fi
done

# 2) Frontend dependencies.
if [ ! -d "$DESKTOP/node_modules" ]; then
    problem "frontend dependencies are missing: $DESKTOP/node_modules"
    warn "  fix: pnpm -C apps/desktop install"
    warn "  (if the default registry is unreachable, add:"
    warn "   --registry=https://registry.npmmirror.com)"
fi

if [ "$MODE" != "browser" ]; then
    # 3) Rust toolchain.  The shell is a separate cargo workspace, so it needs a
    #    real cargo even though the engine crates are not built here.
    if ! command -v cargo >/dev/null 2>&1; then
        problem "cargo is not on PATH"
        warn "  fix: bash scripts/bootstrap-toolchain.sh"
    fi

    # 4) GUI libs.  These come from the host, not from bootstrap; doctor.sh
    #    reports them too, but failing here is friendlier than a compile error
    #    several minutes into the Tauri build.
    if command -v pkg-config >/dev/null 2>&1; then
        for lib in webkit2gtk-4.1 gtk+-3.0 javascriptcoregtk-4.1 libsoup-3.0; do
            pkg-config --exists "$lib" 2>/dev/null || {
                problem "GUI library missing: $lib"
                warn "  fix (Debian/Ubuntu): sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev librsvg2-dev"
            }
        done
    else
        problem "pkg-config is not on PATH (needed to find the GUI libraries)"
    fi

    # 5) A display.  Without one the window cannot open, and Tauri's own error
    #    for this is not obvious.
    if [ -z "${DISPLAY:-}" ] && [ -z "${WAYLAND_DISPLAY:-}" ]; then
        problem "no display: neither DISPLAY nor WAYLAND_DISPLAY is set"
        warn "  run this from a graphical session, or use --browser for a headless preview."
    fi
fi

# 6) Port.  vite runs with strictPort, so a busy 1420 is a hard failure.
if command -v ss >/dev/null 2>&1; then
    holder="$(ss -ltnp 2>/dev/null | grep -E "[:.]$VITE_PORT[[:space:]]" || true)"
    if [ -n "$holder" ]; then
        problem "port $VITE_PORT is already in use"
        warn "  ${holder#*(}"
        warn "  another 'tauri dev' or 'vite' is probably still running; stop it and retry."
    fi
fi

if [ "$PROBLEMS" -ne 0 ]; then
    echo "" >&2
    die "$PROBLEMS preflight problem(s); nothing was launched."
fi

log "preflight ok"

if [ "$MODE" = "check" ]; then
    log "check complete: \`pnpm -C apps/desktop tauri dev\` would run now"
    exit 0
fi

# ---------------------------------------------------------------------------
# Launch
# ---------------------------------------------------------------------------
if [ "$MODE" = "browser" ]; then
    log "starting the vite dev server (no engine behind it — see --help)"
    log "open: $DEV_URL"
    exec pnpm -C "$DESKTOP" dev
fi

log "starting the Tauri shell: vite on $VITE_PORT + cargo build of the desktop app"
log "the first launch compiles the Rust side; later ones are much faster"
log "close with Ctrl+C in this terminal"
echo ""

# exec, so Ctrl+C reaches `tauri dev` directly and it can tear down its own
# vite child.  Intercepting the signal here would only add a way to leak it.
exec pnpm -C "$DESKTOP" tauri dev
