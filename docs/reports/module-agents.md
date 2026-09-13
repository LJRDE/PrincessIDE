# 模块 Agent 台账

> 口径：**一个模块 = 一张任务书 = 一个可见的 Agent**（见 `docs/spec/34-module-dispatch.md` 派发手册）。
> 派发：**由用户执行**（D32：主 Agent 不再自行派发子 Agent）；用户在新会话里派，模型 `mimo-v2.5-pro`。
> 主 Agent 的职责：维护任务书 → 回收时**亲跑门复核** → 选择性提交 → 更新本表。**本表是"哪个模块谁负责、门过没过"的唯一权威入口。**

## 一、模块 → Agent 映射与实时状态

| 模块 | 负责 Agent（`description`） | 任务书 | 波次 | 状态 | 门（命令 → 结果） |
|---|---|---|---|---|---|
| **M1 工具链/环境** | `[M1·Toolchain]` | `docs/dispatch/mimo-m1-task.md` | **W1** | ⏳ 待派 | `bash scripts/doctor.sh` → 待跑 |
| **M12 第二前端** | `[M12·NativeUI]` | `docs/dispatch/mimo-m12-task.md` | **W1** | ⏳ 待派（底层 wgpu/ash 未定） | 视图模型同构 + 离屏哈希 → 待跑 |
| **M10 编排器** | `[M10·Bisect]` | `docs/dispatch/mimo-m10-task.md` | **W2** | ⏳ 待派（骨架已由主 Agent 建好） | bisect 端到端 → 待跑 |
| **M11 插件管理** | `[M11·Plugins]` | `docs/dispatch/mimo-m11-task.md` | **W2** | ⏳ 待派（骨架已建；本轮只做引擎侧，不加 IPC） | manifest 校验 + 越权负样本 → 待跑 |
| **M5+M6 契约面/前端** | `[M5M6·Contracts]` | `docs/dispatch/mimo-m5m6-task.md` | **W3（串行首棒）** | ⏳ 待派 | `check-contract.mjs` ALIGNED + 前端 vitest → 待跑 |
| **M13·JDWP** | `[M13·JDWP]` | `docs/dispatch/mimo-m13-task.md` | **W3（串行次棒）** | ⏳ 待派（需 `DebugBackendKind::Jdwp`，动 M0） | `cargo test -p princess-debug` + JDWP 真会话 → 待跑 |

> **W3 是串行队列**：M5M6 → M13 → M10 契约接线，**每一棒必须等前一棒提交后再发**，因为三者都改 M0 契约三处镜像。

## 二、已完成（历史，供对照）

| 模块 | Agent | 结果 | 提交 |
|---|---|---|---|
| M0/M8/M9 工具链与前端一轮修复 | headless ×N（台账 `.scratch/main/dispatches.md`） | 已复核 | `b888047` `ddf2935` `eb2e3a7` `c7a19ac` `b88b6d3` |
| M12 视图模型层（P-E0a） | headless `bash-9` | 已复核（49 tests） | `6dc1640` |
| **M13 语言模块层（P-F0）** | `048f2edf-…`（另一会话，**可见**） | 已提交 | `7761a9b` |
| **M13 Java 第一版（P-F1）** | `6eb50513-…`（另一会话，**可见**） | 已提交（jdtls 1.39.0 / javac / JVM / LSP 接线） | `30a5984` `94481d8` |
| — QEMU 数据目录缺陷（用户实机报障） | 主 Agent 亲修 | 干净 worktree 对照验证 | `bdbc112` |

## 三、已知缺口（在这些 Agent 之外，别忘）

1. **`doctor.sh` 不检查 QEMU 数据目录**（用户机器 `smoke-boot`/模板/P4 因此跑不了）→ 归 `[M1·Toolchain]`。
2. **`doctor.sh` 对 jdtls 只验"binary present"，不验版本**（D6/D18 要求版本化断言）→ 归 `[M1·Toolchain]`。
3. **P-F1 的负样本用了 `mv … .hidden`**（违反 D28.3；本次无残留但属侥幸）→ 派发手册 §二.5 已把禁令固化。
4. **M5/M6 后端已完工但契约 0 命令、前端 0 行**（最大的"已投入未交付"）→ 归 `[M5M6·Contracts]`。
5. **P-E2（可编辑 + IME）与 M12 的底层选型（wgpu vs ash）仍未决** → 需用户裁决后才派。

## 四、边界门（D33：约定即数据、数据即门）

`python3 scripts/check-boundaries.py`（`ci-gate.sh` 第 7 步，**永远运行**）检查三类边界，违规即非零：

1. **workspace 成员**：`crates/*` 必须在根成员里；独立 workspace 必须登记**且有门覆盖**；
2. **工具链禁令**：`.toolchain/**` 不得改名/删除，不得留 `.hidden` 残留（D28.3 事故特征）；
3. **模块所有权**（`--agent <模块>`，需派发基线）：变动集必须落在该模块 `docs/spec/ownership.toml` 的 `allow` 内；
   `shared` 需显式授权；`governance` 任何 Agent 都不许碰。

**它已经记录在案的真实事故**：

| 事故 | 现状 |
|---|---|
| M11 把 `crates/princess-plugins` 从根 members 删掉（该模块掉出 CI） | **已恢复**（复核时已在 members 内）；假树负样本证明规则会开火 |
| M11 增加 `tempfile` dev-dependency（违反我当时后加的"不新增依赖"） | **保留**（lock 多 4 个包，可接受；禁令晚于它的开工时间，责任在主 Agent） |
| P-F1 用 `mv .toolchain/bin/jdtls → .hidden` 造负样本（D28.3 禁止） | 无残留；**该手法现已由第 2 类检查覆盖** |
| `apps/native`（M12 独立 workspace）**尚未接入 `ci-gate.sh`** | 已在 `ownership.toml` 的 `independent_workspaces.coverage` 里**显式登记为已知缺口** |
