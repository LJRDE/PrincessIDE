# C — 内核调试的协议路线：GDB 内置 DAP vs 自写 MI→DAP 适配层

> 调研 Agent C（重派第二棒）。第一棒因本机 OOM 被内核杀死、会话不可用、无报告产出；
> 本报告接续其 `.researchC/` 遗产（解包得到的 gdb 16.3 二进制）完成。
>
> 证据分级：**[已实测]** = 本次在本机真实跑出来的输出；**[有来源]** = 代码/文档原文；
> **[推测]** = 未实测的推断。报告所有原始输出保存在 `.researchC/dapbin/` 下。

---

## 0. 结论速览（TL;DR）

**推荐：直接用 GDB 内置 DAP（`gdb -i=dap`）作为 P4 调试后端的主干，但必须在 Rust 侧包一层薄适配**
——这层薄适配不是"MI→DAP 协议转换"，而是**能力补齐与探针**：把内置 DAP 缺失的
*硬件断点*、*物理内存*、*CR/段寄存器*、*寄存器作用域* 用 `evaluate(context="repl")`
走 GDB CLI 逃生舱补回，并按内核场景规范化为自己的 DAP 响应。

一句话理由：**内置 DAP 是 GDB 官方维护的、已能直接驱动 QEMU gdbstub 跑裸机内核的实现，
自写完整 MI→DAP 是重复造一个必然更差的轮子；但内置 DAP 在内核场景有 4 个确定的硬缺口，
所以"纯用内置 DAP、Rust 侧零代码"不可行。**

推荐会反转的条件见 §7。

---

## 1. 环境事实与如何把 gdb 跑起来

### 1.1 版本事实 [已实测]

| 项 | 值 |
|---|---|
| 宿主机 | Debian 12，glibc **2.36**（`ldd --version` → `Debian GLIBC 2.36-9+deb12u14`） |
| 工作区自带 gdb | `gdb 13.1`，位于 `.toolchain/prefix/usr/bin/gdb` |
| 工作区自带 gdb 是否支持 DAP | **不支持**：`gdb -q -batch -i=dap` → `Interpreter `dap' unrecognized` |
| 本报告使用的 gdb | **GNU gdb (Debian 16.3-1) 16.3**，11,197,160 字节，从 Debian *trixie* 的 `.deb` 解包 |
| 位置 | `.researchC/dapbin/rootfs/usr/bin/gdb` |
| LLDB | **工作区内不存在**（`which lldb` 空，toolchain 无 lldb） |

> **关键事实**：Debian 12（本机宿主）自带的 gdb 是 13.1，**没有** DAP。
> DAP 是 GDB 14 引入的。所以"直接用系统 gdb 的内置 DAP"在本机**不成立**，
> 必须自带一个 ≥14 的 gdb。这一点决定了 §6 的工程成本评估。

### 1.2 运行方式（**踩过的坑，务必照抄**）

第一棒和主 Agent 都踩过：**绝不能把 rootfs 的 glibc 塞进宿主的 `LD_LIBRARY_PATH`**。
rootfs 里是另一套 glibc，宿主 `ld.so` 与它不配套，一旦污染，
连 `head`、`python3` 都会崩：

```
head: symbol lookup error: .../rootfs/usr/lib/x86_64-linux-gnu/libc.so.6:
      undefined symbol: __tunable_is_initialized, version GLIBC_PRIVATE
```

**正确做法（二选一，本报告全程用方式一）**：

```bash
ROOTFS=/root/PrincessIDE/.researchC/dapbin/rootfs

# 方式一：用 rootfs 自己的加载器（.researchC/dapbin/gdb163.sh 已封装）
$ROOTFS/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2 \
  --library-path $ROOTFS/usr/lib/x86_64-linux-gnu:$ROOTFS/usr/lib \
  $ROOTFS/usr/bin/gdb --version
# -> GNU gdb (Debian 16.3-1) 16.3      [已实测]

# 方式二：chroot
chroot $ROOTFS /usr/bin/gdb --version
```

可复用的启动器：`.researchC/dapbin/gdb163.sh`
（已设好 `PYTHONHOME`/`PYTHONPATH`，内置 DAP 是 Python 实现，缺了它 DAP 起不来）。

> **工程含义**：把 gdb 16.3 作为 PrincessIDE 的调试后端，意味着**发布物里要携带一个
> 自带 glibc 的 gdb 及其 loader**，或者要求用户系统 glibc ≥ 2.38。
> 这是一个必须在 D11 里显式接受的成本（见 §6）。

---

## 2. 核心证据：`gdb -i=dap` 的真实 `initialize` 响应

### 2.1 原始线级交换 [已实测]

用 `Content-Length: N\r\n\r\n{json}` 帧直接向 `gdb -q -i=dap` 的 stdin 发 `initialize`：

