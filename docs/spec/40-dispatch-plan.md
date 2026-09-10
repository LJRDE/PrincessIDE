# PrincessIDE 派发编排计划

> 用途：跨轮次、跨 Agent 的**统一派发口径**。每个实现 Agent 都按本文档的「通用前置」+「本路 brief」派发，保证不同轮次派出去的活口径一致，不会因为主 Agent 上下文漂移而变形。
> 所有人动手前必须先读 `docs/spec/00-decisions.md`（**已冻结的决策，违反 = 返工**）。

---

## 一、通用派发前置（每路 Agent 必给的上下文块）

```
## 项目
PrincessIDE：面向 x86_64 操作系统内核开发的 Linux 桌面 IDE（Tauri v2 + Rust 引擎 + Vite/TS 前端，仅 Linux）。

## 必读（按顺序）
1. docs/spec/00-decisions.md  —— 已冻结决策（D1~D16），**违反即返工**
2. docs/spec/10-contracts.md  —— 事件模型 / IPC / 错误码 / princess.toml / 目录归属
3. docs/spec/20-acceptance.md —— 你这一批要满足的验收项
4. docs/reports/              —— 前序阶段的真实验收证据

## 环境事实（可信，不必重新探测）
- 工作区 /root/PrincessIDE。**每条 bash 都是新 shell**：用任何工具前先在同一条命令里 `source scripts/env.sh`。
- 工具链在 .toolchain/（cargo/qemu/nasm/clang/clangd-16/gdb/xorriso/mtools；grub-mkrescue 在 /usr/bin）。
- 沙箱 danger-full-access，**审批已关闭**（不要请求提权，直接执行；允许 apt-get install，但要在报告里声明装了什么）。
- 无显示器 → GUI 不可肉眼验证；KVM 不可用 → TCG 软件模拟，QEMU 启动慢，给足超时。
- **内存纪律（D16）**：4 核 / 可用约 1.7G / **无 swap**。编译必须 `make -j2` 上限、`CARGO_BUILD_JOBS=2`；重活不要并发；莫名失败先怀疑内存。
- 网络：static.rust-lang.org 极慢（走清华镜像），crates.io/npm/apt 正常。
- 唯一权威测试夹具是 fixtures/refkernel/（横幅 `PrincessIDE reference kernel booted`；故障符号 `refkernel_fault_probe`，位于 kernel.c:100）。**禁止引用 `_work/`、`.research*/`、`_toolchain/` 等废弃草稿。**

## 铁律
1. **一切「完成」必须有真实命令输出 + 退出码支撑**；禁止编造、禁止「预期应该能跑」。
2. **只写你被授权的目录**，其它目录只读。
3. 临时文件放 `.scratch/<你的名字>/`，**禁止污染仓库根目录**。
4. **禁止执行改动 git 的命令**（add/commit/checkout）——提交由主 Agent 统一做。
5. 卡住就如实报告卡在哪条命令、什么错；部分完成也如实说。

## 交接摘要格式（固定）
1. 状态：完成 / 部分完成 / 卡住（卡在哪条命令、什么错）
2. 交付物清单（文件路径）
3. 验收表逐项：命令 → 退出码 → 关键输出一行
4. 有歧义或可能被质疑的设计决定（列出来，别藏着）
5. 给下游 Agent 的接口提示（≤5 条）
```

---

## 一之二、怎么派活（启动命令，主 Agent 专用）

**默认（机械活 / 调研 / 文档 / 模板 / 可视化）→ Mimo**
```bash
cd /root/PrincessIDE && DSH_PERMISSION_MODE=danger-full-access \
  dsh --profile headless "<完整任务书>" 2>&1 | tail -40
```
用 bash 工具的 `run_in_background: true` 启动 → 真正的后台并行，用 `job_output` 收结果。

