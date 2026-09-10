# PrincessIDE 决策日志（ADR）

> 用途：多 Agent 并行施工时，**同一件事只能被决定一次**。任何 Agent 在动手前先读这里；发现冲突先在报告里提出，由主 Agent 裁决后统一更新本文档。
> 状态：生效中。最后更新随每次裁决追加，**不删除历史条目，只标记「已推翻」并给替代项**。
> 每条标注证据档位：**[实测]** = 本工作区真实执行过命令；**[有源]** = 有权威出处；**[裁决]** = 主 Agent 的工程判断。

---

## D1 底座：Tauri 独立桌面应用（非 VS Code 扩展）
- **决策**：Tauri v2 + Rust 引擎 + Vite/TS 前端，仅 Linux。
- **理由**：UI 完全可控、引擎可独立无头验收、体积小。代价是编辑器/LSP/调试器都要自建，已用「引擎与界面分层」对冲。
- **影响**：核心价值必须在 `crates/` 里可机器化验收；`apps/` 只做薄壳。 **[裁决]**

## D2 目标平台与引导路径：x86_64 + Multiboot2 + GRUB ISO
- **决策**：参考内核与 v1 的 E2E 路径为 `x86_64 + Multiboot2 + grub-mkrescue ISO + qemu -cdrom -boot d`。
- **理由**：P0 实测证伪了「x86_64 走 `-kernel`」——QEMU 的 multiboot option ROM 只接受 ELF32，报 `Cannot load x86-64 image, give a 32bit one.`；而夹具必须是真 x86_64。 **[实测]**
- **影响**：`boot = "multiboot1"` 在 `princess.toml` 里保留字段，但 v1 默认走 `multiboot2`。

## D3 唯一权威测试夹具：`fixtures/refkernel/`
- **决策**：`fixtures/refkernel/` 是唯一参考内核。**任何代码、脚本、模板不得引用 `_work/`、`.research*/`、`_toolchain/`**（这些是被中断的调研 Agent 留下的废弃草稿）。
- **固定断言常量**（改这些等于改契约）：
  - 横幅：`PrincessIDE reference kernel booted`
  - 故障符号：`refkernel_fault_probe`（`fixtures/refkernel/kernel.c:100`，`ud2` → #UD，故障 RIP `0x100b3d`）
- **[裁决]**，P0 实测通过由主 Agent 独立复核。

## D4 语言支持顺序：C + 汇编优先
- **决策**：v1 语言服务只做 C（clangd）；Rust/C++/Zig 只留接口与路线图。
- **理由**：clangd 一套覆盖 C；多语言并行会把 P1/P2 周期翻倍。 **[裁决]**

## D5 汇编的 IDE 后端：**不用 clangd**
- **决策**：clangd **不作为汇编的智能后端**（NASM 路径实测是死的；`.S` 支持也残缺）。汇编只提供语法高亮 + 调试期反汇编视图；跳转/补全/诊断不承诺。
- **证据**：clangd 报告 §3 全部实测。 **[实测]**
- **影响**：UI 不得向用户承诺「汇编也能跳转定义」。

## D6 clangd 版本：**clangd-16**，不是 P0 装的 clangd-14
- **决策**：语言服务用 `clangd-16`（bookworm main 为 `1:16.0.6-15~deb12u1`，全能力集已实测）。P0 装进 `.toolchain/` 的 clangd 14.0.6 仅够冒烟，**不足以承载 IDE 语言服务**。
- **待办动作**：扩展 `scripts/bootstrap-toolchain.sh` 增装 clangd-16（调研报告 §0.2 有已实测的解包安装步骤）。 **[实测]**