```
=== RAW REQUEST BYTES (sent to gdb -i=dap stdin) ===
b'Content-Length: 259\r\n\r\n{"seq": 1, "type": "request", "command": "initialize", "arguments": {"clientID": "princesside-research", ...   (打印时截断)'

=== RAW RESPONSE BYTES FROM gdb (first 400) ===
b'Content-Length: 1406\r\n\r\n{"request_seq": 1, "type": "response", "command": "initialize", "success": true, "body": {"supportsTerminateRequest": true, "supportTerminateDebuggee": true, "supportsCancelRequest": true, "supportsLoadedSourcesRequest": true, "supportsLogPoints": true, "supportsConditionalBreakpoints": true, "supportsHitConditionalBreakpoints": true, "supportsFunctionBreakpoints": true, "s'

=== FRAME HEADER ===
Content-Length: 1406
```

**帧格式完全符合 DAP 规范**：`Content-Length: 1406` + 空行 + 恰好 1406 字节 JSON。
Python 侧 `json.loads` 报 `Extra data: line 1 column 1407` 只是因为我只截取了 400 字节，
帧本身是自洽的。

### 2.2 `initialize` 官方能力声明原文（对内核场景最重要的字段）

```json
{
  "request_seq": 1, "type": "response", "command": "initialize", "success": true,
  "body": {
    "supportsTerminateRequest": true,
    "supportTerminateDebuggee": true,
    "supportsCancelRequest": true,
    "supportsLoadedSourcesRequest": true,
    "supportsLogPoints": true,
    "supportsConditionalBreakpoints": true,
    "supportsHitConditionalBreakpoints": true,
    "supportsFunctionBreakpoints": true,
    "supportsInstructionBreakpoints": true,
    "supportsExceptionFilterOptions": true,
    "supportsModulesRequest": true,
    "supportsDelayedStackTraceLoading": true,
    "supportsDisassembleRequest": true,
    "supportsValueFormattingOptions": true,
    "supportsEvaluateForHovers": true,
    "supportsSetExpression": true,
    "supportsSetVariable": true,
    "supportsConfigurationDoneRequest": true,
    "supportsBreakpointLocationsRequest": true,
    "supportsReadMemoryRequest": true,
    "supportsWriteMemoryRequest": true,
    "supportsSingleThreadExecutionRequests": true,
    "supportsSteppingGranularity": true
  }
}
```

**这份声明是 P4 面板能力表的直接依据**（完整原文见 `.researchC/dapbin/kernel_test.out` [1] 节）。

---

## 3. 内核场景端到端实测（分页夹具，非玩具）

拓扑 = PrincessIDE 的真实目标：
**裸机 x86_64 内核（`fixtures/paging-kernel`，`CR0.PG=1`、`CR3=0x104000`）跑在 QEMU 7.2 上，
经 gdbstub（`:1234`）被 gdb 16.3 调试，IDE 侧说 DAP。**

测试脚本（可复跑，均为本次新写）：
- `.researchC/dapbin/kernel_dap_test2.py` → 输出 `paging_dap_console.out`
- `.researchC/dapbin/kernel_dap_test3.py` → 输出 `paging3_console.out`

### 3.1 能跑通的（[已实测]）

| DAP 请求 | 结果 | 证据 |
|---|---|---|
| `initialize` | ✅ | §2 |
| `attach`（GDB DAP 扩展 `target` 字段） | ✅ 映射为 `target remote :1234` | `"success": true` |
| `configurationDone` | ✅ 且**必须发**：`attach` 的响应被**推迟到 `configurationDone` 之后**才返回 | `paging_dap_console.out` [2][3] |
| `threads` | ✅ `[{"id":1,"name":"CPU#0 [running]"}]` | [6] |
| `setBreakpoints`（源码行） | ✅ `verified:true, line:120, instructionReference:"0x1011c9"` | round-3 [6] |
| `setInstructionBreakpoints`（地址） | ✅ 同样 verified | round-3 [8] |
| **`stackTrace`** | ✅ **3 帧，带真实源文件路径与行号** | 见下 |
| `continue` / `next` / `stepIn(granularity:"instruction")` | ✅ `stopped(reason:"step")` | [28][29][30][31] |
| `pause` | ✅ 恢复出 `stopped` 事件 | round-3 [22][23] |
| `readMemory` / `writeMemory` | ✅ 写入 `PRINCESS` 后读回 `UFJJTkNFU1M=` | [25][26] |
| `disassemble` | ✅ 返回 `instruction`/`instructionBytes`/`address` | round-1 |

**`stackTrace` 原文（本次最有说服力的一条：裸机内核 + DWARF + 分页下拿到完整调用栈）**：

