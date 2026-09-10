# PrincessIDE P2-A —— Cargo 工作区 + `princess-core` + `princess-cli` 报告

| 项目 | 值 |
| --- | --- |
| 阶段 | P2-A（core 契约层 + 无头验收载具） |
| 宿主 | Debian GNU/Linux 12 (bookworm), x86_64, 4 核, 无显示器, **KVM 不可用**（TCG 软件模拟） |
| 工作区 | `/root/PrincessIDE` |
| 本文档作者 | P2-A 执行 Agent |
| 所有权范围 | `Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml`、`.cargo/config.toml`、`crates/princess-core/`、`crates/princess-cli/`、`fixtures/events/`、`.scratch/core/`、本文档 |
| 依赖契约 | `docs/spec/10-contracts.md`（§2 事件 / §3 错误 / §4 `princess.toml` / §5 trait 边界）、`docs/spec/20-acceptance.md`（P2-1..P2-8） |
| 所有声明 | **均由真实命令输出支撑**；输出原样粘贴，未验证的内容在「已知缺口」中显式标注 |

---

## 1. 摘要

三件事做完了，并且都是**真跑**出来的：

1. **`princess-core` 冻结并编译通过** —— 事件模型、错误模型、`princess.toml`、trait 边界全部按契约逐条实现，JSON 形状由单元测试钉死（含逐字段顺序）。
2. **`princess-cli` 可无头驱动整条链路** —— `doctor` / `build` / `run` / `symbolicate` / `events`，输出 NDJSON 事件流，`--record` 落盘。
3. **P2-1..P2-8 全部实测通过**（P2-1 有一个需要说明的工作区范围问题，见 §5.1），并额外拿到了 `run.exited.reason` **四种取值全部真实复现**的证据。

三条硬证据（原样）：

```
PrincessIDE reference kernel booted
```

```json
{"v":1,"seq":22,"ts":"2026-09-10T17:10:28.863Z","opId":"op-7997","kind":"run.fault","payload":{"vector":"#UD","rip":1051453,"ripText":"0x0000000000100b3d","errorCode":0,"regs":{"CS":"0x0008","RFLAGS":"0x0000000000000046","RIP":"0x0000000000100b3d"},"symbolicated":{"symbol":"refkernel_fault_probe","file":"/root/PrincessIDE/fixtures/refkernel/kernel.c","line":100}}}
```

```json
{"v":1,"seq":29,"ts":"2026-09-10T17:10:42.703Z","opId":"op-7997","kind":"run.exited","payload":{"exitCode":0,"reason":"timeout","uptimeMs":15064}}
```

---

## 2. 交付物

| 路径 | 说明 |
| --- | --- |
| `Cargo.toml` | workspace 根（成员：`princess-core`、`princess-cli`；其它 Agent 追加了自己的成员） |
| `rust-toolchain.toml` | 固定 `stable`（已装在 `.toolchain/` 内；故意不列 `components`，避免 rustup 去官方源拉包） |
| `.cargo/config.toml` | `[net] retry`、`target-dir = "target"`、crates.io 源替换（USTC 镜像，由主 Agent 加入，本 Agent 保留） |
| `crates/princess-core/` | 领域核心：`event` / `error` / `config` / `traits` / `types` / `stream` / `hash` |
| `crates/princess-core/tests/event_fixture.rs` | P2-8 事件夹具验收测试（反序列化 + seq 单调 + 逐字节往返 + 篡改检测） |
| `crates/princess-cli/` | 无头验收载具：`args` / `env` / `output` / `proc` / `project` / `diagnostics` / `serial` / `symbolize` / `build` / `run` |
| `fixtures/events/refkernel-run.ndjson` | **P3 重放用事件夹具**（真实录制，29 事件，无手改） |
| `.scratch/core/` | 负样本与实验现场（`.scratch/` 已被 `.gitignore` 忽略） |

---

## 3. 冻结的公开 API（`princess-core` 0.1.0）

### 3.1 事件模型（契约 §2）

信封 `Event { v, seq, ts, opId, kind, payload }`，`Serialize` 为**手写**（保证字段顺序与 `kind`/`payload` 不会与 payload 类型脱节）。

`EventKind::ALL`（14 个，与契约表逐条对应）：
`log.append` `build.started` `build.diagnostic` `build.finished` `run.started` `run.fault` `run.exited` `debug.stopped` `debug.breakpoint.changed` `debug.output` `symbols.indexed` `artifact.changed` `ai.chunk` `ai.finished`

关键 payload（字段名与契约一致，camelCase）：
`LogAppendPayload{stream,chunk,encoding}`、`BuildStartedPayload{backend,toolchainId,argv,cwd}`、`BuildDiagnosticPayload{severity,file,line,col,message,source}`、`BuildFinishedPayload{status,exitCode,durationMs,artifacts[{path,kind,size,sha256}]}`、`RunStartedPayload{qemuArgv,gdbStub}`、`RunFaultPayload{vector,rip,ripText,errorCode,regs,symbolicated?}`、`RunExitedPayload{exitCode,reason,uptimeMs}`、`DebugStoppedPayload`、`DebugBreakpointChangedPayload`、`DebugOutputPayload`、`SymbolsIndexedPayload{artifact,buildId,symbolCount}`、`ArtifactChangedPayload`、`AiChunkPayload`、`AiFinishedPayload`。

配套：`EventRecorder<W: Write>`（分配 `seq`、NDJSON、逐事件 flush）、`EventSink`（backend 推事件用，`&mut dyn EventSink`）、`parse_ndjson`、`validate_stream`、`new_op_id()`、`EVENT_MODEL_VERSION = 1`。

