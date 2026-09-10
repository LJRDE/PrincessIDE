#!/usr/bin/env bash
# P4-2 independent oracle: read RIP/CR0/CR3/CR4 and physical memory with a
# SECOND, independent gdb process, and compare against the values the
# PrincessIDE backend reported in the same situation.
#
# Why a second process: P4-2 says the values must match "直接查 GDB" (asking GDB
# directly).  Comparing the backend against its own parsing is circular.  This
# script therefore drives `princess-gdb` in plain CLI mode (`-batch -ex ...`),
# which shares no code at all with `crates/princess-debug`, and prints the two
# side by side.
#
# The machine is the SAME kind of machine (`qemu -s -S`, same ISO, same flags),
# booted fresh for this oracle: two gdbs cannot share one stub's stop state, and
# a shared session would let one interfere with the other.  The register values
# are deterministic at the `paging_fault_probe` entry point because the fixture
# is built `-O0` and the boot path is fixed.
#
# Usage: bash scripts/p4-gdb-oracle.sh [iso] [elf] [outdir]
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=env.sh
source "$SCRIPT_DIR/env.sh"

ISO="${1:-$ROOT/fixtures/paging-kernel/build/pagingkernel.iso}"
ELF="${2:-$ROOT/fixtures/paging-kernel/build/pagingkernel.elf}"
OUT="${3:-$ROOT/.scratch/debug/artifacts}"
GDB="${PRINCESSIDE_DEBUG_GDB:-princess-gdb}"
PORT="${P4_ORACLE_PORT:-1239}"   # distinct from the backend's 1234

mkdir -p "$OUT"
ISO_MARK="$(basename "$ISO")"

cleanup() {
    pkill -f "qemu-system-x86_64.*$ISO_MARK" 2>/dev/null
    sleep 1
}
trap cleanup EXIT

echo "[oracle] starting a fresh qemu -s -S for the independent gdb"
qemu-system-x86_64 -L "$PRINCESSIDE_QEMU_DATA" -m 256M \
    -cdrom "$ISO" -boot d -display none \
    -serial "file:$OUT/oracle-serial.log" -monitor none -no-reboot \
    -gdb "tcp::$PORT" -S >/dev/null 2>&1 &
QEMU_PID=$!

# Wait for the stub rather than sleeping a fixed amount.
for _ in $(seq 1 100); do
    if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then break; fi
    sleep 0.2
done

echo "[oracle] running: $GDB -batch -ex 'target remote :$PORT' -ex 'symbol-file $ELF' -ex ..."
# `-batch` makes gdb exit after the -ex list; every command's output is the
# oracle's raw answer.
"$GDB" -q -batch \
    -ex "set pagination off" \
    -ex "set confirm off" \
    -ex "file $ELF" \
    -ex "target remote :$PORT" \
    -ex "hbreak paging_fault_probe" \
    -ex "continue" \
    -ex "p/x \$pc" \
    -ex "p/x \$cr0" \
    -ex "p/x \$cr3" \
    -ex "p/x \$cr4" \
    -ex "x/16xb \$cr3" \
    -ex "info registers rip cr0 cr3 cr4" \
    -ex "monitor xp /2xg 0x104000" \
    > "$OUT/p4-gdb-oracle.txt" 2>&1
RC=$?
echo "[oracle] gdb exit code: $RC"
echo "[oracle] raw output saved to $OUT/p4-gdb-oracle.txt"
echo "--- oracle output (raw) ---"
cat "$OUT/p4-gdb-oracle.txt"
exit "$RC"