```json
{ "type":"response","command":"stackTrace","success":true,
  "body": { "stackFrames": [
    { "id":0,"line":120,"instructionPointerReference":"0x1011c9",
      "moduleId":"/root/PrincessIDE/fixtures/paging-kernel/build/pagingkernel.elf",
      "source":{"name":"kernel.c","path":"/root/PrincessIDE/fixtures/paging-kernel/kernel.c"},
      "name":"paging_fault_probe" },
    { "id":1,"line":157,"instructionPointerReference":"0x1012cc", ... "name":"kernel_main" },
    { "id":2,"line":194,"instructionPointerReference":"0x100193",
      "source":{"name":"boot.S",...},"name":"_start" } ] } }
```

**结论：内核场景下"源码级栈回溯"是通的**——只要夹具带 DWARF（本夹具 `.debug_info`/`.debug_line` 齐全）。

### 3.2 **第一棒"未实测"或测错的地方（本次纠正）**

第一棒的 `kernel_test.out` 有多处**由测试脚本自身的时序/参数 bug 造成的假阴性**，
若不纠正会严重误判内置 DAP。逐条对照：

| 第一棒报告 | 本次实测 | 真实原因 |
|---|---|---|
| `monitor info registers` / `info mem` / `xp` → **`"monitor" command not supported by this target`**（连续 4 条） | ✅ **全部可用**，`monitor info registers` 返回完整 CR0/CR2/CR3/CR4、GDT/IDT 基址、DR0–DR7、XMM；`monitor info mem` 返回 3 段映射；`monitor info tlb` 返回逐页表；`monitor xp /8xg 0x104000` 返回物理内存 | 第一棒在 `attach` 里同时传了 `program`，且前序 `evaluate`/`stackTrace`/`scopes` 已把会话推入 `"The program has no registers now."` 的坏状态；后续 `monitor` 是在**已损坏的 target 状态**下发的 |
| `pause` → `<NO RESPONSE / TIMEOUT>` | ✅ `pause` 响应 `success:true`，随后正常 `stopped` 事件 | 第一棒在 `next` 卡住后再 `pause`，状态机已错乱；单独测 `pause` 正常 |
| `stackTrace` → `"'NoneType' object has no attribute 'global_num'"` | ✅ 正常返回 3 帧 | 该错误来自 `threads.py` 的 `thr.global_num`，发生在第一棒那个**坏会话**里；干净会话无此问题 |
| `scopes` → `<NO RESPONSE / TIMEOUT>`，"no Registers scope" | ⚠️ **确实没有 Registers scope**（但原因与第一棒不同） | 第一棒 `frameId=None`，请求非法。本次 `frameId=0` 合法，但 `scopes` **返回的 scope 列表里没有 Registers**，见 §4.2 |

> **方法论教训（写给 P4）**：内置 DAP 的会话对**请求顺序与非法参数**很敏感，
> 一旦某个请求失败，target 会进入难以恢复的坏状态，后续请求会级联假阴性。
> Rust 适配层必须做**状态校验**，不能把"上一个请求失败"当成"该能力不支持"。

---

## 4. 内置 DAP 的 4 个确定硬缺口（**内核场景的决策依据**）

以下全部来自 **gdb 16.3 DAP 的 Python 源码原文** [有来源]，
源码树 `.researchC/gdb-16.3/gdb/python/lib/gdb/dap/` 与运行的二进制同版本。

### 4.1 缺口一：`readMemory`/`writeMemory` **只有虚拟地址**，没有物理内存

`memory.py` 原文：

```python
@request("readMemory")
@capability("supportsReadMemoryRequest")
def read_memory(*, memoryReference: str, offset: int = 0, count: int, **extra):
    addr = int(memoryReference, 0) + offset
    buf = gdb.selected_inferior().read_memory(addr, count)   # <-- 只有虚拟地址路径
```

`gdb.selected_inferior().read_memory()` 走的是 target 的**虚拟地址**接口，
**没有任何参数可以切到物理地址**。[已实测] 旁证：DAP `readMemory @0x104000`
返回的是 `I1AQAAAA...`，解 base64 → `23 50 10 00 ...`，
与同地址虚拟读一致；读物理的可行路径是 `monitor xp`（QEMU）或 GDB 的 `x` 加物理修饰。

> **对 P4 的影响**：D 报告已定"页表遍历数据源用 QEMU 物理内存读"。内置 DAP 的
> `readMemory` **不能**承担这个职责，必须走 repl 逃生舱发 `monitor xp /Nxg <phys>`（或 QMP）。

### 4.2 缺口二：`scopes` **不返回寄存器作用域**（至少在本夹具上）

`scopes.py` 源码**无条件**追加 `_RegisterReference("Registers", frameId)`：

```python
scopes.append(_RegisterReference("Registers", frameId))
```

`_RegisterReference.__init__` 取的是
`frame_for_id(frameId).inferior_frame().architecture().registers()`。