## D7 clangd 工程配置的硬约束（防 glibc 头污染）
- **决策**：内核工程必须由 IDE 生成/校验 `.clangd` 配置，至少包含：
  - 显式写死 triple（`--target=x86_64-unknown-none`），**不依赖 `--query-driver` 自动推断**；
  - **用 `-nostdlibinc`，绝不用 `-nostdinc`**：因为 clang 的 `-nostdinc` 会把 **clang 自带的 freestanding 头也一起删掉**，导致内核工程满屏诊断；
  - 在 `CompileFlags.Remove` 里删掉 **gcc 专用 flag 的具体黑名单**（研究 §2.6）：`-fno-tree-loop-distribute-patterns`、`-fconserve-stack`、`-mpreferred-stack-boundary=*`、`-fno-var-tracking-assignments`、`-fno-ipa-icf`、`-mno-direct-extern-access`。
  - **⚠️ 禁止用通配 `-W*` 做 Remove**（A1 实测）：它会把 `-Wall -Wextra` **一起删掉**，导致**告警诊断整片丢失**——语言服务会显得"很干净"，实际是瞎了。必须用上面那张具体黑名单。
- **证据**：报告 §2.4 / §2.5 / §2.6 / §2.7 全部实测。 **[实测]**
- **⚠️ 勘误（A1 独立复核）**：研究报告 §2.4 的**标题**称「`-nostdinc` 会让 clangd **直接崩掉**」，**实测无法复现**——clangd-16 并不崩溃，而是**正常报 16 条诊断、以退出码 3 结束**（3 = 「有诊断」，不是崩溃）。**机制层面的实质结论仍成立**（cc1 参数可见 `-nostdsysteminc -nobuiltininc`，证明 freestanding 头确实被删）。**故决策不变，但依据从「会崩溃」改为「满屏诊断」**。详见 `docs/reports/a1-clangd16.md` §A1-7。
- **附加认知**：target triple 本身**不能**阻止 glibc 头污染；clangd 报「0 errors」**不等于**能编译过。

## D8 `compile_commands.json` 生成策略
- **决策**：首选 **make + bear 3.1.1**；需要零构建开销时用**手写 wrapper shim**；CMake 用 `CMAKE_EXPORT_COMPILE_COMMANDS`。**不用 `clang -MJ`**（坑深）。
- **格式约定**：条目用 `arguments` 而非 `command`；`directory` 用**绝对路径**；放在构建目录顶部的 `compile_commands.json`。 **[实测]**

## D9 QEMU 串口与异常检测的正确姿势
- **决策**：串口捕获固定 `-display none -serial stdio -monitor none`（或 monitor 走 unix socket）。
- **禁止**：`-nographic`（BIOS/iPXE 噪音与 monitor 复用会污染流）；`-serial stdio` 与 `-monitor stdio` 并用（直接报错）。
- **三击故障判定**：必须同时开 `-d cpu_reset` —— `-d int` 里**没有** `Triple fault` 文本。**`-no-reboot` 的退出码 0 不能当作发生 panic 的凭据。**
- **#PF 判定前置条件**：必须先确认 `CR0.PG=1`，否则非法地址只是物理读、不产生页故障。
- **机器接口**：优先用 **QMP 结构化命令**，HMP 文本解析只作兜底。
- **证据**：B 报告 §2.3 / §3.1 / §3.2 / §3.3 / §4.3 全部实测。 **[实测]**

## D10 gdbstub 与架构错配（P4 必读）
- **已知坑**：32 位内核配 `qemu-system-x86_64` 的 gdbstub 会出现 `Remote 'g' packet reply is too long` 与回溯乱码。
- **影响**：P4 的调试后端必须做**架构自检**并在错配时给出明确错误，而不是返回乱数据。 **[实测]**

## D11 调试协议路线：**暂缓，等调研 C 结论**
- **现状**：初定「Rust 侧自写 DAP 适配层包装 GDB/MI」。调研 C 正在核实 GDB 14+ 自带的 `gdb -i=dap` 能否直接用（若能，可省掉整层适配）。
- **决策规则（预先定好，避免返工）**：若 `gdb -i=dap` [实测] 可用且能力覆盖 P4 验收项 → **直接用内置 DAP**；否则自写 MI→DAP 适配层。 **[裁决]**

