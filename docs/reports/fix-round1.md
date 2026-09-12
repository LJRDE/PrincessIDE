# 修复轮次 1 —— BUG-001/002（前端）与 BUG-004/005/006（工具链）

> 日期：2026-09-12
> 执行：两路 Mimo 子 Agent（`dsh --profile headless`，provider=xiaomi-token-plan-cn / model=mimo-v2.5-pro）
> 验收：**主 Agent 亲自跑命令**，只采信原始输出与退出码（D14）；下面所有数字都是主 Agent 跑出来的，不是子 Agent 自述。
> 关联：`docs/reports/fixlist.json`、`docs/spec/00-decisions.md` D14/D15/D18/D25/D27

---

## 0. 派发方式与前提

- 修 D27.1 之前，`dsh --profile headless` 实际跑的是 DeepSeek；本轮先修好该通道（headless 专属 settings 文档），
  产物证据：headless 会话 `session-a02f12c9-…` 记录 `xiaomi-token-plan-cn` / `mimo-v2.5-pro`。
- 两路 Agent 目录不重叠：前端 `apps/desktop/src|tests`，脚本 `scripts/doctor.sh` + `INSTALL.md`。
- 纪律：Agent 禁 git、临时文件入 `.scratch/mimo-*/`、验收命令与原始输出写进各自 `REPORT.md`。

---

## 1. BUG-001 + BUG-002（前端）：已修，主 Agent 复核通过

### 改了什么

| 文件 | 改动 |
|---|---|
| `apps/desktop/src/app.ts` | 挂载 `renderActionPanel`；新增 `MountAppOptions.listenFn`；订阅事件流并重渲染；IPC 失败显式显示（`data-testid="ipc-errors"`） |
| `apps/desktop/src/state/liveStream.ts`（新） | 事件桥：注入式 `ListenFn`（照 `lsp/client.ts` 的 D17 模式），把信封经 `parseEventEnvelope` → `applyEvent`；记录 `build.started` / `run.started` 的 `opId` |
| `apps/desktop/src/components/actionPanel.ts` | 增加 BUG-003 的**临时**路径输入框（`TODO(BUG-003)`，未加依赖） |
| `apps/desktop/src/contract/events.ts` | 新增 `EVENT_CHANNEL = 'princess:event'`（Rust 侧 `events.rs` 的镜像，避免字面量散落） |
| `apps/desktop/src/main.ts` | **主 Agent 补**：Tauri 下把真实 `listen` 传入 `mountApp`（子 Agent 以"不在所有权内"为由留下缺口，不补则运行时不订阅 → 验收不可能满足） |
| `apps/desktop/tests/dom/app.test.ts` | 断言 `action-panel` / `project-open-btn` / `build-btn` / `run-btn`；事件桥测试 |

### 主 Agent 复核（原始输出）

```
$ pnpm exec tsc --noEmit -p tsconfig.json          → tsc_exit=0
$ pnpm exec vitest run                             → 8 files / 95 passed  (vitest_exit=0)   [修复前 92]
$ node scripts/check-contract.mjs                  → result: ALIGNED      (contract_exit=0)
$ pnpm build                                       → built in 4.49s       (BUILD_EXIT=0)
$ grep -c renderActionPanel apps/desktop/src/app.ts → > 0
$ grep -rn princess:event apps/desktop/src/ | wc -l → > 0
```

### 未验证（诚实标注）

- **GUI 视觉**：无显示器，`pnpm tauri dev` 下按钮外观与"点 Build 后事件流出现真实 `build.started → build.finished`"未目视确认（D14：GUI 视觉归用户）。
- 端到端事件链路（外壳 `app.emit` → 前端 `listen`）未在真 Tauri 环境跑过。
- 子 Agent 自报的负样本（注释掉挂载后测试必须失败）写在 `.scratch/mimo-frontend/REPORT.md`，主 Agent **未复跑**该负样本。

---

## 2. BUG-004 + BUG-005 + BUG-006（工具链/文档）：已修，主 Agent 复核通过

- **BUG-004**：`INSTALL.md` 新增「GUI / 桌面依赖（Tauri 外壳必需）」小节（apt 命令 + `pkg-config --modversion webkit2gtk-4.1` 验证 + 真实版本）。
- **BUG-005**：`doctor.sh` 新增 GUI 段，逐个 `pkg-config` 检查 5 个模块；缺失时打印 `MISSING` + apt 包名 + 可直接复制的修复命令，并置非零退出码。
- **BUG-006**：版本化命名放宽为「版本化名 **或** 系统名且版本达标」（clangd/clang ≥16、gdb ≥14）。

### 主 Agent 复核（原始输出）