### 3.2 错误模型（契约 §3）

`ErrorCode`（封闭 10 个，`as_str()` 稳定）：`E_TOOLCHAIN_MISSING` `E_BUILD_FAILED` `E_QEMU_FAILED` `E_TIMEOUT` `E_NOT_FOUND` `E_INVALID_CONFIG` `E_SANDBOX_DENIED` `E_CANCELLED` `E_AI_UNAVAILABLE` `E_INTERNAL`。
`PrincessError{code,message,detail}`（`detail` 装原始 stderr，契约 §0 规则 4）、`Result<T>`、`IpcResponse<T>` = `{ok:true,data}` / `{ok:false,error}`。

### 3.3 `princess.toml`（契约 §4）

`ProjectConfig{schema,project,build,run,debug,toolchain,ai}`，每个 section 都 `deny_unknown_fields`。
`from_toml_str` / `load` / `load_or_default` / `resolve(root)`；`ResolvedProject` 里所有路径已绝对化（`~`、`$VAR`、`${VAR}` 由引擎展开，相对路径按工程根解析）。
`schema` 必填且必须 `= 1`；未知键、未知枚举值、缺 `schema` 一律 `E_INVALID_CONFIG`。

### 3.4 trait 边界（契约 §5）

`BuildBackend`(detect/plan/execute/cancel)、`RunBackend`(plan/launch/serial/stop/classify_exit)、`DebugBackend`(attach/detach/set_breakpoints/continue_/step_over/step_into/stack_trace/scopes/variables/read_memory/write_memory/disassemble/registers)、`BinProvider`(open/sections/symbols/source_line_for_address/disassemble)、`AiProvider`(chat_completions/cancel)。
辅助类型（供 P2-B 直接用）：`BuildPlan`、`BootMedium::{Iso,Kernel,Disk}`、`RunPlan`、`ExitFacts{exit_code,signal,timed_out,cancelled,guest_powered_off,triple_fault}`、`RunOutcome`、`classify_exit(&ExitFacts)`、`CancelToken`、`ToolchainReport`/`ToolInfo`、`BinaryFacts`、`DebugCapabilities`。
另有 `Utf8Chunker`（UTF-8 安全分行，不切断码点；非法字节替换为 U+FFFD 并标 `utf8-lossy`）与 `hash::{sha256_hex,sha256_file,file_size_and_hash}`。

---

## 4. 验收逐项（命令原文 + 真实输出 + 退出码）

### 4.1 P2-1 单元测试

```
$ cargo test -p princess-core -p princess-cli
running 45 tests
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.24s
running 36 tests
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
EXIT=0
```

合计 **85 个测试全绿**（CLI 45 + core lib 36 + core 集成 4）。

覆盖的契约点（举要）：14 种事件 JSON 逐字节形状、`seq` 单调校验、错误码字符串稳定性与去重、`IpcResponse` 两种形状、`princess.toml` 正例（契约 §4 示例逐字段）、未知键 / 缺 `schema` / `schema=2` / 未知枚举 / 非 TOML 反例、路径解析与 `~`/`$ENV` 展开、UTF-8 不切断码点与 `utf8-lossy`、SHA-256 标准向量、退出归因优先级、`*_backend` trait object-safe 性。

**注意（工作区范围）**：`cargo test --workspace` 当前**不是绿的**，原因不在本 Agent 的 crate —— 见 §5.1。

### 4.2 P2-2 构建 E2E

```
$ cargo run -p princess-cli -- build --project fixtures/refkernel
EXIT=0
project: refkernel (config: no princess.toml in fixtures/refkernel; using Makefile-based engine defaults with target `iso`)
build ok in 523 ms (exit Some(0)); 3 artifact(s), 0 diagnostic(s); argv: /usr/bin/make iso
  artifact: /root/PrincessIDE/fixtures/refkernel/build/iso/boot/refkernel.elf (elf, 23992 bytes, sha256 fc41bca5472c219a…)
  artifact: /root/PrincessIDE/fixtures/refkernel/build/refkernel.elf (elf, 23992 bytes, sha256 fc41bca5472c219a…)
  artifact: /root/PrincessIDE/fixtures/refkernel/build/refkernel.iso (iso, 3708928 bytes, sha256 f834dc8a8f52cf85…)

build.finished: {"status": "ok", "exitCode": 0, "durationMs": 523}
artifacts: ['.../build/iso/boot/refkernel.elf', '.../build/refkernel.elf', '.../build/refkernel.iso']
```

**通过**：`build.finished.status=ok`，`artifacts[]` 含 `refkernel.elf`。
说明：该次 `make` 是 no-op（P0 已经构建过，夹具里 `build/` 是既有产物）。为证明**从零真编译**，另在 `.scratch/core/refkernel-clean/`（同一份源码、删掉 `build/`）跑了一次：

```
$ cargo run -q -p princess-cli -- build --project .scratch/core/refkernel-clean
EXIT=0
build ok in 7354 ms (exit Some(0)); 3 artifact(s), 0 diagnostic(s); argv: /usr/bin/make iso
  build log: gcc -m64 -g -c boot.S -o build/boot.o
  build log: gcc -m64 -std=gnu11 -O0 -g -ffreestanding ... -c kernel.c -o build/kernel.o
  build log: ld -m elf_x86_64 -T linker.ld --nostdlib --build-id=none -o build/refkernel.elf ...
  build log: grub-mkrescue --compress=xz -o build/refkernel.iso build/iso
  build log: ISO image produced: 1811 sectors
build.finished status= ok exitCode= 0 durationMs= 7354
   artifact: .../build/refkernel.elf elf 24024
   artifact: .../build/refkernel.iso iso 3708928
```

