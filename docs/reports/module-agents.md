# 模块 Agent 台账

> 口径：**一个模块 = 一张任务书 = 一个可见的 Agent**（见 `docs/spec/34-module-dispatch.md` 派发手册）。
> 派发：**由用户执行**（D32：主 Agent 不再自行派发子 Agent）；用户在新会话里派，模型 `mimo-v2.5-pro`。
> 主 Agent 的职责：维护任务书 → 回收时**亲跑门复核** → 选择性提交 → 更新本表。**本表是"哪个模块谁负责、门过没过"的唯一权威入口。**

## 一、模块 → Agent 映射与实时状态

| 模块 | 负责 Agent（`description`） | 任务书 | 波次 | 状态 | 门（命令 → 结果） |
|---|---|---|---|---|---|
| **M1 工具链/环境** | `[M1·Toolchain]` | `.scratch/main/mimo-m1-task.md` | **W1** | ⏳ 待派 | `bash scripts/doctor.sh` → 待跑 |
| **M12 第二前端** | `[M12·NativeUI]` | `.scratch/main/mimo-m12-task.md` | **W1** | ⏳ 待派（底层 wgpu/ash 未定） | 视图模型同构 + 离屏哈希 → 待跑 |
| **M10 编排器** | `[M10·Bisect]` | `.scratch/main/mimo-m10-task.md` | **W2** | ⏳ 待派（骨架已由主 Agent 建好） | bisect 端到端 → 待跑 |
| **M11 插件管理** | `[M11·Plugins]` | `.scratch/main/mimo-m11-task.md` | **W2** | ⏳ 待派（骨架已建；本轮只做引擎侧，不加 IPC） | manifest 校验 + 越权负样本 → 待跑 |
| **M5+M6 契约面/前端** | `[M5M6·Contracts]` | `.scratch/main/mimo-m5m6-task.md` | **W3（串行首棒）** | ⏳ 待派 | `check-contract.mjs` ALIGNED + 前端 vitest → 待跑 |
| **M13·JDWP** | `[M13·JDWP]` | `.scratch/main/mimo-m13-task.md` | **W3（串行次棒）** | ⏳ 待派（需 `DebugBackendKind::Jdwp`，动 M0） | `cargo test -p princess-debug` + JDWP 真会话 → 待跑 |

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
