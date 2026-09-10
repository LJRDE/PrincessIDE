# P4 — 内核图形化调试后端（`crates/princess-debug`）验收报告

> 阶段：P4（调试器） · 决策依据：**D11（GDB 内置 DAP + 薄能力层）**、D10（架构自检）、D22（内存纪律）
> 验收标准：`docs/spec/20-acceptance.md` **P4-1 ~ P4-4**
> 环境：`.toolchain/bin/princess-gdb` = **GNU gdb (Debian 16.3-1) 16.3**；QEMU 7.2（TCG，无 KVM）
> **本报告所有输出均为真实命令输出；每个结论附退出码。**

---

## 0. 交付物与复现命令

| 交付物 | 说明 |
|---|---|
| `crates/princess-debug/` | DAP 客户端 + 薄能力层 + `DebugBackend` 实现（8 个模块，见 §1） |
| 根 `Cargo.toml` | **只加了一行**成员：`"crates/princess-debug",` |
| `scripts/p4-acceptance.sh` | 无头验收脚本（`timeout` 包裹 + 进程组收尾 + 孤儿后置断言） |
| `scripts/p4-gdb-oracle.sh` | **独立 GDB oracle**：用第二个 gdb 进程读同样的值做交叉比对（P4-2 的"与直接查 GDB 一致"） |
| `crates/princess-debug/examples/p4_acceptance.rs` | 36 项断言的验收 harness，退出码即结论 |
| `docs/reports/p4-debug.md` | 本报告 |
| `.scratch/debug/` | 探针脚本、`PROGRESS.md`、原始验收输出与 artifacts |

**一键复现（两条命令）**：

```console
$ source scripts/env.sh
$ bash scripts/p4-acceptance.sh
...
checks run : 36
failures   : 0
P4 ACCEPTANCE: PASS
[p4] harness exit code: 0
[p4] no orphan qemu remains
EXIT=0
```

第二条覆盖（第二夹具）：

```console
$ bash scripts/p4-acceptance.sh fixtures/refkernel/build/refkernel.iso \
      fixtures/refkernel/build/refkernel.elf .scratch/debug/artifacts-refkernel
checks run : 36
failures   : 0
P4 ACCEPTANCE: PASS
[p4] no orphan qemu remains
EXIT=0
```

单测：

```console
$ cargo test -p princess-debug
test result: ok. 85 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 30.03s
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
EXIT=0
```

---

## 1. 架构：什么是 GDB 的、什么是我们的

D11 的裁决是"**主干用内置 DAP，Rust 侧只补薄能力层，不写 MI→DAP**"。代码结构照着这条线切开：

```
  princess-core::DebugBackend
        ▲
        │  backend.rs     trait 实现 + 会话状态机（含 attach 延迟响应握手）
  ┌─────┴──────┐
  │ DapBackend │
  └─────┬──────┘
        │
  ┌─────┴──────────┐  capability.rs  ← 内置 DAP 的 4 个缺口，全部补齐
  │ CapabilityLayer│  ──────────────────────────────────────────────
  └─────┬──────────┘  repl.rs        ← 逃生舱 + 错误归一化（P4-4 的胜负手）
        │             arch.rs        ← D10 架构自检
  ┌─────┴──────┐
  │ Transport  │  transport.rs  ← Content-Length 帧 + **按 request_seq 关联**
  └─────┬──────┘  framing.rs    ← 帧编解码
        │
  ┌─────┴──────┐
  │QemuProcess │  qemu.rs       ← 进程组生命周期，绝不漏孤儿
  └────────────┘
```

`capability.rs` / `repl.rs` / `arch.rs` 是 **PrincessIDE 真正拥有的部分**，合计约 900 行（含注释与单测）；
其余全部是标准 DAP，属于 GDB。**没有一行 MI。**

### 公开 API