`sha256` 不是编的 —— 与 `sha256sum` 独立比对一致：

```
$ sha256sum fixtures/refkernel/build/refkernel.elf fixtures/refkernel/build/refkernel.iso
fc41bca5472c219aa82f2358c723c4ef150caa7f5f0fb0309b26b6dee1341c7e  fixtures/refkernel/build/refkernel.elf
f834dc8a8f52cf85e5568f9a925bbefa4e3daf8bc0a59969cc90bf3a6615d390  fixtures/refkernel/build/refkernel.iso
# 事件里 build.finished.artifacts[*].sha256 与上面逐字符相同
```

### 4.3 P2-3 运行 E2E（横幅）

```
$ cargo run -p princess-cli -- run --project fixtures/refkernel --record fixtures/events/refkernel-run.ndjson
EXIT=0
```

对应的 `log.append` 事件（原样）：

```json
{"v":1,"seq":13,"ts":"2026-09-10T17:10:28.093Z","opId":"op-7997","kind":"log.append","payload":{"stream":"serial.com1","chunk":"PrincessIDE reference kernel booted\r\n","encoding":"utf8"}}
```

**通过**：`stream=serial.com1` 且 `chunk` 含横幅字符串 `PrincessIDE reference kernel booted`（整行未被切断，见 §3.1 的 UTF-8 约定）。`\r\n` 是 QEMU `-serial stdio` 的真实输出（`cat -A` 验证为 `^M$`），引擎不做有损变换。

### 4.4 P2-4 fault E2E（`rip` + 符号化）

```json
{"v":1,"seq":22,...,"kind":"run.fault","payload":{"vector":"#UD","rip":1051453,"ripText":"0x0000000000100b3d","errorCode":0,
 "regs":{"CS":"0x0008","RFLAGS":"0x0000000000000046","RIP":"0x0000000000100b3d"},
 "symbolicated":{"symbol":"refkernel_fault_probe","file":"/root/PrincessIDE/fixtures/refkernel/kernel.c","line":100}}}
```

**通过**：`rip` 存在（`0x100b3d`，同时保留 guest 原始文本 `ripText`），`symbolicated.symbol=refkernel_fault_probe`、`file=.../kernel.c`、`line=100`。
独立复核（`addr2line` 原文也在事件流里）：

```
$ cargo run -q -p princess-cli -- symbolicate --project fixtures/refkernel
{"kind":"log.append","payload":{"stream":"ide","chunk":"addr2line: refkernel_fault_probe\n",...}}
{"kind":"log.append","payload":{"stream":"ide","chunk":"addr2line: /root/PrincessIDE/fixtures/refkernel/kernel.c:100\n",...}}
{"kind":"log.append","payload":{"stream":"ide","chunk":"symbolicate: 0x0000000000100b3d -> /root/PrincessIDE/fixtures/refkernel/kernel.c:100 (refkernel_fault_probe)\n",...}}
EXIT=0
```

### 4.5 P2-5 退出归因

参考内核运行（同 4.3/4.4 的命令）：

```json
{"v":1,"seq":29,...,"kind":"run.exited","payload":{"exitCode":0,"reason":"timeout","uptimeMs":15064}}
```

四种 reason **全部真实复现**（判据与实验见 §5.2）：

| reason | 实验 | 事件 | 退出码 |
| --- | --- | --- | --- |
| `timeout` | 参考内核（`#UD` 后停机死循环）；死循环变体（P2-7） | `{"exitCode":0,"reason":"timeout","uptimeMs":15064}` / `{"reason":"timeout","uptimeMs":4027}` | 0 |
| `killed` | 同参考内核，加 `--cancel-after-ms 2500` | `{"exitCode":0,"reason":"killed","uptimeMs":2532}` | 0 |
| `guest-shutdown` | 变体：`outw(0x604, 0x2000)` 触发 ACPI 软关机 | `{"exitCode":0,"reason":"guest-shutdown","uptimeMs":1344}` | 0 |
| `triple-fault` | 变体：`lidt` 空 IDT + `ud2`（异常无法投递 → 三击） | `{"exitCode":0,"reason":"triple-fault","uptimeMs":1373}`，并有 `log.append(ide)` 行 `guest reset without a recovery path (triple fault) observed in the QEMU log` | 0 |

### 4.6 P2-6 负样本：构建失败

语法错误注入在 `.scratch/core/badsyntax/kernel.c:115`（参考内核副本，`fixtures/refkernel/` **未被改动**）：

```
$ cargo run -q -p princess-cli -- build --project .scratch/core/badsyntax --strict
EXIT=1
```

事件（原样）：

```json
{"seq":5,"kind":"log.append","payload":{"stream":"build","chunk":"kernel.c:115:40: error: expected expression before ‘;’ token\n  115 |     int this_line_has_a_syntax_error = ;   /* P2-6 injected */\n      |                                        ^\n","encoding":"utf8"}}
{"seq":6,"kind":"build.diagnostic","payload":{"severity":"error","file":"/root/PrincessIDE/.scratch/core/badsyntax/kernel.c","line":115,"col":40,"message":"error: expected expression before ‘;’ token","source":"gcc"}}
{"seq":8,"kind":"build.diagnostic","payload":{"severity":"warning","file":"/root/PrincessIDE/.scratch/core/badsyntax/kernel.c","line":115,"col":9,"message":"warning: unused variable ‘this_line_has_a_syntax_error’ [-Wunused-variable]","source":"gcc"}}
{"seq":11,"kind":"build.finished","payload":{"status":"failed","exitCode":2,"durationMs":55,"artifacts":[]}}
```