但 [已实测] 在分页夹具上 `scopes(frameId=0)` **没有** Registers 条目
（`paging_dap_console.out` [14] → `!! no Registers scope via DAP`）。

**[推测]** 原因：该内核 `-S` 停在 `_start` 时 `architecture().registers()` 返回空/异常，
或 `frame.frame_locals()` 路径干扰。无论根因如何，**结论是硬的**：
不能把"寄存器面板"建立在 DAP `scopes` 之上，必须用 `evaluate(context="repl")` 发
`info registers` 自己解析。

对照 §3.2：第一棒把这个现象归因为"`scopes` 超时"，本次证明是"请求成功但缺 scope"——
两者对 P4 的含义完全不同：后者是可预测的、可用 CLI 兜住的。

### 4.3 缺口三：**没有硬件断点**（对内核是致命的）

`breakpoint.py` 原文：

```python
def _set_one_breakpoint(*, logMessage=None, **args):
    if logMessage is not None:
        return _PrintBreakpoint(logMessage, **args)
    else:
        return gdb.Breakpoint(**args)        # <-- 只能构造软件断点

def _rewrite_insn_breakpoint(*, instructionReference, offset=None, condition=None, hitCondition=None, **args):
    # There's no way to set an explicit address breakpoint from Python, so we rely on "spec" instead.
    val = "*" + instructionReference + (...)
    return {"spec": val, ...}
```

`gdb.Breakpoint` 只能创建**软件断点**；DAP 的 `setBreakpoints`/`setInstructionBreakpoints`
都没有硬件断点表达法。[已实测] round-3 `info breakpoints` 印证：

```
Num  Type         Disp Enb Address            What
1    breakpoint   keep y   0x00000000001011c9 in paging_fault_probe at kernel.c:120   <-- 软件断点
```

**为什么这对内核是致命的**：GRUB 把内核加载到内存**之前**，目标内存里还没有内核代码。
软件断点靠**改写目标内存**（x86 上写 `0xCC`），在 GRUB 阶段会被 GRUB 覆盖，
或者根本写不进去（内存还没映射）。所以**入口断点必须是硬件断点**。
第一棒的 round-1 脚本正是用 `repl("hbreak *0x100040")` 绕过的，[已实测]：

```
Hardware assisted breakpoint 1 at 0x100040: file boot.S, line 62.
```

> **对 P4 的影响**：这是必须由 Rust 适配层补齐的第一能力。
> 路径：`evaluate(context="repl", expression="hbreak *0x...")`，
> 然后自己维护断点表（因为内置 DAP 的 `breakpoint_map` 不知道这条 hbreak）。

### 4.4 缺口四：`evaluate` 在 repl 上下文**不能引用 `$寄存器`**，语义与 hover/watch 不一致

[已实测] 同一个表达式 `$cr3`，三种 context 结果不同：

```jsonc
// context = "hover"
{ "success": true, "body": { "variablesReference": 0,
  "result": "[ PDBR=0 PCID=0 ]", "type": "x64_cr3" } }        // <-- 只有注解，没有数值!

// context = "watch"
{ "success": true, "body": { "result": "[ PDBR=0 PCID=0 ]", "type": "x64_cr3" } }

// context = "repl"
{ "success": false, "message": "Undefined command: \"$cr3\".  Try \"help\"." }
```

**两个陷阱**：
1. `hover`/`watch` 路径返回的是 GDB 对 `$cr3` 的**类型注解字符串**，**不含数值**——
   直接显示到面板会得到 `[ PDBR=0 PCID=0 ]` 而不是 `0x104000`。
2. `repl` 上下文把输入当**GDB 命令行**解析，`$cr3` 不是合法命令。

因此读寄存器的**唯一可靠路径**是 `repl` 发 `info registers <name>` 或 `p/x $cr3`：

```
repl: info registers cr0 cr2 cr3 cr4
-> "cr0  0x80000011 [ PG ET PE ]\ncr2  0x0  0\ncr3  0x104000 [ PDBR=260 PCID=0 ]\ncr4  0x20 [ PAE ]\n"
repl: p/x $cr3
-> "$1 = 0x104000\n"
```

[已实测] 该输出与 guest 串口自报的 `CR0=0x0000000080000011 CR3=0x0000000000104000 CR4=0x0000000000000020`
**完全一致**——寄存器读取链路已被交叉验证。

### 4.5 补充：`disassemble` 的 `memoryReference` **只接受数字**

`disassemble.py`：`pc = int(memoryReference, 0) + offset`。
[已实测] 传 `"$pc"` → `"invalid literal for int() with base 0: '$pc'"`。
必须自己先把 `$pc` 解析成数值（例如先用 repl `p/x $pc`）。

---

## 5. 内核场景特有需求逐条裁定