```
$ bash scripts/doctor.sh                            → doctor_exit=0
    clangd-16 (LSP)  ok  Debian clangd version 16.0.6   clangd-16 ver ok major=16 (>=16)
    gdb DAP          ok  GNU gdb 16.3   gdb DAP (-i=dap) ok  built-in DAP interpreter present
    GUI 段 5 个模块全部 ok（webkit2gtk-4.1 2.50.6 / gtk+-3.0 3.24.38 / jsc 2.50.6 / libsoup 3.2.3 / librsvg 2.54.7）
$ PKG_CONFIG_LIBDIR=/tmp/definitely-empty bash scripts/doctor.sh  → 非零，逐个列出缺失包 + apt 修复命令
$ bash -n scripts/doctor.sh                         → syntax_exit=0
```

**BUG-006 的「系统名达标」分支**（= 用户实机 clang 19 / gdb 16.3 场景）在**隔离副本**里验证（`/tmp/pp-doctor-test` 只复制 `scripts/`，不触碰真工具链）：

```
正向：只有 clang/clangd 19.1.7 与 gdb 16.3，无版本化名字
    clangd-16 (LSP)  ok  Debian clangd version 19.1.7 (system, >=16)
    gdb DAP          ok  GNU gdb 16.3 (system, >=14)     gdb DAP (-i=dap) ok
    （该副本无 .toolchain，故 cargo/qemu 等报缺失、整体 exit 1 —— 与 BUG-006 无关）
负样本：移除 stub 后 clangd-16 (LSP) / gdb DAP 均 MISSING，exit 非零
```

**铁律复核**：`princess-gdb` 那条仍做 `-i=dap` 能力探测（`gdb DAP (-i=dap) ok built-in DAP interpreter present`），未退化为只看版本号；
legacy `gdb` 仍被钉在 13.1（`gdb (P0 legacy) ok version matches /GNU gdb .* 13\./`）。

---

## 3. 事故与修复：脚本 Agent 污染了工具链（已完整恢复）

**现象**：脚本 Agent 为构造"工具缺失/版本过低"的负样本，把工具链真文件改名并塞入 stub，**死在半路未恢复**，
导致 `doctor.sh` 误报 `clangd-16 (LSP) MISSING` / `clangd-16 (too old)` / `gdb DAP MISSING`（一度看起来像 BUG-006 没修好）。

**定位方式（可复现）**：`stat -c '%z'`（ctime）显示 6 个 `.hidden` 文件的 ctime 全为 `2026-09-12 20:47:54`；
`file` 显示 `llvm-14/bin/clang`、`clangd` 与 `usr/bin/gdb` 已变成几十字节的 shell stub。

**恢复**（主 Agent 执行，证据留存于 `.scratch/main/agent-side-effects/toolchain-stubs.txt`）：

| 类型 | 文件 |
|---|---|
| 删除 stub | `prefix/usr/bin/gdb`、`prefix/usr/lib/llvm-14/bin/clang`、`prefix/usr/lib/llvm-14/bin/clangd` |
| 改名还原 | `clang-16.hidden`→`clang-16`、`clangd-16.hidden`→`clangd-16`、`gdb.hidden`→`gdb`、`llvm-14/bin/clang.hidden`→`clang`、`llvm-14/bin/clangd.hidden`→`clangd`、`.toolchain/bin/princess-gdb.hidden`→`princess-gdb` |

**恢复后自检**：

```
clangd-16 16.0.6 | clang-16 16.0.6 | clangd 14.0.6 | clang 14.0.6 | gdb 13.1 | princess-gdb 16.3
find .toolchain -name '*.hidden' → 空
bash scripts/doctor.sh → exit 0
```

**教训（建议追加为决策）**：负样本**不得**通过改名/替换工具链真文件来构造；
应使用 `PKG_CONFIG_LIBDIR` / 临时 `PATH` / 隔离副本（本轮已验证这三种非破坏手法够用）。

---

## 4. 结论

| 项 | 状态 |
|---|---|
| BUG-001 挂载 Open Project / Build / Run | ✅ 修复（GUI 视觉待用户确认） |
| BUG-001 深层缺口：`princess:event` 无订阅 | ✅ 修复（含 `main.ts` 真实 `listen` 接线） |
| BUG-002 补测试 + 负样本 | ✅ 95 tests（+3），负样本见子 Agent 报告（未复跑） |
| BUG-004 GUI 依赖文档 | ✅ |
| BUG-005 doctor GUI 检查 + 负样本 | ✅ |
| BUG-006 版本命名放宽（含系统名分支） | ✅ |
| 工具链污染 | ✅ 已恢复并自检 |
| BUG-003 / BUG-007 / BUG-008 / IMPROVE-001 | 未处理（BUG-003 待裁决；BUG-007 疑为截图误读） |