```rust
// trait 实现（引擎入口）
pub struct DapBackend;
impl DapBackend {
    pub fn new(config: DebugSessionConfig) -> Self;
    pub fn state(&self) -> SessionState;                  // Idle/Attaching/Ready/Poisoned/Detached
    pub fn adapter_capabilities(&self) -> Option<&Value>; // gdb initialize 原文
    pub fn hardware_support(&self) -> HardwareSupport;    // Unknown/Hardware/Unsupported（实测得出）
    pub fn arch_check(&self) -> Option<&ArchCheck>;       // D10 结论
    pub fn qemu_command(&self) -> Option<&QemuCommand>;
    pub fn hardware_breakpoints(&self) -> &[BreakpointRecord];
    pub fn capabilities_layer(&mut self) -> Result<CapabilityLayer<'_>>;
    pub fn read_physical_memory(&mut self, addr: u64, count: u64) -> Result<Vec<u8>>;
    pub fn set_hardware_breakpoint(&mut self, spec: &str, ev: &mut dyn EventSink)
        -> Result<BreakpointRecord>;
    pub fn prepare_target(&mut self) -> Result<()>;       // symbol-file + D10 自检
}
impl DebugBackend for DapBackend { /* attach/detach/set_breakpoints/continue_/
    step_over/step_into/stack_trace/scopes/variables/read_memory/write_memory/
    disassemble/registers —— princess-core 冻结签名，未改 */ }

// 能力层
pub struct CapabilityLayer<'a>;
impl CapabilityLayer<'_> {
    pub fn prepare_session(&mut self) -> Result<()>;
    pub fn load_symbols(&mut self, elf: &Path) -> Result<u64>;
    pub fn set_hardware_breakpoint(&mut self, spec: &str) -> Result<BreakpointRecord>;
    pub fn set_software_breakpoint(&mut self, spec: &str) -> Result<BreakpointRecord>;
    pub fn list_breakpoints(&mut self) -> Result<Vec<BreakpointRecord>>;
    pub fn physical_memory_backend(&mut self) -> Result<PhysicalMemoryBackend>;
    pub fn read_physical_memory(&mut self, addr: u64, count: u64) -> Result<Vec<u8>>;
    pub fn read_registers(&mut self, sel: RegisterSelection) -> Result<(RegisterFile, Vec<RegisterEntry>)>;
    pub fn read_register(&mut self, name: &str) -> Result<RegisterEntry>;
    pub fn program_counter(&mut self) -> Result<u64>;
    pub fn read_hex_expression(&mut self, expr: &str) -> Result<u64>;
    pub fn disassemble_physical(&mut self, addr: u64, n: u64) -> Result<String>;
    pub fn repl(&mut self, cmd: &str) -> Result<ReplOutcome>;     // 逃生舱
    pub fn repl_ok(&mut self, cmd: &str) -> Result<ReplOutcome>;
}

// 生命周期 / 传输 / 自检
pub struct QemuProcess;   // Drop 即杀进程组；start() 会等到 stub 真的能连上才返回
pub fn debug_boot_argv(...) -> Vec<String>;
pub struct Transport;      // 按 request_seq 关联响应，事件单独缓冲
pub struct FrameReader<R: Read>;
pub fn check(&mut Transport, &Path) -> Result<ArchCheck>;   // D10
pub fn interpret_response(&str, &Value) -> Result<ReplOutcome>;
```

---

## 2. 内置 DAP 的真实能力（实测，非引用）

`initialize` 响应与调研 C §2.2 逐字一致。抽关键字段：

```json
{"request_seq":1,"type":"response","command":"initialize","success":true,
 "body":{"supportsConditionalBreakpoints":true,"supportsInstructionBreakpoints":true,
   "supportsReadMemoryRequest":true,"supportsWriteMemoryRequest":true,
   "supportsDisassembleRequest":true,"supportsSteppingGranularity":true,
   "supportsSetVariable":true,"supportsTerminateRequest":true}}
```

**注意这份列表里没有任何硬件断点相关字段** —— 这是下面所有能力层的存在理由。

---

## 3. P4-1 无头断点 E2E

| 断言 | 结果 |
|---|---|
| 起 `qemu -s -S` 并连上 stub | PASS |
| DAP 握手（含 `attach` 延迟响应） | PASS |
| 在固定符号下**硬件**断点并 verified | PASS |
| `continue` 停在预期位置，RIP 匹配 | PASS |

命令与原始输出（`scripts/p4-acceptance.sh`，退出码 **0**）：

