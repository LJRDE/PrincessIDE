# P2-B2 交付报告 · `crates/princess-run`

> 状态：**完成**。所有断言均由真实 QEMU 运行产生，命令、原始输出与退出码逐条列出。
> 复核方式：`bash .scratch/run/acceptance.sh` 一条命令复现全部 E2E 证据（脚本自建探针 ISO，不依赖任何其它 Agent 的临时目录）。
> 交付物：`crates/princess-run/`（5 个模块 + 1 个验收二进制）、根 `Cargo.toml` 仅加成员一行、本报告。

---

## 0. 结论速览

| 验收项 | 结果 | 证据位置 |
|---|---|---|
| P2-3 串口事件流含横幅 | ✅ 通过 | §3.1 / §3.2 |
| P2-5 四种 `run.exited.reason` 全对 | ✅ 通过 | §3.3 ~ §3.6 |
| P2-7 死循环 → `timeout` 且**无孤儿 QEMU** | ✅ 通过 | §3.5 |
| 覆盖两个夹具（`#UD` + `#PF`，**先确认 `PG=1`**） | ✅ 通过 | §3.1 / §3.2 |
| `cargo test -p princess-run` 全绿 | ✅ **62 passed; 0 failed** | §2.1 |
| `cargo clippy -p princess-run` | ✅ 0 warning | §2.2 |
| 全部 E2E 断言 | ✅ **PASS=33 FAIL=0**, exit 0 | §3.7 |

**最有价值的一条结论（D9 的实测复现）**：三击故障探针的 QEMU **退出码是 0**，与 ACPI 正常关机**逐位相同**；唯一区分依据是 `-d cpu_reset` 日志里的 `Triple fault` 文本。本实现对同一次运行分别用 `-d int,cpu_reset,guest_errors` 与 `-d int` 做了**对抗性负样本对照**（§3.4、§3.8），证明两者缺一不可。

---

## 1. 交付物与公开 API

### 1.1 文件

| 文件 | 职责 |
|---|---|
| `src/qemu.rs` | QEMU argv 构造：**D2** 引导路径、**D9** 串口/监视器/`-d` 规则；`-kernel` 拒收 ELF64 |
| `src/serial.rs` | 串口观测 → `log.append(serial.com1)` / `run.fault`；`ripText` **逐字保留**；`#PF` 的 `PG=1` 前置门 |
| `src/exit.rs` | 证据装配 → 调用 **core 的** `classify_exit`；三击故障仅从 `-D` 日志判定 |
| `src/process.rs` | 子进程：独立进程组、`/proc` 扫描存活、**先落盘再推送**、超时/取消按**进程组**收尾 |
| `src/backend.rs` | `RunBackend` 实现 + 事件时序编排 |
| `src/bin/run-e2e.rs` | 验收载体：跑真 QEMU，NDJSON 事件流打 stdout 并 `--record` 落盘 |

### 1.2 关键公开 API

```rust
// 计划（D2 / D9 的单一执行点）
pub fn plan_run(inputs: &PlanInputs, medium: &BootMedium, options: &PlanOptions) -> Result<RunPlan>;
pub fn check_argv_contract(argv: &[String]) -> Result<()>;   // 执行前再验一次 D9
pub fn partition_args(args: &[String]) -> PartitionedArgs;   // 引擎自有 flag 剥离并上报
pub fn classify_elf(path: &Path) -> Result<ElfClass>;

// 串口与故障（D3 / D9）
pub struct SerialObserver;                       // feed(line) -> Vec<SerialObservation>
pub struct RawFault { pub rip: u64, pub rip_text: String, /* ... */ }
pub enum PageFaultGate { PagingConfirmed, PagingUnproven, PagingDisabled, NotApplicable }
pub fn banner_in_log(&str) -> Option<String>;
pub fn fault_rip_in_log(&str) -> Option<u64>;

// 退出归因（D9：绝不自己再写一套）
pub struct QemuEvidence;  // -> into_facts() -> princess_core::classify_exit
pub fn detect_triple_fault(&str) -> bool;
pub fn triple_fault_in_file(&Path) -> bool;

// 进程（进程组纪律）
pub trait ProcessRunner { fn run(&self, spec, cancel, on_chunk) -> Result<ProcessOutcome>; }
pub fn kill_group(pgid, signal) -> Result<()>;
pub fn group_alive(pgid) -> bool;                 // /proc 扫描，僵尸不算存活
pub struct SystemRunner;  pub struct FakeRunner;  // 后者供无 QEMU 单测

// 后端
pub struct QemuBackend<R = SystemRunner>;
impl QemuBackend { pub fn plan_for(..); pub fn launch_plan(..) -> Result<RunOutcome>; }
pub struct ResolvedQemuBackend<R = SystemRunner>;  // impl princess_core::RunBackend
pub type SymbolizeFn<'a> = dyn Fn(u64) -> Option<SymbolicatedLocation> + Send + Sync;
```

