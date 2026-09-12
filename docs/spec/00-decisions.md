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
    - **⚠️ 澄清（见 D26）**：文档中的 `kernel.c:100` **一律指故障指令地址**（`0x100b3d`，即 `ud2`）。**函数入口地址**（`nm` 的 `T refkernel_fault_probe` = `0x100b39`，下断用它）映射到 **`kernel.c:99`**（序言/`{`）。**两者不可混用，断言必须绑定地址而不是符号名。**
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
- **⚠️ 勘误（见 D27）**：本条的「headless 默认跑 Mimo」**已不成立**——`~/.dsh/settings.yaml` 的 `agent-default-model`（用户设置层）会盖掉 profile 补丁层，实测 headless 跑的是 **DeepSeek**；且 `subagent-model-selection` **只对新会话生效**。**派 Mimo 前先读 D27。**

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

## D25 开发机纪律**不得**泄漏进产品行为（`[build] jobs`）
- **发现**：B1 在 `crates/princess-build/src/backend.rs` 中**无条件**注入 `CARGO_BUILD_JOBS=1` 与 `MAKEFLAGS=-j1`（并写了断言「总是 1」的单测）。
- **裁决：必须修**。**[裁决]** 理由：D21 是**我们开发沙箱**的内存纪律（当时还没有 swap），**不是 IDE 该强加给用户机器的属性**——用户 32 核机器也会被强制单线程构建。此外我们已加 4G swap，原理由对开发机也已不成立。
- **契约变更（§4）**：`princess.toml` 的 `[build]` 新增**可选字段 `jobs`**：
  - **缺省 = 什么都不注入**（交给 make/cargo 自行决定）；
  - `jobs = N` 时注入 `MAKEFLAGS=-jN` 与 `CARGO_BUILD_JOBS=N`；
  - 未配置时环境里已有的 `CARGO_BUILD_JOBS`/`MAKEFLAGS` **必须原样保留**。
- **连带修复**：P6 模板的字段符合性自检要求「模板含全部契约字段」，故已给 `templates/x86_64-multiboot2/princess.toml` 补上 `jobs` 示例并**重跑自检通过**（37 → **38** 个字段路径、`0 unknown`、横幅断言 PASS、exit 0）。
- **通用教训**：**开发环境的资源约束只应体现在「派发指令」与「测试脚本的环境变量」里，绝不能写进产品代码路径。** 后续每个阶段审查时都要按这条检查一遍。

## D26 裁决「`kernel.c:100` vs `:99`」——**D3 不改，但补澄清**
- **P4 提出**：D3 与派发手册称 `refkernel_fault_probe` 在 `kernel.c:100`，但它实测 `addr2line` 给 `:99`，建议把 D3 改成 99。
- **我实测定性（两个独立 oracle）**：**两个地址映射到相邻两行，两者都对**：

  | 地址 | 含义 | `addr2line -f -C` |
  |---|---|---|
  | `0x100b39` | **函数入口**（`nm` 的 `T refkernel_fault_probe`，下断用） | `kernel.c:99`（`{` 所在行） |
  | `0x100b3d` | **`ud2` 指令本身**（即 `run.fault` 的故障 RIP） | **`kernel.c:100`** ✅ |

- **裁决**：**D3 的 `kernel.c:100` 保持不变**——它指的是**故障 RIP**（P2-4 的验收值）。P4 的**观察正确**，但它建议的改法会把一个**正确的常量改错**。已在 D3 内补上澄清。
- **通用教训**：**符号名相同 ≠ 地址相同**。断言必须绑定到**地址**，而不是符号名——否则序言、内联、优化都会让"符号级断言"漂移。 **[实测]**

---

## D27 更正 D12'（Mimo 路由已失效 / 设置只对新会话生效）+ 新增委派可见性插件

> 触发：用户问"为什么派出去的不是 Mimo"。三条都是本工作区真实命令产出，非推断。

### D27.1 [实测] `dsh --profile headless` 现在跑的是 **DeepSeek**，不是 Mimo

