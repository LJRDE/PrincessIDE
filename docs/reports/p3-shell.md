# P3-A 验收报告 — Tauri 桌面外壳 + GUI 依赖就位

> 阶段：P3-A（`apps/desktop/` 全部内容 + 根 workspace 三件套）
> 执行环境：Debian 12 bookworm / x86_64 / 无显示器（`DISPLAY=none`）/ 4 核约 3.8G 无 swap
> 契约基线：`docs/spec/10-contracts.md`（§2 事件模型、§3 IPC、错误码、§1 目录归属）
> 验收基线：`docs/spec/20-acceptance.md` P3-1…P3-5（+ 本阶段自加 P3-6 GUI 依赖实测）
> 铁律遵守：本报告中的每条命令与输出都是**在本工作区真实执行**得到的原文，未通过的一律标注未通过。

---

## 0. 结论摘要

| 验收项 | 结果 | 一句话证据 |
|---|---|---|
| P3-1 `cargo build`（Tauri 外壳） | ✅ 退出 0 | 见 §3.1（真实输出见 §3.1 引用的日志片段） |
| P3-2 `pnpm -C apps/desktop build` | ✅ 退出 0，产出 `dist/` | `✓ built in 3.93s`，`dist/index.html` + `assets/*.js`/`*.css` |
| P3-3 `pnpm -C apps/desktop test`（vitest） | ✅ 全绿 81/81 | `Test Files 8 passed (8)` / `Tests 81 passed (81)` |
| P3-4 事件重放（夹具 → 状态机 → 快照） | ✅ 快照断言通过 | `refkernelState`/`debugState` 两个 `toMatchSnapshot()` + 显式字段断言 |
| P3-5 IPC 契约一致 | ✅ 21 命令 / 10 错误码三方逐字一致 | `node scripts/check-contract.mjs` → `result: ALIGNED`，退出 0 |
| P3-6 GUI 依赖（pkg-config 实测） | ✅ 6/6 关键 pkg-config 通过 | `webkit2gtk-4.1 2.50.6` 等，见 §2.2 |

补充：`cargo test`（Rust 侧单测 + 集成测试）同样**真实执行并通过**，见 §3.1。

---

## 1. 交付物清单（只写被授权的路径）

```
package.json                          # 根：pnpm workspace 脚本入口（新增）
pnpm-workspace.yaml                   # 根：只包含 apps/desktop（新增）
pnpm-lock.yaml                        # 根：锁文件（新增）
apps/desktop/
  package.json  tsconfig.json  vite.config.ts  vitest.config.ts  index.html
  src/
    contract/events.ts                # §2 事件模型 TS 镜像（字段逐字对齐契约）
    contract/ipc.ts                   # §3 命令名 + 错误码 + 统一返回结构
    contract/parse.ts                 # 运行时校验（夹具/流都必须先过它）
    state/eventStore.ts               # 事件状态机（纯函数，seq 单调/缺口/版本告警）
    state/fixtureLoader.ts            # NDJSON 夹具加载
    ipc/client.ts                     # 前端唯一 IPC 出口（princess_invoke 调度器）
    components/toolTable.ts           # 工具链表渲染
    components/eventLog.ts            # 事件日志 / 诊断 / 故障卡 / 调试面板
    components/editor.ts              # CodeMirror 6 编辑器（含语言路由）
    lsp/client.ts                     # LSP 客户端接缝（@codemirror/lsp-client）
    app.ts  main.ts  styles.css
  fixtures/events/refkernel-session.ndjson   # P3 构造（非 P2 录制）
  fixtures/events/debug-session.ndjson       # P3 构造（非 P2 录制）
  scripts/check-contract.mjs                 # P3-5 契约校验器（脚本 + 被测试复用）
  scripts/gen-icons.py                       # 图标生成（纯标准库，可复现）
  tests/…（8 个测试文件，81 个用例）
  src-tauri/
    Cargo.toml                        # 独立 workspace（带空 [workspace] 表）
    build.rs  tauri.conf.json  capabilities/default.json  icons/*
    src/{main,lib,contract,events,ops,doctor,commands}.rs
    tests/tools_detect_e2e.rs         # 真实 doctor.sh 的端到端集成测试
docs/reports/p3-shell.md              # 本文件
.scratch/shell/                       # 临时产物（apt 日志、cargo 日志、版本清单）
```