**唯一出口是 `EventSink`**（core 的 trait）：本 crate 不碰 `EventRecorder`，`seq`/`ts`/`opId` 仍归引擎。
**不解析 DWARF**：`run.fault.symbolicated` 由调用方注入的 `SymbolizeFn` 填充，符号化归 `princess-symbol`（B3）/P5，避免第二套真相。

---

## 2. 单测与静态检查

### 2.1 `cargo test -p princess-run`

```console
$ CARGO_BUILD_JOBS=2 cargo test -p princess-run
test result: ok. 62 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.63s
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```
**退出码 0**（`EXIT=0`）。

单测里几条特意为契约写的负样本/边界：

- `an_elf64_kernel_medium_is_refused_with_the_d2_explanation` — D2：ELF64 走 `-kernel` 在 **plan 期**就报 `E_INVALID_CONFIG` 并指出应改用 `-cdrom`，而不是让 QEMU 抛难懂错误。
- `check_argv_contract_rejects_a_plan_that_could_not_see_a_triple_fault` — 把 `-d` 改成 `int`（缺 `cpu_reset`）必须被拒；`-nographic`、`-monitor stdio` 同样被拒。
- `exit_zero_with_a_triple_fault_in_the_log_is_not_a_shutdown` — D9 的核心陷阱，固化为单测。
- `a_grandchild_is_killed_with_the_group_and_leaves_no_orphan` — 起 `sleep 30 &`，超时后断言孙进程真的消失（不是变成僵尸）。
- `the_serial_tee_is_written_before_the_chunk_is_handed_over` — 回调里**当场读文件**，证明「先落盘再推送」。
- `a_garbled_fault_line_produces_no_event` / `a_page_fault_without_paging_evidence_is_flagged_unproven` — 不编造、不假装已验证。

### 2.2 `cargo clippy -p princess-run`

```console
$ cargo clippy -p princess-run 2>&1 | grep -cE "^warning.*princess-run"
0
```
本 crate **0 warning**。（`princess-core` 自身有 9 条 `derivable_impls` 提示，属冻结的上游 crate，未改动。）

---

## 3. E2E 验收（真实 QEMU）

复现命令（一条）：

```console
$ bash .scratch/run/acceptance.sh
...
PASS=33 FAIL=0
ACCEPTANCE_EXIT=0
```

脚本自建 3 个探针内核（`shutdown` / `triplefault` / `deadloop`，由 `fixtures/refkernel/` 派生并改动 `kernel_main`），因此**不依赖任何其它 Agent 的 `.scratch/` 目录**。

### 3.1 P2-3 · 参考夹具 `#UD`（`fixtures/refkernel/`）

