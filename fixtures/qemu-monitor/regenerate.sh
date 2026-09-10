#!/usr/bin/env bash
# PrincessIDE P5 — regenerate the recorded QEMU monitor samples.
#
# One command, non-interactive, no dependency on the caller's cwd or PATH: it
# locates the repository from its own path, sources scripts/env.sh, builds the
# paging-kernel fixture ISO, boots it headless with QMP on a unix socket, waits
# until the guest has *enabled paging* and taken its deliberate #PF, then
# records `info registers` / `info mem` / `info tlb` / `info cpus` (plus a few
# structured QMP queries) into this directory.
#
#   bash fixtures/qemu-monitor/regenerate.sh
#
# Why wait for the #PF: `info mem` / `info tlb` are degenerate while CR0.PG=0
# ("PG disabled" / empty).  The kernel prints PAGING_ENABLED (CR0.PG=1, CR3
# non-zero) and then faults; after the fault it halts forever with CR3 still
# loaded, so the monitor reads a stable, paging-enabled state.
#
# Concurrency: `make -j2` at most (D16); only one QEMU is started.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=../../scripts/env.sh
source "$ROOT/scripts/env.sh"

OUT_DIR="$SCRIPT_DIR"
FIXTURE_DIR="$ROOT/fixtures/paging-kernel"
ISO="$FIXTURE_DIR/build/pagingkernel.iso"
BOOT_TIMEOUT="${PRINCESSIDE_MONITOR_BOOT_TIMEOUT:-60}"
MEMORY="${MEMORY:-256M}"

die() { echo "[qemu-monitor] ERROR: $*" >&2; exit 1; }

command -v qemu-system-x86_64 >/dev/null 2>&1 \
    || die "qemu-system-x86_64 not found (source scripts/env.sh)"
[ -n "${PRINCESSIDE_QEMU_DATA:-}" ] && [ -d "$PRINCESSIDE_QEMU_DATA" ] \
    || die "PRINCESSIDE_QEMU_DATA missing (source scripts/env.sh)"
command -v python3 >/dev/null 2>&1 || die "python3 not found"

echo "[qemu-monitor] step 1/4: build $ISO"
make -C "$FIXTURE_DIR" -j2 iso >/dev/null
[ -f "$ISO" ] || die "ISO was not produced at $ISO"

TMP="$(mktemp -d "$ROOT/.scratch/paging/monitor.XXXXXX")"
SERIAL="$TMP/serial.log"
QEMU_ERR="$TMP/qemu.stderr.log"
QMP_SOCK="$TMP/qmp.sock"
QPID=""

cleanup() {
    if [ -n "$QPID" ] && kill -0 "$QPID" 2>/dev/null; then
        kill "$QPID" 2>/dev/null || true
        wait "$QPID" 2>/dev/null || true
    fi
    rm -rf "$TMP"
}
trap cleanup EXIT

echo "[qemu-monitor] step 2/4: boot headless, QMP on unix socket"
# D9: -display none -serial stdio -monitor none; the machine interface is QMP
# on a unix socket, so stdio is never shared between two character devices.
# D2: GRUB Multiboot2 ISO via -cdrom + -boot d.
set -m
qemu-system-x86_64 \
    -L "$PRINCESSIDE_QEMU_DATA" \
    -m "$MEMORY" \
    -cdrom "$ISO" \
    -boot d \
    -display none \
    -serial stdio \
    -monitor none \
    -qmp "unix:$QMP_SOCK,server=on,wait=off" \
    -no-reboot \
    < /dev/null > "$SERIAL" 2> "$QEMU_ERR" &
QPID=$!
set +m

deadline=$((SECONDS + BOOT_TIMEOUT))
while kill -0 "$QPID" 2>/dev/null; do
    if grep -q 'PAGING_ENABLED' "$SERIAL" 2>/dev/null \
       && grep -q 'PANIC' "$SERIAL" 2>/dev/null; then
        break
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
        echo "[qemu-monitor] guest serial so far:" >&2
        cat "$SERIAL" >&2 || true
        die "timed out waiting for paging-enabled + #PF (${BOOT_TIMEOUT}s)"
    fi
    sleep 0.1
done
kill -0 "$QPID" 2>/dev/null || die "QEMU exited before the samples were taken"

# The guest is now halted after its deliberate #PF: CR3 is loaded and the page
# tables are the ones we want the monitor to report.  Give the final writes a
# moment to drain, then query.
sleep 0.5
grep -q 'PAGING_ENABLED' "$SERIAL" || die "paging was never enabled (no PAGING_ENABLED marker)"

echo "[qemu-monitor] step 3/4: capture HMP + QMP samples"
python3 "$SCRIPT_DIR/qmp_capture.py" "$QMP_SOCK" "$OUT_DIR"

echo "[qemu-monitor] step 4/4: record provenance"
cp "$SERIAL" "$OUT_DIR/capture-serial.log"
qemu-system-x86_64 --version > "$OUT_DIR/qemu-version.txt"

CR3="$(grep -oE 'PAGING_ENABLED cr3=0x[0-9a-f]+' "$SERIAL" | head -n1 | cut -d= -f2 || true)"
{
    echo "recorded-by: fixtures/qemu-monitor/regenerate.sh"
    echo "recorded-at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "fixture:     fixtures/paging-kernel (patched-paging multiboot2 kernel)"
    echo "guest-state: halted after deliberate #PF; paging enabled, CR3=$CR3"
    echo "qemu-argv:   qemu-system-x86_64 -L \$PRINCESSIDE_QEMU_DATA -m $MEMORY \\"
    echo "               -cdrom build/pagingkernel.iso -boot d -display none \\"
    echo "               -serial stdio -monitor none -qmp unix:<tmp>/qmp.sock,server=on,wait=off -no-reboot"
    echo "hmp-queries: info registers | info mem | info tlb | info cpus"
    echo "qmp-queries: query-version | query-status | query-cpus-fast | query-memory-size-summary"
} > "$OUT_DIR/capture-metadata.txt"

echo "[qemu-monitor] OK: samples regenerated in $OUT_DIR"
ls -1 "$OUT_DIR"/info-*.txt "$OUT_DIR"/qmp-*.json