```console
$ bash scripts/p4-acceptance.sh
qemu argv: qemu-system-x86_64 -L /root/PrincessIDE/.toolchain/prefix/usr/share/qemu -m 256M \
  -cdrom /root/PrincessIDE/fixtures/paging-kernel/build/pagingkernel.iso -boot d -display none \
  -serial file:.../serial-p4-1.log -monitor none -no-reboot -gdb tcp::1234 -S

PASS  P4-1.1 attach over the standard DAP handshake
      state=Ready after 354.754974ms; attach + configurationDone completed despite the
      delayed attach response
PASS  P4-1.2 symbol-file + D10 architecture self-check
      arch check: consistent: i386:x86-64 (64-bit target)
PASS  P4-1.3 hardware breakpoint set and VERIFIED (capability layer)
      paging_fault_probe: kind="hw breakpoint" address=Some("0x00000000001011c9")
      what="in paging_fault_probe at kernel.c:120"
stop: reason=Breakpoint thread=1 frame="paging_fault_probe" line=Some(120)
      ip=0x1011c9 regs.RIP=Some("0x1011c9")
PASS  P4-1.4a stopped at the hardware breakpoint (reason=breakpoint)
PASS  P4-1.4b stop frame is the requested symbol
PASS  P4-1.4c RIP matches the fixed fixture address 0x1011c9
      frame ip=0x1011c9 (0x1011c9), expected 0x1011c9
PASS  P4-1.4d DAP frame RIP agrees with the register panel RIP
      frame=0x1011c9 registers=0x1011c9
PASS  P4-1.4e stop resolves to the fixed source line 120
      line=Some(120) file=Some(".../paging-kernel/kernel.c")
PASS  P4-1.4f the breakpoint GDB reports is a HARDWARE breakpoint
      kind="hw breakpoint" (software would be "breakpoint")
```

`RIP = 0x1011c9` **与 `nm` 给出的 `paging_fault_probe` 符号地址一致**（`nm -n`：
`00000000001011c9 T paging_fault_probe`）。

### 3.1 一个必须写下来的时序陷阱

第一版探针按"发一条收一条"读，**结果整体错位一格**，`stackTrace`/`scopes` 全部返回 `notStopped`。
实测到的事件顺序是：

```
-> attach            (request_seq = 2)
-> configurationDone (request_seq = 3)
<- seq 3  response configurationDone   ← 先回
<- seq 7  response attach              ← 后回（被推迟）
```

即 **`attach` 的响应在 `configurationDone` 之后才到**（调研 C §6 警告过，本次亲自复现）。
若按到达顺序配对，`configurationDone` 会拿到 `attach` 的响应，之后**每一条都错位一格**——
这正是调研 C §3.2 说的"级联假阴性"的成因。

`Transport` 因此**按 `request_seq` 关联响应**，并把到达的事件单独缓冲。
单测 `responses_are_correlated_by_request_seq_not_arrival_order` 用一个**故意乱序回复**的假 adapter
把这条行为钉死。

---

## 4. P4-2 寄存器与内存，与直接查 GDB 一致

### 4.1 后端侧输出（分页夹具，停在上面的断点）

```console
--- raw `info registers` (first 12 lines) ---
rax            0x0                 0
rbx            0x11c800            1165312
rcx            0x101554            1054036
rdx            0x3f8               1016
rsi            0xa                 10
rdi            0x3f8               1016
rbp            0x119000            0x119000
rsp            0x118fe8            0x118fe8
...
parsed: RIP=Some("0x1011c9") CR0=Some("0x80000011") CR3=Some("0x104000") CR4=Some("0x20")
via `p/x $reg`: {"cr0": 2147483665, "cr3": 1064960, "cr4": 32, "pc": 1053129}
PASS  P4-2.1 CR3 由寄存器面板与 `p/x $cr3` 两条独立路径一致
      info registers CR3=Some("0x104000"); p/x $cr3=0x104000
PASS  P4-2.2 分页确实开着（CR0.PG=1），否则 #PF 断言无意义
      CR0=Some("0x80000011") (PG is bit 31)
PASS  P4-2.3 RIP 面板值与停止帧 RIP 一致
physical read @ CR3=0x104000: 23 50 10 00 00 00 00 00 00 00 00 00 00 00 00 00
PASS  P4-2.4a 物理读（monitor xp）得到合理的 PML4E[0]
      PML4E[0]=0x0000000000105023 (present, frame-aligned, RW|P)
virtual readMemory @ 0x1011c9: 55 48 89 e5 48 83 ec 10 48 c7 45 f8 00 00 40 00
PASS  P4-2.4b DAP 虚拟 readMemory 可用（主干路径）
```