**重难点（核心引擎实现、关键架构决策、最终独立验收）→ DeepSeek Flash**
```bash
cd /root/PrincessIDE && DSH_PERMISSION_MODE=danger-full-access \
  dsh --profile headless --patch scripts/dispatch/flash.patch.yml "<完整任务书>" 2>&1 | tail -40
```
或者直接用 `subagent` 工具（会话默认就是 Flash，且支持 `send_message` 中途纠偏 + 自动完成通知）。

**为什么必须这样**：`subagent` 工具没有 model 参数（宿主 `subagent-model-selection` 需重启会话才生效，已放弃）。headless profile 的默认模型已在 `~/.dsh/profiles/headless/cordis.patch.yml` 里改为 `mimo-v2.5-pro`，用户的 web 会话不受影响。详见 `00-decisions.md` D12'。

**代价与纪律**：
- headless 任务**无法中途 steering、无自动完成通知** → 任务书必须一次给足，且**要求 Agent 把进度与最终报告写进文件**（放 `docs/reports/` 或 `.scratch/`），我靠轮询文件与 job 输出来收活。
- **每次 headless = 一个 Node 进程 + 一个 Agent**；配合 D16，**同时最多 2~3 个**，重活错开。
- ⚠️ **QEMU 必须保证被杀干净（实测踩过）**：本工作区出现过**父进程已死、PPID=1 的孤儿 `qemu-system-x86_64` 持续空转**的情况。所有启动 QEMU 的脚本与命令必须：① 用 `timeout` 包裹；② 结束前按**进程组**收尾（`kill -- -$PGID` 或 `pkill -f` 精确匹配），不能只杀父进程。**理由是它会直接污染验收**：P2-7 断言「超时后 `pgrep qemu-system-x86_64` 必须为空」，一个别人的孤儿进程就能让该断言假失败或假通过。主 Agent 复核前也应先清理孤儿。

---

## 二、批次总表

| 批次 | 内容 | 依赖 | 并行度 | 状态 |
|---|---|---|---|---|
| **P0** | 环境 + 参考内核夹具 + 冒烟/符号化脚本 | — | 1 | ✅ 已交付并由主 Agent 独立复核 |
| **A1** | 工具链补装 clangd-16 + bear（含复核调研结论 §2.4） | P0 | 1 | 进行中 |
| **P2-A** | Cargo 工作区 + `princess-core` + `princess-cli` + 事件夹具 | P0 | 1 | 进行中 |
| **RC / RD** | 调研 C（调试协议）、调研 D（二进制工具选型） | — | 2 | 进行中 |
| **P3-A** | Tauri 外壳 + GUI 依赖就位 + 一条真实纵向切片 | P0 | 1 | 进行中 |
| **P2-B** | `princess-build` / `princess-run` / `princess-symbol` | **P2-A 的 core 类型冻结** | **3** | 待派 |
| **P2-C** | 全链集成 + 事件夹具打通 + 主 Agent 亲验 | P2-B | 1 | 待派 |
| **P4** | 调试器（DAP 后端 + 寄存器/内存/栈/反汇编面板） | 调研 C 结论（落定 D11） | 2 | 待派 |
| **P5** | 可视化（ELF/hex/反汇编/页表/GDT-IDT/monitor） | 调研 D 结论 | 4 | 待派 |
| **P6** | 模板与工具链向导 | P2-C | 2 | 待派 |
| **P7** | AI 辅助（OpenAI 兼容抽象层） | P2-C | 2 | 待派 |
| **P8** | 产品化（打包、文档、冒烟 CI） | P4~P7 | 2 | 待派 |

---

## 三、P2-B 三路 brief（下一批，待 P2-A 冻结 core 后立即派发）