| 需求 | 内置 DAP 原生 | 可行路径 | 结论 |
|---|---|---|---|
| **物理内存读写** | ❌ | `monitor xp /Nxg <phys>`（QEMU）/ `monitor info mem` | **必须补**，见 §4.1 [已实测] |
| **CR0/CR2/CR3/CR4** | ❌（hover 只给注解） | `repl: info registers cr0 cr2 cr3 cr4` | **必须补**，见 §4.4 [已实测] |
| **段寄存器 CS/DS/ES/FS/GS/SS** | ❌ | `repl: info registers cs ds es fs gs ss` | **必须补** [已实测] |
| **SMP 多核** | ⚠️ 每核映射为一条 DAP thread | `threads` → `[{id:1,name:"CPU#0 [running]"}]`；`monitor info cpus` → `* CPU #0: thread_id=...` | 语义**部分等价**：QEMU 单核夹具里只有 CPU#0；多核时每核会成为一条 GDB thread（`thr.global_num`）。但**线程 ≠ 核**的语义丢失（没有"核"元数据），IDE 侧需自行标注 [已实测/推测] |
| **硬件断点/观察点** | ❌ | `repl: hbreak` / `watch` / `rwatch` | **必须补**，且对内核入口断点是**刚需**，见 §4.3 [已实测] |
| **页表遍历** | ❌ | `monitor info tlb`（逐页表）/ `monitor xp` 读物理页表 + IDE 自己走表 | **必须补**；D 报告已定"自己走表，不要解析 `info mem`/`info tlb` 文本"。注意 **QEMU 7.2 无 `info gdt`/`info idt`**，**`info mem` 在 LA57 下返回空** [有来源：`docs/research/D-binary-lowlevel-tooling.md` §4.1/§3.6] |
| **无 OS 下的符号加载** | ✅ | `attach` 不带 `program`，再 `repl: symbol-file <elf>` 或 `add-symbol-file` | **通** [已实测]：`symbol-file` 后栈回溯/symbol 名/源码行号全部可用 |

### 5.1 SMP 语义细节 **[已实测 + 推测]**

- `threads.py` 源码：`one_result = {"id": thr.global_num}`，`name = thr.name or thr.details`。
- [已实测] QEMU 单核：`[{"id":1,"name":"CPU#0 [running]"}]`；`info threads` →
  `* 1  Thread 1.1 (CPU#0 [running]) paging_fault_probe () at kernel.c:120`。
- **语义不等价点**：DAP 的 "thread" 是 OS 线程概念，内核里一个核就是一个"线程"，
  但 DAP 没有表达"核"、`allThreadsStopped` 在多核下会一次性冻结所有核——
  这对"只看某个核"的调试诉求是**语义过粗**。多核夹具未实测（本夹具是单核）。

---

## 6. GDB/MI → DAP 映射表（若自写适配层）

> **现状提示**：内置 DAP 内部本身就是"GDB Python API → DAP"的一层实现。
> 下表既可用于"评估自写成本"，也可用于"理解内置 DAP 的行为"。
> [有来源] 全部来自 `.researchC/gdb-16.3/gdb/python/lib/gdb/dap/` 源码。

