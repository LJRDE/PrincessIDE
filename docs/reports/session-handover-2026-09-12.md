# 会话交接简报（2026-09-12 → 新会话）

> **用途**：新会话对上一段对话**零记忆**。这份文件是唯一的上下文入口 —— 先读完它，再动手。
> **用户已同意的第一件事**：在**新会话**里用 `subagent` 工具**逐次直选 Mimo**（可见 + 后台 + 可纠偏）。

---

## 0. 你要做的第一件事

用户已批准开工 **P-F0**（语言模块层），任务书**已写好**：

```
.scratch/main/mimo-pf0-task.md
```

用下面的方式派发（这是用户本轮唯一的要求：**Agent 必须可见**）：

```
subagent(
  description: "P-F0 语言模块层",
  prompt: <把 .scratch/main/mimo-pf0-task.md 全文粘进去>,
  provider: "xiaomi-token-plan-cn",
  model: "mimo-v2.5-pro",
  run_in_background: true            # 可见 + 后台；需要时用 send_message 纠偏
)
```

- 若报 `child model selection is disabled for this tool instance` → 说明这不是新会话（D27.2：模型选择只对**新会话**生效）。回退顺序：`workflow`（逐路 provider/model，可见但**前台阻塞**）→ 仍不行才用 headless（**GUI 不可见**，见 D28.1）。
- 旧会话的 headless 派发方式仍在（`DSH_PERMISSION_MODE=danger-full-access dsh --profile headless "<任务书>"`，已修成真 Mimo），但它**在 GUI 里结构性不可见**。

---

## 1. 项目与当前状态（实测数字）

| 项 | 值 |
|---|---|
| 仓库 | `/root/PrincessIDE`，分支 `master`，**64 commits**，271 个跟踪文件，工作树**干净** |
| **CI 门** | **已存在且全绿**：`bash scripts/ci-gate.sh` → **`pass=6 fail=0`**（tsc / vitest / 契约三方一致 / vite build / `cargo test --workspace` / src-tauri `cargo test`）；`.github/workflows/ci.yml` 调它 |
| 前端测试 | **124 passed**（9 文件） |
| fixlist | `docs/reports/fixlist.json` **10 项全部处理完**（9 fixed + 1 not-a-bug + BUG-009 fixed） |
| 决策 | `docs/spec/00-decisions.md` **D1–D31**，开放待办 **A1–A18** |
| 模块表 | `docs/spec/30-modules.md`：**M0–M13**，11 条边界规则，分期 P-A…P-F |

**本轮（上一段对话）做完的事**：BUG-001/002/003/004/005/006/008 修复、BUG-007 销项、IMPROVE-001 并行下载（含我复现并退回修的"失败收集失效"）、D27–D31 五条决策、`princesside-delegation-view` 插件（GUI 里可见子 Agent + Token 统计）、**CI 门 + 视图注册表**、**门抓出并修好 BUG-009**、`crates/princess-view`（共享视图模型层）。

---

## 2. 待办轨道（按建议优先级）

| 轨道 | 内容 | 状态 |
|---|---|---|
| **P-F0** | 语言模块层本体（manifest + 适配器 + **用 C 路径证明缝能容纳现状**） | **任务书已就绪，见 §0** |
| **P-F1** | Java 第一版（jdtls 入 `.toolchain/` + doctor 断言、`fixtures/javaproj/`、javac 构建、JVM 运行、编辑器 LSP） | 待派（P-F0 之后） |
| **P-B** | M10 编排器 `princess-bisect`：`git` CLI 薄封装 + `git bisect run` 接"构建+QEMU+断言" | 待派 |
| **P-C** | M11 声明式插件 + 能力模型（原生插件最后） | 待派 |
| **P-D** | 补 P5/P7 契约面与前端：`symbols:`/`bin:`/`ai:` 命令 + 反汇编/页表/hex 视图 + AI 面板（三处后端已完成但**契约 0 命令、前端 0 行**） | 待派 |
| **P-E** | M12 双前端：视图模型层**已落地**（`crates/princess-view`，`6dc1640`）；剩 P-E0b（契约 `view:` 三方原子迁移）+ P-E1（`apps/native/` 骨架，底层 wgpu/ash **未定**） | 用户明确"先不写" |
| **P-F2** | JDWP 调试后端（第二调试协议，独立验收） | 后置 |

---

## 3. 派发纪律（每条都有血泪）

