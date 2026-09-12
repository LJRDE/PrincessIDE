# PrincessIDE 模块表 v0.1（草案 · 待复核）

> 依据：`docs/spec/00-decisions.md` **D29**（用户 8 模块提案的批判与裁决）。
> 定位：这张表是**所有权与验收地图**，不是重写计划。每条"门"必须是**现在就能跑、退出码即结论**的命令。
> 状态：v0.1 已提交（`012780b`）；**P-A 落地后本节状态列已更新**（见各行「现状」）。

## 一、模块表

| # | 模块 | 职责 | 所有权（目录 / 文件） | 门（验收命令） | 现状 |
|---|---|---|---|---|---|
| **M0** | **契约层**（core 的对外面） | 事件模型、错误码、IPC 命令、`princess.toml`；**唯一的**引擎↔UI 真相来源 | `crates/princess-core`（冻结）、`docs/spec/10-contracts.md`、`apps/desktop/src/contract/` | `node apps/desktop/scripts/check-contract.mjs` + `cargo test -p princess-core` | 已有（**冻结**） |
| **M1** | **工具链 / 环境** | 探测、安装、版本断言（clangd-16 / bear / gdb≥14 / QEMU / nasm …） | `scripts/doctor.sh`、`scripts/bootstrap-toolchain.sh`、`src-tauri/src/doctor.rs` | `bash scripts/doctor.sh`（exit 0）+ 缺失负样本（非零） | 已有，但**散在两处**、口径待合并 |
| **M2** | **构建** | make/CMake 后端、CDB、`.clangd` 生成（D7/D8/D18） | `crates/princess-build` | `cargo test -p princess-build` + `bash .scratch/build/run-acceptance.sh` | 已有 |
| **M3** | **运行** | QEMU 编排、串口、退出归因（D9） | `crates/princess-run` | `cargo test -p princess-run` + `bash scripts/smoke-boot.sh` | 已有 |
| **M4** | **调试** | gdb 内置 DAP + 薄能力层（hbreak 等，D11） | `crates/princess-debug` | `cargo test -p princess-debug` + `bash scripts/p4-acceptance.sh` | 引擎已有；**前端面板 = BUG-008（在做）** |
| **M5** | **符号化 / 二进制** | ELF·DWARF·行号·反汇编（D20） | `crates/princess-symbol`、`crates/princess-bin` | `cargo test -p princess-symbol -p princess-bin` | 后端完成（109 测试）；**契约 0 命令、前端 0 行** |
| **M6** | **AI** | OpenAI 兼容抽象层 | `crates/princess-ai` | `cargo test -p princess-ai` | 同上（**无契约面**） |
| **M7** | **夹具与验收** | 权威夹具与断言常量（D3/D26）、验收标准 | `fixtures/**`、`docs/spec/20-acceptance.md`、`scripts/*acceptance*.sh`、`templates/verify-template.sh` | `bash scripts/smoke-ci.sh` + `bash templates/verify-template.sh` | 已有，**但无主人、无版本** |
| **M8** | **前端外壳 / 视图** | 布局 + IPC 接线 + **视图注册表**（每个视图自注册） | `apps/desktop/src/**` | `pnpm -C apps/desktop exec vitest run` + `tsc --noEmit` + `pnpm build` | 已有；**注册表已落地**（P-A，提交 `b88b6d3`）：5 个视图自注册、`app.ts` 手工 render 调用归零、124 个测试入门 |
| **M9** | **桌面壳** | Tauri 窗口、24 命令路由、事件桥、capabilities | `apps/desktop/src-tauri/**` | `cargo build` + `cargo test`（在 src-tauri 内，独立工作区） | 已有；**其测试已入门**（`scripts/ci-gate.sh`），**门首次运行即抓出 2 个长期失败**（`doctor.rs` 解析，修复中 `bash-11`） |
| **M10** | **编排器**（原"版本控制"） | `git` CLI 薄封装 + `git bisect run` 接"构建 + QEMU + 断言" | 新 crate `crates/princess-bisect` | 新增：bisect 端到端脚本 + 负样本 | **待做（P-B）** |
| **M11** | **插件管理** | 声明式 manifest（面板 / 命令 / 模板 / 主题）+ 能力模型 | 新 crate + 前端加载器 | 新增：manifest 校验 + 越权拒绝负样本 | **待做（P-C）；原生插件最后** |
| **M12** | **第二前端渲染后端（可选）** | 与 Web+JS 前端**并列可切换**的原生渲染路径（自绘 GUI，GPU 后端）。**不是"加速某些视图"** | 新目录 `apps/native/`（Rust）+ 共享的视图模型契约在 M0 | 新增：**视图模型单测（无头）** + 离屏渲染哈希比对 + 与 Web 前端的**同构断言**（同一输入 → 同一视图模型） | **尚未开工（P-E，先做 spike）**；详见 §五 |