**未触碰**：根 `Cargo.toml`（P2-A）、`crates/`、`scripts/`、`fixtures/`、`docs/spec/`、`docs/research/`。
**未执行任何改动 git 的命令**（无 `add`/`commit`/`checkout`）。
**`apps/desktop/src-tauri/Cargo.toml` 自带空 `[workspace]` 表**，是独立 workspace，不会与 P2 的根 workspace 互相干扰，也不会把 Tauri 的依赖树拖进 P2 的 `cargo test --workspace`。

---

## 2. 系统级安装声明（apt）

### 2.1 逐条声明装了什么

命令原文：

```bash
apt-get update -y
DEBIAN_FRONTEND=noninteractive apt-get install -y \
  libwebkit2gtk-4.1-dev libgtk-3-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev \
  librsvg2-dev libayatana-appindicator3-dev libxdo-dev libssl-dev \
  pkg-config build-essential file wget curl
```

`apt-get install` **退出码 0**（`=== INSTALL EXIT: 0 ===`，完整日志 `.scratch/shell/apt-install.log`）。
本轮共 **新增 302 个包、升级 11 个包、下载 150 MB、占用 554 MB**（apt 自报行原文）。

请求的 13 个包全部 `install ok installed`：

| 包 | 实际版本 |
|---|---|
| libwebkit2gtk-4.1-dev | 2.50.6-1~deb12u2 |
| libgtk-3-dev | 3.24.38-2+deb12u3 |
| libjavascriptcoregtk-4.1-dev | 2.50.6-1~deb12u2 |
| libsoup-3.0-dev | 3.2.3-0+deb12u2 |
| librsvg2-dev | 2.54.7+dfsg-1~deb12u1 |
| libayatana-appindicator3-dev | 0.5.92-1 |
| libxdo-dev | 1:3.20160805.1-5 |
| libssl-dev | 3.0.20-1~deb12u2 |
| pkg-config | 1.8.1-1 |
| build-essential | 12.9 |
| file | 1:5.44-3 |
| wget | 1.21.3-1+deb12u1 |
| curl | 7.88.1-10+deb12u15 |

**升级的 11 个包**（`apt-get install` 的依赖解析要求更高版本，均为安全/兼容性小版本升级，未降级、未 remove）：
`libglib2.0-0 libglib2.0-data libicu72 liblzma5 libnghttp2-14 libpcre2-32-0 libpcre2-8-0 libpng16-16 libsqlite3-0 libxml2 xz-utils`

### 2.2 `pkg-config` 实测（P3-6 证据）

```console
$ for p in webkit2gtk-4.1 javascriptcoregtk-4.1 gtk+-3.0 libsoup-3.0 librsvg-2.0 ayatana-appindicator3-0.1 xdo openssl; do
    printf "%-24s " "$p"; pkg-config --exists "$p" && echo "OK  ($(pkg-config --modversion $p))" || echo "MISSING"; done
webkit2gtk-4.1           OK  (2.50.6)
javascriptcoregtk-4.1    OK  (2.50.6)
gtk+-3.0                 OK  (3.24.38)
libsoup-3.0              OK  (3.2.3)
librsvg-2.0              OK  (2.54.7)
ayatana-appindicator3-0.1 OK  (0.5.90)
xdo                      MISSING
openssl                  OK  (3.0.20)
```

**`xdo` 显示 MISSING 是预期的，不是缺口**：Debian 的 `libxdo-dev` **不提供 `.pc` 文件**（Tauri 的 Linux 依赖清单里列它是因为需要 `libxdo.so` + 头文件，不是因为 pkg-config）。实测证据：