### B1 · `crates/princess-build/`
- **所有权**：`crates/princess-build/`、`docs/reports/p2-build.md`、`.scratch/build/`
- **输入**：`princess-core` 已冻结的事件类型与错误码（`BuildEvent`、`E_TOOLCHAIN_MISSING`、`E_BUILD_FAILED`）
- **交付**：
  1. 工具链检测（跨编译器路径/版本；缺失 → `E_TOOLCHAIN_MISSING` 并给修复建议）
  2. `BuildBackend` 的 **make 后端**（cmake 后端可延后）
  3. `compile_commands.json` 生成：**首选 bear**；bear 不可用时用手写 wrapper shim（D8）。**必须遵守 D18**：经 PATH 里的 `.toolchain/bin/bear` **启动器**调用（直调 `prefix/usr/bin/bear` 会失败或静默漏条目）；**生成前先 `make clean`**（否则第二次 bear 会用空数组 `[]` **覆盖**已有条目）。
  4. clang/ld/nasm 的**诊断解析** → `build.diagnostic{severity,file,line,col,message,source}`
  5. 构建日志流式回传（`log.append{stream:"build"}`）+ `build.started/finished{artifacts[]}`
  6. **按 D7/D17 生成并校验内核工程的 `.clangd` 配置**（triple 写死 `--target=x86_64-unknown-none`、用 `-nostdlibinc`、`CompileFlags.Remove` 用 **D7 那张具体黑名单**）——**这是内核特化差异化的落点**；**禁止通配 `-W*`**（会连 `-Wall -Wextra` 一起删掉，告警诊断全丢）；语言服务必须用 `clangd-16`（D18）。LSP 的编辑器交互不在引擎侧（D17）
- **验收**：P2-2（构建产出 `refkernel.elf`）、P2-6（**负样本**：故意语法错误 → `diagnostic` 带正确 `file`/`line`）、工具链缺失路径 → `E_TOOLCHAIN_MISSING`
- **注意**：编译数据库条目用 `arguments` 而非 `command`；`directory` 用**绝对路径**（D8）

### B2 · `crates/princess-run/`
- **所有权**：`crates/princess-run/`、`docs/reports/p2-run.md`、`.scratch/run/`
- **输入**：`RunEvent`、`E_QEMU_FAILED`、`E_TIMEOUT`
- **交付**：
  1. QEMU 启动编排，串口参数固定 `-display none -serial stdio -monitor none`（D9）
  2. 串口**既落盘又实时流式**（先落盘再推送）
  3. 异常/退出**归因**：`-d int` 与 `-d cpu_reset` **同时开**（三击故障只在后者里）；`-no-reboot` 退出码 0 **不得**当作 panic 依据（D9）
  4. 超时与**杀进程组**（不得留孤儿 QEMU）
  5. QMP 客户端做结构化解接口（HMP 文本解析仅兜底）
- **验收**：P2-3（串口事件流含横幅）、P2-5（`run.exited.reason` 归因正确）、P2-7（**负样本**：死循环 → `timeout` 且 `pgrep qemu-system-x86_64` 为空）

