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

## D11 调试协议路线：**已落定 —— 用 GDB 内置 DAP + 一层薄能力层**
- **原设想（已被推翻）**：「Rust 侧自写完整 DAP 适配层包装 GDB/MI」。调研 C 实测证明这是**不必要的重活**。
- **裁决（[实测] 支撑）**：
  1. **主干直接用 GDB 内置 DAP**：`gdb -i=dap` 在 **gdb 16.3** 上实测可用，`initialize` 返回真实能力集（`supportsReadMemoryRequest` / `supportsWriteMemoryRequest` / `supportsDisassembleRequest` / `supportsSteppingGranularity` 等）。**前端只对接标准 DAP，不碰 MI**。
  2. **但必须补一层薄的"能力补齐"层（不是全量转换）**，因为实测发现内置 DAP **没有硬件断点**：`_set_one_breakpoint` 只构造 `gdb.Breakpoint`，`info breakpoints` 显示 `Type breakpoint` 而非 `hw breakpoint`——**而内核入口断点必须用 `hbreak`**。还需补：物理内存读写、寄存器面板、以及一个 REPL 逃生舱（monitor/QMP 交互）。
  3. **版本硬要求**：工作区自带的 **gdb 13.1 实测 `Interpreter 'dap' unrecognized`（没有 DAP）**，P4 必须用 **gdb ≥14**（当前可用的是调研 C 解包的 **16.3**）。
- **能力已提前验证**：对 `fixtures/paging-kernel/` 的真实 DAP `stackTrace` 拿到了**完整源码级回溯**（`paging_fault_probe` @kernel.c:120 → `kernel_main` @157 → `_start` @194）——即 **P4-3 的验收形态在 P4 开工前就已被证明可达**。
- **⚠️ 重要勘误**：调研 C 第一棒留下的 `kernel_test.out` 里有**多处假阴性**（把 `monitor` 报成"not supported"、`pause` 超时、`stackTrace` 崩溃），第二棒复现后确认**那些都是脚本把会话推进坏状态所致，干净会话下全部可用**。**P4 不要采信第一棒的产物文件，以第二棒的报告为准。**
- 依据：`docs/research/C-debug-protocol.md`（523 行，含独立「反例与风险」「未实测」两节；未实测项：SMP 多核、LLDB/CodeLLDB、cppdbg、LA57）。 **[实测]**

## D12 模型路由：~~只用 DeepSeek Flash，Mimo 弃用~~ → **已推翻，见 D12'**
- ~~原决策：一律 `deepseek-official/deepseek-v4-flash`，禁用 `deepseek-v4-pro`，Mimo 通道弃用。~~
- **推翻原因**：先前判定「Mimo 不可用」是**我自己的错**——探测时模型 id 猜成了 `mimo`（无效 id），并非通道故障。 **[实测]**

## D12' 模型路由（现行）
- **默认（现行，用户指令）**：**所有** Agent 一律用 **`xiaomi-token-plan-cn` / `mimo-v2.5-pro`**——实现、调研、文档、模板、可视化、打包、**代码审查与分析**，全覆盖。**Mimo token 不限量。**
- **⚠️ DeepSeek 配额告急（用户指令）**：DeepSeek 的 token 快用完，**原则上不再派发任何 DeepSeek Agent**（原「重点任务用 Flash」的例外条款**暂停**）。凡事先用 Mimo；**确实非 DeepSeek 不可时才提出并说明理由**。
- **禁用**：`deepseek-v4-pro`（用户明确要求不用）。
- **主 Agent 自身也在消耗 DeepSeek**：本会话（web profile 默认模型）跑的就是 `deepseek-v4-flash`。因此主 Agent **必须主动压缩自身开销**：报告从简、不做无谓的大文件重读、批量工具调用、避免把大段输出灌进上下文。用户可在 GUI 的模型设置里把本会话切到 Mimo；或由主 Agent 预置 web profile 覆盖层，**下次会话启动即生效**（见 A10）。
- **独立验收的对抗性如何保障**（DeepSeek 退出后的补偿措施）：写者与判者仍分离，但改由**结构保证**而非模型差异——判者必须：① 亲自执行验收命令、② 只看**原始输出与退出码**不看自述、③ 必须打**负样本**、④ 必须**主动找反例**（调研 C 第二棒正是靠这条推翻了第一棒的假阴性）。
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