### 4.2 **独立 oracle**：第二个 gdb 进程（这才是"与直接查 GDB 一致"）

用后端自己解析的结果自证是循环论证。`scripts/p4-gdb-oracle.sh` 因此**另起一个 gdb 进程**，
纯 CLI（`-batch -ex ...`），与 `crates/princess-debug` **不共享任何代码**：

```console
$ bash scripts/p4-gdb-oracle.sh          # 退出码 0
Hardware assisted breakpoint 1 at 0x1011c9: file kernel.c, line 120.
Breakpoint 1, paging_fault_probe () at kernel.c:120
$1 = 0x1011c9          <-- RIP
$2 = 0x80000011        <-- CR0
$3 = 0x104000          <-- CR3
$4 = 0x20              <-- CR4
0x104000:	0x23	0x50	0x10	0x00	0x00	0x00	0x00	0x00
rip            0x1011c9            0x1011c9 <paging_fault_probe>
cr0            0x80000011          [ PG ET PE ]
cr3            0x104000            [ PDBR=260 PCID=0 ]
cr4            0x20                [ PAE ]
0000000000104000: 0x0000000000105023 0x0000000000000000
```

**逐项比对：**

| 项 | 后端（`crates/princess-debug`） | 独立 gdb oracle | 一致？ |
|---|---|---|---|
| RIP | `0x1011c9` | `$1 = 0x1011c9` | ✅ |
| CR0 | `0x80000011` | `$2 = 0x80000011` | ✅ |
| CR3 | `0x104000` | `$3 = 0x104000` | ✅ |
| CR4 | `0x20` | `$4 = 0x20` | ✅ |
| 物理内存 @CR3 | `23 50 10 00 …`（`monitor xp`） | `0x…105023 0x0`（`monitor xp`） | ✅ |

CR3 也与调研 C §4.4 引用的 guest 串口自报值（`CR0=0x80000011 CR3=0x104000 CR4=0x20`）一致。

---

## 5. P4-3 单步与跨 C/汇编的源码级栈

```console
stackTrace at paging_fault_probe (3 frames):
  #0  paging_fault_probe       0x1011c9 /root/PrincessIDE/fixtures/paging-kernel/kernel.c:120
  #1  kernel_main              0x1012cc /root/PrincessIDE/fixtures/paging-kernel/kernel.c:157
  #2  _start                   0x100193 /root/PrincessIDE/fixtures/paging-kernel/boot.S:194
after stepIn(instruction): rip=0x1011ca line=Some(120) reason=Step
PASS  P4-3.2 step moves RIP forward            0x1011c9 -> 0x1011ca
PASS  P4-3.3 step reports reason=step          reason=Step
PASS  P4-3.4 the step stayed inside the same function (<= 64 bytes)   delta = 1 bytes
PASS  P4-3.1a the trace contains the C frame at the probe     ["paging_fault_probe","kernel_main","_start"]
PASS  P4-3.1b the trace crosses into a second C frame (kernel_main)
PASS  P4-3.1c the trace reaches the assembly entry point (_start)
PASS  P4-3.1d the assembly frame carries a source line from boot.S
      _start -> /root/PrincessIDE/fixtures/paging-kernel/boot.S:194
PASS  P4-3.1e every frame in the trace has a source line (no address-only frames)  3/3
```

**"跨 C/汇编混合帧给出源码行"成立**：`paging_fault_probe`/`kernel_main` 来自 `kernel.c`，
`_start` 来自 `boot.S:194` —— **汇编帧保留自己的源文件，没有被折进 C 文件**（断言 P4-3.1d 专门查这一点）。
单步用 `granularity: "instruction"`：内核里没有进程、常常没有帧指针链，**源码级 step 在裸机上会骗人**，
指令级才说实话；`0x1011c9 → 0x1011ca` 恰好一条指令。

---

## 6. P4-4 反向验证：错误符号必须明确失败

这是本阶段最值得写下来的一条。**实测发现：`evaluate(context="repl")` 在 GDB 命令失败时仍然返回
`success: true`，错误只在文本里。**