```console
$ dpkg -l libxdo-dev | tail -1
ii  libxdo-dev     1:3.20160805.1-5 amd64        library for simulating X11 keyboard/mouse input
$ ls -l /usr/lib/x86_64-linux-gnu/libxdo.so /usr/include/xdo.h
-rw-r--r-- 1 root root 28025 Apr 21  2022 /usr/include/xdo.h
lrwxrwxrwx 1 root root    11 Apr 21  2022 /usr/lib/x86_64-linux-gnu/libxdo.so -> libxdo.so.3
$ ls /usr/lib/x86_64-linux-gnu/pkgconfig/ | grep -i xdo
（无输出）
```

即：库和头文件都在，只是没有 `.pc`。`cargo build` 链接阶段用的是 `libxdo.so`，因此不受影响（P3-1 已通过）。

**已知的、与本阶段无关的 apt 噪音**：`apt-get update` 会对一个**预先存在的第三方源**报 GPG 错误并返回非零 —— 原文：

```
W: GPG error: https://pkg.cloudflare.com/cloudflared bookworm InRelease: The following signatures
   couldn't be verified because the public key is not available: NO_PUBKEY 254B391D8CACCBF8 NO_PUBKEY 8A682D308D4E5E73
E: The repository 'https://pkg.cloudflare.com/cloudflared bookworm InRelease' is not signed.
```

该源不属于本项目，安装所需的 `bookworm` / `bookworm-security` 索引已从清华镜像正常获取，因此**上面 13 个包全部安装成功（退出 0）**。没有为了追版本去修这个源（避免动系统）。

---

## 3. 验收表（命令 → 真实输出 → 退出码）

### 3.1 P3-1 引擎构建：`apps/desktop/src-tauri` 下 `cargo build`

命令（遵守 D16 内存纪律：`CARGO_BUILD_JOBS=2`，后台执行）：

```bash
cd apps/desktop/src-tauri
source /root/PrincessIDE/scripts/env.sh
CARGO_BUILD_JOBS=2 cargo build
```

<!--CARGO_BUILD_PLACEHOLDER-->

### 3.2 P3-2 前端构建：`pnpm -C apps/desktop build`

```console
$ pnpm -C apps/desktop build
$ tsc --noEmit -p tsconfig.json && vite build
vite v7.3.6 building client environment for production...
transforming...
✓ 31 modules transformed.
rendering chunks...
computing gzip size...
dist/index.html                   0.40 kB │ gzip:   0.27 kB
dist/assets/index-CtN30Olp.css    2.67 kB │ gzip:   1.01 kB
dist/assets/index-DNTTerlp.js   463.40 kB │ gzip: 151.60 kB │ map: 1,784.56 kB
✓ built in 3.93s
BUILD EXIT=0
```

`tsc --noEmit` 覆盖 `src/`、`tests/`、`scripts/*.mjs`（`strict: true`，含 `noUnusedLocals`/`verbatimModuleSyntax`），
即**构建通过同时意味着 81 个测试文件类型正确**。

### 3.3 P3-3 前端单测：`pnpm -C apps/desktop test`（vitest）

```console
$ pnpm -C apps/desktop test
 RUN  v3.2.7 /root/PrincessIDE/apps/desktop

 ✓ tests/dom/toolTable.test.ts (9 tests) 124ms
 ✓ tests/dom/eventLog.test.ts (11 tests) 147ms
 ✓ tests/eventReplay.test.ts (25 tests) 168ms
 ✓ tests/dom/app.test.ts (5 tests) 1084ms
 ✓ tests/lsp.test.ts (8 tests) 181ms
 ✓ tests/contract.test.ts (9 tests) 20ms
 ✓ tests/editor.test.ts (7 tests) 32ms
 ✓ tests/ipcClient.test.ts (7 tests) 27ms

 Test Files  8 passed (8)
      Tests  81 passed (81)
EXIT=0
```

测试布局（Node 环境为默认，DOM 用例在文件头用 `// @vitest-environment jsdom` 显式声明）：