## 二、边界规则（谁不许做什么）

1. **引擎↔UI 只有 M0 一条路**；不得新增"模块总线"（会产生第二真相来源，违反 D23）。
2. **M0 冻结**：改 core 必须先改契约 + 加决策 + 加验收。
3. **不自研成熟协议栈**：编辑器/LSP（D17）、VCS（D29.2 ⑥）、执行器（D29.2 ④）一律薄封装或编排。
4. **执行真相唯一**：只有 QEMU 算数；任何模拟器/解释器**不得进入断言路径**。
5. **M7 的常量不可改**（D3/D26）：横幅、故障 RIP `0x100b3d`、符号名。
6. **单目录所有权**（D15）：跨模块改动需主 Agent 授权，任务书显式写明。
7. **产品代码不得包含开发机约束**（D25）。
8. **M12 是"可选前端"，不是唯一前端**：Web+JS 保持**默认与参考实现**；任何功能都不得只在原生前端实现，否则无 GPU/驱动的用户直接失去该功能。M12 **不得**成为引擎层（`crates/` 里的分析/解析）的逻辑依赖——它只能画，不能算；**两个前端必须消费同一份视图模型**。

## 三、分期（D29.5）

`P-A` 门 + 视图注册表 → `P-B` M10 编排器 → `P-C` M11 声明式插件 → `P-D` M5/M6 契约面与前端 → **`P-E` M12：先冻结视图模型，再做时限盒 spike**（见 §5.6） → "解释器"按指标（函数级测试 <1s）重新提案。

## 四、门的现状盘点（这是最该先补的）

| 模块 | 现在有门吗 | 缺什么 |
|---|---|---|
| M0 / M2 / M3 / M4 / M5 / M6 | ✅ | `cargo test` + **契约校验现已入门**（`ci-gate.sh` 第 3 步） |
| M1 | ⚠️ 手工 | 无 CI 调用（GUI 段与版本放宽是本轮才补） |
| M7 | ⚠️ 手工 | `smoke-ci.sh` 不在任何 CI |
| M8 | ✅ | 已入门（P-A）：124 个前端测试 + 视图注册表的通用挂载保证 |
| M9 | ⚠️ | 已入门，但**当前是红的**：`doctor.rs` 2 个失败（历史遗留，正修）。这正是门第一次运行的价值 |
| M10 / M11 / M12 | — | 尚不存在 |

**一句话结论**：D29.4 第一步已达成 —— 现在有一道门（`scripts/ci-gate.sh` + `.github/workflows/ci.yml`），覆盖 M0/M8/M9 与引擎；**且它第一次全量运行就抓出了 `doctor.rs` 的 2 个历史失败**。当前门为红，修完即转绿；M10 / M11 / M12 仍不存在，按 §三 的分期推进。

## 五、M12 第二前端渲染后端：批判与前置条件

> 用户意图（2026-09-12）：前端要有**两种可选渲染方式** —— (a) 现在的 Web+JS（Tauri webview），(b) **自己写的 Vulkan 渲染引擎**。

### 5.1 先把三条事实摆平（否则方向会偏）

