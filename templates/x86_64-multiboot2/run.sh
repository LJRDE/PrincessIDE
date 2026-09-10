#!/usr/bin/env bash
# PrincessIDE kernel project template — headless QEMU runner.
#
# Boots build/kernel.iso with no display, captures COM1 to build/serial.log and
# exits 0 only when the template banner appears.  It is what `make run` invokes,
# but it can also be called directly:
#
#   source /path/to/PrincessIDE/scripts/env.sh
#   bash run.sh [timeout_seconds]
#
# Run it from a shell where scripts/env.sh has been sourced: QEMU needs
# $PRINCESSIDE_QEMU_DATA (the firmware / option-ROM image directory).
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

TIMEOUT="${1:-${BOOT_TIMEOUT:-30}}"
MEMORY="${MEMORY:-256M}"
BANNER="${BANNER:-PrincessIDE template kernel booted}"
ISO="build/kernel.iso"
LOG="build/serial.log"
QEMU_ERR="build/qemu.stderr.log"

if [ ! -f "$ISO" ]; then
    echo "[template] run: $ISO not found; run 'make iso' first" >&2
    exit 1
fi
if ! command -v qemu-system-x86_64 >/dev/null 2>&1; then
    echo "[template] run: qemu-system-x86_64 not found; source scripts/env.sh" >&2
    exit 1
fi
if [ -z "${PRINCESSIDE_QEMU_DATA:-}" ] || [ ! -d "$PRINCESSIDE_QEMU_DATA" ]; then
    echo "[template] run: PRINCESSIDE_QEMU_DATA unset or missing; source scripts/env.sh" >&2
    exit 1
fi

mkdir -p build
: > "$LOG"
: > "$QEMU_ERR"

# -display none -serial stdio -monitor none : clean headless COM1 capture (D9)
# -cdrom + -boot d                          : GRUB Multiboot2 ISO path (D2)
# The template kernel halts forever, so poll for the banner and stop early.
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
    grep -qF "$BANNER" "$LOG" 2>/dev/null && break
    [ "$SECONDS" -ge "$deadline" ] && break
    sleep 0.1
done

kill "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null

if grep -qF "$BANNER" "$LOG"; then
    echo "[template] PASS: $BANNER"
    echo "[template] serial log: $HERE/$LOG"
    exit 0
fi

echo "[template] FAIL: banner not found in serial output" >&2
cat "$LOG" >&2
if [ -s "$QEMU_ERR" ]; then
    echo "[template] --- qemu stderr ---" >&2
    cat "$QEMU_ERR" >&2
fi
exit 1