## D12 模型路由：~~只用 DeepSeek Flash，Mimo 弃用~~ → **已推翻，见 D12'**
- ~~原决策：一律 `deepseek-official/deepseek-v4-flash`，禁用 `deepseek-v4-pro`，Mimo 通道弃用。~~
- **推翻原因**：先前判定「Mimo 不可用」是**我自己的错**——探测时模型 id 猜成了 `mimo`（无效 id），并非通道故障。 **[实测]**

## D12' 模型路由（现行）
- **默认**：子 Agent 一律用 **`xiaomi-token-plan-cn` / `mimo-v2.5-pro`**（MiMo-V2.5-Pro）——机械活、调研、文档、模板、可视化、打包等。
- **例外**：**只有重点任务**用 **`deepseek-official` / `deepseek-v4-flash`**——核心引擎实现（P2-B `princess-build`/`run`/`symbol`）、关键架构决策、最终独立验收。
- **禁用**：`deepseek-v4-pro`（用户明确要求不用）。
- **模型 id 权威来源** [实测]：`@earendil-works/pi-ai/dist/providers/data/xiaomi-token-plan-cn.json`
  - provider `xiaomi-token-plan-cn`，baseUrl `https://token-plan-cn.xiaomimimo.com/v1`，api `openai-completions`
  - 可用模型：`mimo-v2.5-pro`（MiMo-V2.5-Pro，1M 上下文 / 128K 最大输出）、`mimo-v2.5`
- **派发机制（主 Agent 已实测打通，取代早先的绕过方案）** [实测]：
  - **Mimo（默认走这条）**：后台执行 `dsh --profile headless "<完整任务书>"`。headless profile 的默认模型已通过 `~/.dsh/profiles/headless/cordis.patch.yml` 改为 `xiaomi-token-plan-cn/mimo-v2.5-pro`，**不占用交互会话、无需重启、可真正后台并行**。
    - **必须带 `DSH_PERMISSION_MODE=danger-full-access`**，否则 headless 默认 `workspace-write` + 审批 `ask`，而无应答者会 fail closed。
    - **必须在项目根目录启动**（`workspaceRoot = process.cwd()`）。
    - 已验证：composed config 生效为 mimo，且真实跑通一个带 bash 工具调用的任务（返回 `42`，exit 0）。
  - **Flash（重难点专用）**：同一命令加 `--patch scripts/dispatch/flash.patch.yml`，把默认模型覆盖回 `deepseek-official/deepseek-v4-flash`（已实测两路各自解析正确）。
  - **两条通道的取舍（重要）**：

    | 通道 | 模型 | 后台 | 中途 steering | 完成通知 |
    |---|---|---|---|---|
    | `bash` + `dsh --profile headless` | **Mimo**（默认） | ✅ | ❌ 无 agent id | ❌ 需 `job_output` 轮询 |
    | `subagent` 工具 | Flash（会话默认） | ✅ | ✅ `send_message` | ✅ 自动推送 |

    → **Mimo 任务必须一次给足完整任务书，并要求 Agent 把进度与结论写进文件**，由我轮询；不能像 `subagent` 那样中途纠正。
  - `workflow` 仍可用（能逐路指定模型），但它**前台阻塞**，只适合短任务。
  - **不采用的方案**：宿主的 `subagent-model-selection` 设置——它只在插件安装时构建工具 schema，需重启 web 会话才生效，而 headless 通道已完全覆盖需求，故不动用户会话。
  - **并发上限（配合 D16）**：headless 每次运行 = 一个新 Node 进程 + 一个 Agent（约 200~300MB）。本机可用内存约 1.4G，**同时最多 2~3 个 headless 任务**，重活必须错开。