```console
CMD: target/debug/run-e2e --project fixtures/refkernel \
     --boot fixtures/refkernel/build/refkernel.iso \
     --record .scratch/run/e2e/refkernel.ndjson --expect-reason timeout --timeout-ms 8000 --verbose
EXIT: 0
LAST: [run-e2e] last fault: vector=#UD rip=1051453 ripText=0x0000000000100b3d
PASS  refkernel: banner in log.append(serial.com1)
PASS  refkernel: run.fault vector=#UD rip=0x100b3d with verbatim ripText
PASS  refkernel: seq strictly monotonic and run.exited last
PASS  refkernel: run.started argv honours D9 (no -nographic, -serial stdio, -monitor none, -d int,cpu_reset)
PASS  refkernel: no orphan qemu-system-x86_64 (P2-7)
```

`run.fault` 的实际载荷（`ripText` 是 guest 原文，未被重新格式化）：

```json
{ "vector": "#UD", "rip": 1051453, "ripText": "0x0000000000100b3d", "errorCode": 0,
  "regs": { "CS": "0x0008", "RFLAGS": "0x0000000000000046", "RIP": "0x0000000000100b3d" } }
```

`0x100b3d` 与 **D3 冻结常量**逐字一致。

### 3.2 P2-3 / D9 · 分页夹具 `#PF`（`fixtures/paging-kernel/`）

```console
CMD: target/debug/run-e2e --project fixtures/paging-kernel \
     --boot fixtures/paging-kernel/build/pagingkernel.iso \
     --record .scratch/run/e2e/paging.ndjson --expect-reason timeout --timeout-ms 8000 --verbose
EXIT: 0
LAST: [run-e2e] last fault: vector=#PF rip=1053149 ripText=0x00000000001011dd
PASS  paging: banner in log.append(serial.com1)
PASS  paging: run.fault vector=#PF rip=0x1011dd verbatim
PASS  paging: D9: CR0.PG=1 observed BEFORE the #PF
PASS  paging: reason=timeout
PASS  paging: no orphan qemu-system-x86_64 (P2-7)
```

**D9 的 `#PF` 前置条件**在这里是可机器验证的：串口流先出现 `CR0.PG=1`，之后才有 `run.fault`。
事件序列（`seq` 从 1 连续到 50，末事件是 `run.exited`）：

```text
1  run.started
3  log.append ide    guest banner observed on serial.com1: PrincessIDE paging kernel booted
11 log.append ide    guest paging state: CR0.PG=1          ← PG=1 证据
41 run.fault         #PF ripText=0x00000000001011dd        ← 之后才是故障
48 log.append ide    serial: banner_seen=true panic_seen=true faults=1 last_fault=0x…1011dd #PF
50 run.exited        timeout
```
共 **41 个 `log.append(stream=serial.com1)`** 事件承载完整 serial 文本（逐行推送；事件数随 guest 打印行数与分块时机变化，`seq` 单调性才是契约断言）。

### 3.3 P2-5 · `guest-shutdown`

```console
CMD: target/debug/run-e2e --project .scratch/run/probes/shutdown \
     --boot .scratch/run/probes/shutdown/build/refkernel.iso \
     --record .scratch/run/e2e/shutdown.ndjson --expect-reason guest-shutdown --timeout-ms 20000 --verbose
EXIT: 0
LAST: [run-e2e] reason=guest-shutdown exit=Some(0) uptime=1270ms
PASS  shutdown: reason=guest-shutdown with exitCode 0
PASS  shutdown: no orphan qemu-system-x86_64 (P2-7)
```
探针通过 ACPI PM（`PM1_CNT`, port `0x604`）请求软关机，QEMU 正常退出。

### 3.4 P2-5 · `triple-fault`（**D9 关键：退出码 0 但不是关机**）

```console
CMD: target/debug/run-e2e --project .scratch/run/probes/triplefault \
     --boot .scratch/run/probes/triplefault/build/refkernel.iso \
     --record .scratch/run/e2e/triplefault.ndjson --expect-reason triple-fault --timeout-ms 20000 --keep-debug-log --verbose
EXIT: 0
LAST: [run-e2e] reason=triple-fault exit=Some(0) uptime=1198ms
PASS  triplefault: reason=triple-fault DESPITE exitCode 0 (D9)
PASS  triplefault: -d log contains 'Triple fault' (cpu_reset channel)
PASS  triplefault: no orphan qemu-system-x86_64 (P2-7)
```
注意 `exitCode` 与 §3.3 的关机**同为 0**，而 `reason` 正确区分开了——归因来自 `-D` 日志，不是退出码。