1. **一切"完成"必须有真实命令输出 + 退出码**（D14）。子 Agent 的自述**不算证据** —— 你要**自己复跑**验收命令（本会话已两次靠这抓到问题：`main.ts` 的 `listen` 没接、`bootstrap` 的失败收集失效）。
2. **负样本必做**（D28.3）：**绝对禁止**改名/替换/移动工具链真文件来造场景（上一轮有 Agent 这么做并死在半路，污染了 `.toolchain`）。只许：临时 `PATH`（stub 在 `/tmp`）、环境变量、`/tmp` 或 `.scratch/` 副本。
3. **单目录所有权**（D15）：任务书必须显式写"允许写/禁止触碰"；**并行两路的目录不许重叠**。注意契约命令的"三方一致性"（`docs/spec/10-contracts.md` §3 + `apps/desktop/src/contract/ipc.ts` + `apps/desktop/src-tauri/src/contract.rs`）—— 加命令必须三处**原子迁移**，否则 `check-contract.mjs` 会红。
4. **子 Agent 禁 git**（D15）：提交由你（主 Agent）做。提交要**选择性 add**，别把别路 Agent 的进行中改动一起提交。
5. **内存纪律**（D22）：`CARGO_BUILD_JOBS=2`，同时最多 2 路重型构建。
6. **frontend/外壳/契约永远进门**：任何改动后跑 `bash scripts/ci-gate.sh`（迭代用 `--fast`，收尾跑全量）。

---

## 4. 关键文件索引

| 想知道 | 看这里 |
|---|---|
| 项目全貌 | `docs/HANDOVER.md` |
| **决策日志（31 条，动手前必读）** | `docs/spec/00-decisions.md` |
| 接口契约 | `docs/spec/10-contracts.md` |
| **模块表与边界规则** | `docs/spec/30-modules.md` |
| 双前端（M12）需求与工作单 | `docs/spec/31-native-frontend.md` |
| 视图模型（M12 的前置，已实现） | `docs/spec/32-view-models.md` |
| **语言模块层（M13）需求与工作单** | `docs/spec/33-language-modules.md` |
| **待修/已修清单** | `docs/reports/fixlist.json`（10 项） |
| 上一轮修复报告（含事故记录 §3） | `docs/reports/fix-round1.md` |
| **子 Agent 派发台账** | `.scratch/main/dispatches.md` |
| 任务书目录 | `.scratch/main/mimo-*-task.md`（含已就绪的 `mimo-pf0-task.md`） |
| IDE 插件（GUI 里看子 Agent / Token 统计） | `~/.dsh/profiles/web/node_modules/princesside-delegation-view/`（仓库外） |

---

## 5. 环境事实（别重新踩）

- **工具链**：`source scripts/env.sh`（每个 shell 都要）。`clangd-16` 16.0.6 / `clang-16` 16.0.6 / 不带版本号的 `clangd`/`clang` 仍是 14（D18）/ `gdb` 13.1（P0 legacy）/ `princess-gdb` 16.3（DAP）/ `doctor.sh` exit 0（36 工具）。**`jdtls`/`mvn`/`gradle` 不在**；`java`/`javac` = 17.0.20.1。
- **网络**：crates.io 必须走 USTC 镜像（`.cargo/config.toml`，D19）；`github.com` **超时**，`api.github.com` 通，**SSH(22/443) 通**。用户说 GitHub 他自己登。
- **显示/GPU**：无显示器（GUI 视觉只能用户验，D14）；本机是 QEMU 虚拟机、无硬件 GPU，Vulkan 只有 `lavapipe` 软件光栅；**KVM 不可用**。
- **DSH 环境**：`~/.dsh/settings.yaml` 已启用 `subagent-model-selection`（白名单 `xiaomi-token-plan-cn/mimo-v2.5-pro`）；`~/.dsh/headless-settings.yaml` 让 headless 真跑 Mimo（D27.1 修复）。**headless 会话在 GUI 里不可见**（D28.1：宿主只登记它自己认识的会话；磁盘 45 个会话，宿主索引只有 3 个）。

---

## 6. 用户偏好（照做）

- **回复简洁**；要**批判性分析**，不要附和 —— 用户明确要过"批判"三次，且接受"我不对"的结论（D27.2/D29 都写了我的判断错误）。
- **不确定就用 question 问**，不要猜；**批量问**，别一次一个。
- **动手前确认需求**；用户会亲自复核关键决策。
- 派 Agent **要可见**（本轮新增要求）。
- 关键数字/结论要贴**原始输出与退出码**，引用文件用可点击的路径。