**通过**：`status=failed`（退出码 2 被真实传递），`build.diagnostic` 的 `file` 是**绝对路径且指向注入行**、`line=115` 与注入位置一致；gcc 原始 stderr 也完整保留在 `build` 流里（未吞异常、未静默降级）。

### 4.7 P2-7 负样本：死循环 + 无孤儿

死循环变体 `.scratch/core/deadloop/kernel.c:128`（参考内核副本）：

```
$ cargo run -q -p princess-cli -- run --project .scratch/core/deadloop --timeout-ms 4000
EXIT=0
run reason=timeout exit=Some(0) uptime=4027 ms
{"seq":24,"kind":"run.exited","payload":{"exitCode":0,"reason":"timeout","uptimeMs":4027}}

$ pgrep -af qemu-system-x86_64 | grep -v "pgrep\|grep\|bash -c" || echo "(empty)"
(empty)

$ for p in /proc/[0-9]*; do l=$(readlink $p/exe 2>/dev/null); case "$l" in *qemu-system-x86_64*) echo "ORPHAN $p -> $l";; esac; done; echo scan-done
scan-done
```

**通过**：`reason=timeout`，且**没有任何残留 QEMU 进程**（`pgrep` 与 `/proc/*/exe` 双重检查）。
> 说明：直接跑 `pgrep -af qemu-system-x86_64` 会匹配到**执行这条命令的 shell 自身的命令行**，所以上面额外过滤掉了 shell/pgrep 自身；`/proc/*/exe` 扫描是本 Agent 的权威判据（`pgrep -x` 对 16 字符以上的进程名无效，实测会报 `pattern ... longer than 15 characters`）。另外 `kill_group_and_wait` 先 `SIGTERM` 再 `SIGKILL`，QEMU 收到 SIGTERM 会优雅退出，因此 `exitCode` 是 `0` 而不是信号码 —— 见 §5.2 第 2 点。

### 4.8 P2-8 事件契约校验

```
$ cargo run -q -p princess-cli -- events --file fixtures/events/refkernel-run.ndjson
EXIT=0
events: 29 event(s) parsed and validated from fixtures/events/refkernel-run.ndjson
events: model version 1, seq 1..=29 (strictly increasing)
events: log.append                 19
events: build.started              1
events: artifact.changed           3
events: symbols.indexed            2
events: build.finished             1
events: run.started                1
events: run.fault                  1
events: run.exited                 1
events: opId(s): op-7997
```

`princess-core` 侧（`crates/princess-core/tests/event_fixture.rs`）直接解析该夹具：

```
$ cargo test -p princess-core --test event_fixture
running 4 tests
test recorded_stream_deserialises_and_is_monotonic ... ok
test recorded_stream_carries_the_fixed_acceptance_evidence ... ok
test recorded_lines_round_trip_byte_for_byte ... ok
test a_tampered_stream_is_rejected ... ok
test result: ok. 4 passed; 0 failed
EXIT=0
```

**通过**：夹具被 `princess-core` 完整反序列化；`seq` 严格递增且从 1 开始稠密；`v == 1`；`opId` 唯一；每种事件的 JSON **逐字节往返一致**（最强的形状漂移检测）；横幅只出现一次且未被切断；`run.fault`/`run.exited` 的固定断言值命中；篡改（重复 `seq`、未知 kind、payload 类型错）一律被拒。

### 4.9 附：`doctor`（`princess:tools:detect` 的 CLI 形态）

```
$ cd /tmp && /root/PrincessIDE/target/debug/princess-cli doctor --quiet
workspace : /root/PrincessIDE
grub-mkrescue          ok         /usr/bin/grub-mkrescue (GRUB) 2.06-13+deb12u1   /usr/bin/grub-mkrescue
make                   ok         GNU Make 4.3   /usr/bin/make
doctor: all required tools present (21 resolved).
EXIT=0
```

（从 `/tmp` 执行也正确解析工作区 —— 这是本次修掉的一个真 bug，见 §6.1。）

---

## 5. 需要裁决/说明的问题

### 5.1 `cargo test --workspace` 为什么不是绿的（P2-1 的范围说明）

P2-1 的通过条件是「`cargo test --workspace` 全绿」。当前工作区里除了本 Agent 的两个 crate，还被其它在飞 Agent 追加了成员，它们**各自的代码尚在半成品状态**：

```
$ cargo test --workspace
error[E0583]: file not found for module `descriptor`
error[E0583]: file not found for module `guestmem`
error[E0583]: file not found for module `hex`
error[E0583]: file not found for module `monitor`
error[E0583]: file not found for module `pagetable`
error[E0583]: file not found for module `provider`
error[E0107]: enum takes 1 lifetime argument but 2 lifetime arguments were supplied
error[E0061]: this method takes 0 arguments but 1 argument was supplied
error[E0599]: no method named `byte` found for reference `&Instruction` in the current scope
error[E0599]: no associated function or constant named `new` found for struct `addr2line::Context<R>` in the current scope
error: could not compile `princess-bin` (lib) due to 16 previous errors
EXIT=101
```

同一时间窗内 `princess-build`（P2-B）的测试也有 4 个失败（与本 Agent 的 core 无关）：