### 3.5 P2-7 · 死循环 → `timeout`，**无孤儿**

```console
CMD: target/debug/run-e2e --project .scratch/run/probes/deadloop \
     --boot .scratch/run/probes/deadloop/build/refkernel.iso \
     --record .scratch/run/e2e/deadloop.ndjson --expect-reason timeout --timeout-ms 4000 --verbose
EXIT: 0
LAST: [run-e2e] reason=timeout exit=None uptime=4034ms
PASS  deadloop: reason=timeout
PASS  deadloop: run.fault x0
PASS  deadloop: no orphan qemu-system-x86_64 (P2-7)
```

孤儿断言（脚本内 `qemu_count()`，按 `comm` 精确匹配，不会自匹配 shell 命令行）：

```console
$ ps -eo comm | grep -c '^qemu-system-x86'
0
```

> **关于任务书里字面命令 `pgrep -a qemu-system-x86_64` 的一个实测坑**：该名字长度 >15，`pgrep` 不使用 `-f` 时**永远匹配不到**，只会往 stderr 打印
> `pgrep: pattern that searches for process name longer than 15 characters will result in zero matches`，然后以退出码 1 结束。
> 因此「`pgrep -a qemu-system-x86_64` 为空」这条断言**天然恒真、不构成证据**；本报告改用 `ps -eo comm` 精确匹配作为**真正有效**的孤儿断言（结果 0）。用 `pgrep -f` 则会被自身命令行误匹配，同样不可靠。
> 无论哪种口径，结论一致：**超时后没有任何 QEMU 残留，也没有 PPID=1 的孤儿**。

### 3.6 P2-5 · `killed`（真实取消）

```console
CMD: target/debug/run-e2e --project .scratch/run/probes/deadloop \
     --boot .scratch/run/probes/deadloop/build/refkernel.iso \
     --record .scratch/run/e2e/killed.ndjson --expect-reason killed --timeout-ms 30000 --cancel-after-ms 2500 --verbose
EXIT: 0
LAST: [run-e2e] reason=killed exit=None uptime=2544ms
PASS  killed: reason=killed
PASS  killed: no orphan qemu-system-x86_64 (P2-7)
```
超时上限设 30000ms 而取消在 2500ms 触发，因此 `reason=killed` 只可能来自取消。

### 3.7 结果汇总

```console
$ bash .scratch/run/acceptance.sh
...
=== SUMMARY ===
PASS=33 FAIL=0
ACCEPTANCE_EXIT=0
```

### 3.8 D9 对抗性负样本：只用 `-d int` 会**看不见**三击故障

```console
CMD: qemu ... -d int -D .scratch/run/e2e/tf-intonly.log  (int only, no cpu_reset)
exit: 0 (the probe still exits 0, which is exactly the D9 trap)
'Triple fault' occurrences with -d int only: 0
PASS  D9 negative control: -d int alone hides the triple fault
```
这与 §3.4（`int,cpu_reset` 下出现 1 次）构成对照，是「`-d int` 与 `-d cpu_reset` **缺一不可**」的直接实测证据。

---

## 4. 上游接口提示（≤5 条）