## D20 二进制/可视化技术栈（依调研 D 的实测结论冻结）
- **ELF / 目标文件解析** → **`object` 0.40.0**（**明确不用** `goblin`、不用 `elf`）
- **DWARF 与源码行** → **`gimli` 0.34.0 + `addr2line` 0.27.1**（两者都要，分层使用）
- **反汇编** → **`iced-x86` 1.21.0** 为主；`capstone` 0.14.0 **仅作多架构备选**
- **demangle** → `rustc-demangle` 0.1.28 + `cpp_demangle` 0.5.1，按符号前缀分派
- **对 UI 的硬性含义**：反汇编视图必须**「反汇编 + 源码行」并排呈现**，**不能只给一个行号当结论**（调研 D 实测指出：单看行号会误导）。 **[实测]**
- 依据：`docs/research/D-binary-lowlevel-tooling.md` §1（逐项结论 + 版本 + 许可证 + 风险）
- **影响**：B3（`princess-symbol`）与 P5（可视化）一律按此栈实现，不要另起选型。

## D21 并发上限收紧（实测 OOM 两次，必须串行化重活）
- **事件**：自主期发生 **2 次全局 OOM 杀进程**（dmesg: `oom-kill:constraint=CONSTRAINT_NONE,...global_oom`，被杀进程 `anon-rss ≈ 1.05 GB`）。
- **实际损失**：P2-B1（`princess-build`）**刚派出就被打断、一个文件都没产出**；调研 C 的工作进程也被杀且**其 Agent 已变为不可用（无法唤醒），报告至今缺失**。
- **根因**：3.8G 内存中**非项目进程已占约 1.7G**（`dsh web` ≈1G、`hermes dashboard` ≈490M、`claude` ≈220M），留给项目的仅约 2G；而**单个 rustc 编译大型依赖树可占数百 MB**，多路并发必然 OOM。
- **裁决（硬规则，收紧 D16）**：
  1. **同一时间只允许一路重型构建**（Rust 依赖树编译 / Tauri build / 批量 QEMU）。派发前先 `free -h`：**可用 < 1.2G 时不得启动新的重活**。
  2. 重型构建一律 **`CARGO_BUILD_JOBS=1`**（不再是 2）；`make` 上限 `-j2`。
  3. **不要同时派两个需要编译的 Agent**——宁可串行慢，也不要第三次 OOM。
  4. 构建「莫名其妙失败」时，**先查 `dmesg | grep -i oom-kill`**，再怀疑代码。
- **禁止**为此去杀用户的非项目进程（`hermes dashboard`、`claude` 等）——那是越界，不属于项目编排权限。 **[实测]**

## D22 已启用 4G swap（OOM 根治手段）——**D21 的串行规则据此放宽**
- **做了什么** [实测]：创建 `/swapfile`（**4 GiB**，ext 上 `fallocate` 成功）→ `mkswap` → `swapon` **成功**；已写入 `/etc/fstab`（**重启后仍生效**）；`vm.swappiness` **60 → 20**，持久化于 `/etc/sysctl.d/99-princesside-swap.conf`。
- **为什么 swappiness 取 20 而非默认 60**：Agent 多数时间在等待、而 rustc 爆发式吃内存；低 swappiness 让内核**优先回收 page cache**、少换出匿名页；同时避免重度换页把 **QEMU 基于超时的断言**（如 P2-7 的 `reason=timeout`）变成偶发失败。4G swap 仍为极端情况兜底，**不会再像之前那样直接 OOM**。
- **D21 据此放宽为**：
  1. 允许**最多 2 路**重型构建并发（不再严格串行）；**仍禁止 3 路以上**。
  2. `CARGO_BUILD_JOBS` 上限回到 **2**；`make` 仍 `-j2`。
  3. `free -h` 预检保留，阈值由 1.2G **放宽到 800Mi**（有 swap 兜底）。
  4. 其余不变：构建莫名失败**先查 `dmesg | grep -i oom-kill`**；**仍禁止**杀用户的非项目进程。
- **注意**：swap **只防 OOM、不提升速度**。若 `vmstat 1` 见到 `si/so` 持续非零（明显换页），应主动降到 1 路构建。