```
running 50 tests
test result: FAILED. 46 passed; 4 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.63s
failures:
    artifacts::tests::a_stale_binary_is_not_claimed_as_this_builds_artifact
    clangd::tests::the_shim_records_a_compile_and_delegates_to_the_real_compiler
    diagnostics::tests::nasm_and_ld_shapes_are_understood
    toolchain::tests::the_manifest_override_wins_over_the_candidate_list
```

**本 Agent 无法在自己的所有权范围内让 `--workspace` 变绿**（不能改别人的 crate）。因此这里的结论是：**`cargo test -p princess-core -p princess-cli` 全绿（85/85，EXIT=0）**；`--workspace` 的红色来自 `princess-bin`（编译失败）与 `princess-build`（4 个测试失败），请在它们各自收敛后重跑 `cargo test --workspace` 作为 P2 整体验收。

### 5.2 关于 `reason=timeout`（按主 Agent 要求，逐条回答）

**Q1：是有意为之，还是掩盖了 bug？**

是**有意为之**，而且不是「因为没关进程所以一律超时」—— 判据是「谁结束了这台机器」：

```
run reason=timeout exit=Some(0) uptime=15064 ms
  qemu argv: .../qemu-system-x86_64 -L .../usr/share/qemu -m 256M -cdrom .../refkernel.iso -boot d
             -display none -monitor none -serial stdio -no-reboot -d int,cpu_reset,guest_errors -D .../build/qemu-debug.log
{"seq":25,"kind":"log.append","payload":{"stream":"ide","chunk":"run exceeded its 15000 ms deadline; killing process group 368456\n"}}
{"seq":26,"kind":"log.append","payload":{"stream":"qemu.monitor","chunk":"qemu-system-x86_64: terminating on signal 15 from pid 367975 (target/debug/princess-cli)\n"}}
```

原文证据链：① 参考内核串口打印了 `PANIC: unhandled CPU exception, halting`，随后 `cli;hlt` 死循环，**QEMU 自己永远不会退出**；② 唯一结束它的是引擎在 15000 ms 期限上发出的 SIGTERM（QEMU 自己的 stderr 行写明 `terminating on signal 15 from pid … (princess-cli)`）；③ `uptimeMs=15064` 与 `run.timeout_ms=15000` 吻合。若这是「掩盖 bug」，那么反例（下面 Q2 的 shutdown / triple-fault 变体）就不该被归成别的 reason —— 但它们确实被区分开了。

**Q2：判据是什么？为什么它「应当」是 timeout 而不是别的 reason？**

`reason` 回答的是「这台机器为什么停了」，四种取值的**可判定证据**如下（`classify_exit()` 的优先级：`cancelled > timed_out > triple_fault > guest_powered_off > exit code`）：

| reason | 判定依据 | 实测 |
| --- | --- | --- |
| `timeout` | **引擎的期限先到**，随后的 `exitCode` 是我们杀它造成的 | `uptimeMs=15064≈15000`；`qemu.monitor` 明写 `terminating on signal 15 … (princess-cli)` |
| `killed` | 取消（`run:stop` / `op:cancel` / `--cancel-after-ms`）先于期限 | `--cancel-after-ms 2500` → `{"reason":"killed","uptimeMs":2532}` |
| `guest-shutdown` | QEMU **自己**退出（exit 0）且日志里没有不可恢复的 reset：只有 guest 请求 ACPI 关机才会这样 | 变体 `outw(0x604,0x2000)` → `{"reason":"guest-shutdown","uptimeMs":1344}`，**没有任何 kill 事件** |
| `triple-fault` | QEMU `-d int,cpu_reset` 日志里出现不可恢复的 CPU 重置 | 变体 `lidt` 空 IDT + `ud2` → `{"reason":"triple-fault"}`，且引擎打印 `guest reset without a recovery path (triple fault) observed in the QEMU log` |

关键点：**`exitCode` 单独不足以区分**（`guest-shutdown` 与 `triple-fault` 两种情况下 QEMU 都是 exit 0），所以引擎同时检查「是不是我们杀的」和「QEMU 日志里有没有 triple fault」。参考夹具 `exitCode=0 + reason=timeout` 看起来反直觉，但它是准确的：**0 是 QEMU 收到我们的 SIGTERM 后优雅退出的码，不是 guest 关机的码**。

**Q3：`classify_exit()` 的语义够不够用？要不要扩展 core？**

**本 Agent 的判断：够用，不建议在本阶段扩展 `reason`。** 理由：

1. 契约 §2 把 `reason` 定成封闭四值（`guest-shutdown|triple-fault|timeout|killed`），它描述的是**机器停止的原因**；而「guest 出了什么故障」已经由 `run.fault` 事件独立表达（`vector=#UD`、`rip`、`symbolicated`），两者正交。
2. 参考内核这种「异常 → 停机死循环」的形态，从 QEMU 视角就是**永久运行**，engine 侧能观测到的事实只有「期限到了 + 我把它杀了」；把它叫 `guest-panic`/`halted` 会是**engine 对 guest 内部状态的猜测**，违反 §6 规则 4「reason 由引擎归因、UI 不得猜测」的精神。
3. 若产品上确实想让 UI 一眼看到「这是异常停机」，**不需要改 core**：P2-run/P3 可以把既有事实组合起来（同一 op 内先出现 `run.fault` 再出现 `run.exited{reason:timeout}` ⇒ 渲染为「异常停机后挂起，已超时终止」），现有的 `ide` 日志行也已经写明 `run exceeded its 15000 ms deadline; killing process group …`。
4. 如仍要改，本 Agent 建议的**最小破坏**方案（供裁决，**本 Agent 未实施**）：不动封闭枚举，只给 `RunExitedPayload` **新增可选字段**（契约 §7 允许新增可选字段），例如 `terminatedBy: "engine"|"guest"` 或 `haltedAfterFault: true`。需要主 Agent 裁决后由 core owner 统一改动。