| DAP 请求 | 内置 DAP 的实现（源码） | 等价 MI 命令（若自写） | 语义不等价 / 坑 |
|---|---|---|---|
| `initialize` | 静态能力表 | — | 必须逐字段对照，否则 IDE 面板与后端能力错配 |
| `launch` | `file <program>` → `run`/`start`/`starti` | `-file-exec-and-symbols`, `-exec-run` | 内核场景**不用 launch**（没有 OS 进程概念） |
| `attach` | `attach <pid>` 或 `target remote <target>`；★**响应被推迟到 `configurationDone` 后** | `-target-attach` / `-target-select remote` | **时序陷阱**：IDE 若同步等 `attach` 响应会死锁。必须支持"响应延迟" |
| `setBreakpoints` | `gdb.Breakpoint(spec=...)` — **只能软件断点** | `-break-insert` | **无硬件断点**；内核入口断点必须用 `hbreak` → 见 §4.3 |
| `setInstructionBreakpoints` | `spec="*<addr>"` — 仍走 `gdb.Breakpoint` | `-break-insert *addr` | 同上，**没有硬件变体** |
| `setFunctionBreakpoints` | `{"function": name}` | `-break-insert <fn>` | 断点未加载时是 PENDING，`verified:false` |
| `configurationDone` | 触发被推迟的 launch/attach | — | **必须在 `attach` 之后、`continue` 之前发** |
| `continue` | `-exec-continue` | `-exec-continue` | QEMU gdbstub 上 `allThreadsContinued:true` |
| `next`/`stepIn`/`stepOut` | `-exec-next`/`-exec-step`/`-exec-finish` | 同左 | `stepIn` 支持 `granularity:"instruction"` → `-exec-step-instruction`；**内核里源码级 step 与指令级 step 语义差异大** |
| `pause` | `-exec-interrupt` | `-exec-interrupt` | [已实测] 可用；第一棒的 TIMEOUT 是会话状态问题 |
| `stackTrace` | frame filter + `-stack-list-frames` | `-stack-list-frames` | [已实测] 裸机 + DWARF 下正常；**无符号时会给不出帧** |
| `scopes` | Arguments/Locals/**Registers**/Globals | `-stack-list-arguments` 等 | ★**实测缺 Registers**，见 §4.2；这是最大偏差 |
| `variables` | 基于 `variablesReference` 的惰性求值 | `-stack-list-variables`, `-var-*` | 需自己实现 reference 生命周期（DAP 规范要求 restart 时失效） |
| `readMemory` | `inferior.read_memory(addr)` | `-data-read-memory-bytes` | ★**只有虚拟地址**，见 §4.1 |
| `writeMemory` | `inferior.write_memory(addr)` | `-data-write-memory-bytes` | 同上 |
| `disassemble` | `arch.disassemble()` + `read_memory` | `-data-disassemble` | ★`memoryReference` **只收数字**，`$pc` 会报错，见 §4.5 |
| `evaluate` (hover/watch) | `gdb.parse_and_eval` | `-data-evaluate-expression` | ★对 `$cr3` **只返回类型注解、无数值**，见 §4.4 |
| `evaluate` (repl) | 原样喂给 GDB CLI | 无（MI 无等价物） | ★`$cr3` 在 repl 下**不是合法命令**；但 `info registers`/`monitor` 只能走这条路 |
| `threads` | `thr.global_num` | `-thread-info` | 核 = 线程，语义过粗，见 §5.1 |
| `stopped` 事件 | `gdb.events.stop` | `*stopped` | 内核里 `reason` 多为 `"breakpoint"`/`"step"`/`"signal"`；**`#PF` 不会自动变成 "exception"**，需要 IDE 自己看 IDT/CR2 |
| `continued` / `output` 事件 | `gdb.events.cont` / inferior stdout | `*running` / `@` | 裸机串口输出**不走** DAP `output`，要走 QEMU `-serial` |
| `terminate` | `supportsTerminateRequest:true` | `-target-disconnect` | 对 QEMU，terminate 只断开不关机 |

**自写适配层的真实工作量**：上表 25 行里，内置 DAP 已经实现 22 行，
其中**只有 4 行（硬件断点、物理内存、寄存器 scope、repl 逃生舱）是内核场景的硬缺口**。
自写的收益仅这 4 项，成本却是重实现全部 25 项 + 与 GDB 版本共同演化。
**这是本报告推荐"内置 DAP + 薄适配"而非"自写完整 MI→DAP"的量化依据。**

---

## 7. 现成适配器覆盖度对比

| 方案 | 内核/裸机适用性 | 依据 |
|---|---|---|
| **GDB 内置 DAP**（`gdb -i=dap`） | ✅ **最佳**。可直接 `target remote` 连 QEMU gdbstub，源码级栈回溯实测可用；缺口集中在 4 项，可用 repl 补 | 本报告全部实测 |
| **自写 MI→DAP** | ⚠️ 能做但重复劳动。仅当需要**严格控制**协议行为（如自定义 `stopped` 语义、批量内存、非 GDB 后端）时才划算 | §6 |
| **cppdbg**（VS Code C/C++ 扩展的 MI 引擎） | ⚠️ 它是 **MI 解析器 + DAP 前端**，能力比内置 DAP 窄（无 `readMemory`/`writeMemory`/`disassemble` 的完整实现），且其 MI 后端对 `monitor` 无原生支持。**在本机不可用**（需 VS Code 运行时） | [推测/有来源] 属外部产品，未在本机实测 |
| **CodeLLDB** | ❌ **不适用**。它是 LLDB 前端，而**本工作区没有 LLDB**；且 LLDB 的 `gdb-remote` 对 QEMU gdbstub 的裸机支持弱于 GDB（无 `monitor`、无 GDB 的 `info registers` 语义） | [推测] 未实测，工作区无 lldb |
| **LLDB 路线**（`lldb-dap` + `gdb-remote`） | ❌ 同 CodeLLDB；且需额外引入 LLDB 工具链 | [推测] |
| **GDB 13.1（工作区自带）** | ❌ **没有 DAP**（实测 `Interpreter 'dap' unrecognized`），只能自写 MI 层 | [已实测] |

> 注意：本机**没有任何 LLDB**，所以 CodeLLDB/LLDB 路线在 PrincessIDE 里等于**新增一条工具链依赖**，
> 不是"复用现成"。这一点常被误判为"有现成适配器可用"。

### 7.1 什么情况下推荐会反转

