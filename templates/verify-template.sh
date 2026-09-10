#!/usr/bin/env bash
# PrincessIDE P6 — kernel-template self-verification.
#
# Copies templates/x86_64-multiboot2 into a throwaway directory, builds it,
# boots the resulting GRUB ISO headless in QEMU, and asserts the template banner
# appears on COM1.  Exit status 0 means every step passed.
#
# Repeatable and non-interactive: it locates the repository from its own path,
# sources scripts/env.sh itself and does not depend on the caller's cwd or PATH.
#
# Usage:  bash templates/verify-template.sh [--keep]
#   --keep   keep the temporary project directory instead of deleting it
#
# Environment overrides:
#   PRINCESSIDE_TEMPLATE_BOOT_TIMEOUT  QEMU boot budget in seconds (default 45)
#   PRINCESSIDE_TEMPLATE_MEMORY        guest RAM (default 256M)
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMPLATE="$SCRIPT_DIR/x86_64-multiboot2"

# shellcheck source=../scripts/env.sh
source "$ROOT/scripts/env.sh"

BANNER="PrincessIDE template kernel booted"
FOREIGN_BANNER="PrincessIDE reference kernel booted"
BOOT_TIMEOUT="${PRINCESSIDE_TEMPLATE_BOOT_TIMEOUT:-45}"
MEMORY="${PRINCESSIDE_TEMPLATE_MEMORY:-256M}"

KEEP=0
[ "${1:-}" = "--keep" ] && KEEP=1

WORK_BASE="$ROOT/.scratch/templates"
mkdir -p "$WORK_BASE"
WORK="$(mktemp -d "$WORK_BASE/verify.XXXXXX")"
PROJ="$WORK/x86_64-multiboot2"
LOG="$PROJ/build/serial.log"
QEMU_ERR="$PROJ/build/qemu.stderr.log"