- **做法与产物**：跑 `DSH_PERMISSION_MODE=danger-full-access dsh --profile headless "只回答两个字：可用"` → **exit 0**，回答「可用」；但该次运行的会话 `session-a2fb9cb9-0b12-4a84-93eb-f7794f866d06` 的日志里记录的是
  `"provider":"deepseek-official"` / `"model":"deepseek-v4-flash-vision-exp"`（各 5 次）。
- **根因（层级优先序）**：`~/.dsh/settings.yaml` 的 `agent-default-model` 属于**用户设置层**，优先级**高于** profile 的 `cordis.patch.yml` 补丁层，因此 headless profile 里那条 Mimo 覆盖被静默盖掉。
- **影响**：D12' 表格里「`dsh --profile headless` = Mimo」的假设**不成立**。此前所有以 Mimo 名义发出的 headless 派发，很可能都在消耗 DeepSeek —— 与「DeepSeek 配额告急」互为因果。
- **修法（已落地，2026-09-12 补记）**：**不给 headless 关行、也不动交互会话的偏好**，而是让 headless 读**自己的** settings 文档——把 `~/.dsh/profiles/headless/cordis.patch.yml` 里 `settings` 行的 `config.path` 指向新建的 `/root/.dsh/headless-settings.yaml`（内含 `agent-default-model: xiaomi-token-plan-cn/mimo-v2.5-pro` 与 `llm-pi-ai.providers.xiaomi-token-plan-cn.apiKeyEnv`）。
  - 为什么必须带 `llm-pi-ai` 段：它在 `dsh-base` 里是**休眠挂载**（零路由），provider 只能由 settings 文档提供，缺了它 xiaomi 路由根本不注册。
  - 为什么用独立文档而不是删 `~/.dsh/settings.yaml` 的键：那个键是**用户在 GUI 里选的模型偏好**，删掉会连带改掉交互会话的行为（越界）。
  - **验证（只看产物）**：`dsh --profile headless "只回答两个字：可用"` → **exit 0**，其会话 `session-a02f12c9-6c78-4808-a621-e01815657cb7` 日志记录 `"provider":"xiaomi-token-plan-cn"` / `"model":"mimo-v2.5-pro"`（各 7 次），**不再是 DeepSeek**。
  - 回退：删掉 headless 补丁里那个 `- id: settings` 的 config 块，即恢复「读 `~/.dsh/settings.yaml`」的旧行为。
- **怎么自查（30 秒，仍然适用）**：跑完一次 headless 后，去 `~/.dsh/sessions/<workspace>/<session-id>/session.jsonl.zstd` 里 `zstd -dc | grep -oE '"model":"[^"]+"' | sort -u`。**看产物里的 model id，不要看配置文件。**

### D27.2 [实测] `subagent-model-selection` **只对新会话生效**——D12' 原判断是对的，我在本次会话中的口头判断是错的

- **我先说错、后被实测顶回来**：我读代码后曾下结论"该设置实时采样、不需重启"，并在补丁文件里也这么写了。随后在本会话用 `subagent` 传 `provider`/`model` 实测，直接报
  **`Error: child model selection is disabled for this tool instance`** —— 参数**被接受**（说明 schema 已刷新）但实例是 disabled。
- **代码依据**（`dsh-tool-subagent/lib/index.js:586-600`）：`selectForAgent` 在 `ctx.agent` 存在时**在安装期只算一次**并固定；且只有 `freshSession`（`session.firstLiveSeq === 0` 且首事件不是 `session/end-seed`）才会去读设置。**老会话即使重算也仍是 disabled。**
- **裁决**：**D12' 的原始判断（"需重启 web 会话才生效"）正确**，设置本身有效，但**只对新建会话**可用。已据此改写开放待办 A6。
- **现状（已配置，待新会话生效）**：`~/.dsh/settings.yaml` 已启用
  ```yaml
  subagent-model-selection:
    enabled: true
    allowedModels:
      - provider: xiaomi-token-plan-cn
        model: mimo-v2.5-pro
  ```
  新会话里 `subagent` 工具会带出 `provider`/`model`/`reasoning_effort` 与 `list_subagent_models`，届时才可按 D12' 的"内置 subagent 直选 Mimo"派活。

