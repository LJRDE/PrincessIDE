#!/usr/bin/env bash
# PrincessIDE P4 acceptance — headless, timeout-wrapped, orphan-free.
#
# Runs `crates/princess-debug/examples/p4_acceptance.rs` against the paging
# fixture with a REAL qemu-system-x86_64 and a REAL gdb 16.3 DAP adapter.
#
# Orphan safety (the acceptance criterion is explicit about this):
#
#   * the harness itself owns QEMU through `princess_debug::QemuProcess`, which
#     puts it in its own process group and kills the group on Drop;
#   * this script wraps the whole run in `timeout`, so a hung session is killed
#     from outside;
#   * the trap below kills any machine this run started, even on Ctrl-C;
#   * the final check in the harness greps for a surviving `qemu-system-x86_64`
#     carrying this fixture's ISO path and fails the run if one is found.
#
# `timeout` is used WITHOUT --foreground on purpose: the child gets its own
# process group, so `timeout`'s SIGTERM reaches the whole tree.
#
# Usage:
#   bash scripts/p4-acceptance.sh [iso] [elf] [outdir]
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=env.sh
source "$SCRIPT_DIR/env.sh"

ISO="${1:-$ROOT/fixtures/paging-kernel/build/pagingkernel.iso}"
ELF="${2:-$ROOT/fixtures/paging-kernel/build/pagingkernel.elf}"
OUT="${3:-$ROOT/.scratch/debug/artifacts}"
TIME_BUDGET="${P4_TIME_BUDGET:-420}"

cd "$ROOT"
mkdir -p "$OUT"

log() { printf '[p4] %s\n' "$*"; }

# ---------------------------------------------------------------- preflight --
if [ ! -f "$ISO" ]; then
    log "FAIL: $ISO not found; build the fixture first (make -C fixtures/paging-kernel iso)"
    exit 2
fi
if [ ! -f "$ELF" ]; then
    log "FAIL: $ELF not found; build the fixture first (make -C fixtures/paging-kernel)"
    exit 2
fi
if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    log "FAIL: qemu-system-x86_64 not on PATH; source scripts/env.sh"
    exit 2
fi

# The adapter may be mid-rebuild by a concurrent toolchain job (the gdb16 rootfs
# is unpacked into .toolchain/, and a bootstrap run replaces it).  Wait briefly
# rather than reporting a spurious failure, then give up loudly.
ADAPTER="${PRINCESSIDE_DEBUG_GDB:-princess-gdb}"
for _ in $(seq 1 60); do
    if [ -x "$ADAPTER" ] || command -v "$ADAPTER" >/dev/null 2>&1; then
        break
    fi
    log "waiting for the debug adapter ($ADAPTER) to appear ..."
    sleep 5
done
if [ ! -x "$ADAPTER" ] && ! command -v "$ADAPTER" >/dev/null 2>&1; then
    log "FAIL: debug adapter '$ADAPTER' is not executable"
    log "      run: bash scripts/bootstrap-toolchain.sh   (installs gdb 16.3, A9/D11)"
    exit 2
fi
export PRINCESSIDE_DEBUG_GDB="$ADAPTER"

log "adapter : $ADAPTER"
log "iso     : $ISO"
log "elf     : $ELF"
log "out     : $OUT"
log "timeout : ${TIME_BUDGET}s"

# ------------------------------------------------------------------- run it --
# Kill anything this script started, whatever the exit path.
P4_QEMU_ISO_MARK="$(basename "$ISO")"
cleanup() {
    local rc=$?
    # Only ever match machines booting THIS fixture, never another agent's.
    if pgrep -af 'qemu-system-x86_64' 2>/dev/null | grep -F "$P4_QEMU_ISO_MARK" | grep -qv pgrep; then
        log "cleanup: killing leftover qemu for $P4_QEMU_ISO_MARK"
        pkill -f "qemu-system-x86_64.*$P4_QEMU_ISO_MARK" 2>/dev/null
        sleep 1
    fi
    return $rc
}
trap cleanup EXIT

LOG="$OUT/p4-acceptance.log"
# shellcheck disable=SC2086
CARGO_BUILD_JOBS=2 timeout --signal=TERM --kill-after=15 "$TIME_BUDGET" \
    cargo run -q -p princess-debug --example p4_acceptance -- "$ISO" "$ELF" "$OUT" \
    2>&1 | tee "$LOG"
RC="${PIPESTATUS[0]}"

log "harness exit code: $RC"
if [ "$RC" -eq 124 ] || [ "$RC" -eq 137 ]; then
    log "FAIL: the acceptance run exceeded ${TIME_BUDGET}s and was killed"
fi

# Post-condition, independent of the harness's own check: no orphan machine.
if pgrep -af 'qemu-system-x86_64' 2>/dev/null | grep -F "$P4_QEMU_ISO_MARK" | grep -qv pgrep; then
    log "FAIL: an orphan qemu-system-x86_64 survived the run:"
    pgrep -af 'qemu-system-x86_64' | grep -F "$P4_QEMU_ISO_MARK" | grep -v pgrep
    exit 1
fi
log "no orphan qemu remains"

exit "$RC"