cleanup() {
    if [ "$KEEP" = "1" ]; then
        echo "[verify] kept temporary project: $WORK"
    else
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

fail() {
    echo "[verify] FAIL: $*" >&2
    if [ -f "$LOG" ]; then
        echo "[verify] --- captured serial output ($LOG) ---" >&2
        cat "$LOG" >&2
        echo "[verify] --- end captured serial output ---" >&2
    fi
    if [ -s "${QEMU_ERR:-}" ]; then
        echo "[verify] --- qemu stderr ---" >&2
        cat "$QEMU_ERR" >&2
    fi
    exit 1
}

[ -d "$TEMPLATE" ] || fail "template directory not found: $TEMPLATE"
command -v qemu-system-x86_64 >/dev/null 2>&1 \
    || fail "qemu-system-x86_64 not on PATH (source scripts/env.sh / run bootstrap)"
[ -n "${PRINCESSIDE_QEMU_DATA:-}" ] && [ -d "$PRINCESSIDE_QEMU_DATA" ] \
    || fail "PRINCESSIDE_QEMU_DATA missing (source scripts/env.sh)"
command -v grub-mkrescue >/dev/null 2>&1 \
    || fail "grub-mkrescue not found (install grub-pc-bin / grub2-common)"

echo "[verify] template : $TEMPLATE"
echo "[verify] workdir  : $WORK"

# ---------------------------------------------------------------- 1. copy ---
echo "[verify] step 1/5: copy template -> $PROJ"
cp -a "$TEMPLATE" "$PROJ" || fail "could not copy the template"

# --------------------------------------------------------------- 2. build ---
echo "[verify] step 2/5: make -j2   (compile + link)"
if ! make -C "$PROJ" -j2 >"$WORK/make.log" 2>&1; then
    cat "$WORK/make.log" >&2
    fail "'make -j2' failed"
fi
[ -f "$PROJ/build/kernel.elf" ] || fail "build/kernel.elf was not produced"

# ----------------------------------------------------------------- 3. iso ---
echo "[verify] step 3/5: make -j2 iso   (GRUB Multiboot2 ISO)"
if ! make -C "$PROJ" -j2 iso >"$WORK/iso.log" 2>&1; then
    cat "$WORK/iso.log" >&2
    fail "'make -j2 iso' failed"
fi
[ -f "$PROJ/build/kernel.iso" ] || fail "build/kernel.iso was not produced"

# ---------------------------------------------------------------- 4. boot ---
echo "[verify] step 4/5: headless QEMU boot (timeout ${BOOT_TIMEOUT}s)"
mkdir -p "$PROJ/build"
: > "$LOG"
: > "$QEMU_ERR"

set -m
qemu-system-x86_64 \
    -L "$PRINCESSIDE_QEMU_DATA" \
    -m "$MEMORY" \
    -cdrom "$PROJ/build/kernel.iso" \
    -boot d \
    -display none \
    -serial stdio \
    -monitor none \
    -no-reboot \
    < /dev/null > "$LOG" 2> "$QEMU_ERR" &
QPID=$!
set +m

deadline=$((SECONDS + BOOT_TIMEOUT))
while kill -0 "$QPID" 2>/dev/null; do
    grep -qF "$BANNER" "$LOG" 2>/dev/null && break
    [ "$SECONDS" -ge "$deadline" ] && break
    sleep 0.1
done

kill "$QPID" 2>/dev/null
wait "$QPID" 2>/dev/null

# -------------------------------------------------------------- assert -----
grep -qF "$BANNER" "$LOG" || fail "banner not found: \"$BANNER\""
if grep -qF "$FOREIGN_BANNER" "$LOG"; then
    fail "reference-kernel banner leaked into the template output"
fi

# -------------------------------------------- 5. princess.toml schema check ---
# Compare the generated descriptor's key names (and value types) against the
# frozen schema in docs/spec/10-contracts.md §4, parsed straight from the doc.
# This needs python3 + tomllib; on a host without them the boot checks above
# still decide pass/fail and only the schema comparison is skipped.
CONTRACT="$ROOT/docs/spec/10-contracts.md"
if command -v python3 >/dev/null 2>&1 && python3 -c 'import tomllib' >/dev/null 2>&1; then
echo "[verify] step 5/5: princess.toml fields vs contracts §4"
python3 - "$CONTRACT" "$PROJ/princess.toml" <<'PY' || fail "princess.toml does not match contract §4"
import sys
import tomllib

contract_path, generated_path = sys.argv[1], sys.argv[2]

text = open(contract_path, encoding="utf-8").read()
if "## 4." not in text or "```toml" not in text:
    print("[verify]   ERROR: could not locate the §4 toml block", file=sys.stderr)
    sys.exit(1)
section = text.split("## 4.", 1)[1]
block = section.split("```toml", 1)[1].split("```", 1)[0]

contract = tomllib.loads(block)
with open(generated_path, "rb") as fh:
    generated = tomllib.load(fh)


def flatten(node, prefix=""):
    out = {}
    for key, value in node.items():
        path = f"{prefix}{key}"
        out[path] = type(value).__name__
        if isinstance(value, dict):
            out.update(flatten(value, path + "."))
    return out


expected = flatten(contract)
actual = flatten(generated)

missing = [k for k in expected if k not in actual]
unknown = [k for k in actual if k not in expected]
type_mismatch = [
    k for k in expected if k in actual and expected[k] != actual[k]
]

for key in sorted(expected):
    if key in missing:
        mark = "MISSING"
    elif key in type_mismatch:
        mark = f"BAD-TYPE(want {expected[key]}, got {actual[key]})"
    else:
        mark = "ok"
    print(f"[verify]   {mark:<10} {key}")

if missing or unknown or type_mismatch:
    for key in missing:
        print(f"[verify]   ERROR missing field: {key}", file=sys.stderr)
    for key in unknown:
        print(f"[verify]   ERROR unknown field: {key}", file=sys.stderr)
    for key in type_mismatch:
        print(f"[verify]   ERROR type mismatch: {key}", file=sys.stderr)
    sys.exit(1)

print(f"[verify]   all {len(expected)} contract field paths present, 0 unknown")
PY
else
    echo "[verify] step 5/5: SKIP princess.toml schema check (python3 + tomllib unavailable)"
fi

echo "[verify] PASS"
echo "[verify]   banner    : $BANNER"
echo "[verify]   serial log: $LOG"
exit 0