### D27.3 [实测] Mimo 通道本身是通的（换一条路验证）

- `workflow` 工具支持逐路 `provider`/`model` 覆盖，不受上述门控。实跑一次
  `provider=xiaomi-token-plan-cn` / `model=mimo-v2.5-pro` → 返回「PrincessIDE Mimo 通道验证成功。」；
  其子会话 `87123613-d6db-431f-b702-c24c3b7345c5` 的日志记录 `"model":"mimo-v2.5-pro"`，且
  `origin: subagent`、`delegationDepth: 1`、`parent: session-4a47c0d9-…`。
- **含义**：凭据（`XIAOMI_TOKEN_PLAN_CN_API_KEY`）与 provider 路由**都没问题**；出问题的只是"哪条通道选中了哪个默认模型"。故 D27.1 是**配置优先级**问题，不是通道故障。

### D27.4 [裁决] 新增插件 `princesside-delegation-view`（委派可见性）

- **动机**：GUI 自带的 `dsh-client-ui-subagent` 能画父子谱系，但它读的 `SessionSummary` **没有模型字段**，所以"这个子 Agent 是 Mimo 还是 DeepSeek 跑的"在界面上看不见。
- **落点**：宿主半边把 `request/header` / `request/context` 折叠成一个**客户端可见的会话投影** `sessionModel`（`{provider, model, reasoningEffort} | null`）；浏览器半边在 `conversation.session.header.actions` 槽位加「子 Agent (N) ▾」面板，列出 本会话模型 / 本会话的子 Agent / 全部委派（`父 ← 子 → provider/model`）。
- **位置与接线**：`~/.dsh/profiles/web/node_modules/princesside-delegation-view/`（**仓库外**，与 D25"开发机纪律不进产品代码"同精神）；在 web profile 补丁里以 `- insert:` 挂载（改现有行用 `- id:`，**新增行必须 `- insert:`**）。
- **生效机制 [实测]**：该 profile `patchReload: live`，保存补丁即**热加载宿主半边**——本会话的投影缓存 `~/.dsh/storages/session_projcache/sessions/session-4a47c0d9-….json` 里出现了 `sessionModel` 行（`{"provider":"deepseek-official","model":"deepseek-v4-flash"}`，seq 递增）即为证；**浏览器半边需刷新页面**才进 roster。
- **验证（两个独立脚本 + 负样本，退出码即结论）**：
  ```bash
  node .scratch/main/verify-plugin.mjs         # 宿主：真会话日志折叠 + 5 个负样本 → exit 0
  node .scratch/main/verify-plugin-client.mjs  # 浏览器：react-dom/server 真渲染 + 降级路径 → exit 0
  dsh --profile web --dump-config | grep princesside-delegation-view   # 组合进树
  dsh --profile headless --patch <overlay> "只回答 ok"                 # 加载 → exit 0
  #   同一 overlay 指向不存在的包 → ERR_MODULE_NOT_FOUND, exit 1（证明上一条不是假绿）
  ```
- **未验证（诚实标注）**：浏览器内**实际视觉**与 roster 是否收录本包。无显示器、且 `/plugins/*` 未登录不可探测（对照实验：现成的 `dsh-client-ui-jobs` 包在未登录下**同样 404**，说明该探测本身无效）。按 D14，GUI 视觉验证归用户。

### D27 通用教训