## D13 环境与权限现状（P0 之后发生变化）
- 文件沙箱已放开为 **danger-full-access**；**审批提示已关闭**（不要请求提权，直接执行）。
- 因此**允许 `apt-get install`**。P0 的「.deb 解包到前缀」方案仍保留（可复现、不污染系统），但 GUI 依赖建议走系统级安装。
- 已知环境限制：无显示器（GUI 不可肉眼验证）；**KVM 不可用**（TCG 软件模拟，QEMU 启动慢，测试要给足超时）；网络 `static.rust-lang.org` 实测仅 ~62 B/s，必须走清华镜像（`RUSTUP_DIST_SERVER`），crates.io/npm/apt 正常。 **[实测]**

## D14 验收纪律
- **一切「完成」必须有真实命令输出 + 退出码支撑**；禁止编造、禁止把未跑过的说成通过。
- 每个实现 Agent 的产出由**独立验收 Agent** 复核（写者与判者分离）。
- **GUI 视觉验证不计入自动验收**，由用户本地确认；无头环境下不许试图截图或启动窗口。
- 负样本必测（构建失败、启动超时且无孤儿进程、符号缺失）。 **[裁决]**

## D15 Agent 施工卫生（P0 期间踩过）
- **单目录所有权**，跨目录改动必须主 Agent 授权。
- 临时文件一律放 `.scratch/<agent 名>/`，**禁止污染仓库根目录**（P0 期间出现过 `_work/`、`_toolchain/`、`.research*/` 等并发杂物）。
- **Agent 禁止执行改动 git 的命令**（`add`/`commit`/`checkout`），提交由主 Agent 统一做，避免并发踩索引锁。 **[裁决]**

## D16 并发与内存纪律（本机无 swap，硬约束）
- 本机 **4 核 / 3.8G 内存，可用约 1.7G，且没有 swap**。多 Agent 并行编译极易互相挤爆；OOM 会**静默杀掉别人的构建**，表现为「莫名其妙的失败」。
- **规则**：编译/构建命令必须有界并行——`make -j2` 为上限、`CARGO_BUILD_JOBS=2`；**禁止 `-j4` / 满核**。
- 重活（大型源码编译、Tauri `cargo build`、apt 批量装包）**不要与其它同类重活同时开**；能后台跑就后台跑并给足超时。
- 出现莫名构建失败时，**先怀疑内存压力**，再怀疑代码。
- 大型源码编译优先级最低：能拿二进制包就不要源码编译（例如 GDB 新版优先从 apt/backports 解包）。 **[实测]**
- 证据：5 Agent 并行时 load 3.50、可用内存 1.7G、`Swap: 0B`；调研 C 当时正在 `make -j3 all-gdb`（已提醒降到 `-j2` 并设时间盒）。

## D17 LSP 分工：引擎管「内核特化配置」，前端管「编辑器交互」
- **决策**：
  - **引擎侧（Rust）**负责 clangd 的**工程配置生成与校验**（`.clangd`：triple 写死、`-nostdlibinc`、`CompileFlags.Remove` 清理 gcc 专用 flag；`compile_commands.json` 的生成与校验）——这是**内核特化**的部分，也是我们真正的差异化，**可无头机器化验收**。
  - **前端侧（TS）**用**成熟的 LSP 客户端库**（CodeMirror 6 的 `lsp-client` 或 Monaco 的 `monaco-languageclient`）做编辑器交互（补全/跳转/悬停/诊断 UI），经 Tauri 传输桥接本地 clangd 进程。
- **理由**：我们的差异化不在重造 LSP 协议栈。在 Rust 里自实现一遍 JSON-RPC/LSP 能力协商是纯成本，且前端侧同样能用 vitest 无头验证。 **[裁决]**
- **影响**：
  - **不新建** `crates/princess-lsp`。
  - `.clangd` 与 `compile_commands.json` 的生成归 **`crates/princess-build`（P2-B/B1）**。
  - **P3 及以后的前端 LSP 工作不得在 Rust 侧重写协议栈**；若发现成熟库不够用，先写进交接摘要由主 Agent 裁决。