1. **`RunBackend::plan/launch` 的签名不含「已解析工程」**。core 的 `RunBackend::plan(&ResolvedProject, &BootMedium)` 拿不到 `[run] args` 之外的信息，我把解析结果装进 `PlanInputs` 由 `ResolvedQemuBackend` 持有。P2-C 集成时请用 `ResolvedQemuBackend::new(QemuBackend::new(RunOptions{..}), PlanInputs{..})`，`PlanInputs.serial_tee` 与 `root` 必须是**绝对路径**（QEMU 的 cwd 是 `build_cwd`，相对路径会解析错——这正是我在 E2E 里踩到并修掉的坑）。
2. **`RunBackend::stop(op_id)` 无法仅凭 opId 杀进程组**。契约要求按进程组收尾，但进程组句柄（pgid）只有运行期才有；我让 `stop` 明确返回 `E_INTERNAL` 并说明原因，真正的停止路径是持 `CancelToken` 的取消。若 P3/P2-C 需要 `princess:run:stop`，请把 pgid 登记表放在编排层（Tauri 壳/CLI），不要指望后端能靠 id 找回进程组。
3. **`run.fault.symbolicated` 靠外部注入**。`QemuBackend::launch_plan(.., symbols: Option<&SymbolizeFn>)`；`ResolvedQemuBackend::with_symbolizer(Box<dyn Fn(u64)->Option<SymbolicatedLocation> + Send + Sync>)`。B3 交付后把 `source_line_for_address` 包一层传进来即可，本 crate 无需改动。
4. **`run.fault` 每次运行最多 16 条**（`MAX_FAULT_EVENTS`），超出会在 `ide` 流里显式写 `suppressing further run.fault events` 与 `suppressed=N`。前端不要假设「故障数 == 事件数」，请读摘要行。
5. **`-D` 调试日志默认用完即删**（`RunOptions.keep_debug_log=false`），仅在需要三击故障取证时保留。需要留存时置 `true`；日志路径固定为 `<root>/build/qemu-debug.log`（可由 `PlanOptions.debug_log` 覆盖）。

---

## 5. 有歧义/可能被质疑的设计决定

| # | 决定 | 理由与风险 |
|---|---|---|
| 1 | 串口无「关机」专有标志时，**退出码 0 且无三击故障 → `guest-shutdown`** | core 的 `classify_exit` 已冻结此语义（"ACPI shutdown is the only path that makes QEMU exit 0 without `-no-shutdown`"）。我**没有**再写第二套归因，只负责把证据（尤其 `triple_fault`）备齐。风险：QEMU 因其它原因干净退出会被记成关机；缓解手段是 `run.started` 里保留了完整 argv、且摘要行同时给出 `exit` 与 `banner_seen`。 |
| 2 | 把 `#PF` 的 `PG=1` 做成**可观测门**（`PageFaultGate`）而非硬拒绝 | D9 说「`#PF` 判定前必须先确认 `CR0.PG=1`」。探针确实打印了 `CR0.PG=1`，所以正式夹具路径得到 `PagingConfirmed`；但若上游夹具不打印 paging 状态，**直接丢弃 `run.fault` 会掩盖真实故障**。折中：仍上报故障，同时在 `ide` 流写「paging is unproven」，把不确定性显式交给用户。这是**刻意的**「宁可标注不确定，也不静默丢事实」。 |
| 3 | `-no-reboot` 始终由引擎追加，且**绝不**用其退出码做 panic 依据 | D9 明文要求。实现上 `triple_fault` 标志**只能**由 `-D` 日志置位。 |
| 4 | 验收探针（shutdown/triplefault/deadloop）建在 `.scratch/run/probes/`，不进 `fixtures/` | 任务书只授权我写 `crates/princess-run/`、`docs/reports/p2-run.md`、`.scratch/run/`。`fixtures/` 归 P0。`acceptance.sh` 会按需从参考夹具重新生成探针，故证据可离线复现；若主 Agent 认为这些探针应升格为正式夹具，请裁决后再迁入 `fixtures/`。 |
| 5 | `run-e2e` 自带一个基于系统 `addr2line` 的 `--symbolize` | 仅为证明 `run.fault.symbolicated` 的**管道**通（P2-4 的完整符号化归 B3）。不引入对 `princess-symbol` 的依赖，避免循环依赖与第二套真相。 |
| 6 | 报告改用 `ps -eo comm` 而非任务书字面的 `pgrep -a qemu-system-x86_64` | 见 §3.5 的实测说明：字面命令因进程名 >15 字符**恒为空**，作为断言无效。改用能真正发现孤儿的检查。 |