1. **"配置层写了对的值" ≠ "运行时用了对的值"**。同一件事在 settings 层、profile 补丁层各有取值，还有"何时采样"（安装期 / 首次发布 / 每次请求）这一维；只看配置文件会自我安慰。
2. **结论要么有产物证据，要么明确标注为推断**。我在 D27.2 上先给了一个只有代码阅读支撑的结论，被一句运行时报错推翻——这正是 D14 要防的事。判据应该是**产物里的 model id**，而不是我读代码的心得。
3. **无头环境下也要给界面改动设计可自动化的验证**：插件前端无法目视，就用 `react-dom/server` 把组件真渲染出来断言文本与降级路径，把"无法验证"压缩到只剩纯视觉那一小块。

---

## D28 headless 子 Agent 在 GUI 里结构性不可见 + 负样本纪律

> 触发：用户两次问"为什么看不到子 Agent"。第一次我以为是面板缺口（确实有，已修）；第二次实测发现根因在**宿主索引**，比面板更底层。

### D28.1 [实测] web 宿主只登记它自己认识的会话，进程外会话一律不进 GUI

| 数据源 | `/root/PrincessIDE` 的会话 |
|---|---|
| 磁盘上真实会话目录 | **45 个**（其中 26 个带 `session-` 前缀） |
| 运行中 web 宿主的工作区索引 `~/.dsh/storages/workspace.json` | **只有 3 个** |

- 因此 `dsh --profile headless`（**另起进程**建的会话）**不被 web 宿主登记**：侧边栏没有、插件面板读到的 `SessionListState.byId` 里也没有、连"点开"都会失败（宿主对未知 id 是 fail loud）。**这是任何前端 UI 都无解的**——不是面板 bug。
- **只有进程内委派**（`subagent` / `subagent_fork` / `workflow`）产生的子会话才带 `origin=subagent` + `parentSession`，并被宿主登记/编目，从而在所有 UI 里可见可点。
- 由此形成一条**取舍**（三选一，不可兼得）：

  | 通道 | 后台不阻塞 | GUI 可见 | 每次可选 Mimo |
  |---|---|---|---|
  | `dsh --profile headless` | ✅ | ❌ | ✅（D27.1 修好后） |
  | `subagent` 工具 | ✅ | ✅ | 仅**新会话**（D27.2） |
  | `workflow` 逐路覆盖 | ❌ 前台阻塞 | ✅ | ✅ |

- **⚠️ 勘误（对 D27.4）**：D27.4 说的"顶层会话（无父子标记）"分组能补可见性，**只对 store 里已有的会话成立**；headless 会话压根不在 store 里，该分组**补不了这个洞**。已实测：面板里"顶层会话"只列出那 3 个宿主认识的会话。

### D28.2 [裁决] 用户选择：维持 headless（不阻塞优先），接受 GUI 不可见

- 交换条件明确：**后台不阻塞 > GUI 可见**。后续派发仍走 `dsh --profile headless`（真 Mimo）。
- **补偿措施（主 Agent 强制义务）**：
  1. 每次派发/回收都在汇报里给出**子会话 id、模型、任务、产物路径**；
  2. 维护台账 `.scratch/main/dispatches.md`（会话 id → 任务 → 状态 → 报告路径），把"看不见"退化成"另找一处看"；
  3. 若要 GUI 可见，按 D28.1 的两条可见通道重新裁决即可——这是**显式取舍**，不是能力缺失。

### D28.3 [裁决] 负样本纪律（本轮事故的直接教训）

- **事件**：脚本子 Agent 为构造"工具缺失/版本过低"的负样本，把 `.toolchain` 里 6 个真文件改名为 `*.hidden` 并塞入 stub，**死在半路未恢复**；导致 `doctor.sh` 误报 `clangd-16` / `gdb` / `princess-gdb` 缺失，一度被误判成"BUG-006 没修好"。恢复与证据见 `docs/reports/fix-round1.md` §3。
- **规则（对所有派发生效）**：构造负样本**禁止**改名 / 替换 / 移动工具链与环境的**真文件**。只许三种非破坏手法，本轮均已实测够用：
  1. **临时 `PATH`**（stub 放 `/tmp`）；
  2. **环境变量**（如 `PKG_CONFIG_LIBDIR=/tmp/empty`）；
  3. **隔离副本**（把 `scripts/` 或所需目录 `cp` 到 `/tmp` 再跑；环境变量不可覆盖时尤其有用——`env.sh` 里 `PRINCESSIDE_TOOLCHAIN` 是从脚本位置推导的，无法用 env 覆盖）。
