#!/usr/bin/env bash
# PrincessIDE P0 — symbolication of the reference kernel's fault RIP.
#
# Reads the fault RIP that the reference kernel reported on COM1, then maps it
# back to a source location with addr2line and shows the faulting instruction
# with objdump.
#
# The RIP is taken from, in order of precedence:
#   1. an address passed on the command line,
#   2. the log named by --log (default: fixtures/refkernel/build/smoke-boot.log),
#   3. a fresh boot performed by scripts/smoke-boot.sh.
#
# Exit status: 0 when the RIP resolves to a real source location inside
# fixtures/refkernel/ and lands in refkernel_fault_probe(); non-zero otherwise.
#
# Works in a clean shell: it locates and sources scripts/env.sh itself.
#
# Usage:
#   bash scripts/symbolicate.sh                 # use the last smoke log, else boot
#   bash scripts/symbolicate.sh 0x100b3d        # explicit RIP
#   bash scripts/symbolicate.sh --log FILE      # use a specific serial log
#   bash scripts/symbolicate.sh --elf FILE      # use a specific ELF
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=env.sh
source "$SCRIPT_DIR/env.sh"

FIXTURE_DIR="$ROOT/fixtures/refkernel"
BUILD_DIR="$FIXTURE_DIR/build"
ELF="$BUILD_DIR/refkernel.elf"
DEFAULT_LOG="$BUILD_DIR/smoke-boot.log"

EXPECTED_FUNCTION="refkernel_fault_probe"

LOG="$DEFAULT_LOG"
RIP=""

while [ $# -gt 0 ]; do
    case "$1" in
        --log)     LOG="$2"; shift 2 ;;
        --elf)     ELF="$2"; shift 2 ;;
        -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
        -*)        echo "symbolicate: unknown option: $1" >&2; exit 2 ;;
        *)         RIP="$1"; shift ;;
    esac
done

die() { printf '[symbolicate] %s\n' "$*" >&2; exit 1; }

# ------------------------------------------------------------- the ELF file ---
if [ ! -f "$ELF" ]; then
    mkdir -p "$BUILD_DIR"
    echo "[symbolicate] $ELF is missing, building it"
    if ! make -C "$FIXTURE_DIR" >"$BUILD_DIR/build.log" 2>&1; then
        echo "[symbolicate] build failed:" >&2
        cat "$BUILD_DIR/build.log" >&2
        exit 1
    fi
fi

command -v addr2line >/dev/null 2>&1 || die "addr2line not on PATH"
command -v objdump   >/dev/null 2>&1 || die "objdump not on PATH"

# ------------------------------------------------------------ the fault RIP ---
if [ -z "$RIP" ]; then
    if [ ! -f "$LOG" ]; then
        echo "[symbolicate] no serial log at $LOG, booting the fixture to obtain one"
        bash "$SCRIPT_DIR/smoke-boot.sh" >/dev/null \
            || die "smoke-boot.sh failed; cannot obtain a fault RIP"
    fi
    RIP="$(grep -oE 'FAULT_RIP=0x[0-9a-fA-F]+' "$LOG" | head -n1 | cut -d= -f2)"
    [ -n "$RIP" ] || {
        echo "[symbolicate] no FAULT_RIP=<hex> line found in $LOG" >&2
        tail -n 20 "$LOG" >&2
        exit 1
    }
    RIP_SOURCE="serial log $LOG"
else
    RIP_SOURCE="command line"
fi

case "$RIP" in
    0x*|0X*) : ;;
    *) RIP="0x$RIP" ;;
esac

case "$RIP" in
    *[!0-9a-fA-FxX]*) die "not a hexadecimal address: $RIP" ;;
esac

ADDR=$((RIP))
START_HEX="$(printf '0x%x' $((ADDR - 32)))"
END_HEX="$(printf '0x%x' $((ADDR + 8)))"

# ---------------------------------------------------------------- symbolise ---
mapfile -t A2L < <(addr2line -f -C -e "$ELF" "$RIP")
FUNCTION="${A2L[0]:-?}"
LOCATION="${A2L[1]:-??:0}"

SYMBOL=""
while read -r addr type name; do
    case "$addr" in
        ""|*[!0-9a-fA-F]*) continue ;;
    esac
    if [ $((16#$addr)) -le "$ADDR" ]; then
        SYMBOL="$addr $type $name"
    fi
done < <(nm -n "$ELF" 2>/dev/null)

printf 'symbolicate: reference kernel fault report\n'
printf '  elf          : %s\n' "$ELF"
printf '  rip source   : %s\n' "$RIP_SOURCE"
printf '  fault RIP    : %s\n' "$RIP"
printf '  addr2line -f : %s\n' "$FUNCTION"
printf '  addr2line -e : %s\n' "$LOCATION"
printf '  nm symbol    : %s\n' "${SYMBOL:-<none>}"
printf '\n'

printf -- '--- objdump: faulting instruction and line mapping (%s .. %s) ---\n' "$START_HEX" "$END_HEX"
objdump -d -Mintel --line-numbers \
    --start-address="$START_HEX" --stop-address="$END_HEX" "$ELF"
printf '\n'

printf -- '--- addr2line -f -C -e %s %s ---\n' "$(basename "$ELF")" "$RIP"
addr2line -f -C -e "$ELF" "$RIP"
printf '\n'

# --------------------------------------------------------------- validate ----
case "$LOCATION" in
    "??:0"|"??"|"")
        die "FAIL: addr2line could not map $RIP to a source location"
        ;;
esac

if [ "$FUNCTION" != "$EXPECTED_FUNCTION" ]; then
    printf '[symbolicate] FAIL: expected the fault RIP inside %s(), got %s\n' \
        "$EXPECTED_FUNCTION" "$FUNCTION" >&2
    exit 1
fi

if ! objdump -d --start-address="$RIP" --stop-address="$((ADDR + 4))" "$ELF" | grep -qi 'ud2'; then
    printf '[symbolicate] warning: no ud2 instruction found at the reported RIP\n' >&2
fi

printf '[symbolicate] PASS: %s -> %s (%s)\n' "$RIP" "$LOCATION" "$FUNCTION"
exit 0
