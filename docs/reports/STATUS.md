# PrincessIDE 项目状态看板

> 主 Agent 维护。用户离开期间（约 8 小时）自主推进，**所有决策按推荐项自定并写入 `00-decisions.md`**。
> 每轮更新。**本文件是「项目现在到哪了」的唯一权威入口。**

---

## 一、阶段状态

| 阶段 | 内容 | 状态 | 独立验收（主 Agent 亲验） |
|---|---|---|---|
| **P0** | 环境工具链 + 参考内核夹具 + 冒烟/符号化脚本 | ✅ **完成** | ✅ `doctor` exit 0（24→28 工具）、`smoke-boot` exit 0（横幅 + `#UD` + `FAULT_RIP=0x100b3d`）、`symbolicate` exit 0（`kernel.c:100`） |
| **A1** | 补装 clangd-16 + bear，并复核调研结论 | ✅ **完成** | ✅ 我亲跑 `doctor` exit 0（`clangd-16 16.0.6`、`bear 3.1.1`）+ `smoke-boot` 回归 exit 0（A1 改过 env.sh 后 P0 链未被破坏） |
| **P6** | x86_64 Multiboot2 内核工程模板 | ✅ **完成**（提前于计划） | ✅ 我亲跑 `templates/verify-template.sh` → exit 0、`37 contract field paths present, 0 unknown`、横幅断言通过；脚本自带负样本（改错横幅 → 非零退出） |
| **调研 A** | clangd 在 freestanding 内核工程上可用 | ✅ 完成 | ✅ 其 §2.4 标题结论被 A1 对抗复核**推翻并已勘误**（`-nostdinc` 不崩溃，只报诊断） |
| **调研 B** | QEMU 运行编排 + panic 符号化 | ✅ 完成 | ⏳ 部分结论已被 P0/A1 实践印证（GRUB ISO 路径、串口约定） |
| **P3-A** | Tauri 外壳 + GUI 依赖 + 纵向切片 | ✅ **完成** | ✅ 我亲跑 `check-contract.mjs` → **`result: ALIGNED`**（21 命令 × 三方逐字一致、10 错误码全对）；`pnpm -C apps/desktop test` → **8 文件 / 81 测试全绿**（含真 CodeMirror 编辑器、事件重放 25 项）；`cargo build`/`pnpm build`/pkg-config 6/6 由其报告提供证据 |
| **P2-A** | Cargo 工作区 + `princess-core` + `princess-cli` | 🔄 进行中（已被 D19 镜像修复解堵，正在真编译） | — |
| **调研 C** | 调试协议路线（`gdb -i=dap` 能否直接用） | 🔄 进行中 | — |
| **调研 D** | ELF/反汇编/hex/页表工具选型 | ✅ **完成** | ✅ 报告逐项表态（`object` 0.40.0 / `gimli`+`addr2line` / `iced-x86` 1.21.0），已冻结为 **D20** |
| **P5 前置** | 开启分页的内核夹具 + QEMU monitor 样本 | 🔄 交付中（报告待出） | 🔍 我已只读抽查：`CR0.PG=1`、`CR3=0x104000`、真实 `#PF @0x400000` 且错误位解码正确；`info-tlb` 1021 行非退化 → **夹具可用** |
| **P2-B** | `princess-build` / `run` / `symbol` | ⏸ 等 core 冻结 | — |
| **P2-C** | 全链集成 + 事件夹具 | ⏸ | — |
| **P4** | 调试器（DAP + 寄存器/内存/栈/反汇编） | ⏸ 等调研 C | — |
| **P5** | 可视化（ELF/hex/反汇编/页表/GDT-IDT/monitor） | ⏸ 等调研 D | — |
| **P7** | AI 辅助（OpenAI 兼容抽象层） | ⏸ | — |
| **P8** | 产品化（打包、文档、冒烟 CI） | ⏸ | — |

---

## 二、关键产出（可直接查看）

