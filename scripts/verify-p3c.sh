#!/usr/bin/env bash
# PrincessIDE P3-C 独立验证脚本
# 用法: bash scripts/verify-p3c.sh
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
source "$SCRIPT_DIR/env.sh"
cd "$ROOT"

log() { printf '[verify-p3c] %s\n' "$*"; }
pass() { printf '[verify-p3c] PASS: %s\n' "$*"; }
fail() { printf '[verify-p3c] FAIL: %s\n' "$*" >&2; exit 1; }

# 1. Tauri 侧 cargo build
log "1/6 cargo build (Tauri shell)"
if (cd apps/desktop && cargo build 2>&1 | tail -10); then
    pass "cargo build"
else
    fail "cargo build"
fi

# 2. 前端构建
log "2/6 pnpm build (frontend)"
if (cd apps/desktop && pnpm build 2>&1 | tail -5); then
    pass "pnpm build"
else
    fail "pnpm build"
fi

# 3. 前端测试
log "3/6 vitest (frontend)"
if (cd apps/desktop && pnpm test 2>&1 | tail -10); then
    pass "vitest"
else
    fail "vitest"
fi

# 4. 契约一致性
log "4/6 contract alignment check"
if node scripts/check-contract.mjs 2>&1 | tail -15; then
    pass "contract aligned"
else
    fail "contract misaligned"
fi

# 5. 引擎测试回归
log "5/6 cargo test --workspace (engine regression)"
if cargo test --workspace 2>&1 | grep "test result:" | tail -15; then
    pass "workspace tests"
else
    fail "workspace tests"
fi

# 6. 冒烟 CI (skip p4)
log "6/6 smoke CI"
if bash scripts/smoke-ci.sh --skip-p4 2>&1 | tail -10; then
    pass "smoke CI"
else
    fail "smoke CI"
fi

echo ""
log "============================================"
log "P3-C VERIFICATION COMPLETE"
log "============================================"
