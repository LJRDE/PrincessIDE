#!/usr/bin/env bash
# PrincessIDE — 便携包（tar.gz）构建器。
#
# 产出：<repo>/dist-packages/PrincessIDE-<version>-x86_64-linux.tar.gz
#
# 形态决定（用户 2026-09-12 裁决）：**裸便携包 tar.gz，硬预算 < 15 MiB**。
# 为什么是这个形态：
#   * 二进制 `ldd` 实测依赖 169 个**系统**库（webkit2gtk-4.1 / gtk-3 / soup-3 /
#     javascriptcoregtk-4.1 …）—— 打包它们会是几百 MB，所以本包**只装应用本体**，
#     系统库由 `run.sh` 自检并给出 apt 修复命令。
#   * 前端资源由 Tauri 在**编译期内嵌**进二进制（tauri.conf.json `frontendDist`），
#     因此包内不需要 dist/（`vite.config.ts` 也关掉了 sourcemap：它是随二进制发货的
#     2 MB 重量）。
#
# 预算写进脚本而不是靠人记：超过 PRINCESSIDE_PORTABLE_BUDGET_MB（默认 15）即**非零退出**。
#
# 用法：bash scripts/package-portable.sh [--no-build]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=env.sh
source "$SCRIPT_DIR/env.sh" >/dev/null

TAURI_DIR="$ROOT/apps/desktop/src-tauri"
FRONTEND_DIR="$ROOT/apps/desktop"
OUT_DIR="$ROOT/dist-packages"
BUDGET_MB="${PRINCESSIDE_PORTABLE_BUDGET_MB:-15}"
BUDGET_BYTES=$((BUDGET_MB * 1024 * 1024))
DO_BUILD=1
[ "${1:-}" = "--no-build" ] && DO_BUILD=0

log()  { printf '[portable] %s\n' "$*"; }
fail() { printf '[portable] ❌ %s\n' "$*" >&2; exit 1; }

VERSION="$(grep -m1 '^version' "$TAURI_DIR/Cargo.toml" | sed -E 's/.*"(.*)".*/\1/')"
[ -n "$VERSION" ] || fail "无法从 $TAURI_DIR/Cargo.toml 读取 version"

# ---------------------------------------------------------------------------
# 1) 构建前端（会被 Tauri 内嵌进二进制）
# ---------------------------------------------------------------------------
if [ "$DO_BUILD" = 1 ]; then
    log "构建前端（pnpm -C apps/desktop build）"
    ( cd "$FRONTEND_DIR" && pnpm build )
fi
[ -f "$FRONTEND_DIR/dist/index.html" ] || fail "前端产物缺失：$FRONTEND_DIR/dist/index.html"

# ---------------------------------------------------------------------------
# 2) release 构建（profile 已设 strip = true；再用 readelf 复核确实剥了）
# ---------------------------------------------------------------------------
if [ "$DO_BUILD" = 1 ]; then
    log "构建 release（CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}）"
    ( cd "$TAURI_DIR" && CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}" cargo build --release )
fi
BIN="$TAURI_DIR/target/release/princesside-desktop"
[ -x "$BIN" ] || fail "release 二进制缺失：$BIN"

if readelf -S "$BIN" 2>/dev/null | grep -q '\.symtab'; then
    log "警告：二进制仍带 .symtab（profile 的 strip 未生效），就地剥离"
    strip --strip-all "$BIN"
fi
BIN_BYTES="$(stat -c %s "$BIN")"
log "二进制：$((BIN_BYTES / 1024 / 1024)) MiB ($BIN_BYTES bytes)"

# ---------------------------------------------------------------------------
# 3) 组装 staging（只放应用本体 + 自检启动器 + 说明 + 图标 + desktop 项）
# ---------------------------------------------------------------------------
STAGE_ROOT="$(mktemp -d)"
trap 'rm -rf "$STAGE_ROOT"' EXIT
STAGE="$STAGE_ROOT/PrincessIDE-$VERSION"
mkdir -p "$STAGE/icons"

install -m 0755 "$BIN" "$STAGE/princesside-desktop"
for icon in 32x32.png 128x128.png 128x128@2x.png icon.ico; do
    [ -f "$TAURI_DIR/icons/$icon" ] && install -m 0644 "$TAURI_DIR/icons/$icon" "$STAGE/icons/$icon"
done