| 路径 | 内容 |
|---|---|
| `docs/spec/00-decisions.md` | **决策日志（ADR）D1~D18** —— 所有冻结决策与勘误，Agent 动手前必读 |
| `docs/spec/10-contracts.md` | 接口契约：事件模型 / IPC / 错误码 / `princess.toml` / 目录归属 |
| `docs/spec/20-acceptance.md` | 各阶段可机器执行的验收标准 |
| `docs/spec/40-dispatch-plan.md` | 派发口径：通用前置、批次表、B1/B2/B3 brief、**怎么派活** |
| `docs/reports/a1-clangd16.md` | 工具链升级报告（含对调研结论的对抗复核） |
| `docs/reports/p6-templates.md` | 模板交付报告 |
| `templates/x86_64-multiboot2/` + `templates/verify-template.sh` | **可用的内核工程模板 + 自检脚本** |
| `fixtures/refkernel/` | 唯一权威参考内核夹具（横幅 `PrincessIDE reference kernel booted`，故障符号 `refkernel_fault_probe`） |
| `scripts/` | `bootstrap-toolchain.sh`（幂等）/ `env.sh` / `doctor.sh` / `smoke-boot.sh` / `symbolicate.sh` |
| `scripts/dispatch/flash.patch.yml` | 把 headless Agent 临时切回 DeepSeek Flash 的覆盖层 |

---

## 三、运行机制（自主期生效）

- **模型路由（D12'）**：子 Agent 默认 **`mimo-v2.5-pro`**（token 不限量）；**只有重难点**（核心引擎、关键架构决策、最终独立验收）用 `deepseek-official/deepseek-v4-flash`；**禁用 `deepseek-v4-pro`**。
- **后台派发**：Mimo 走 `dsh --profile headless`（后台 bash job，不占用用户会话）；重难点走 `subagent` 工具（可 `send_message` 纠偏 + 自动通知）。
- **内存纪律（D16）**：4 核 / 无 swap / 可用约 1.1G；编译 `-j2` 上限；**同时最多 2~3 个 headless Agent**；莫名构建失败先怀疑内存。
- **网络纪律（D19，关键）**：本机国际带宽极差，**crates.io 官方源下不动**（0~35 KB/s）。已在 `.cargo/config.toml` 配置 **USTC 镜像**（2.1 MB/s），实测 `cargo fetch` **10.7 秒下完 31 个包**（修复前 10 分钟 0 个）。遇到「cargo 构建很久没动静」先看 registry 里 `.crate` 是否在增长，别误判成代码问题。
- **QEMU 纪律**：必须 `timeout` 包裹 + **按进程组收尾**，禁止留孤儿（孤儿会污染 P2-7 的「无孤儿进程」断言）。

---

## 四、自主期已自行拍定的决策（等你回来可复核）

| # | 决策 | 依据 |
|---|---|---|
| D7 勘误 | `.clangd` 的 `CompileFlags.Remove` **禁止通配 `-W*`**（会连 `-Wall -Wextra` 一起删，告警诊断全丢），改用具体黑名单 | A1 实测 |
| D7 勘误 | `-nostdinc` 的正确理由是「会删掉 clang 自带 freestanding 头 → 满屏诊断」，**不是**研究报告原标题说的「会让 clangd 崩溃」 | A1 四种形态对抗实测 |
| D18 | 语言服务必须显式 `clangd-16`；`bear` 走 `.toolchain/bin/bear` 启动器；生成 CDB 前必须 `make clean` | A1 实测 |
| D17 | LSP 分工：**引擎管内核特化配置**（`.clangd`/`compile_commands.json`），**前端用成熟库**做编辑器交互；**禁止在 Rust 侧重写 LSP 协议栈** | 工程判断 + 已通知 P3-A |
| P2 派发节奏 | P2-B **先单跑 B1，再并行 B2+B3**（避免同一 `target/` 锁互踩与 OOM） | 实测资源状况 |
| 新夹具 | 新增 `fixtures/paging-kernel/`（开启分页、触发 `#PF`）——P4 读 CR3 / P5 页表可视化**没有它无法验收** | 复查计划时发现缺口 |

## 七、运行环境加固（2026-09-11）

- **已启用 4G swap**（`/swapfile`，已写入 `/etc/fstab` 持久化），`vm.swappiness` 调为 20。
  此前自主期发生 **2 次全局 OOM**（B1、A9 各被打断一次）；现在有 swap 兜底，**OOM 风险大幅下降**。
  D21 的「严格串行」据此放宽为「**最多 2 路重型构建**」（见 D22）。
- swap **只防 OOM、不提升速度**；若观察到持续换页应主动降到 1 路构建。
- **P5 分页夹具**：`fixtures/paging-kernel/` + `fixtures/qemu-monitor/` 已交付，并由**主 Agent 亲跑验证**（`run.sh` exit 0；横幅与真实 `#PF` 两条断言 PASS）。将作为 **P4-2（读 CR3）/ P5-5（页表逐级解析）** 的正式测试夹具。