### 5.3 契约增补（本 Agent 已实施，请裁决是否写回 `docs/spec/10-contracts.md`）

| 项 | 契约原文 | 本 Agent 的做法 | 理由 |
| --- | --- | --- | --- |
| `build.diagnostic.source` | `clang\|ld\|nasm` | **新增 `gcc`** | 参考夹具 `Makefile` 用 GNU `gcc`/`ld` 构建。把 gcc 的报错标成 `clang` 就是在事件流里说谎（§0 规则 4）。其余三值语义未变。 |
| `run.fault` | `vector`、`rip`、`errorCode`、`regs`、`symbolicated?` | 新增 `ripText`（guest 原始 16 位 hex 文本） | 保证 guest 原文可追溯；`rip` 本体保持数值（便于与 GDB/`addr2line` 比对）。属新增可选语义，前端可忽略。 |
| `symbols.indexed.buildId` | `buildId` | 允许 `null` | 夹具链接参数是 `--build-id=none`，没有就是没有，不编造（实测输出 `"buildId": null`）。 |
| `[run] kernel` | 示例中是必填路径 | 类型为 `Option<PathBuf>`，缺省 =「构建产出的 ELF」 | 契约 §4 允许「缺失字段用引擎默认值」；同时让 `princess.toml` 不必写死产物名。 |
| `[build] artifacts` | 声明式列表 | 允许为空 = 「由引擎发现构建产物」 | 参考夹具故意没有 `princess.toml`（属 P0 夹具，本 Agent 无权新增文件），发现规则是：声明路径 ∪ `build/`、`out/`、`dist/` 下 3 层内的 `*.elf/*.iso/*.img/*.bin`（排序去重、逐个算 size+sha256）。P2-2 的 `artifacts[]` 就是这样来的，**没有硬编码夹具文件名**。 |
| `run.exited.exitCode` | `exitCode` | 允许 `null` | 子进程死于信号时没有退出码；填 0 才是编造。 |
| 事件 `*.finished` 位置 | 「必须收尾/必达」 | 实现为**操作的最后一个事件** | 便于 P3 重放状态机把 `*.finished`/`run.exited` 当作 end-of-op 标记（`build.finished` 与 `run.exited` 之后不再有同类操作事件）。 |

### 5.4 其它设计决定（可能你不认同，列出）

1. **`--project` 指向没有 `princess.toml` 的目录时不再报错**，而是合成「引擎默认配置」并**在每个事件流里显式声明**：
   `config: no princess.toml in fixtures/refkernel; using Makefile-based engine defaults with target 'iso'`。
   这是为了让 `--project fixtures/refkernel`（P2-2/P2-3 的验收命令）在**不改动 P0 夹具**的前提下可跑。若你更希望「无 manifest 即 `E_INVALID_CONFIG`」，只需把 `ProjectConfig::load_or_default` 的回退分支关掉，并在 `fixtures/refkernel/princess.toml` 放一份显式配置——**一行代码 + 一个文件**的事，但那需要授权我写 `fixtures/refkernel/`。
2. **退出码语义**：`build`/`run` 只要**产生了完整事件流**就退出 0，失败状态在事件里（`status=failed` / `reason=timeout`）。加了 `--strict` 让脚本能在失败时拿到非 0。理由：验收表要断言的字段是事件字段，把「操作完成」与「操作成功」混在一个数字里会让 CI 判断歧义。P2-6 已展示两种用法（无 `--strict` → 0；`--strict` → 1）。
3. **QEMU 归引擎所有的参数会被从 `[run] args` 里剔除**（`-serial/-monitor/-display/-nographic/-d/-D/-cdrom/-boot/-kernel/-no-reboot`），并推一条 `ide` 事件说明剔除了什么：
   `[run] args contains emulator flags the engine owns and ignores: -display none -no-reboot`。理由是契约示例里就有 `-serial stdio -display none`，而串口必须由引擎独占（否则 §6 规则 2 的「必须有读者」无从保证）。这是「可见的忽略」，不是静默覆盖。
4. **`-d int,cpu_reset,guest_errors -D <build>/qemu-debug.log` 恒定开启**，用于 triple-fault 判定（guest 自己没法报告三击故障），跑完即删（`--keep-qemu-log` 可保留）。代价是每次运行多写一个小日志文件。
5. **进程组终止先 SIGTERM 后 SIGKILL**（各 0.5 s 宽限），既保证干净退出/刷日志，也保证不残留；`group_alive()` 用 `/proc` 精确判断「非 Z 状态」的组成员，而不是 `kill(-pgid,0)`（后者会把「已死未回收的僵尸」当成存活 —— 这是本 Agent 实测踩到并修掉的真 bug）。
6. **符号化是 `addr2line`/`nm`/`readelf` 的临时实现**（`crates/princess-cli/src/symbolize.rs`），真正的 ELF/DWARF 引擎是 P2-B 的 `princess-symbol`。之所以先自己来一遍：否则 P2-4 无法验收。该模块只报工具真的给出的结果，映射不到就**不填 `symbolicated`**。
7. **串口读到的 `\r\n` 原样推送**（不做 CRLF→LF 归一），因为契约 §2 要求「前端只做追加渲染，不做有损变换」。
8. **`doctor` 输出成 `log.append(ide)` 事件**，而不是新增一个 `tools.detected` 事件 —— 契约的事件 kind 是封闭集合，本 Agent 不擅自新增 kind；结构化的工具表由 P3 的 `princess:tools:detect` IPC 返回（`ToolchainReport` 类型已在 core 中定义并实现）。