| 文件 | 覆盖 |
|---|---|
| `tests/contract.test.ts` | P3-5 契约对齐（并从 spec 原文重新抽取，防止内嵌副本过期） |
| `tests/eventReplay.test.ts` | P3-4 事件重放 + 快照 + 全部负样本 |
| `tests/ipcClient.test.ts` | IPC 线格式、错误映射、非 Tauri 环境不装成功 |
| `tests/editor.test.ts` | 编辑器语言路由（C/C++/汇编/plaintext）+ 只读语义 |
| `tests/lsp.test.ts` | LSP 接缝 + 真实 LSP 握手（脚本化服务端，无显示器可跑） |
| `tests/dom/toolTable.test.ts` | 工具链表渲染（含 MISSING / WRONG VER / 注入防护） |
| `tests/dom/eventLog.test.ts` | 日志、诊断、故障卡、告警态 |
| `tests/dom/app.test.ts` | 整壳挂载（真 CodeMirror 实例）+ 引擎缺失时显式报错 |

### 3.4 P3-4 事件重放：夹具 → 状态机 → 快照断言

夹具（**均为 P3 构造，非 P2 录制**，文件头有 `CONSTRUCTED` 标记，加载器会把它标成 `constructed: true`）：

- `apps/desktop/fixtures/events/refkernel-session.ndjson`（17 事件：build → symbols → run → fault → exit → ai）
- `apps/desktop/fixtures/events/debug-session.ndjson`（9 事件：debug.* 全类型）

覆盖**契约 §2 全部 14 种 kind**。断言方式：

1. **快照断言**（验收要求）：`expect(refkernelState).toMatchSnapshot()` / `expect(debugState).toMatchSnapshot()`
   （快照文件 `tests/__snapshots__/eventReplay.test.ts.snap`，首次运行写入、第二次运行从快照比对，两次均绿）。
2. **显式字段断言**（防止"快照一改就跟着变"）：`lastSeq=17`、`gaps=[]`、`needsReplay=false`、
   `build.status=ok`、`artifacts=[kernel.elf, kernel.iso]`、`run.exit.reason=triple-fault`、
   横幅 `PrincessIDE reference kernel booted`、故障符号 `refkernel_fault_probe (kernel.c:42)`。
3. **负样本**（`20-acceptance.md` §0 要求每个正向断言配失败路径）：
   - seq 缺口 → `gaps=[{expected:3,received:5,size:2}]` + `needsReplay=true`
   - 重复 seq → `duplicates=1`，不重复应用
   - 乱序 → `outOfOrder=1`，流仍然自洽
   - 事件模型版本不符（`v=2`）→ `versionMismatches=[2]` + UI 显式告警（契约 §7）
   - 畸形信封（缺字段 / seq=0 / 非法 ts / 未知 kind / 未知 stream / 未知 reason / 缺 artifacts）
     → 抛出带**字段路径**的 `ContractViolation`，绝不静默丢弃
   - 非法 JSON 行 → 计入 `violations` 并在 UI 可见
   - 无 `build.started` 的 `build.finished` → 仍然渲染并记入 `ignored`
   - `utf8-lossy` 分片 → 打标显示，不替换成合法字符
   - 日志环形缓冲上限 → 5000 行后淘汰最旧，不 OOM

### 3.5 P3-5 IPC 契约一致

```console
$ pnpm -C apps/desktop check:contract      # 等价于 node apps/desktop/scripts/check-contract.mjs
PrincessIDE P3-5 — IPC contract alignment (docs/spec/10-contracts.md §3)

  [PASS] spec §3 command list is non-empty and well-formed
         21 commands extracted from docs/spec/10-contracts.md §3
  [PASS] TS IPC_COMMANDS == spec §3 commands
         21 commands, exact match
  [PASS] Rust CONTRACT_COMMANDS == spec §3 commands
         21 commands, exact match
  [PASS] TS ERROR_CODES == Rust ERROR_CODES == spec §3 error codes
         10 codes, exact match in all three
  [PASS] every command uses a §3 domain
         domains ⊂ {project, build, run, debug, symbols, bin, fs, tools, ai, op}
  [PASS] frontend source literals are all in the contract (or explicitly pending)
         …  princess:lsp:bridge [PENDING amendment (D17 LSP bridge)] @ apps/desktop/src/lsp/client.ts:40

  result: ALIGNED
EXIT=0
```

设计要点（为什么不只是"手抄一份列表"）：