### B3 · `crates/princess-symbol/`
- **所有权**：`crates/princess-symbol/`、`docs/reports/p2-symbol.md`、`.scratch/symbol/`
- **输入**：`SymbolsIndexed` 事件、参考内核 ELF
- **交付**：ELF 解析、DWARF 行号、符号索引、RIP → `{symbol,file,line}`；`run.fault.symbolicated` 的填充；guest 自己打印的 panic/回溯文本解析
- **验收**：P2-4（`symbolicated = {symbol:"refkernel_fault_probe", file, line:100}`），且结果必须与 `addr2line` 对同一 RIP 的输出**一致**（golden 对比）
- **选型（已冻结，见 D20，不要再自行比较）**：ELF 解析用 **`object` 0.40.0**；DWARF 与源码行用 **`gimli` 0.34.0 + `addr2line` 0.27.1`**（分层）；**不用** `goblin`/`elf`。反汇编不归你，那是 P5（`iced-x86` 1.21.0）。
- **UI 含义（D20）**：符号化的结果必须能支撑「反汇编 + 源码行并排」的呈现，因此 `source_line_for_address` 要能同时给出地址区间与行号范围，不要只回一个行号。

**冲突规则**：三路都依赖 core 类型但**都不得修改 core**。若发现 core 类型不够用，写进交接摘要由主 Agent 裁决，不要各自去改。

**派发节奏（2026-09-11 依实测资源状况裁决，并由 D21 收紧）**：**重活必须串行**。
- **实测背景（已付出代价）**：本机 4 核 / 无 swap。自主期发生 **2 次全局 OOM 杀进程**（`dmesg` 可见 `global_oom`，被杀进程 anon-rss ≈ 1.05 GB）。**后果**：P2-B1（`princess-build`）刚派出即被打断、**零产出**；调研 C 第一棒 Agent 被杀且**会话变为不可用**。根因是内存中非项目进程已占约 1.7G（`dsh web` / `hermes dashboard` / `claude`），留给项目的仅约 2G。
- **裁决（硬规则，见 D21）**：
  1. **同一时间只允许一路重型构建**；派发前先 `free -h`，**可用 < 1.2G 不得启动新重活**。
  2. 重型构建一律 **`CARGO_BUILD_JOBS=1`**；`make` 上限 `-j2`。
  3. **不同时派两个需要编译的 Agent**：**先 B1 单独跑完并冻结**，再串行 B2（`run`）→ B3（`symbol`）。宁可慢，也不要第三次 OOM。
  4. 构建莫名失败时**先查 `dmesg | grep -i oom-kill`**，再怀疑代码。
  5. 附加理由：三路同属一个 Cargo 工作区，并发 `cargo build` 会在 `target/` 锁上互相阻塞，反而更容易撞工具超时。

---

## 四、P2-C 集成与主 Agent 亲验
- 由**主 Agent 亲自**跑全链：`doctor → build → run → symbolicate`，逐条比对 `docs/spec/20-acceptance.md` 的 P2-* 断言。
- 再派**独立验收 Agent**（写者与判者分离）做对抗式复核，重点打负样本与边界。
- 复核记录写入 `docs/reports/p2-review.md`（**主 Agent 撰写**，与实现者的报告分开存放）。

---

## 五、后续阶段 outline

- **P4 调试器**：**D11 已落定**——主干用 **GDB 内置 DAP**（工作区自带 gdb 13.1 **没有** DAP，必须 **gdb ≥14**，当前可用 16.3），**只需在 Rust 侧补一层薄能力层**（硬件断点 `hbreak`、物理内存读写、寄存器面板、REPL 逃生舱），**不要自写完整 MI→DAP 转换**。做无头断点 E2E（P4-1~P4-4），必须含**架构自检**（D10 的 gdbstub 错配问题）。**调试目标建议直接用 `fixtures/paging-kernel/`**——调研 C 已实测 DAP 对它可给出完整源码级回溯（`paging_fault_probe` → `kernel_main` → `_start`），验收形态已被证明可达。**注意**：调研 C 第一棒的产物文件含**假阴性**，以第二棒报告为准。
- **P5 可视化**：按调研 D 结论选定 crate；ELF/反汇编做 **golden 文件比对**（对 `readelf`/`objdump`）；monitor 相关一律用**录制样本**做夹具，不依赖实时 QEMU。
- **P6 模板与向导**：模板生成 → 构建 → 启动 → 横幅断言；工具链检测要给**可执行的修复建议**。
- **P7 AI**：provider 抽象用本地 mock 做单测；真实调用默认跳过（需显式环境变量）；AI 不可用不得影响主流程。
- **P8 产品化**：一条命令跑通端到端冒烟；README 命令逐条可复现；打包产物由用户本地确认运行。

---

## 六、主 Agent 的独立验收协议（固定动作）

1. **复核人 ≠ 实现人**：亲自执行该阶段全部验收命令，比对真实输出，不接受转述。
2. **抽查负样本**：至少打一次失败路径（构建失败 / 超时 / 缺失符号）。
3. **污染检查**：确认没有引用废弃草稿（`_work/` 等）；确认构建产物与 `.toolchain/` 没被提交进 git。
4. **越界检查**：确认 Agent 只改了自己被授权的目录（`git show --stat` 可查）。
5. 复核结论写进 `docs/reports/<phase>-review.md`，**与实现者报告分开**。