---

## 6. 过程中修掉的真问题（留痕，避免后续误判）

1. **相对工作区根**：`--project .`（CLI 默认）会让 `Toolchain::discover` 把工作区根解析成 `"."`，于是 `build.started.argv` 和 doctor 表里出现 `./.toolchain/cargo/bin/cargo` 这类**相对路径**（从 `/tmp` 调用就会失效）。修为「先绝对化再向上找 `scripts/env.sh`」，并加了回归测试 + 从 `/tmp` 实测（§4.9）。
2. **僵尸被当成存活进程**：`group_alive()` 原本用 `kill(-pgid, 0)`，被杀的 qemu 在 `wait()` 回收前是僵尸，于是超时路径误报 `process group … still has survivors after SIGKILL`。改为扫 `/proc/<pid>/stat` 判断「pgrp 相同且状态非 Z」。
3. **`UTF-8 chunker` 吞字符**：非法字节后的合法序列会被一起丢成 U+FFFD；改为只消费 `valid_up_to + error_len` 的字节，其余留在缓冲。
4. **`*.finished` 之后还有事件**：人类摘要原本排在 `build.finished`/`run.exited` 之后，违背「finished 收尾」；已改为摘要在前、终态事件最后。

---

## 7. 给下一批（`princess-build` / `princess-run` / `princess-symbol`）的接口提示

1. **事件只经 `EventSink` 推**：签名为 `&mut dyn EventSink`，`EventRecorder` 已 `impl EventSink`；不要绕过它直接写 stdout/file，否则 `--record`、`seq` 分配和 `opId` 一致都会漏。
2. **拿 `ResolvedProject` 而不是原始 `ProjectConfig`**：路径已绝对化、`~`/`$ENV` 已展开；`BootMedium` 由 `Project::boot_medium()` 决定（x86_64+multiboot2 只能 GRUB ISO 的 `-cdrom -boot d`，`-kernel` 只吃 32 位 ELF，实测见 P0 §4.2）。
3. **退出归因只用 `classify_exit(&ExitFacts)`**：优先级 `cancelled > timed_out > triple_fault > guest_powered_off > exit code`；`ExitFacts.triple_fault` 需要在 QEMU `-d int,cpu_reset -D <log>` 输出里找 `Triple fault`（guest 自己无法报告）。
4. **子进程一律 `process_group(0)` + 组杀**：`crates/princess-cli/src/proc.rs` 里的 `kill_group_and_wait` / `group_alive` 可直接搬；注意「僵尸不算存活」这个坑。
5. **`Utf8Chunker` 是串口/编译输出的正确读法**：`take_lines()` 保证不切断码点且整行成事件；`take_available()` 用于 idle/EOF；非法字节自动标 `utf8-lossy`。串口必须**先落盘再推事件**（`fixtures/refkernel/build/serial.log` 就是这么写的）。
7. **`scripts/env.sh` 的工具目录列表在 CLI 里被复制了一份**（`crates/princess-cli/src/env.rs` 的 `Toolchain::discover`：`.toolchain/cargo/bin`、`prefix/usr/lib/llvm-14/bin`、`prefix/usr/bin`、`prefix/bin`、`prefix/usr/sbin`），目的是让 CLI 在**没有 source env.sh 的干净 shell / GUI 子进程**里也能跑。本 Agent 工作期间 A1/A9 往 `env.sh` 里加了 LLVM 16、`bear`、`princess-gdb` 等条目（`scripts/` 由他们所有，本 Agent 未改）。**如果 `env.sh` 再引入新的工具目录（例如 `llvm-16/bin`），需要同步这份列表**，否则 CLI 仍会解析到 LLVM 14（当前实测解析正常：`clang 14.0.6` / `clangd 14.0.6`，与 P0 `doctor.sh` 一致）。
8. **`princess-cli` 的实现是「参考实现」，不是抽象层**：`build.rs`/`run.rs` 里的解析与编排逻辑在被 `princess-build`/`princess-run` 取代后，应当由 CLI 改为调用它们的 trait（`Box<dyn BuildBackend>` 等，object-safe 已被单测钉住），而不是两套逻辑长期并存。尤其 `serial.rs` / `diagnostics.rs` / `symbolize.rs` 里那些**夹具相关**的解析，长期归属应是 `princess-run` / `princess-build` / `princess-symbol`。

---

## 8. 已知缺口与诚实声明

