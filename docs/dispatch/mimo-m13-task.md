# [M13·JDWP] Java 调试后端（P-F2）

> 通用口径见 **`docs/spec/34-module-dispatch.md`**；语言模块层见 `33-language-modules.md`。
> **本路在 W3 串行队列第二位**：必须等 `[M5M6·Contracts]` **提交后**才开派（两者都改 M0 契约三处镜像）。

## 1. 模块与职责

M13 的 P-F2：给 Java 加**第二个调试后端 JDWP**，与既有 GDB DAP 并列。
P-F1 已刻意**不做**它（`languages/java.toml` 的 `[language.debug]` 段留着 `TODO(P-F2): JDWP`）。本轮补上。

**设计红线（D11 的精神照搬）**：JDWP 是**第二个协议**，适配器要薄、能力要显式；
**不得**把 JDWP 塞进 GDB DAP 的适配器里，也不得为它改造既有 DAP 路径。

## 2. ⚠️ 本路会动冻结的 `princess-core`（四件套）

- `crates/princess-core/src/config.rs`：`DebugBackendKind` 增加 `Jdwp` 变体（`#[serde(rename = "jdwp")]`）；
- `docs/spec/10-contracts.md` §4 的取值注释同步；
- **引用决策编号**（D31 / 本任务）；
- **不改任何既有语义**（`gdb-dap` 路径的行为逐字不变）。

若你判断还必须在 §3 加新命令（例如 `princess:debug:*` 的 JDWP 专属字段），**按三处原子迁移做**：
`10-contracts.md` §3 + `apps/desktop/src/contract/ipc.ts` + `apps/desktop/src-tauri/src/contract.rs`（+ `commands.rs` 路由）。
**能复用既有 12 个 `debug:*` 命令就优先复用**（attach / setBreakpoints / continue / stepOver / stepInto /
stackTrace / scopes / variables / readMemory / writeMemory / disassemble / registers）。

## 3. 所有权

- **允许写**：`crates/princess-core/src/config.rs`（**仅枚举变体与其测试**）、`crates/princess-debug/**`、
  `languages/java.toml`（`[language.debug]` 段）、`apps/desktop/src-tauri/src/{debug_handler.rs,commands.rs,contract.rs}`（若需路由）、
  `docs/spec/10-contracts.md`（仅 §3/§4 增量）、`apps/desktop/src/contract/ipc.ts`（若加命令）、
  `fixtures/javaproj/**`（**只加**调试所需的最小改动，例如固定入口行号）、`.scratch/mimo-m13/`
- **禁止触碰**：其它 `crates/**`、`apps/desktop/src/**`（前端本轮不动）、`scripts/**`、`docs/spec/` 下其它文件
- **禁止 git 命令**

## 4. 要做的

1. **`DebugBackendKind::Jdwp`**（四件套同上）。
2. **薄 JDWP 适配器**（`princess-debug` 内新模块）：握手 → 设置断点 → 继续 → 停靠事件 → 栈回溯 → 线程/帧 → 变量/寄存器；
   **明确列出"JDWP 有而 DAP 没有"和"没有而需要绕过"的能力**（照 D11 对内置 DAP 的处理方式：能力补齐层，而不是全量转换）。
3. **接线**：`princess.toml` 的 `[debug] backend = "jdwp"` 时走 JDWP；`= "gdb-dap"` 时**行为逐字不变**。
4. **独立验收**：**不能复用 `scripts/p4-acceptance.sh`**（那是 GDB DAP 的断言集）。本轮的验收要针对 JDWP 自己写：
   在 `fixtures/javaproj` 上（或一个新的最小 Java 夹具）下断点、命中、看栈回溯，断言**具体行号与帧序列**。

## 5. 门（亲自跑，贴原始输出与退出码）

```bash
cd /root/PrincessIDE && source scripts/env.sh
CARGO_BUILD_JOBS=2 cargo test -p princess-core -p princess-debug; echo "engine_exit=$?"
CARGO_BUILD_JOBS=2 cargo test -p princess-debug --test <你的JDWP验收>; echo "jdwp_exit=$?"
node apps/desktop/scripts/check-contract.mjs | tail -2; echo "contract_exit=$?"    # 必须 ALIGNED
cd apps/desktop/src-tauri && CARGO_BUILD_JOBS=2 cargo test; echo "shell_exit=$?"
bash scripts/p4-acceptance.sh; echo "p4_exit=$?"                                   # 必须仍然 36/36（零回归）
```

## 6. 负样本（必做，三条）

1. **jdb/JDWP 不可用**（`jdb` 不在、端口不通）→ 明确错误（例如 `E_TOOLCHAIN_MISSING` 或既有错误码），**不许**静默或假装已连接；
2. **下断点失败**（非法行号/类名不存在）→ 明确错误 + 引擎原始信息；
3. **guest/目标进程提前退出** → 会话结束要如实归因，不能报"仍在运行"。

**D28.3（照抄，违反即返工）**：**绝对禁止**改名 / 替换 / 移动工具链与环境的**真文件**构造负样本
（**注意**：P-F1 曾用 `mv .toolchain/bin/jdtls → .hidden` 造负样本，属违规，虽侥幸无残留）；
只许**临时 `PATH`（stub 放 `/tmp`）**、**环境变量**、**`/tmp` 或 `.scratch/` 隔离副本**。

## 7. 交付物

`.scratch/mimo-m13/REPORT.md`（中文、简洁）：**四件套逐处 diff 摘要**、JDWP 能力对照表（有/无/绕过）、
门命令的原始输出与退出码、三条负样本证据、**诚实列出未验证项**（真机编辑器交互、GUI 面板未接）、**逐步 STATUS**、末尾总 `STATUS:`。

## 不做

不碰前端（调试 UI 复用既有面板）；不改 DAP 路径行为；不做远程 JDWP over SSH；不动 `scripts/`。