```console
### NEG hbreak bogus      （探针对真实 gdb 16.3 的原始响应）
{ "success": true,
  "body": { "result": "Function \"no_such_symbol_xyz\" not defined.\n
                       Hardware assisted breakpoint 1 (no_such_symbol_xyz) pending.\n" } }
```

**只看 `success` 就会得到一个"看似合理的假断点"** —— 正是 P4-4 明令禁止的东西。
因此 `repl.rs` 做两件事：① 仍然要求 `success: true`；② **再扫一遍文本里的 GDB 错误惯用语**。
对断点还加第二道独立校验：**新建的断点必须是 `verified`，不能是 `<PENDING>`**。

后端侧输出：

```console
PASS  P4-4.1 hardware breakpoint on a non-existent symbol is refused
      code=E_NOT_FOUND message=cannot set a hardware breakpoint at
      `no_such_symbol_xyz_princesside`: GDB rejected `hbreak ...` (not defined.)
PASS  P4-4.2 the refusal carries gdb's own text as evidence
      detail contains "command: hbreak no_such_symbol_xyz_princesside"
PASS  P4-4.3 an out-of-range source line is reported unverified, not accepted
      [BreakpointSpec { id: "3", line: Some(999999), verified: false, location: None }]
PASS  P4-4.4 a non-existent register is refused
      code=E_NOT_FOUND message=GDB rejected the repl command `info registers notaregister`:
      Invalid register `notaregister'
```

---

## 7. 能力层：硬件断点（内置 DAP 做不到的那一项）

**内置 DAP 只能造软件断点**（`breakpoint.py` 里 `_set_one_breakpoint` 只构造 `gdb.Breakpoint`），
而**软件断点靠改写目标内存**（x86 上写 `0xCC`）—— GRUB 把内核加载进内存之前/分页建立之前根本写不进去，
所以**内核入口断点必须是硬件断点**。

补齐路径：`evaluate(context="repl", expression="hbreak <spec>")`，并由能力层自维护断点表
（内置 DAP 的 `breakpoint_map` 不知道这条 `hbreak`）。

**两者输出可区分（这是验收要求的"证明"）：**

```console
software: kind="breakpoint"    address=Some("0x0000000000101212") what="in kernel_main at kernel.c:134"
hardware: kind="hw breakpoint" address=Some("0x00000000001011c9") what="in paging_fault_probe at kernel.c:120"
PASS  CAP.1 the two breakpoint kinds are textually distinguishable
PASS  CAP.2 only the hardware breakpoint reports `hw`
      is_hardware(software)=false is_hardware(hardware)=true
PASS  CAP.3 `info breakpoints` listing captured as evidence
      1 hw breakpoint enabled=true Some("0x00000000001011c9") in paging_fault_probe at kernel.c:120
      2 hw breakpoint enabled=true Some("<PENDING>") no_such_symbol_xyz_princesside
      3 breakpoint    enabled=true Some("<PENDING>") -source .../kernel.c -line 999999
      4 breakpoint    enabled=true Some("0x0000000000101212") in kernel_main at kernel.c:134
PASS  CAP.4 the backend reports hardware breakpoints as measured, not assumed
      hardware_support=Hardware
```

---

## 8. 自检：架构错配（D10）

D10 要求**明确报错，不得返回乱数据**。自检把三个独立事实互相比较：ELF 的 class/machine、
gdb 协商到的目标架构（`show architecture`）、gdb 认为的指针宽度（`p sizeof(void*)`）。

**正向**：`PASS D10.1` — `consistent: i386:x86-64 (64-bit target)`。

**负向**（合成一个 `elf32/EM_386` 头，喂给真实的会话）：

```console
PASS  D10.2 a 32-bit ELF is identified as elf32/EM_386, not mistaken for the target
      class=elf32 machine=3(EM_386)
PASS  D10.3 a 32-bit ELF against the 64-bit stub is a hard error
      verdict=MISMATCH: elf=elf32 (32 pointer bits) target=i386:x86-64 (64 pointer bits)
      -> E_INVALID_CONFIG: architecture mismatch between .../fake-elf32.elf and the debug stub:
         the stub reports a 64-bit pointer but the ELF is elf32