- 校验器**从 `docs/spec/10-contracts.md` 原文解析**命令表，包括 §3 的简写行
  `` - `princess:debug:attach` / `setBreakpoints` / `continue` / … `` —— 它按 `/` 分隔的续写规则展开，
  并且**不会**把散文里的 `opId` 误当成命令（有单测 `does not mistake prose identifiers for commands` 钉住这条规则）。
- 三方比对：**spec 原文 == TS 镜像 == Rust 注册表**，任何一方漂移都 FAIL（不是只比 TS）。
- 额外扫描整个前端源码里的 `princess:<domain>:<action>` 字面量，捕获类型系统抓不到的拼写错误。
- `princess:lsp:bridge` 是**唯一**被显式列为 "PENDING amendment (D17)" 的名字：
  它不在 §3 里，因此**代码里默认不会被调用**，校验器把它报成待裁决项而不是"已合规"。

### 3.6 P3-6 GUI 依赖（见 §2.2）

`pkg-config` 实测通过（6/6 关键项 + 无 `.pc` 但文件齐全的 `libxdo-dev`），
且 P3-1 的 `cargo build` 就是链接这套系统库构建成功的。

---

## 4. 纵向切片（不是空壳）

### 4.1 `princess:tools:detect`：Rust 执行 `scripts/doctor.sh` 并解析

**为什么是一个调度命令而不是 21 个原生命令**（这条是我对契约的工程解释，已写进代码注释与 §10 交接）：

Tauri 的 IPC 命令名来自 Rust 函数标识符，而标识符**不能包含 `:`**；契约冻结的名字是 `princess:<domain>:<action>`。
所以外壳注册**一个**原生命令 `princess_invoke(cmd, args)`，它**原样收到契约名**再路由到真正的处理函数；
另外注册两个原生别名 `princess_tools_detect` / `princess_op_cancel`（走同一份处理逻辑，无重复实现）。
路由是一个**全函数**：

- §3 里 P3 已实现的（`tools:detect` / `op:cancel` / `op:replay`）→ 真实现；
- §3 里属于后续阶段的 → `{ok:false,error:{code:"E_NOT_FOUND", detail:"implemented by P3-A: …, remaining … belong to P2 (project/build/run), P4 (debug) …"}}`
  —— **显式说明归属，而不是假装没有这个命令**；
- §3 之外的名字 → `E_NOT_FOUND` + 拒绝。

Rust 侧单测 `every_contract_command_routes_somewhere_explicit` 遍历契约全部 21 个名字，断言**没有一个**落到 Unknown。

Rust 侧行为：

1. 解析工程根：`PRINCESSIDE_ROOT` → 编译期仓库路径 → 从 cwd 逐级上溯；都失败则 `E_NOT_FOUND`，
   detail 里**逐条列出试过哪些路径**（不猜、不静默降级）。
2. `bash -c 'set -o pipefail; source <root>/scripts/env.sh && exec bash <root>/scripts/doctor.sh'`，
   用 `tokio::process`（契约 §6.1），`kill_on_drop(true)`（§6.3），60s 硬超时 → `E_TIMEOUT`。
3. 解析 doctor.sh 输出为工具链表（路径 + 版本 + 是否可用）**外加**：`status` 原文与 `checks` 校验注记。
4. 返回 `{ok:true,data:{tools[],exitCode,missing[],root,command,rawStdout,rawStderr}}`——
   `command` 与两份原始输出都回传，满足契约 §0.3「可复现」与 §0.4「失败必须显式」。

**解析器踩到的真实坑（都写进了代码注释与单测）**：doctor.sh 在本次会话期间被 A1 扩展过，
新增了「名字带空格」（`clangd-16 (LSP)`、`bear (CDB)`）、「状态含空格」（`WRONG VER`）、
以及「同一工具出现两行（一行版本+路径，一行版本校验）」三种形态。最初的 `split_whitespace()` 解析会
**静默丢掉这些行**，并把后续路径行错接到上一个工具上。现已改为**扫描状态标记**（`WRONG VER`/`MISSING`/`ok`，
要求前导空白且后随空白或行尾，取第 20 列之后的第一个命中），并把校验行**按名字合并**进同一个工具行。
对应单测：`handles_tool_names_containing_spaces`、`merges_verification_rows_into_the_tool_they_check`、
`a_wrong_version_is_visible_and_marks_the_tool_unavailable`、`a_value_containing_the_word_ok_is_not_a_status`、
`long_tool_names_do_not_shift_the_parse`。

