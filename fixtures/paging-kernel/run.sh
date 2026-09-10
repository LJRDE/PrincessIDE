#!/usr/bin/env bash
# PrincessIDE paging kernel — headless QEMU runner.
#
# Boots build/pagingkernel.iso with no display, captures COM1 to
# build/serial.log and exits 0 only when BOTH hold:
#   1. the banner "PrincessIDE paging kernel booted" appears, and
#   2. the #PF report ("FAULT_RIP=0x...") appears, which can only happen with
#      CR0.PG=1 (paging really on).
#
# It is what `make run` invokes, but it can also be called directly:
#
#   source /path/to/PrincessIDE/scripts/env.sh
#   bash run.sh [timeout_seconds]
#
# Non-interactive: it locates the repo from its own path and sources
# scripts/env.sh itself, so it does not depend on the caller's cwd or PATH.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../../scripts/env.sh
source "$HERE/../../scripts/env.sh"

cd "$HERE"

TIMEOUT="${1:-${BOOT_TIMEOUT:-45}}"
MEMORY="${MEMORY:-256M}"
BANNER="${BANNER:-PrincessIDE paging kernel booted}"
FAULT_MARK="${FAULT_MARK:-FAULT_RIP=}"
ISO="build/pagingkernel.iso"
LOG="build/serial.log"
QEMU_ERR="build/qemu.stderr.log"

if [ ! -f "$ISO" ]; then
    echo "[pagingkernel] run: $ISO not found; run 'make iso' first" >&2
    exit 1
fi
if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "[pagingkernel] run: qemu-system-x86_64 not found; source scripts/env.sh" >&2
    exit 1
fi
if [ -z "${PRINCESSIDE_QEMU_DATA:-}" ] || [ ! -d "$PRINCESSIDE_QEMU_DATA" ]; then
    echo "[pagingkernel] run: PRINCESSIDE_QEMU_DATA unset or missing; source scripts/env.sh" >&2
    exit 1
fi

mkdir -p build
: > "$LOG"
: > "$QEMU_ERR"

# -display none -serial stdio -monitor none : clean headless COM1 capture (D9)
# -cdrom + -boot d                          : GRUB Multiboot2 ISO path (D2)
# The kernel halts forever after the panic, so poll and stop early.
set -m
qemu-system-x86_64 \
    -L "$PRINCESSIDE_QEMU_DATA" \
    -m "$MEMORY" \
    -cdrom "$ISO" \
    -boot d \
    -display none \
    -serial stdio \
    -monitor none \
    -no-reboot \
    < /dev/null > "$LOG" 2> "$QEMU_ERR" &
QPID=$!
set +m

deadline=$((SECONDS + TIMEOUT))
while kill -0 "$QPID" 2>/dev/null; do
    grep -qF "$FAULT_MARK" "$LOG" 2>/dev/null && break
    [ "$SECONDS" -ge "$deadline" ] && break
    sleep 0.1
done

kill "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null

if ! grep -qF "$BANNER" "$LOG"; then
    echo "[pagingkernel] FAIL: banner not found in serial output" >&2
    cat "$LOG" >&2
    if [ -s "$QEMU_ERR" ]; then
        echo "[pagingkernel] --- qemu stderr ---" >&2
        cat "$QEMU_ERR" >&2
    fi
    exit 1
fi

if ! grep -qF "$FAULT_MARK" "$LOG"; then
    echo "[pagingkernel] FAIL: banner seen but no #PF report (\"$FAULT_MARK\")" >&2
    cat "$LOG" >&2
    exit 1
fi

echo "[pagingkernel] PASS: $BANNER"
echo "[pagingkernel] PASS: #PF reported (paging on, fault taken)"
echo "[pagingkernel] serial log: $HERE/$LOG"
exit 0