1. **`cargo test --workspace` 不绿**（§5.1）：那是 `princess-bin`（编译失败）与 `princess-build`（4 失败）的在飞状态，不是本 Agent 的 crate。本 Agent 的 85 个测试全绿。
2. **`fixtures/refkernel/` 内没有 `princess.toml`**（本 Agent 无写权限），因此走的是「引擎默认配置」路径；如果主 Agent 决定改成显式配置，需要授权我创建该文件（或由 P0 owner 创建）。
3. **负面实验都在 `.scratch/core/`**（`badsyntax` / `deadloop` / `shutdown` / `triplefault` / `refkernel-clean`），`.scratch/` 被 `.gitignore` 忽略，**不会入库**；报告里保留了当时的命令与关键输出，但**复现需要在工作区重建这些副本**（步骤见 §9.4）。
4. **`guest-shutdown` / `triple-fault` 两个变体是「我改的副本」，不是 P0 夹具**：它们只用来证明 `classify_exit` 能区分四种 reason，P2 验收的夹具断言仍全部基于 `fixtures/refkernel/` 原样源码。
5. **`symbols.indexed.buildId` 恒为 `null`**：夹具链接参数 `--build-id=none`。这不是实现缺陷，是事实（`readelf -n` 实测无 Build ID 段）。
6. **未做（超出 P2-A 范围）**：IPC 命令注册（`princess:*`，属 P3/Tauri 壳）、DAP 调试后端（P4）、ELF/反汇编 provider 的真实实现（P2-B/P5）、AI provider 实现（P7）、事件重放缓冲（P3）。core 里只提供 trait 定义与类型，**没有**动态加载/插件（契约 §5 明确 v1 不做）。
7. **`princess-cli` 的 `build`/`run` 逻辑是自带的编排实现**（不依赖 P2-B 的 crate），因为 P2-A 交付时它们尚不存在；这与 §7 第 6 条一致，属于**有意为之的过渡形态**。
8. **QEMU 仍跑在 TCG**（无 KVM），参考内核一次运行（含构建）约 15-20 秒；P2 全套验收在本机实测总时长约 2 分钟。

---

## 9. 复现步骤（照抄可重跑）

### 9.1 前置

```bash
cd /root/PrincessIDE
source scripts/env.sh          # 每个新 shell 都要 source（PATH/LD_LIBRARY_PATH/RUSTUP_HOME/CARGO_HOME）
free -m                        # 本机内存紧张：可用 < 1.2G 时不要起重型构建；重型构建用 CARGO_BUILD_JOBS=1
```

### 9.2 正向

```bash
cargo test -p princess-core -p princess-cli                                # P2-1（85/85）
cargo run -p princess-cli -- doctor                                        # 工具链表
cargo run -p princess-cli -- build --project fixtures/refkernel            # P2-2
cargo run -p princess-cli -- run   --project fixtures/refkernel \
    --record fixtures/events/refkernel-run.ndjson                          # P2-3/P2-4/P2-5 + 重录夹具
cargo run -p princess-cli -- events --file fixtures/events/refkernel-run.ndjson   # P2-8（CLI 侧）
cargo test -p princess-core --test event_fixture                           # P2-8（core 侧）
cargo run -p princess-cli -- symbolicate --project fixtures/refkernel      # P2-4 的独立复核
```

### 9.3 负样本（P2-6 / P2-7）

```bash
# P2-6：参考内核副本 + 语法错误（不要动 fixtures/refkernel/）
rm -rf .scratch/core/badsyntax && cp -r fixtures/refkernel .scratch/core/badsyntax
rm -rf .scratch/core/badsyntax/build
python3 - <<'PY'
p='/root/PrincessIDE/.scratch/core/badsyntax/kernel.c'
lines=open(p).read().split('\n')
i=next(n for n,l in enumerate(lines) if 'serial_puts("PrincessIDE reference kernel booted' in l)
lines.insert(i+1, '    int this_line_has_a_syntax_error = ;   /* P2-6 injected */')
open(p,'w').write('\n'.join(lines))
PY
cargo run -q -p princess-cli -- build --project .scratch/core/badsyntax --strict   # 期望 EXIT=1

# P2-7：把故障探针换成死循环
rm -rf .scratch/core/deadloop && cp -r fixtures/refkernel .scratch/core/deadloop
rm -rf .scratch/core/deadloop/build
python3 - <<'PY'
p='/root/PrincessIDE/.scratch/core/deadloop/kernel.c'
s=open(p).read().replace('    refkernel_fault_probe();',
                         '    for (;;) { __asm__ volatile("cli; hlt"); }', 1)
open(p,'w').write(s)
PY
cargo run -q -p princess-cli -- run --project .scratch/core/deadloop --timeout-ms 4000   # reason=timeout
pgrep -af qemu-system-x86_64 | grep -v "pgrep\|grep\|bash -c" || echo "(no orphan)"
```

### 9.4 四种退出原因

```bash
# killer
cargo run -q -p princess-cli -- run --project .scratch/core/deadloop \
    --timeout-ms 20000 --cancel-after-ms 2500                     # reason=killed

# guest-shutdown：在副本的 kernel_main 里把 refkernel_fault_probe(); 换成
#   __asm__ volatile("outw %0, %1" :: "a"((unsigned short)0x2000), "d"((unsigned short)0x604));
#   for (;;) { __asm__ volatile("cli; hlt"); }
cargo run -q -p princess-cli -- run --project .scratch/core/shutdown --timeout-ms 8000

# triple-fault：在副本里先 lidt 一个空 IDT，再 ud2
cargo run -q -p princess-cli -- run --project .scratch/core/triplefault --timeout-ms 8000
```

---

## 10. 结论

- **P2-A 交付完成**：`princess-core` 契约层冻结、`princess-cli` 无头载具可用、事件夹具入库。
- **P2-1..P2-8 逐项实测通过**（P2-1 的 `--workspace` 全绿取决于其它 Agent 的 crate，见 §5.1；本 Agent 的 crate 85/85 全绿）。
- **`run.exited.reason` 四种取值全部有真实复现证据**，`timeout` 的归因有意为之且可判定（§5.2）。
- **需要主 Agent 裁决的 3 件事**：① §5.2 Q3 是否扩展 `RunExitedPayload`（本 Agent 判断**不需要**）；② §5.3 的契约增补是否写回 `docs/spec/10-contracts.md`；③ 是否授权我（或 P0 owner）给 `fixtures/refkernel/` 补一份显式 `princess.toml`。