- **附加要求**：任务书必须显式写明这条禁令；Agent 若确需不可逆操作，应 `STATUS: BLOCKED` 说明，而不是"先做了再说"。

---

## D29 模块图 v2：用户 8 模块提案的批判与裁决

> 触发：用户提出 8 个模块（①核心：系统调用/接口/模块间通信 ②项目管理/添加 ③代码编辑 ④编译、**解释器** ⑤调试 ⑥**版本控制** ⑦GUI 显示 ⑧**插件管理**），要求批判性分析。

### D29.0 [实测] 事实基线（批判只能建立在这些之上）

- **本项目没有任何 CI 配置**（无 `.github`、无 `.gitlab-ci.yml`）；`scripts/smoke-ci.sh` 靠手跑，且**不含**前端 vitest、契约校验、外壳测试。
- 引擎与外壳里**零处调用 `git`**（⑥是纯新增）。
- 验收规格只规划到 **P0–P8**：①②④⑥⑧**都不在其中** → 该提案实质是 **v2 模块图**。
- 契约 24 命令的领域分布：`debug` 13、`lsp` 6、`run` 4、`project` 4、`op` 4、`build` 4、`tools` 2。
  **`symbols:` / `bin:` / `fs:` / `ai:` 命令数为 0** —— 而 `princess-symbol`、`princess-bin`（109 测试）、`princess-ai` **后端已完成**，属于"投入了但没交付"。

### D29.1 [裁决] 这是"v2 模块图"，不是"把现有代码模块化"

8 项里 **5 项已经存在（只是换名字）**：①≈`princess-core`+契约、②≈`project:*`+`princess.toml`+P6 模板、③≈CodeMirror+`lsp/client.ts`、④(编译)≈`princess-build`、⑤≈`princess-debug`、⑦≈`apps/desktop`；
**2 项真新增**：⑥版本控制、⑧插件管理；**1 项不是模块**：模块间通信（横切关注点）。
**故不得以"改成模块化"为名做大爆炸重构** —— 45k 行 / 102+454 测试 / 冻结契约 / 29 条决策，重构会作废已验证资产（D24 集成门、D3/D26 断言常量都要重验）。

### D29.2 [裁决] 逐项定性：保留 / 改形态 / 砍