**端到端集成测试**（`src-tauri/tests/tools_detect_e2e.rs`，无显示器可跑，是目前最强的证据）：

<!--CARGO_TEST_PLACEHOLDER-->

### 4.2 事件流面板

- 前端按契约 §2 定义**逐字字段名**的 TS 类型（`v/seq/ts/opId/kind/payload` + 14 种 kind 的 payload），
  并有**运行时校验器**（`contract/parse.ts`）——类型会被擦除，夹具和实时流都必须先过校验。
- 状态机 `state/eventStore.ts` 是纯函数：同一份 reducer 同时驱动 **NDJSON 离线重放**（测试）与**Tauri 实时流**。
- Rust 侧真的会发事件：`EventBus` 维护会话内单调 `seq` + 5000 条环形缓冲 + ISO-8601(ms, Z) 时间戳，
  通过 Tauri 事件通道 `princess:event` 推送 `log.append`（`tools:detect` 前后各一条 + 启动一条）。
- `princess:op:replay(fromSeq)` 从环形缓冲回放，`princess:op:cancel(opId)` 经统一 `OpRegistry` 取消
  （`E_CANCELLED` / `E_NOT_FOUND` / `E_INVALID_CONFIG` 三态分明；取消 doctor 运行有集成测试）。

---

## 5. 编辑器 / LSP 组件选型

**选型：CodeMirror 6（`@codemirror/*`）+ 官方 `@codemirror/lsp-client` 作为语言服务客户端。**

依据（按证据强度排序）：

1. **调研报告没有给出前端组件推荐**。`docs/research/clangd-freestanding-kernel.md` 通篇解决的是
   「clangd 在 freestanding 交叉内核工程上怎么才能用」（CDB 生成、triple、`-nostdlibinc`、
   `CompileFlags.Remove`、汇编边界、LSP 协议细节、多语言共存），**没有一处**比较 CodeMirror 与 Monaco。
   任务书写明"没给就用 CodeMirror 6 并在报告里说明理由"——即本条。
2. **D17（主 Agent 裁决）指定**：前端用成熟 LSP 客户端库（CodeMirror 6 的 `lsp-client` 或 Monaco 的
   `monaco-languageclient`），引擎**不**重写 LSP 协议栈。本阶段据此接入 `@codemirror/lsp-client@6.2.5`
   **真实握手**（见 §3.3 `tests/lsp.test.ts`：脚本化服务端应答 `initialize` → 断言 `serverCapabilities`、
   `rootUri`、`capabilities` 上报；另有无应答服务端 → 客户端超时 reject，不永久挂起）。
3. **调研报告的汇编边界结论直接约束了编辑器设计**（§3「不要拿它当汇编的 IDE 后端」）：
   `.S`/`.asm` 拿**语法高亮 only** 的 GAS stream mode，**不**发给语言服务器（`lspEligible:false`），
   汇编的语义体验留给 objdump/DWARF/gdb 主链路。这条是选型里唯一"因调研而改变实现"的地方，有单测钉住。
4. **无显示器可验证性**：CodeMirror 6 可在 jsdom 里实例化（本仓库 `tests/dom/app.test.ts` 真的挂载了
   编辑器并断言 DOM 与缓冲区内容），Monaco 需要 Web Worker + 真实布局，在无头环境下要付出多得多的
   脚手架才能得到同等强度的回归测试。这既是工程理由，也是本环境的现实理由。
5. 体积/依赖面：CodeMirror 6 是模块化 ESM，产物 JS 463 kB（含 LSP 客户端与 legacy-modes），无 worker 要求。

**实现边界（严格遵守 D17）**：

