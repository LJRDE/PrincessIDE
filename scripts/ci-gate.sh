#!/usr/bin/env bash
# PrincessIDE P-A — CI gate script.
#
# Runs all checks that must pass before any merge.  This is the single entry
# point that covers frontend types, tests, contract alignment, production build,
# engine workspace tests, and the Tauri shell tests.
#
# Usage:
#   bash scripts/ci-gate.sh          # full gate (all 6 steps)
#   bash scripts/ci-gate.sh --fast   # skip the two cargo steps (frontend-only iteration)
#
# Exit status: 0 when ALL checks pass, non-zero on first failure.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ---------------------------------------------------------------------------
# Options
# ---------------------------------------------------------------------------
FAST=0
while [ $# -gt 0 ]; do
    case "$1" in
        --fast) FAST=1; shift ;;
        -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
        *) echo "ci-gate: unknown argument: $1" >&2; exit 2 ;;
    esac
done

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
log()   { printf '\n[ci-gate] ═══ %s ═══\n' "$*"; }
pass()  { printf '[ci-gate] ✅ PASS: %s\n' "$*"; }
fail()  { printf '[ci-gate] ❌ FAIL: %s\n' "$*" >&2; exit 1; }

PASS_COUNT=0
FAIL_COUNT=0

record_pass() {
    PASS_COUNT=$((PASS_COUNT + 1))
    pass "$1"
}

record_fail() {
    FAIL_COUNT=$((FAIL_COUNT + 1))
    fail "$1"
}

# ---------------------------------------------------------------------------
# Step 1: Frontend type-check
# ---------------------------------------------------------------------------
log "1/6  Frontend type-check (tsc --noEmit)"
if pnpm -C "$ROOT/apps/desktop" exec tsc --noEmit -p tsconfig.json 2>&1; then
    record_pass "frontend type-check"
else
    record_fail "frontend type-check"
fi

# ---------------------------------------------------------------------------
# Step 2: Frontend tests
# ---------------------------------------------------------------------------
log "2/6  Frontend tests (vitest run)"
if pnpm -C "$ROOT/apps/desktop" exec vitest run 2>&1; then
    record_pass "frontend tests"
else
    record_fail "frontend tests"
fi

# ---------------------------------------------------------------------------
# Step 3: Contract alignment (three-way check)
# ---------------------------------------------------------------------------
log "3/6  Contract alignment (check-contract.mjs)"
if node "$ROOT/apps/desktop/scripts/check-contract.mjs" 2>&1; then
    record_pass "contract alignment"
else
    record_fail "contract alignment"
fi

# ---------------------------------------------------------------------------
# Step 4: Frontend production build
# ---------------------------------------------------------------------------
log "4/6  Frontend production build (vite build)"
if pnpm -C "$ROOT/apps/desktop" build 2>&1; then
    record_pass "frontend production build"
else
    record_fail "frontend production build"
fi

# ---------------------------------------------------------------------------
# Step 5: Engine workspace tests (cargo test --workspace)
# ---------------------------------------------------------------------------
if [ "$FAST" = "0" ]; then
    log "5/6  Engine workspace tests (cargo test --workspace)"
    # Source env.sh to get the toolchain on PATH (cargo, rustc, etc.)
    # shellcheck source=env.sh
    source "$SCRIPT_DIR/env.sh"
    if CARGO_BUILD_JOBS=2 cargo test --workspace 2>&1; then
        record_pass "engine workspace tests"
    else
        record_fail "engine workspace tests"
    fi
else
    log "5/6  Engine workspace tests — SKIPPED (--fast)"
fi

# ---------------------------------------------------------------------------
# Step 6: Tauri shell tests (src-tauri)
# ---------------------------------------------------------------------------
if [ "$FAST" = "0" ]; then
    log "6/6  Tauri shell tests (src-tauri cargo test)"
    # shellcheck source=env.sh
    source "$SCRIPT_DIR/env.sh"
    if (cd "$ROOT/apps/desktop/src-tauri" && CARGO_BUILD_JOBS=2 cargo test) 2>&1; then
        record_pass "Tauri shell tests"
    else
        record_fail "Tauri shell tests"
    fi
else
    log "6/6  Tauri shell tests — SKIPPED (--fast)"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
log "Summary"
printf '[ci-gate] pass=%d  fail=%d\n' "$PASS_COUNT" "$FAIL_COUNT"
if [ "$FAST" = "1" ]; then
    printf '[ci-gate] (mode: --fast, cargo steps skipped)\n'
fi

if [ "$FAIL_COUNT" -gt 0 ]; then
    printf '[ci-gate] ❌ GATE FAILED (%d failure(s))\n' "$FAIL_COUNT" >&2
    exit 1
fi

printf '[ci-gate] ✅ ALL CHECKS PASSED\n'
exit 0
