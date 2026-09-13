# [M5M6·Contracts] 补 M5/M6 的契约面与前端（P-D）

> 通用口径见 **`docs/spec/34-module-dispatch.md`**。**本路在 W3 串行队列首位**：只有等它**提交**后，M13(JWDP) 与 M10 契约接线才可开派。

## 1. 模块与职责

M5 = 符号化/二进制（`princess-symbol` + `princess-bin`，后端完成，109 测试）；
M6 = AI（`princess-ai`，后端完成）。
**两者都是"已投入未交付"**：契约里 **0 条命令、前端 0 行**（实测：24 命令的领域分布里没有 `symbols:`/`bin:`/`fs:`/`ai:`）。
本轮把它们**暴露成契约命令 + 前端视图**。

## 2. ⚠️ 本路会改 M0 契约（三处原子迁移，缺一即红）

必须**在同一次交付里**同时改：

1. `docs/spec/10-contracts.md` §3（命令名 + 参数/返回形状）；
2. `apps/desktop/src/contract/ipc.ts`（`IPC_COMMANDS` + `IpcMap` 类型）；
3. `apps/desktop/src-tauri/src/contract.rs`（`IMPLEMENTED_COMMANDS`），并在 `commands.rs` 里加路由 + 新增 handler。

**门**：`node apps/desktop/scripts/check-contract.mjs` 必须 `result: ALIGNED`（命令数会从 24 增加，**这是预期的**；
注意 `tests/contract.test.ts` 与 shell 侧断言可能硬编码 24 → 一并更新，但**不许**把校验器改松）。

## 3. 所有权

- **允许写**：`docs/spec/10-contracts.md`（§3/§4 增量）、`apps/desktop/src/contract/ipc.ts`、`apps/desktop/src/ipc/client.ts`、
  `apps/desktop/src/components/**`、`apps/desktop/src/views/**`、`apps/desktop/tests/**`、
  `apps/desktop/src-tauri/src/{commands.rs,contract.rs}` + **新增 handler 文件**、`.scratch/mimo-m5m6/`
- **禁止触碰**：`crates/princess-core/**`（**冻结**：不得改其语义；只允许 `princess-symbol`/`princess-bin`/`princess-ai` 的既有公开 API 被调用）、
  其它 `crates/**`、`scripts/**`、`docs/spec/` 下其它文件、`fixtures/` 下别人的子目录
- **禁止 git 命令**

## 4. 要做的

1. **命令**（建议，最终命名你定并同步三处）：`princess:symbols:index`、`princess:bin:hex`、
   `princess:bin:disassemble`、`princess:bin:pageTables`、`princess:ai:complete`（或等价）。
   返回必须**沿用统一信封**（`{ok,data}` / `{ok,error:{code,message,detail}}`），错误码**只能用既有 10 个**（新增错误码要改契约并说明）。
2. **前端视图**（走 M8 的**视图注册表**，每个视图自注册 `{id,title,testid,mount}`）：
   - **hex 视图**（地址/字节/ASCII）、**反汇编视图**、**页表树**（若时间不够，至少 hex + 反汇编）；
   - **D20 硬要求**：反汇编**必须与源码行并排**呈现（不能只给行号）；数据来自引擎，前端**只画不算**；
   - AI 面板若本轮做不完，**明确写进报告的"未完成"并保持契约命令可用**（前端可后补）。
3. **测试**：新命令的 IPC 形状测试 + 视图渲染测试（注入式 IPC，无需真 Tauri）+ 至少一条**引擎错误必须显式显示**的负样本。

## 5. 门（亲自跑，贴原始输出与退出码）

```bash
cd /root/PrincessIDE
node apps/desktop/scripts/check-contract.mjs | tail -3; echo "contract_exit=$?"     # 必须 ALIGNED
pnpm -C apps/desktop exec vitest run; echo "vitest_exit=$?"                          # 只增不减
pnpm -C apps/desktop exec tsc --noEmit -p apps/desktop/tsconfig.json; echo "tsc_exit=$?"
pnpm -C apps/desktop build; echo "build_exit=$?"
CARGO_BUILD_JOBS=2 cargo test -p princess-symbol -p princess-bin -p princess-ai; echo "engine_exit=$?"
cd apps/desktop/src-tauri && CARGO_BUILD_JOBS=2 cargo test; echo "shell_exit=$?"
```

## 6. 负样本（必做）

1. 引擎返回 `E_NOT_FOUND`（不存在的符号/地址）→ 前端必须显示错误码与消息，**不许**空白或假装成功；
2. `check-contract.mjs` 的**负样本**：临时从 TS 列表里删掉一条新命令 → 校验器**必须报不一致**（贴出两次输出，然后恢复）；
3. 反汇编**缺源码行**的数据（`sourceLine: null`）→ 必须如实显示"无源码行"，**不许**编造行号（D20）。

**D28.3（照抄，违反即返工）**：**绝对禁止**改名 / 替换 / 移动工具链与环境的**真文件**构造负样本；
只许**临时 `PATH`（stub 放 `/tmp`）**、**环境变量**、**`/tmp` 或 `.scratch/` 隔离副本**。

## 7. 交付物

`.scratch/mimo-m5m6/REPORT.md`（中文、简洁）：**三处原子迁移的逐处 diff 摘要**、新命令清单与形状、
视图清单与 testid、门命令的原始输出与退出码、三条负样本证据、
**诚实列出未验证项**（GUI 目视、真 ELF 大数据量性能等）、**逐步 STATUS**、末尾总 `STATUS:`。

## 不做

不改 `princess-core` 语义；不新增错误码（除非契约同步 + 说明）；不做 AI 的 UI 之外的模型调用；不碰 `apps/desktop/src-tauri/src/lsp_handler.rs`。