- Rust 侧**没有任何** LSP 代码：没有 JSON-RPC 帧、没有能力协商、没有生命周期管理，不新建 `crates/princess-lsp`。
- `.clangd` / `compile_commands.json` 的生成与校验**不在本阶段**（归 `crates/princess-build`，P2/B1）。
- `src/lsp/client.ts` 只做三件事：把 `Transport` 接口暴露成接缝、提供内存传输对（测试用）、
  提供 `onShutdown` 钩子。**并明确记录了一条实测结论**：该库刻意不实现 `shutdown`/`exit`
  握手（README 原文："Responsibility for how to actually talk to the server, how to connect and to handle
  disconnects are left to the code that implements the transport"），因此
  **引擎侧桥接必须按调研报告 §4.8 发 `shutdown` → `exit`（顺序反了退出码为 1）**，`onShutdown` 就是给它的位置。
- D18：语言服务应使用 `clangd-16`（`scripts/env.sh` 导出的 `PRINCESSIDE_LANG_SERVICE_CLANGD`），
  本阶段不生成、不校验 `.clangd`，因此不涉及 `CompileFlags.Remove` 的黑名单问题；仅在此交接给引擎侧。

---

## 6. 只能由用户本地验证的清单（无显示器，无法在本容器确认）

本阶段交付的是「能构建、能单测、能无头启动进程」。以下**没有**、也**无法**在本容器自动验证：

1. **窗口能否真的打开并渲染**：`cargo build` 成功只证明编译链接通过；WebKitGTK 需要 X11/Wayland。
   用户本地应执行 `pnpm -C apps/desktop tauri dev`（或先 `pnpm -C apps/desktop build` 再
   `cd apps/desktop/src-tauri && cargo run`）确认窗口出现。
2. **视觉与交互**：配色、布局、编辑器字体/行高、日志滚动、面板比例。验收文档 §P3 明确把"视觉体验、
   交互手感、布局是否好看"排除在自动验收之外。
3. **真实 clangd 的 LSP 体验**：补全/跳转/悬停/诊断 UI 目前由脚本化服务端验证协议层；
   真实 clangd-16 + 真实 `compile_commands.json` 的端到端体验要等引擎侧桥接落地后在本地看。
4. **`princess:tools:detect` 在真实 Tauri IPC 上的往返**：Rust 侧已有集成测试直取真实 `doctor.sh`，
   但「前端 invoke → Rust → 前端渲染」这一跳需要真实 webview；容器内只覆盖到"非 Tauri 环境显式报
   `E_INTERNAL`"与"引擎返回失败信封时原样展示"两条。
5. **`princess:event` 实时事件流的推送与渲染**：Rust 侧 `EventBus`/环形缓冲有单测，
   前端有夹具重放测试；真正的跨进程推送需窗口运行。
6. **打包产物安装后启动**（P8-3 范畴）。
7. **托盘图标 / 窗口图标在桌面环境下的观感**：`icons/*.png|ico` 是脚本生成的像素图形（可复现），
   实际观感需要人眼确认。

---

## 7. 已知缺口与交接

| # | 缺口 | 归属 / 建议 |
|---|---|---|
| 1 | `fixtures/events/*.ndjson`（P2 录制）尚不存在，本阶段用的是 **P3 构造**夹具（文件头已标注 `CONSTRUCTED`） | P2 产出后直接放进 `fixtures/events/`，`tests/eventReplay.test.ts` 与加载器无需改动即可消费 |
| 2 | clangd 的**字节管道桥接**（起进程 + `Content-Length` 帧 + `shutdown`→`exit`）不存在 | 引擎侧（P2/B1）。前端接缝已就位（`src/lsp/client.ts`），`princess:lsp:bridge` 已标为 PENDING amendment |
| 3 | §3 其余 18 个命令（project/build/run/debug）返回 `E_NOT_FOUND` + 明确归属说明 | P2/P4/P5；前端已按契约把名字全部列好，接入时只加实现 |
| 4 | `xdo` 无 pkg-config 文件 | 非缺口，已在 §2.2 取证 |
| 5 | `doctor.sh` 输出格式会随 A1 演进（本阶段已因一次演进改过解析器） | 解析器已改为"识别状态标记 + 按名合并"，并有 5 个格式单测；若 A1 再加状态值，需同步 `STATUS_MARKERS` |
| 6 | 前端未做虚拟滚动（5000 行日志直接进 DOM） | 真实会话压测后决定，P5/P8 范畴 |
| 7 | Rust `cargo build` 首次编译耗时约 <!--CARGO_MINUTES--> 分钟（共享 CARGO_HOME 下 cargo 索引更新一度与其他 Agent 争锁） | 见 §8 内存/网络说明 |