1. **Vulkan 不是 UI 框架，它是画三角形的 API。** 一个代码编辑器真正需要的是：文本整形（HarfBuzz）、字形图集与亚像素排版、CJK/IME、软换行、选区与多光标、滚动与虚拟化、剪贴板与拖放、DPI 缩放、无障碍、输入法预编辑……**这些没有一样是 Vulkan 提供的**。自研 Vulkan 渲染器 ≈ 自研一整套 GUI 工具包。
2. **DOM 那条路本来就已在 GPU 上**（WebKitGTK 2.50.6 用平台 GL/EGL 合成）。所以 M12 的收益**不是"把渲染搬到 GPU"**，而是"**换成原生、可控、无 webview**"。卖点要写对：延迟、确定性、**去掉 WebKitGTK 依赖**（本项目的 13 个 GUI 系统包、BUG-004/005 全来自它），而不是"加速"。
3. **自研 Vulkan 渲染器会作废前端既有投资**：CodeMirror 6、`lsp/client.ts` 的 D17 分工、102 个前端测试、契约类型层；而 M8 的视图注册表、BUG-008 调试面板、P5/P7 面板都还在路上——**它们会全部需要在两个前端各实现一遍**。

### 5.2 划算与不划算的部分（公道话）

- **划算的是"引擎/契约边界"**：M0 已经是**传输无关的 JSON 契约**（24 命令、`{ok,data}|{ok,error}` 信封、`princess:event` 事件流）。所以"再来一个前端"在**架构上是可行的**，不需要动引擎——这一点是用户直觉正确的地方。
- **不划算的是"UI 本身"**：成本不在渲染，而在**编辑器与所有面板**。两个前端 = 两套视图、两套测试、两倍未来功能成本（后面还有 5–8 个待做面板），而且现在**一个 CI 都没有**。

### 5.3 让这条路变可行的唯一形态：**先冻结"视图模型"，两个前端只换渲染**

1. **M0 增加"前端无关的视图模型"**：每个视图由"引擎数据 → 视图模型（纯 JSON/纯数据）→ 渲染器"三段组成。视图模型是**共享资产**，其单测是**共享门**；Web 与原生只是两个渲染器。
   **没有这一层，第二前端就是把工作量翻倍；有了这一层，第二前端才是一次"渲染器替换"。**
2. **不要手写 Vulkan**：用 wgpu 系成熟栈（`egui`/`eframe`、`iced`、或 GPUI 风格），Vulkan 只是它的后端之一（本机可回退 lavapipe 做无头验证）。手写 Vulkan 的诚实工作量是"**以年计**"，并且在和 Zed/Helix 已有的东西竞争。
3. **可选、非唯一**（沿用边界规则 8）：Web 前端保持**默认与参考实现**；原生前端用开关启用（如 `--ui=native`），任何功能都不得只在原生前端实现。
4. **先 spike、后承诺**（时限盒）：只做一个**纵向切片**——打开工程 → 构建 → 事件流 → 只读显示 `kernel.c`——并产出实测：实现耗时、代码行数、输入延迟、启动时延、以及 clangd 集成的可行性结论。**拿到数字再决定**（D14）。

### 5.4 门槛（无显示器也要能验收）

- **视图模型层**：纯 Rust/纯数据单测（无头可跑）。
- **同构断言**：同一份事件流喂给两个前端，产出的**视图模型必须逐字相等**（这是"两个前端不漂移"的唯一硬保证）。
- **渲染层**：离屏渲染 → 图像哈希与黄金哈希比对（lavapipe 即可，无显示器可跑）。
- **做不到以上三点的第二前端，不要合入。**

### 5.5 本机现状（决定"能不能在本机验证"）

- `lspci`：**QEMU 标准 VGA（1234:1111）**，无硬件 GPU；Vulkan ICD 中唯一可用的是 **`lvp`（lavapipe 软件光栅）**。→ 本机能做**正确性**验证，**不能**做"快了多少"的验证。
- 前端今天 `canvas`/WebGL 用量为 0；GUI 由 WebKitGTK 2.50.6 渲染。

### 5.6 顺序（不能颠倒）

`M0 视图模型契约` → `M8 视图注册表（Web 侧先跑顺）` → `M12 spike（时限盒，一个纵向切片 + 实测数字）` → 决定是否投入。**在没有视图模型层之前动手写 Vulkan 前端，是最贵的一条路。**