| 提案 | 裁决 | 关键理由 |
|---|---|---|
| ① 核心：系统调用 + 模块间通信 | **收窄**，不新建通信层 | 本产品里没有"系统调用"的对应物（IDE 不是 OS）。需要的只有**一条**引擎↔UI 契约（已有 24 命令/10 错误码）＋引擎内**直接 crate 依赖**（Rust 不需要总线）。再叠一层总线会产生**两个真相来源**（trait 一套、IPC 一套），违反 D23 的正交性论证。核心 = 契约 + 配置 + 事件 + 取消/操作注册 |
| ② 项目管理/添加 | **保留为目标，但不建 crate** | 能力已有（`project:open/validate` + 模板）；缺的是**交互向导**与"添加项目"入口（BUG-003 本轮已补原生目录选择器） |
| ③ 代码编辑 | **保留** | 已有；红线是**不许自研编辑器/语言服务**（D5/D17 已两次踩过同类坑） |
| ④ 编译 / **解释器** | 编译保留；**解释器砍掉，换成可测指标** | 两个可能形态都不该做：(a) 自研 x86_64 模拟器＝**第二执行真相来源**，必然与 QEMU 漂移，产生"模拟器过、QEMU 挂"的恶性 bug（D9：只有 QEMU 的行为算准）；(b) DSL/脚本引擎无需求。**改为指标**："内核**函数级**测试 <1s 且不启动 QEMU"（host 编译 + 假 MMIO；加速用 QEMU 快照 / KVM） |
| ⑤ 调试 | **保留** | 本项目最扎实的部分（D11 GDB 内置 DAP + 薄能力层，36/36 验收）；缺口只在前端（BUG-008） |
| ⑥ 版本控制 | **改形态**：`git` CLI 薄封装 + **编排器** | 不自研 VCS（D17 同款原则：别重写成熟协议栈），永不触碰 packfile/索引格式。差异化在**编排**：引擎已有"构建 + 启动 QEMU + 断言"，接到 `git bisect run` 上＝**自动二分找出哪个提交让内核启动失败**。模块名建议 `princess-bisect`，不是"版本控制模块" |
| ⑦ GUI 显示 | **改目标**：做**视图注册表** | 病根不是"没分文件"（component 已分文件），而是 `app.ts` 是**组装根**：337 行、4 处手工建容器、11 处手工 `render*()`，漏一处就是孤儿组件（BUG-001 的成因）。注册表 = 每个视图自注册 `{id,title,testid,mount,render}`，`app.ts` 只留布局 + IPC 接线 |
| ⑧ 插件管理 | **保留，但分期且必须有能力模型** | ①**先声明式**（manifest 声明面板/命令/模板/主题），不做原生代码加载：原生插件＝任意代码执行，而 **Rust 无稳定 ABI**，会被迫引入 WASM 或进程 RPC 边界；②必须有**能力模型**（Tauri v2 capabilities，最小授权、可审计）——插件能触达 QEMU 与编译器，能力极大；③兼容性**版本化**（`v` 字段、投影 `stateVersion` 已有先例）＋ manifest 声明兼容范围，否则重演 D7 那类配置漂移；④引擎扩展边界应是**进程/RPC**，复用现有命令，不是共享 ABI |

### D29.3 [裁决] 提案缺了 4 个本项目真正需要的模块

1. **工具链/环境模块**：`doctor.sh`/`bootstrap-toolchain.sh`/clangd-16/bear/gdb≥14/QEMU 版本 —— 现在**散落在 `scripts/` 与 `src-tauri/src/doctor.rs` 两处**；本轮 3 个 bug（BUG-004/005/006）全出在这里，是**最大支持痛点**。
2. **夹具与验收模块**：`refkernel`/`paging-kernel`/`smoke-boot`/`p4-acceptance` —— 项目真正的资产（D3 的固定常量是全部断言基础），应有名字、有主人、有版本。
3. **符号化与二进制可视化**：后端已完成、契约 0 命令、前端 0 行（最大的"已投入未交付"）。
4. **AI**：同上，`princess-ai` 无契约面。
（横切的"事件/操作/取消"已有，**不拆**。）

### D29.4 [裁决] **门先于切割**（本条最重要）

模块图落地前必须先给每个模块**一条机器可跑的门**：该模块测试 + 契约校验 + **一条负样本**。
现状是 `cargo test --workspace` 是唯一门槛，而**前端 102 测试、契约校验、外壳测试三者都在门外** —— BUG-001（组件从未挂载、92 测试全绿）正是这条不存在的门生的孩子。
**没有门的模块图只会变成一张更漂亮的债。**

### D29.5 [裁决] 落地顺序（增量）

1. **P-A 门 + 视图注册表**：CI（`cargo test --workspace` + 前端 vitest + `check-contract` + `src-tauri` 测试）＋前端视图注册表＋"每个注册视图必须在 DOM 里"的测试。零新功能、全可验证。
2. **P-B VCS 编排**：新 crate + 少量契约命令；`git bisect run` 接构建/QEMU；git 只走 CLI。
3. **P-C 声明式插件 + 能力模型**：manifest 注册面板/命令/模板；原生插件留到最后一期。
4. **P-D 补 P5/P7 契约面与前端**：`symbols:`/`bin:`/`ai:` 命令 + 面板（D20 要求反汇编与源码行并排）。
5. **"解释器"按指标重新提案**，不先建模块。

### D29 通用教训