---

## 8. 环境、内存与网络决策（如实记录）

- **内存纪律（D16）**：所有 cargo 命令都带 `CARGO_BUILD_JOBS=2` 并在后台执行；
  `[profile.dev] debug = 1` 以缩小 Tauri debug 二进制与内存峰值。构建期间实测可用内存约 1.3–1.5G，
  未出现 OOM。
- **网络**：`static.rust-lang.org` 未使用。cargo 走 crates.io（在共享 `CARGO_HOME` 下曾长时间
  "Blocking waiting for file lock on package cache" —— 同时有另外两个 Agent 在跑 cargo，属正常争用，
  不是失败；已用后台执行 + 大超时避免误判）。
  `pnpm` 使用已有配置的 **npmmirror**（`https://registry.npmmirror.com/`），无需换源，实测安装 108 个包在有
  两次超时重试后成功。
- **pnpm 12 的构建脚本白名单**：`pnpm install` 首次以 `ERR_PNPM_IGNORED_BUILDS` 退出 1
  （esbuild 的 postinstall 被拦）。已在 `pnpm-workspace.yaml` 同时写入 pnpm 12 的 `allowBuilds: {esbuild: true}`
  与 pnpm 10 的 `onlyBuiltDependencies: [esbuild]`，重跑退出 0。
- **子进程/超时/取消**：doctor 运行有 60s 硬超时与 `E_TIMEOUT`；取消走统一 `OpRegistry`（`E_CANCELLED`）。
  长操作的**进程组**取消语义留给 P2 引擎（本阶段只有一个短命检测子进程，`kill_on_drop` 已足够）。

---

## 9. 从零重跑步骤

```bash
# 0) 环境（P0 已提供）
source scripts/env.sh
bash scripts/doctor.sh                 # 期望退出 0

# 1) 系统 GUI 依赖（Debian 12 bookworm；已装过可跳过，幂等）
apt-get update -y || true              # 已知会因无关的 pkg.cloudflare.com 源报 GPG 错
DEBIAN_FRONTEND=noninteractive apt-get install -y \
  libwebkit2gtk-4.1-dev libgtk-3-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev \
  librsvg2-dev libayatana-appindicator3-dev libxdo-dev libssl-dev \
  pkg-config build-essential file wget curl
pkg-config --exists webkit2gtk-4.1 && echo OK

# 2) 前端依赖（npmmirror；esbuild 构建脚本白名单已在 pnpm-workspace.yaml）
pnpm install

# 3) 图标（已被仓库提交，仅在需要重生成时跑）
python3 apps/desktop/scripts/gen-icons.py

# 4) 前端：类型检查 + 构建（产出 apps/desktop/dist，Tauri 的 frontendDist 依赖它）
pnpm -C apps/desktop build

# 5) 前端测试（81 用例；首次运行写快照，第二次比对快照）
pnpm -C apps/desktop test

# 6) 契约一致性（P3-5）
pnpm -C apps/desktop check:contract

# 7) Tauri 外壳构建（D16：限制并行度；首次编译很慢，建议后台跑）
cd apps/desktop/src-tauri
source /root/PrincessIDE/scripts/env.sh
CARGO_BUILD_JOBS=2 cargo build

# 8) Rust 单测 + 集成测试（含真实 scripts/doctor.sh 的端到端检测）
CARGO_BUILD_JOBS=2 cargo test

# 9) 本地（有显示器）验证 —— 本容器无法执行
pnpm -C apps/desktop tauri dev
```

---

## 10. 实测输出附录（关键命令原文）

<!--APPENDIX_PLACEHOLDER-->
