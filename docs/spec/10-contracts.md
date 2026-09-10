# PrincessIDE 接口契约 v1（冻结候选）

> 状态：待用户确认。**任何阶段的实现 Agent 不得擅自偏离本文档**；需要变更时先在本文档提出修改，由主 Agent 裁决后统一更新。
> 目的：让多个 Agent 并行写代码而不互相踩脚。契约的价值在于"先定死边界，再并行施工"。

---

## 0. 总原则

1. 引擎（Rust）与界面（前端）之间**只有两种交互**：**命令（IPC 调用）** 与 **事件流（单向推送）**。
2. **引擎不做 UI 决策，界面不做工程决策**。构建怎么跑、QEMU 怎么起、符号怎么解，全部归引擎。
3. 一切长耗时操作必须**可取消、可观测、可复现**。
4. **失败必须显式**：错误码 + 原始 stderr 原文，禁止吞异常、禁止静默降级。
5. 所有跨边界数据必须 **serde 可序列化**，前端直接消费，不做二次翻译层。

---

## 1. 目录归属（防并行冲突的铁律）

| 路径 | 所有者 | 说明 |
|---|---|---|
| `crates/princess-core/` | P2 | 领域类型、事件模型、错误模型、trait 边界 |
| `crates/princess-build/` | P2 | 工具链检测、构建后端（make/cmake/cargo/zig/custom） |
| `crates/princess-run/` | P2 | QEMU 参数构造、启动、串口捕获、退出归因 |
| `crates/princess-symbol/` | P2 | ELF/DWARF 解析、符号化、栈回溯还原 |
| `crates/princess-debug/` | P4 | 调试后端（GDB/MI → DAP 适配） |
| `crates/princess-bin/` | P5 | hex / 反汇编 / 段节布局 / 页表 / 描述符表解析 |
| `crates/princess-ai/` | P7 | AI provider 抽象（OpenAI 兼容） |
| `apps/desktop/src-tauri/` | P3 | 窗口、IPC 注册、子进程编排 |
| `apps/desktop/src/` | P3/P5 | 前端 UI（按功能模块再细分目录） |
| `templates/` | P6 | 内核项目模板 |
| `fixtures/refkernel/` | P0 | 参考内核（端到端测试夹具） |
| `scripts/` | P0 | 环境、冒烟、符号化脚本 |
| `docs/spec/` | 主 Agent | 规格与契约 |
| `docs/research/` | 研究 Agent | 调研报告 |
| `docs/reports/` | 各阶段 | 验收报告（与主 Agent 的复核报告分开） |
| `Cargo.toml` / `rust-toolchain.toml` / `.cargo/` | P2-A | 根 Cargo 工作区定义 |
| `package.json` / `pnpm-workspace.yaml` / `pnpm-lock.yaml` | P3-A | 前端工作区定义 |
| `fixtures/events/` | P2-A | 录制的事件夹具（供前端重放验证） |
| `.clangd` / `compile_commands.json` 的**生成逻辑** | P2-B/B1 | 内核特化配置（见 D7/D17）；LSP 编辑器交互在前端（D17） |

**规则**：一个目录同一时间只有一个所有者。跨目录改动必须由主 Agent 显式授权。

---

## 2. 事件流契约

引擎 → 界面为**单向有序事件流**。统一信封：

```jsonc
{
  "v": 1,                 // 事件模型版本
  "seq": 12345,           // 会话内单调递增，前端据此检测丢包并请求重放
  "ts": "2026-05-05T12:00:00.123Z",
  "opId": "op-7f3a",      // 关联的长操作；无关联时为 null
  "kind": "build.finished",
  "payload": { }
}
```

### 事件类型

| kind | payload（关键字段） |
|---|---|
| `log.append` | `stream`: `build`\|`serial.com1`\|`qemu.monitor`\|`gdb.console`\|`ide`；`chunk`；`encoding`: `utf8`\|`utf8-lossy` |
| `build.started` | `backend`、`toolchainId`、`argv`、`cwd` |
| `build.diagnostic` | `severity`、`file`、`line`、`col`、`message`、`source`: `clang`\|`ld`\|`nasm` |
| `build.finished` | `status`: `ok`\|`failed`\|`cancelled`；`exitCode`；`durationMs`；`artifacts[]` = `{path, kind, size, sha256}` |
| `run.started` | `qemuArgv`、`gdbStub`（可为 null） |
| `run.fault` | `vector`: `#DE`\|`#UD`\|`#PF`\|…；`rip`；`errorCode`；`regs`；`symbolicated?` = `{symbol, file, line}` |
| `run.exited` | `exitCode`；`reason`: `guest-shutdown`\|`triple-fault`\|`timeout`\|`killed`；`uptimeMs` |
| `debug.stopped` | `reason`: `breakpoint`\|`step`\|`signal`\|`entry`；`threadId`；`frame`；`regs` |
| `debug.breakpoint.changed` | `id`、`verified`、`location` |
| `debug.output` | `category`: `console`\|`stdout`\|`stderr`；`text` |
| `symbols.indexed` | `artifact`、`buildId`、`symbolCount` |
| `artifact.changed` | `path`、`kind` —— 触发 hex/ELF/反汇编视图刷新 |
| `ai.chunk` / `ai.finished` | `requestId`、`text` / `usage` |
| `lsp.message` | `serverId`、`message`（**已去掉帧头的 JSON-RPC 原文**）。语言服务回包唯一走这条事件流 |
| `lsp.stopped` | `serverId`、`reason`: `shutdown`\|`crashed`\|`killed` |