**模块图的价值是"所有权与验收地图"，不是"重写计划"。** 它值得写下来（多 Agent 施工需要明确的所有权边界，D15 现在靠任务书临时划分），但落地方式必须是"先立门、逐块迁移"，而不是按图重写。

---

## D30 双前端选配：原生自绘前端（Vulkan）作为长期并行轨道

> 触发：用户提出"前端要有两种可选渲染方式"，并在需求澄清中拍板。需求明细见 `docs/spec/31-native-frontend.md`。

### D30.1 [裁决] 需求（用户拍板，2026-09-12）

| 项 | 结论 |
|---|---|
| 形态 | **双前端选配**：Web+JS（默认/参考实现）+ 原生自绘前端，可切换 |
| 动机 | ①Web 兼容性更好 ②Vulkan 性能更好、可做更精美动画 ③**不多一个浏览器内核吃内存** |
| 预算 | **长期并行项目**（用户明确接受双前端维护成本） |
| 自研边界 | 自研**渲染核心 + 字形图集**；窗口/输入用 `winit`；文本整形用现成库 |
| 第一版视图 | 事件流（虚拟化滚动）、只读代码、hex/反汇编 |
| 编辑器 | 目标可编辑（含 IME）；**分阶段**：P-E1 只读，P-E2 才做编辑与 IME |
| LSP | 原生第一版**不做**（D17 那层继续只留在 Web 前端） |
| 平台 | **X11 + Wayland 都要** |
| 视图模型 | 新增契约域 `princess:view:*`，**引擎算好**，两个前端只画 |

### D30.2 [实测/裁决] 三条事实澄清（决定"收益怎么写"，不改变决定）

1. **Tauri 不打包浏览器内核**（不是 Electron），但会**加载系统 WebKitGTK**，后者有自己的进程与内存 → 用户动机③**成立**，收益即"省掉 WebKitGTK 常驻"（也正是本项目 13 个系统包与 BUG-004/005 的来源）。
2. **DOM 那条路本来就在 GPU 上**（WebKitGTK 用 GL/EGL 合成）→ 动机②必须落到具体场景（动画流畅度、输入延迟、大视图帧率、渲染控制力），**不是**"把渲染搬到 GPU"。
3. **"更精美动画"是设计自由度收益**（自绘能做 DOM 里别扭的效果），不是性能收益。写卖点时不得混为一谈。

### D30.3 [裁决] 边界与门（不可协商）

1. **Web 前端保持默认与参考实现**；原生前端必须**可选**（如 `--ui=native`），任何功能都不得只在原生前端实现。
2. **视图模型是共享资产**：逻辑归 `princess:view:*`（引擎计算），**两个前端只换绘制**；其单测与**同构断言**（同一输入 → 同一模型，逐字相等）是共享门。
3. **原生前端只读契约**：不得直接依赖引擎内部类型；不得在渲染层做解析/符号化（只能画不能算）。
4. **反汇编必须并排**（D20）：并排关系落在**视图模型**里（`{addr, bytes, mnemonic, sourceFile, sourceLine}`），不允许两个渲染器各拼一遍。
5. **本机只能验正确性**（只有 `lvp` 软件光栅）：性能数字必须由用户在真机取证；**IME 与 Wayland 输入只能由用户验收**。

### D30.4 [裁决] 退出条件（防沉没成本）

P-E1 结束时，若 ①冷启动 / 按键→上屏延迟 / 10 万行滚动帧时三项指标不达标，或 ②实现成本明显失控，则**记录数字并砍掉该模块**，不留半成品继续消耗。

### D30 通用教训

**"双实现"的成本不在渲染，而在编辑器与每一个面板。** 唯一让这条路不翻倍的形态是"视图模型共享 + 渲染各写"；三个视图之后若仍无共享模型，应立即重新评估而不是继续加面板。

---

## 开放待办（Open Actions）

