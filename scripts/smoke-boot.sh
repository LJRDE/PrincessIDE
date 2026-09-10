#!/usr/bin/env bash
# PrincessIDE P0 — headless boot smoke test.
#
# Boots the reference kernel fixture in QEMU with no display, captures the COM1
# serial output to a log file, and asserts the P0 fixture contract:
#
#   1. the fixed banner "PrincessIDE reference kernel booted" appears,
#   2. a CPU exception is taken (vector 6, #UD),
#   3. the panic report contains the faulting RIP in "FAULT_RIP=0x..." form.
#
# Exit status: 0 when every assertion holds, non-zero otherwise (the full
# captured output is dumped on failure).
#
# Works in a clean shell: it locates and sources scripts/env.sh itself.
#
# Usage:  bash scripts/smoke-boot.sh [-t SECONDS] [--no-build]
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=env.sh
source "$SCRIPT_DIR/env.sh"

FIXTURE_DIR="$ROOT/fixtures/refkernel"
BUILD_DIR="$FIXTURE_DIR/build"
ELF="$BUILD_DIR/refkernel.elf"
ISO="$BUILD_DIR/refkernel.iso"
LOG="$BUILD_DIR/smoke-boot.log"

BOOT_TIMEOUT=25
DO_BUILD=1
MEMORY=256M

while [ $# -gt 0 ]; do
    case "$1" in
        -t|--timeout) BOOT_TIMEOUT="$2"; shift 2 ;;
        --no-build)   DO_BUILD=0; shift ;;
        -h|--help)    sed -n '2,18p' "$0"; exit 0 ;;
        *) echo "smoke-boot: unknown argument: $1" >&2; exit 2 ;;
    esac
done

BANNER="PrincessIDE reference kernel booted"
VECTOR_PATTERN="EXCEPTION: vector=0x06"
RIP_PATTERN="FAULT_RIP=0x"

fail() {
    printf '\n[smoke-boot] FAIL: %s\n' "$*" >&2
    if [ -f "$LOG" ]; then
        printf '[smoke-boot] --- captured serial output (%s) ---\n' "$LOG" >&2
        cat "$LOG" >&2
        printf '[smoke-boot] --- end captured serial output ---\n' >&2
    else
        printf '[smoke-boot] no serial log was produced at %s\n' "$LOG" >&2
    fi
    exit 1
}

# ------------------------------------------------------------------- build ---
if [ "$DO_BUILD" = "1" ] || [ ! -f "$ISO" ] || [ ! -f "$ELF" ]; then
    mkdir -p "$BUILD_DIR"
    echo "[smoke-boot] building reference kernel and ISO"
    if ! make -C "$FIXTURE_DIR" iso >"$BUILD_DIR/build.log" 2>&1; then
        echo "[smoke-boot] build failed:" >&2
        cat "$BUILD_DIR/build.log" >&2
        exit 1
    fi
fi

[ -f "$ISO" ] || fail "ISO not found at $ISO"
command -v qemu-system-x86_64 >/dev/null 2>&1 || fail "qemu-system-x86_64 not on PATH (run scripts/bootstrap-toolchain.sh)"
[ -d "$PRINCESSIDE_QEMU_DATA" ] || fail "QEMU data dir missing: $PRINCESSIDE_QEMU_DATA"

# ---------------------------------------------------------------- boot it ---
: > "$LOG"

# -display none          : fully headless, there is no DISPLAY on this host
# -serial stdio          : COM1 on stdio, redirected into $LOG (output on disk)
# -no-reboot             : stop instead of rebooting the machine on a triple fault
# -monitor none          : keep the monitor out of the way
# -boot d                : boot from the CD-ROM (the GRUB ISO)
# timeout                : hard guard; the kernel halts forever by design
set -m
timeout -k 5 "$BOOT_TIMEOUT" \
    qemu-system-x86_64 \
        -L "$PRINCESSIDE_QEMU_DATA" \
        -m "$MEMORY" \
        -cdrom "$ISO" \
        -boot d \
        -display none \
        -serial stdio \
        -no-reboot \
        -monitor none \
        < /dev/null > "$LOG" 2>&1 &
QEMU_PID=$!
set +m
# Return as soon as the panic report lands instead of always waiting the full
# timeout: the kernel deliberately halts in an infinite loop after the panic.
deadline=$((SECONDS + BOOT_TIMEOUT))
while kill -0 "$QEMU_PID" 2>/dev/null; do
    grep -q "$RIP_PATTERN" "$LOG" 2>/dev/null && break
    [ "$SECONDS" -ge "$deadline" ] && break
    sleep 0.1
done

kill "$QEMU_PID" 2>/dev/null
wait "$QEMU_PID" 2>/dev/null

# --------------------------------------------------------------- assert -----
if ! grep -qF "$BANNER" "$LOG"; then
    fail "banner not found in serial output: \"$BANNER\""
fi

if ! grep -qF "$VECTOR_PATTERN" "$LOG"; then
    fail "expected exception not taken: pattern \"$VECTOR_PATTERN\" not found"
fi

if ! grep -qE "$RIP_PATTERN[0-9a-f]{16}" "$LOG"; then
    fail "no fault RIP reported: pattern \"${RIP_PATTERN}<16 hex digits>\" not found"
fi

FAULT_RIP="$(grep -oE "${RIP_PATTERN}[0-9a-f]{16}" "$LOG" | head -n1 | cut -d= -f2)"
if [ "$FAULT_RIP" = "0x0000000000000000" ]; then
    fail "fault RIP is null"
fi

printf '[smoke-boot] PASS\n'
printf '[smoke-boot]   banner    : %s\n' "$BANNER"
printf '[smoke-boot]   exception : %s\n' "$(grep -m1 -F "$VECTOR_PATTERN" "$LOG" | sed 's/^\[refkernel\] //')"
printf '[smoke-boot]   fault rip : %s\n' "$FAULT_RIP"
printf '[smoke-boot]   serial log: %s\n' "$LOG"
exit 0