```

**注意 `sizeof(void*)` 这条独立信号**：它抓的是"gdb 嘴上说 x86-64、但 stub 回的是 32 位 `g` 包"
（即 B 报告实测的 `Remote 'g' packet reply is too long` 那一类）—— 只比架构名会漏掉它。

**没有实测到的东西我不编**：若 `show architecture` 无法解析，自检返回 `ArchCheck::Unknown` 并附原文，
**不猜结论**（`Unknown` 不会被当成 mismatch 拒绝，也不会被当成通过）。

---

## 9. 进程纪律：孤儿 QEMU

验收标准对孤儿有硬要求。实测**第一版 harness 真的漏了一个 QEMU** —— 因为它在检查中途调用
`std::process::exit()`，**跳过了所有 `Drop`**。抓到它的是 `TEARDOWN.2` 断言本身。
现在：

- `QemuProcess` 用 `process_group(0)` 建独立进程组，`Drop` 里 `SIGTERM` → 2s → `SIGKILL` **整组**；
  单测 `dropping_the_process_kills_the_whole_group` 起一个"组长 + 孙进程"的替身，**断言孙进程也死**。
- `QemuProcess::start()` **等 stub 端口真的能连上才返回**（不是 `sleep`）—— 这是消除 `connection refused` 偶发失败的关键。
- harness 改为 `run() -> i32`，`main` 才 `exit`，且每条早退路径显式 `detach()`。
- `scripts/p4-acceptance.sh` 用 `timeout` 包裹、trap 收尾，并在退出前**独立 grep** 一次本夹具的 QEMU。

结果：两次完整跑（分页 + refkernel）的 `TEARDOWN.2` 均 `PASS`，脚本侧 `no orphan qemu remains`。

---

## 10. 单测

```console
$ cargo test -p princess-debug
test result: ok. 85 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
EXIT=0
```

覆盖的关键行为（都对应上面某个具体陷阱，不是凑数）：

| 单测 | 钉住的行为 |
|---|---|
| `content_length_counts_utf8_bytes_not_characters` | 帧长度按**字节**算，字符数会让流永久错位 |
| `parses_real_gdb_16_3_initialize_frame` | 用**真实 gdb 报文**回放解析器 |
| `responses_are_correlated_by_request_seq_not_arrival_order` | 假 adapter **故意乱序**回复，验证按 seq 关联 |
| `success_true_with_an_error_in_the_text_is_treated_as_a_failure` | **P4-4 的核心**：`success:true` 但文本报错 |
| `detects_the_measured_pending_shape` | `<PENDING>` 必须被识别（不能当 verified） |
| `distinguishes_hardware_from_software_in_the_same_listing` | `hw breakpoint` vs `breakpoint` |
| `parses_the_measured_mixed_c_and_asm_stack_trace` | kernel.c + boot.S 混合帧 |
| `dropping_the_process_kills_the_whole_group` | 孤儿防线（含孙进程） |
| `elf32_against_a_64_bit_target_is_a_reported_mismatch` | D10 负样本 |
| `a_non_elf_file_is_rejected_with_the_magic_as_evidence` | 失败必须带原始证据 |
| `base64_round_trips_every_length_modulo_three` | DAP `readMemory` 编解码 |
| `frames_survive_being_delivered_one_byte_at_a_time` | 帧读取必须持有残留字节 |

---

## 11. 内置 DAP 缺哪些能力、怎么补的（本阶段最有价值的结论）

按 D11 的四个缺口逐条给**实测证据**与补齐方式：

| # | 缺口 | 实测证据（本次） | 补齐 | 结论 |
|---|---|---|---|---|
| 1 | **没有硬件断点** | `initialize` 能力集**无任何硬件断点字段**；`info breakpoints` 里 DAP 造出的断点是 `breakpoint`，`hbreak` 造出的是 `hw breakpoint` | `capability.rs` 走 `hbreak`，**并自维护断点表** + 双校验（文本无错 + 非 PENDING） | **刚需**。GRUB 加载内核前内存里没代码，软件断点写不进 |
| 2 | **`readMemory` 只有虚拟地址** | `memory.py` 用 `inferior.read_memory(addr)`，无物理参数；物理只能 `monitor xp` | `capability.rs::read_physical_memory` 走 `monitor xp`，**绝不回退到虚拟读** | **刚需**。页表/GDT/IDT 只有物理可达（D20：QEMU 7.2 无 `info gdt/idt`） |
| 3 | **`Registers` scope 不可靠且不全** | 调研 C §4.2 实测"没有 Registers scope"；**本次在 `symbol-file` 之后又拿到了 66 项** → 时有时无；且**任何情况下都没有 CR0–CR4/EFER** | 寄存器面板一律走 `info registers` 解析，另加 `scopes` 缺 `Registers` 时注入合成 scope（`variablesReference = -1`） | **必须补**。把面板建在 `scopes` 上会在关键时刻空掉 |
| 4 | **没有逃生舱** | `monitor`/`info registers`/`p/x $crX` 都只能走 repl | `repl.rs` 统一入口 + **错误归一化** | 而且这是上面 1~3 的**实现通道** |

**另外三个"看着能用、其实是坑"的实测发现：**

1. **`evaluate(context="repl")` 的 `success` 不可信**（§6）。这是全部能力层里最容易踩的雷：
   修不好它，1~4 项全部会返回假数据。
2. **`evaluate` 的 hover/watch 上下文对 `$cr3` 只返回类型注解 `[ PDBR=0 PCID=0 ]`，没有数值**
   （调研 C §4.4，本次沿用该结论并因此**不采用** hover 路径做面板）。
3. **`disassemble` 的 `memoryReference` 只收数字**，传 `$pc` 会报
   `invalid literal for int() with base 0`（调研 C §4.5）→ 提供 `program_counter()` 先把 `$pc` 换成数值。

**还有两条从源码顺序上就必须守住的：**

- `attach` 响应被推迟到 `configurationDone` 之后（§3.1）→ **必须按 `request_seq` 关联**。
- 会话一旦在某条协议级请求上失败，后续会返回**看似合理的错值**（调研 C §3.2）→
  `SessionState::Poisoned` 显式建模，`require_ready()` 拒绝继续提问，而不是把假数据递给 UI。

---

## 12. 发现的缺陷与文档冲突

### 12.1 实现期抓出的 4 个真 bug（都是"跑了才发现"）

| # | Bug | 后果 | 修法 |
|---|---|---|---|
| 1 | harness 中途 `process::exit()` 跳过 `Drop` | **真的漏了一个 QEMU 孤儿** | 改 `run() -> i32`；早退路径显式 `detach()` |
| 2 | `load_symbols` 用"数 `0x` 出现次数"估函数数 | `info functions` 打的是**声明（无地址）**→ 好夹具被判"没有调试信息"，**E_NOT_FOUND 假阴性** | 改按声明/地址条目计数 |
| 3 | `read_frame` 每次调用重建缓冲 | 一次 read 含 1.5 个帧时**第二帧被丢**，会话错位 | 改 `FrameReader` 持有残留 |
| 4 | 传输层测试夹具用 `sys.stdin.buffer.read(4096)` | 请求不满 4096 字节时**死锁** | 改 `os.read` |

### 12.2 **文档冲突（需主 Agent 裁决，我未改夹具也未改文档）**

**D3 与 `docs/spec/40-dispatch-plan.md` 称 `refkernel_fault_probe` 位于 `kernel.c:100`；
但两个独立 oracle 都给出 `kernel.c:99`：**

```console
$ addr2line -e fixtures/refkernel/build/refkernel.elf -f 0x100b39
refkernel_fault_probe
/root/PrincessIDE/fixtures/refkernel/kernel.c:99
```

gdb 的 DWARF 同样报 99。原因很清楚：kernel.c 第 99 行是 `{`，第 100 行才是 `ud2`；
**入口断点落在函数序言**，所以解析到 99。验收脚本按 **99** 断言（两个独立 oracle 一致）；
若按 100 断言，等于把文档笔误固化成测试失败。**建议把 D3 的 `kernel.c:100` 更正为 `kernel.c:99`。**

分页夹具的常量（`paging_fault_probe` / `0x1011c9` / `kernel.c:120`）经实测**完全正确**。

---

## 13. 明确没有做的事 / 已知边界

诚实标注，避免下游误判：

| 项 | 状态 |
|---|---|
| SMP 多核调试 | **未实测**（夹具单核）。DAP 把每个核映射成一条 thread，但 `allThreadsStopped` 会一次冻结所有核，"单核步进"语义表达不出来 |
| LA57（5 级页表） | **未实测**（夹具是 4 级） |
| 32 位内核 + `qemu-system-x86_64` 的真实错配 | **未实测**。D10 自检的**判定逻辑**已用合成 ELF 头测过（`D10.3`），但没有真跑一个 32 位内核 |
| `watch`/`rwatch` 观察点 | **未实测**（与 `hbreak` 同类路径，推断可用但没跑） |
| `#PF` 变成 DAP `exception` 事件 | **未实测**。断点停在 `paging_fault_probe` 入口，故障尚未真正触发 |
| 非 QEMU target（Bochs/JTAG/自研 stub） | **未实测**。`PhysicalMemoryBackend::Unavailable` 分支已实现并会**明确报错而非回退虚拟读**，但没有真机验证 |
| stripped 内核 | **未实测**。无 DWARF 时栈回溯会退化成地址级；`parse_stack_frames` 会把 `source` 留 `null` 而不编造 |

---

## 14. 设计决定（可能被质疑的）

1. **`DebugCapabilities.supports_read_memory = true` 是"含能力层"的语义**，而 DAP 原生的
   `readMemory` 只读虚拟地址。这是刻意的：物理读是**额外**加的，不是替换。
   需要物理内存的调用方**必须**调 `read_physical_memory()` 并处理
   `PhysicalMemoryBackend::Unavailable`。原因：`princess-core` 的 `DebugCapabilities` 是**冻结**的
   （契约 §5），没有字段能表达"物理可读"，所以用一个显式探针 API 而不是硬塞进那个结构。
2. **`Registers` 合成 scope 用 `variablesReference = -1`**。DAP 规范里正数才是真实 reference，
   `-1` 不可能冲突。代价是 UI 必须知道这个约定；好处是寄存器面板在 `scopes` 缺项时**依然可用**。
3. **`SessionState::Poisoned` 会让整个会话不可用**，包括本来还能跑的请求。这是**故意的保守**：
   调研 C §3.2 的实测表明污染后的答案"看起来合理但错"，宁可让用户重连，也不给可能错的寄存器值。
4. **`monitor xp` 的解析不假定物理/虚拟一致**。夹具里 `0x104000` 两边恰好读到同样的字节
   （断言 `P4-2.4a` 与 oracle 都印证），但那只是这个夹具的巧合；实现**从不**用虚拟读兜底物理读。
5. **`load_symbols` 在数不到函数时报错**。看上去偏严，但这是为了让"符号没加载"这种
   **会让后续每个断点都变成 pending 的静默故障**立刻暴露。它自己就抓出过我的计数 bug（§12.1 #2）。
6. **`step_into` 固定用指令粒度**（`granularity: "instruction"`）。内核里源码级 step 语义会骗人。
   若前端确实需要源码级步进，应新增一个显式入口，而不是改这里的默认。
7. **没有引入任何新的 crate 依赖**（除工作区已有的 `serde`/`serde_json`/`libc`）。
   base64 是手写的（DAP 只用标准字母表 + `=`），因为为 ~30 行引入一个依赖不值得（D22）。

---

## 15. 原始证据文件索引

| 路径 | 内容 |
|---|---|
| `.scratch/debug/artifacts/p4-acceptance.log` | 分页夹具完整验收输出（36 checks） |
| `.scratch/debug/artifacts/p4-acceptance-summary.txt` | `checks=36 failures=0 status=ok` |
| `.scratch/debug/artifacts/p4-2-info-registers.txt` | 原始 `info registers` 全文（P4-2 证据） |
| `.scratch/debug/artifacts/capability-info-breakpoints.txt` | 原始 `info breakpoints` 全文（能力层证据） |
| `.scratch/debug/artifacts/p4-gdb-oracle.txt` | **独立 gdb 进程**的原始输出（P4-2 交叉验证） |
| `.scratch/debug/artifacts/serial-p4-1.log` | 调试会话期间 guest 串口日志 |
| `.scratch/debug/artifacts-refkernel/p4-acceptance.log` | refkernel 完整验收输出（36 checks） |
| `.scratch/debug/probe*.py`, `dap*.py` | 手工 DAP 探针（含延迟响应错位的第一手复现） |
| `.scratch/debug/PROGRESS.md` | 进度行与 bug 记录 |