| # | 事项 | 归属 | 阻塞谁 |
|---|---|---|---|
| A1 | 扩展 bootstrap 增装 **clangd-16** | ✅ 完成 | — |
| A2 | **调研 C 重派**（原 Agent 因 OOM 不可用、无报告）：落定 D11（内置 `gdb -i=dap` vs 自写 MI 适配层） | 主 Agent（Mimo） | **P4 全部** |
| A3 | 清理废弃草稿（`_work/`、`_toolchain/`、`.researchA/`） | 主 Agent | 无（已 gitignore） |
| A4 | **P2-B1 重派**（被 OOM 打断、零产出）；随后按 D21 串行派 B2/B3 | 主 Agent | P2 验收 |
| A5 | 编辑器组件选型复核（P3 已交付，主 Agent 复核） | 主 Agent | P3 收尾 |
| A6 | **开启 `subagent-model-selection`**：已写入 `~/.dsh/settings.yaml`（白名单 `xiaomi-token-plan-cn/mimo-v2.5-pro`）。**但只对新建会话生效**，老会话仍报 `child model selection is disabled for this tool instance`（见 D27.2）。原"改用 headless 直接路由 Mimo"的说法已作废（见 D27.1） | ✅ 配置完成，待新会话验证 | 主 Agent 按 A 路线派 Mimo |
| A7 | P2-A 收尾：`princess-cli` 实现 + 事件夹具 + P2-6/P2-7 负样本 | P2-A（已唤醒） | P2-C 集成 |
| A8 | 按 **D21** 复查所有派发命令：重活 `CARGO_BUILD_JOBS=1`、派发前查 `free -h` | 主 Agent | 全部 |
| A9 | **把 gdb ≥14 提升为一线工具链**：现在它只存在于 `.researchC/dapbin/rootfs` 这个**临时草稿目录**里。需扩展 `scripts/bootstrap-toolchain.sh` 装到 `.toolchain/`、`env.sh` 导出（如 `PRINCESSIDE_DEBUG_GDB`）、`doctor.sh` 断言版本 ≥14。**不完成则 P4 依赖草稿目录，随时可能被清理** | 待派（Mimo） | **P4 全部** |
| A10 | **修 headless 通道的模型路由**：已完成（D27.1 补记）。做法是给 headless 一份独立 settings 文档（`/root/.dsh/headless-settings.yaml` + headless 补丁里 `settings.config.path`），实测 headless 会话已记录 `xiaomi-token-plan-cn/mimo-v2.5-pro` | ✅ 完成（2026-09-12） | — |
| A11 | **目视确认委派面板**：用户已确认「面板在」——`princesside-delegation-view` 的宿主半边与浏览器半边均已生效。后续又加了「点击行打开子会话」与「Token 统计（会话/模型/输入/输出/命中率）」两个入口，待用户刷新页面复看 | ✅ 面板确认；新入口待复看 | D27.4 扩展 |
| A12 | **P-A：门 + 视图注册表**（D29.5 第 1 步）：加 CI（workspace 测试 + 前端 vitest + `check-contract` + `src-tauri` 测试）；前端引入**视图注册表**并把 `app.ts` 收成"布局 + IPC 接线"；补"每个注册视图必须出现在 DOM 里"的测试（BUG-002 的推广） | 待派（用户已批准 D29，未批准开工） | D29 全部后续步骤 |
| A13 | **P-B：`princess-bisect` 编排器**（D29.2 ⑥）：`git` CLI 薄封装 + `git bisect run` 接"构建 + QEMU + 断言"；先只读 + 少量写 | 待派 | P5/P7 之后 |
| A14 | **P-C：声明式插件 + 能力模型**（D29.2 ⑧）：manifest 注册面板/命令/模板；原生插件（WASM/进程 RPC）留最后一期 | 待派 | 插件生态 |
| A15 | **P-D：补 P5/P7 契约面与前端**：`symbols:`/`bin:`/`ai:` 命令 + 反汇编/页表/hex 视图（D20 要求与源码行并排）+ AI 面板 | 待派 | "已投入未交付"收口 |