### 流式文本约定
- 引擎**保证不切断 UTF-8 码点**；非法字节用 `utf8-lossy` 标记并替换。
- `serial.com1` 必须**先落盘再推送**，保证日志可回溯（QEMU 丢日志不可接受）。
- 前端只做追加渲染，不在事件流上做有损变换。

---

## 3. IPC 命令契约

命名：`princess:<domain>:<action>`，domain ∈ `project` `build` `run` `debug` `symbols` `bin` `fs` `tools` `ai` `op` `lsp`。

统一返回：

```jsonc
{ "ok": true,  "data": { } }
{ "ok": false, "error": { "code": "E_BUILD_FAILED", "message": "…", "detail": "原始 stderr" } }
```

### 错误码（封闭集合，新增需改本契约）
`E_TOOLCHAIN_MISSING` `E_BUILD_FAILED` `E_QEMU_FAILED` `E_TIMEOUT` `E_NOT_FOUND`
`E_INVALID_CONFIG` `E_SANDBOX_DENIED` `E_CANCELLED` `E_AI_UNAVAILABLE` `E_INTERNAL`

### 必备命令（v1 最小集）
- `princess:project:open` / `princess:project:validate`
- `princess:tools:detect` → 工具链表（路径 + 版本 + 是否可用）
- `princess:build:start` / `princess:build:cancel`
- `princess:run:start` / `princess:run:stop`
- `princess:debug:attach` / `setBreakpoints` / `continue` / `stepOver` / `stepInto` / `stackTrace` / `scopes` / `variables` / `readMemory` / `writeMemory` / `disassemble` / `registers`
- `princess:op:cancel`（统一取消入口，参数 `opId`）
- `princess:op:replay`（从 `seq` 重放事件，用于重连 UI）
- **语言服务桥（D17 裁决，`lsp` 域）**——引擎负责 clangd 进程生命周期与帧封装，前端只做编辑器交互：
  - `princess:lsp:start` `{ projectRoot }` → `{ serverId, command, args }`（引擎按 **D7** 生成/校验 `.clangd`、按 **D8** 保证 CDB，并按 **D18** 使用 `clangd-16`）
  - `princess:lsp:send` `{ serverId, message }` → `{}`（`message` 为**不带帧头**的 JSON-RPC 原文，帧封装由引擎负责）
  - `princess:lsp:stop` `{ serverId }` → `{}`（干净 `shutdown`→`exit`，随后按进程组收尾）
  - 回包一律走事件流 `lsp.message` / `lsp.stopped`，**不新增第二条传输通道**（贯彻 §0 第 1 条）。

---

## 4. `princess.toml` 工程描述文件

工程根目录下的声明式配置。**未知键必须报错**（fail loud），不带版本号的配置拒绝加载。

```toml
schema = 1

[project]
name = "mykernel"
language = "c"          # c | asm | cpp | rust | zig
arch = "x86_64"

[build]
backend  = "make"       # make | cmake | cargo | zig | custom
command  = "make"       # 可选：覆盖默认调用
cwd      = "."
targets  = ["all"]
artifacts = ["build/kernel.elf"]
compile_commands = "compile_commands.json"   # 供 clangd 使用

[run]
backend   = "qemu"
kernel    = "build/kernel.elf"
boot      = "multiboot1"   # multiboot1 | multiboot2 | uefi | raw
args      = ["-m", "512M", "-serial", "stdio", "-display", "none", "-no-reboot"]
timeout_ms = 15000
serial    = { device = "com1", tee_to_file = "build/serial.log" }

[debug]
backend = "gdb"            # gdb（经 DAP 适配层）
symbols = "build/kernel.elf"
stub    = { host = "127.0.0.1", port = 1234, mode = "launch" }  # launch | attach

[toolchain]
cc = "x86_64-elf-gcc"
as = "nasm"
ld = "x86_64-elf-ld"
gdb = "gdb"

[ai]
provider = "openai-compatible"
base_url = ""              # 留空则用全局设置
model    = ""
```

约定：相对路径一律相对工程根；`~` 与 `$ENV` 展开由引擎负责；缺失字段用引擎默认值并在 UI 中标注"使用默认值"。

---

## 5. 引擎 trait 边界（扩展点，v1 只预留不实现动态加载）

在 `princess-core` 定义、按静态分发使用：

- `BuildBackend`：`detect() / plan() / execute(→事件流) / cancel()`
- `RunBackend`：`plan() / launch(→事件流) / serial() / stop() / classify_exit()`
- `DebugBackend`：DAP 能力集子集（断点、单步、寄存器、内存、反汇编）
- `BinProvider`：ELF 段节/符号/源码行/反汇编查询
- `AiProvider`：OpenAI 兼容 `chat/completions`（流式）

**v1 明确不做**：插件加载器、动态库加载、脚本插件、插件市场。用户可扩展性只通过 `backend = "custom"` + `command` 实现。

---

## 6. 进程与流式约定

1. 子进程统一用 `tokio::process` 启动；stdout/stderr **分别**成流回传，不合并。
2. QEMU 串口用 `-serial stdio` 或 `-serial unix:<sock>` 捕获；**必须保证有读者**，否则 QEMU 会因管道写满而阻塞（经典陷阱）。
3. 每个子进程都带 `opId`、超时、以及**杀进程组**的取消语义（不能只杀父进程留孤儿）。
4. 退出码与**退出原因**分离：`reason` 由引擎归因（正常关机 / 三击故障 / 超时 / 被取消），UI 不得自行猜测。
5. 引擎必须在进程异常终止时也保证事件流收尾（`*.finished` 事件必达）。

---

## 7. 版本与兼容

- 事件模型版本 `v`、配置 `schema` 独立演进；新增字段必须可选，删除/改语义必须升版本。
- 前后端版本不匹配时，UI 必须显式告警，**不允许静默错乱**。