- **状态**：已通知 P3-A（防止其正在做的语言服务接入方向走偏）。

## D18 语言服务与编译数据库的使用纪律（A1 实测，P3/P2-B 必须遵守）
- **clangd 必须显式使用 `clangd-16`**：不带版本号的 `clangd` **按设计仍然是 14**（零回归），拿错了能力就残缺。`scripts/env.sh` 已导出 `PRINCESSIDE_LANG_SERVICE_CLANGD=clangd-16`，引擎与前端一律用它。
- **bear 必须通过 PATH 里的启动器调用**（`.toolchain/bin/bear`）：该启动器补齐 4 个硬编码路径；直接调 `prefix/usr/bin/bear` 会失败或**静默漏条目**。
- **生成 `compile_commands.json` 前必须先 `make clean`**：实测第二次 bear 会用空数组 `[]` **覆盖**已有条目（这是很难察觉的数据损坏）。
- 不带版本号的 `clang` 同样是 14；LLVM 16 一律走带版本号的名字（`clang-16`、`clangd-16`）。 **[实测]**

## D19 crates.io 必须走国内镜像（否则所有 Rust 构建慢性瘫痪）
- **背景（实测）**：本机国际带宽极差。官方 `static.crates.io` 下载 **0~35 KB/s**，790KB 的包 15 秒都传不完（只传了 535KB 就被截断）；`static.rust-lang.org` 仅 ~62 B/s。
- **症状**：`cargo build` 卡在**依赖下载**阶段 10 分钟以上，连 `target/` 目录都没生成；`.toolchain/cargo/registry` 有 14M 索引缓存但 **`.crate` 文件数为 0**。**极易被误判成"代码编译不过"或"卡死"。**
- **决策**：工作区 `.cargo/config.toml` 配置 `[source.crates-io] replace-with = "ustc"`，镜像 `sparse+https://mirrors.ustc.edu.cn/crates.io-index/`；备选 rsproxy（已作为注释保留在文件里）。
- **实测速度排序**（tokio-1.40.0.crate，790KB）：**USTC 2.1 MB/s** > rsproxy 982 KB/s ≫ 官方 35 KB/s（且截断）> 清华 905 B/s（其 `config.json` 的 `dl` 指回官方慢速源，等于没用）。
- **修复验证**：`cargo fetch`（serde + serde_json + tokio full features）**10.7 秒**下完 **31 个 crate**，exit 0；修复前 10 分钟 0 个。
- **纪律**：任何 Agent 遇到「cargo 构建很久没动静」——**先查 registry 里 `.crate` 数量是否在增长**，再怀疑代码；**禁止**回退 `.cargo/config.toml` 的源替换。 **[实测]**

---

## 开放待办（Open Actions）

| # | 事项 | 归属 | 阻塞谁 |
|---|---|---|---|
| A1 | 扩展 bootstrap 增装 **clangd-16**（见 D6） | 待派 | P3 语言服务、P2 诊断解析 |
| A2 | 调研 C 出结论后落定 D11（自写适配层 vs 内置 DAP） | 进行中 | P4 全部 |
| A3 | 清理废弃草稿（`_work/`、`_toolchain/`、`.researchA/`）——**等调研 C/D 用完 `.researchC/.researchD` 后再删** | 主 Agent | 无（已 gitignore） |
| A4 | 主编排：P2-B（`princess-build`/`run`/`symbol`）派发，依赖 P2-A 的 `princess-core` 类型 | 主 Agent | P2 验收 |
| A5 | 编辑器组件最终选型复核（P3 Agent 自决，主 Agent 复核） | P3 进行中 | P3 验收 |
| A6 | **是否开启 `subagent-model-selection`**：开启后我才能「后台派发 + 指定 Mimo 模型」；不开启则路由 Mimo 只能用前台阻塞的 `workflow`（见 D12'） | **待用户决定** | 我的派发方式与效率 |