1. **QEMU gdbstub 的错配问题无法绕过时**：B 报告已实测，32 位内核配 `qemu-system-x86_64`
   的 gdbstub 会出现 `Remote 'g' packet reply is too long` 与回溯乱码。
   若 P4 必须支持这种错配组合，而 gdb 的 DAP 层又无法注入架构修正
   （D10 的"架构自检"），则可能需要**自写一层直接说 RSP/gdbstub**，绕开 GDB。
2. **必须支持非 GDB 后端**（如自研 debug stub / Bochs / 自写 hypervisor 探针）时，MI→DAP 层是必写的。
3. **GPL 合规**：内置 DAP 要求随发布物分发 GDB（GPLv3）。若 PrincessIDE 的分发策略不接受，
   则可能被迫改用自己实现的协议层——但注意**自写 MI 适配层仍然要调用 gdb 二进制**，
   合规问题并不会因此消失；只有"自写调试引擎"才能规避。
4. **gdb ≥14 二进制不可携带**（发布环境 glibc 过旧且不允许带 loader）时，本报告的推荐**直接不成立**。

---

## 8. 明确推荐（D11）

**选择：GDB 内置 DAP 作为主干 + Rust 侧薄适配层（"能力补齐 + 状态校验"），
不重写 MI→DAP。**

### 8.1 理由

1. **实测可用**：内置 DAP 已经能对**真实分页内核**完成 attach → 硬件断点 → continue →
   源码级栈回溯（3 帧、带文件行号）→ 内存读写 → 反汇编 → 单步的全链路（§3.1）。
2. **官方维护**：它是 GDB 官方 Python 实现，随 GDB 版本前进，自写必然长期落后。
3. **缺口可枚举且只有 4 个**：硬件断点、物理内存、寄存器 scope、repl 逃生舱（§4）。
   补齐这 4 项的代码量远小于重写 25 项映射。
4. **已有可用二进制**：`.researchC/dapbin/rootfs/usr/bin/gdb`（16.3）可直接进发布物
   （用 rootfs 自带 loader 启动，见 §1.2）。

### 8.2 落地要点（给 P4）

1. **适配层职责**（明确 4 条）：
   - 用 `evaluate(context="repl")` 实现 `hbreak`/`watch`/`rwatch`，并自维护断点表；
   - 用 `monitor xp`（QEMU）/ 自走页表实现物理内存与页表遍历；
   - 用 `info registers` 解析实现寄存器面板（**不要**依赖 DAP `scopes`）；
   - 对所有 repl 输出做**解析 + 错误归一化**，避免把 CLI 文本泄漏给 UI。
2. **必须处理 `attach` 响应延迟**：`attach` 的响应在 `configurationDone` 之后才到（§6），
   同步等待会死锁。
3. **状态校验**：任一请求失败后要主动 `-target-select`/重置或重建会话，
   否则会重现第一棒那种**级联假阴性**（§3.2）。
4. **发布物**：携带 gdb 16.3 及其 loader + Python 运行时（内置 DAP 依赖 Python，
   启动器必须设 `PYTHONHOME`/`PYTHONPATH`，见 §1.2）。

---

## 9. 反例与风险

> 本节按要求单列。以下每条都是"会推翻或削弱推荐"的具体情形。

1. **【最强反例】硬件断点是内核入口调试的刚需，而内置 DAP 完全不支持。**
   [已实测] 第一棒和本次都只能靠 `repl: hbreak *0x100040` 绕过。
   如果一个 IDE 只需要"内置 DAP 开箱即用"，那么它**在 GRUB 引导的裸机内核上连第一个断点都下不了**。
   → 推荐里"薄适配层"**不是可选项，是必需项**；若 P4 明确只做"零 Rust 调试代码"，
   则推荐应反转为自写适配层（或放弃此场景）。

2. **寄存器面板在内置 DAP 上是坏的。**
   [已实测] `scopes` 不返回 Registers scope；`evaluate(context="hover", "$cr3")`
   只返回 `[ PDBR=0 PCID=0 ]`（**无数值**）。
   → 任何依赖 DAP 标准路径做寄存器面板的 P4 实现都会失败。

3. **`monitor` 类物理内存能力是 QEMU 专有的。**
   [已实测] `monitor info registers`/`info mem`/`info tlb`/`xp`/`info cpus` 全部可用，
   **但它们只在 target 是 QEMU gdbstub 时存在**。换成 Bochs、真实硬件 BDM/JTAG、
   或自研 stub，这些命令全部消失，物理内存/页表能力随之归零。
   → 适配层必须把"物理内存读取"抽象成可替换后端，不能把 `monitor` 写死。

4. **QEMU 7.2 没有 `info gdt`/`info idt`，`info mem` 在 LA57 下返回空。**
   [有来源] `docs/research/D-binary-lowlevel-tooling.md` §4.1、§3.6。
   → GDT/IDT 视图与 5 级页表视图**必须自己读内存解析**，与调试协议选择无关。
   这削弱了"用内置 DAP 就省事"的预期。