# 启动器：先自检系统依赖，缺什么就报什么（显式失败优于"点了没反应"）
cat > "$STAGE/run.sh" <<'LAUNCHER'
#!/usr/bin/env bash
# PrincessIDE 便携包启动器：先自检系统依赖，再启动。
# 本包**不**附带 WebKitGTK/GTK 等系统库（那会有几百 MB），只做检查与提示。
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

declare -A MODULE_PKG=(
    [webkit2gtk-4.1]=libwebkit2gtk-4.1-dev
    [gtk+-3.0]=libgtk-3-dev
    [javascriptcoregtk-4.1]=libjavascriptcoregtk-4.1-dev
    [libsoup-3.0]=libsoup-3.0-dev
    [librsvg-2.0]=librsvg2-dev
)
missing=()
if ! command -v pkg-config >/dev/null 2>&1; then
    missing+=(pkg-config)
else
    for mod in "${!MODULE_PKG[@]}"; do
        pkg-config --exists "$mod" 2>/dev/null || missing+=("${MODULE_PKG[$mod]}")
    done
fi
if [ "${#missing[@]}" -gt 0 ]; then
    echo "PrincessIDE 缺少运行所需的系统库：" >&2
    printf '  - %s\n' "${missing[@]}" >&2
    echo >&2
    echo "修复（Debian/Ubuntu）：" >&2
    echo "  sudo apt-get update -y && sudo apt-get install -y ${missing[*]}" >&2
    exit 1
fi

exec "$HERE/princesside-desktop" "$@"
LAUNCHER
chmod 0755 "$STAGE/run.sh"

cat > "$STAGE/princesside.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=PrincessIDE
Comment=x86_64 kernel development IDE
Exec=princesside-desktop
Icon=princesside
Categories=Development;IDE;
Terminal=false
DESKTOP

cat > "$STAGE/README.txt" <<README
PrincessIDE $VERSION — 便携包 (x86_64-linux)
===========================================

怎么跑
------
  1) ./run.sh            # 先自检系统依赖，缺什么会直接告诉你 apt 命令
  2) 或直接 ./princesside-desktop

这个包里有什么
--------------
  princesside-desktop   应用本体（release、已 strip；前端资源已内嵌进二进制）
  run.sh                启动器（系统依赖自检 + 修复命令提示）
  princesside.desktop   桌面项（可选，拷到 ~/.local/share/applications/）
  icons/                图标

不包含什么（有意为之）
----------------------
  WebKitGTK 4.1 / GTK3 / libsoup3 / librsvg 等系统库 —— 实测二进制依赖 169 个
  系统库，打包它们会让体积到几百 MB。请用发行版包管理器安装：
    sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \\
      libjavascriptcoregtk-4.1-dev libsoup-3.0-dev librsvg2-dev
  内核开发的工具链（QEMU/nasm/clangd-16/bear/gdb≥14、grub-mkrescue 等）同样
  不在此包内：请跑仓库里的 scripts/bootstrap-toolchain.sh 与 scripts/doctor.sh。
README

# ---------------------------------------------------------------------------
# 4) 打包 + **预算门**（超限即失败，不靠人记）
# ---------------------------------------------------------------------------
mkdir -p "$OUT_DIR"
TARBALL="$OUT_DIR/PrincessIDE-$VERSION-x86_64-linux.tar.gz"
tar -C "$STAGE_ROOT" -czf "$TARBALL" "PrincessIDE-$VERSION"
TAR_BYTES="$(stat -c %s "$TARBALL")"
log "产物：$TARBALL"
log "体积：$TAR_BYTES bytes = $(awk -v b="$TAR_BYTES" 'BEGIN{printf "%.2f", b/1024/1024}') MiB（预算 ${BUDGET_MB} MiB）"
echo
log "包内清单："
tar -tzf "$TARBALL" | sed 's/^/    /'
echo
if [ "$TAR_BYTES" -ge "$BUDGET_BYTES" ]; then
    fail "超过预算：$TAR_BYTES >= $BUDGET_BYTES bytes（${BUDGET_MB} MiB）。可用的压缩手段：LTO/opt-level=z（改 src-tauri 的 [profile.release]）或 xz 替代 gzip。"
fi
log "✅ 在预算内（$(awk -v b="$TAR_BYTES" -v m="$BUDGET_BYTES" 'BEGIN{printf "%.1f%%", b/m*100}') 的 ${BUDGET_MB} MiB）"
