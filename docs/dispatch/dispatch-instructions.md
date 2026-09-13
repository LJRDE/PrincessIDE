# 派发指令（自然语言版 · 给网页端直接粘）

> 用法：在**新会话**里让主 Agent 读本文件，然后按下面的话派子 Agent。
> 全部子 Agent 都要求：**模型 = Mimo-V2.5-Pro**、**后台运行**、**对用户可见**。
> 一次最多 2 路，按波次来。

---

## 一句话口径（对新会话说）

> 按 `docs/dispatch/dispatch-instructions.md` 派模块子 Agent：一律用 Mimo-V2.5-Pro、后台运行、可见；
> 每路只读它自己的任务书、只写任务书里允许的目录；落地后你（主 Agent）亲跑门再提交，并更新 `docs/reports/module-agents.md`。

---

## 第一波（两路，可同时派）

1. **派一个子 Agent，名字叫「[M12·NativeUI] 原生骨架」。**
   任务书：`docs/dispatch/mimo-m12-task.md`（让它先读 `docs/spec/31-native-frontend.md`、`docs/spec/32-view-models.md`、
   `docs/spec/34-module-dispatch.md`）。它只写 `apps/native/`，不要碰根工作区与 Web 前端。
   交付：原生窗口骨架 + 自研渲染核心（先按 wgpu/Vulkan 后端实现，但要求可下沉到 ash）+ 离屏渲染哈希门 + 实测数字。

2. **同时派一个子 Agent，名字叫「[M1·Toolchain] 两个缺口」。**
   任务书：`docs/dispatch/mimo-m1-task.md`。它只写 `scripts/doctor.sh`（以及 `env.sh` 里 QEMU 那几行、
   `bootstrap-toolchain.sh` 的 QEMU 数据段）。
   交付：doctor 增加 **QEMU 数据目录检查**、jdtls **版本断言**、以及"用户机器上该目录为何缺失"的**根因结论**。

> 这两路目录完全不交叉，是唯一可以真正并行的一组。

---

## 第二波（两路，等第一波落地并提交后再派）

3. **派「[M10·Bisect] 编排器」**，任务书 `docs/dispatch/mimo-m10-task.md`。
   它只写 `crates/princess-bisect/`（骨架已建好）。
   ⚠️ 特别提醒它：**绝不允许在 `/root/PrincessIDE` 里跑 git**（bisect 会切走 HEAD），只能在它自己建的 `/tmp` 克隆里跑。

4. **同时派「[M11·Plugins] 声明式插件」**，任务书 `docs/dispatch/mimo-m11-task.md`。
   它只写 `crates/princess-plugins/`（骨架已建好）与 `fixtures/plugins/`。
   本轮**只做引擎侧**：manifest 解析/校验 + 能力模型 + 越权拒绝负样本；**不加载原生代码、不加 IPC、不碰前端**。

---

## 第三波（**串行**，一次只能派一个）

5. **先派「[M5M6·Contracts] 契约面与前端」**，任务书 `docs/dispatch/mimo-m5m6-task.md`。
   它要改 M0 契约的**三处镜像**（`docs/spec/10-contracts.md` §3 + `apps/desktop/src/contract/ipc.ts` +
   `apps/desktop/src-tauri/src/contract.rs`），加 `symbols:`/`bin:`/`hex:`/`ai:` 命令与前端视图（反汇编必须与源码行并排）。

6. **等它提交后**，再派「[M13·JDWP] Java 调试后端」，任务书 `docs/dispatch/mimo-m13-task.md`。
   它要给 `DebugBackendKind` 加 `Jdwp`（动冻结 core 的"四件套"）+ 写薄 JDWP 适配器 + 独立验收；
   并且 `scripts/p4-acceptance.sh` 必须仍然 36/36 零回归。

> 第 5、6 路**必须串行**：两者都改同一批契约文件，同时跑必然互相覆盖。

---

## 主 Agent 每回收一路后要做的事

- **亲跑任务书里的门**（不采信子 Agent 自述），贴原始输出与退出码；
- 复核**负样本**是否真按 D28.3 做的（**禁止** `mv 真文件 → .hidden` 那类手法）；
- 选择性 `git add`（别把别路在制品带进来）并提交；
- 更新 `docs/reports/module-agents.md` 的对应行（状态 + 门的结果）；
- 有"未验证项"就原样写进台账，不要抹平。