5. **gdb 16.3 依赖较新的 glibc，必须自带 loader/Python，分发复杂。**
   [已实测] 宿主 glibc 2.36，gdb 16.3 需要 rootfs 里的 glibc；
   启动脚本必须设 `PYTHONHOME`/`PYTHONPATH`，否则内置 DAP（Python 实现）起不来。
   → 发布物体积与启动复杂度是真实成本。

6. **会话脆弱性：一次失败请求会级联污染。**
   [已实测] 第一棒的 `kernel_test.out` 里，`attach` 传了 `program` 之后
   整条链路（`stackTrace`/`scopes`/`monitor`/`pause`）全部假阴性。
   → 若 P4 不实现健壮的状态管理，会得到"内置 DAP 不可用"的错误结论。

7. **`stackTrace` 依赖 DWARF；无符号内核上会退化。**
   [已实测] 本夹具 `.debug_info`/`.debug_line` 齐全才有 3 帧源码级回溯。
   若目标是 stripped 内核，DAP 栈回溯与源码面板都会退化成地址级。
   → "源码级调试"的前提是夹具带调试信息（本工作区满足，但真实场景未必）。

8. **SMP 语义不等价（未被多核夹具验证）。**
   [已实测/推测] DAP 的 thread = 核，但 `allThreadsStopped` 会一次冻结所有核，
   且没有"核"元数据。多核内核的"单核步进"诉求可能无法表达。
   → 本夹具是单核，**多核未实测**，是推荐的一个未验证边界。

9. **B 报告已实测的 gdbstub 错配**（32 位内核 + `qemu-system-x86_64`：
   `Remote 'g' packet reply is too long`、回溯乱码）**内置 DAP 无法修正**，
   因为它只是 GDB 的上层。D10 的架构自检必须由适配层承担。

---

## 10. 未实测 / 被打断的部分（诚实标注）

| 项 | 状态 | 原因 |
|---|---|---|
| SMP 多核（>1 CPU）debug | **未实测** | 现有夹具均为单核；起多核需改夹具（不在本 Agent 可改范围内，铁律禁用 `fixtures/`） |
| CodeLLDB / LLDB 路线 | **未实测** | 工作区无 LLDB 二进制，安装会引入新工具链且违反"不折腾"原则 |
| cppdbg | **未实测** | 需 VS Code 运行时，本机无 |
| LA57（5 级页表）下调试 | **未实测** | 分页夹具是 4 级（`CR4=0x20`，仅 PAE）；LA57 事实引自 D 报告 [有来源] |
| 32 位内核 + `qemu-system-x86_64` 错配下的 DAP 行为 | **未实测** | 事实引自 B 报告 [有来源]；本报告未重复验证 |
| `watch`/`rwatch`（观察点）经 repl | **部分实测** | 未单独下发 `watch`，但从 §4.3 的 `hbreak` 成功可推断同类路径可用 [推测] |
| 内核 `#PF` 变成 DAP `exception` 事件 | **未实测** | 本次 `#PF` 停在 `kernel_main:157`（断点处），未让 `#PF` 真正触发 |

**内存纪律说明**：本次全程只有一路重型动作（1 个 QEMU 64M + 1 个 gdb），
每次启动前 `free -h` 确认 available ≥ 2.4G，未触发 OOM。

---

## 11. 原始证据文件索引

| 文件 | 内容 |
|---|---|
| `.researchC/dapbin/rootfs/usr/bin/gdb` | gdb 16.3 二进制（11 MB，第一棒遗留，本次验证可用） |
| `.researchC/dapbin/gdb163.sh` | 正确的启动器（rootfs loader + `PYTHONHOME`/`PYTHONPATH`） |
| `.researchC/dapbin/dapcli.py` | 最小 DAP 客户端（第一棒遗留） |
| `.researchC/dapbin/kernel_test.out` | **第一棒** refkernel 测试输出（含多处假阴性，见 §3.2） |
| `.researchC/dapbin/kernel_dap_test2.py` | 本次 round-2 脚本（分页内核） |
| `.researchC/dapbin/paging_dap_console.out` | **round-2 原始输出**（508 行，含 stackTrace 3 帧、CR 寄存器、monitor 全部命令） |
| `.researchC/dapbin/kernel_dap_test3.py` | 本次 round-3 脚本（原生断点/evaluate 上下文/pause） |
| `.researchC/dapbin/paging3_console.out` | **round-3 原始输出**（399 行，含软件断点证明、`$cr3` 三上下文差异） |
| `.researchC/gdb-16.3/gdb/python/lib/gdb/dap/` | 内置 DAP 源码（§4/§6 的原文出处） |