## D23 裁决 P2-A 提出的三项（core 不改、夹具不加 manifest、`exitCode` 可为 null）
1. **不扩展 `RunExitedPayload` / 不改 `classify_exit` 语义**。**[裁决]**
   - 理由（采纳 P2-A 的论证并经我复核）：契约 §2 的 `reason` 回答的是「**机器为什么停了**」，四值封闭即可；「**guest 出了什么故障**」已由 `run.fault` **正交**表达。若为「异常后停机」新增枚举值，等于让引擎去**猜 guest 内部状态**，违反契约 §6 规则 4；且此刻有 5 个 Agent 正编译在你冻结的 API 上，**动 core 的代价远大于收益**。
   - **UI 侧表达方式**：同一 `opId` 内先出现 `run.fault` 再出现 `run.exited{reason:"timeout"}` → 前端渲染成「已发生异常并停机」，无需引擎改动。
   - `timeout` 归因的**判据**（P2-A 已写入 `docs/reports/p2-core.md`）：QEMU 自身 stderr 有 `terminating on signal 15 from pid … (princess-cli)`，且 `uptimeMs=15064 ≈ timeout_ms=15000` → 是**引擎按期限终止**，而非 guest 自主退出。
2. **`run.exited.exitCode` 允许 `null`**（信号致死无退出码）—— 已同步进契约 §2。 **[裁决]**
3. **不给 `fixtures/refkernel/` 加 `princess.toml`**。**[裁决]**
   - 理由：refkernel 恰好用来覆盖「**无 manifest → 引擎默认配置**」这条路径；而「**显式 manifest**」这条路径已由 `templates/x86_64-multiboot2/princess.toml` 覆盖（P6 已验证，其自检脚本逐字核对 37 个契约字段）。
   - 因此**两条路径都被测到**，且**不必去动一个已被验证过的夹具**（改动已验证资产要付重新验证的成本）。

## D24 P2 验收的「workspace 全绿」是集成门（当前未达成）
- **现状**：`cargo test --workspace` **不是绿的**——`princess-bin`(P5) 有 **16 个编译错误**、`princess-build`(P2-B) 曾有 4 个失败测试。P2-A 只能保证 `-p princess-core -p princess-cli` **85/85 全绿**（这部分已达成）。
- **裁决**：验收文档里 P2-1 的 `cargo test --workspace` 是**集成门**，只有等 B/P5 各路收敛后才可能满足；**在 **P2-C** 阶段由主 Agent 亲自把关**，届时必须全绿才可宣布 P2 完成。在此之前，**不得**把「某 crate 自己绿」表述为「P2 完成」。 **[裁决]**

---

## 开放待办（Open Actions）

| # | 事项 | 归属 | 阻塞谁 |
|---|---|---|---|
| A1 | 扩展 bootstrap 增装 **clangd-16** | ✅ 完成 | — |
| A2 | **调研 C 重派**（原 Agent 因 OOM 不可用、无报告）：落定 D11（内置 `gdb -i=dap` vs 自写 MI 适配层） | 主 Agent（Mimo） | **P4 全部** |
| A3 | 清理废弃草稿（`_work/`、`_toolchain/`、`.researchA/`） | 主 Agent | 无（已 gitignore） |
| A4 | **P2-B1 重派**（被 OOM 打断、零产出）；随后按 D21 串行派 B2/B3 | 主 Agent | P2 验收 |
| A5 | 编辑器组件选型复核（P3 已交付，主 Agent 复核） | 主 Agent | P3 收尾 |
| A6 | ~~开启 `subagent-model-selection`~~ **已不需要**：改用 `dsh --profile headless` 直接路由 Mimo，不占用用户会话 | ✅ 解决 | — |
| A7 | P2-A 收尾：`princess-cli` 实现 + 事件夹具 + P2-6/P2-7 负样本 | P2-A（已唤醒） | P2-C 集成 |
| A8 | 按 **D21** 复查所有派发命令：重活 `CARGO_BUILD_JOBS=1`、派发前查 `free -h` | 主 Agent | 全部 |
| A9 | **把 gdb ≥14 提升为一线工具链**：现在它只存在于 `.researchC/dapbin/rootfs` 这个**临时草稿目录**里。需扩展 `scripts/bootstrap-toolchain.sh` 装到 `.toolchain/`、`env.sh` 导出（如 `PRINCESSIDE_DEBUG_GDB`）、`doctor.sh` 断言版本 ≥14。**不完成则 P4 依赖草稿目录，随时可能被清理** | 待派（Mimo） | **P4 全部** |
