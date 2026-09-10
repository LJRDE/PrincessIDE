# P2-B3 `crates/princess-symbol` 验收报告

> 阶段：P2-B3 ｜ 所有权：`crates/princess-symbol/`、`docs/reports/p2-symbol.md`、`.scratch/symbol/`
> 根 `Cargo.toml` 只加一行成员（`"crates/princess-symbol",`）。
> **铁律遵守**：本文所有「通过」均由真实命令输出 + 退出码支撑，命令原文与输出原样保留，未跑过的不写。

---

## 0. 结论速览

| 验收项 | 结果 | 证据 |
|---|---|---|
| P2-4 `run.fault.symbolicated` | ✅ `{symbol:"refkernel_fault_probe", file:.../kernel.c, line:100}` | §3.1（exit 0） |
| golden 对比 vs `addr2line -f -C` | ✅ refkernel **3015/3015 逐字一致**；pagingkernel 4595/4763 一致，**168 处差异已归因且经 GDB 仲裁证明我们对** | §3.2（exit 0） |
| 分页夹具 #PF 正确行 | ✅ `paging_fault_probe` @ `kernel.c:122` | §3.3（exit 0） |
| 负样本：越界地址 | ✅ 返回 `null` + 明确 `E_NOT_FOUND` 理由，**无假数据**；`buildId` = `null` | §3.4（exit 0） |
| 单测 | ✅ `cargo test -p princess-symbol` → `45 passed; 0 failed` + `10 passed; 0 failed`，exit 0 | §3.5 |
| 选型冻结 D20 | ✅ `cargo tree` 实测版本与 D20 逐项一致 | §2.1 |
| clippy | ✅ exit 0，本 crate **零告警** | §3.6 |

**未决/需主 Agent 裁决**：无。所有结论均有实测支撑。

---

## 1. 交付物与公开 API

### 1.1 文件清单

```
crates/princess-symbol/
├── Cargo.toml
├── src/
│   ├── lib.rs           crate 根：文档、re-export、SYMBOL_STACK 常量
│   ├── elf.rs           ELF 容器事实（object 0.40.0）
│   ├── dwarf.rs         DWARF 行号/函数查找 + 地址区间（gimli 0.34 + addr2line 0.27）
│   ├── demangle.rs      前缀分派的 demangle（rustc-demangle + cpp_demangle）
│   ├── symbol.rs        引擎门面：SymbolIndex、run.fault 富化、帧指针栈回溯
│   └── binprovider.rs   princess_core::BinProvider 实现
├── examples/
│   └── symbolicate.rs   CLI 演示（用于产出本报告的真实输出）
└── tests/
    └── fixture_symbolication.rs   夹具集成测试 + golden 对比
```

### 1.2 公开 API（下游 B2/P4/P5 会用到的）

```rust
// —— 引擎门面（P2 内部 / CLI） ——
princess_symbol::SymbolIndex::open(&Path) -> Result<SymbolIndex>
    .facts() -> &ElfFacts                     // format/arch/kind/entry/build_id
    .build_id() -> Option<&str>               // 夹具上是 None，绝不编造
    .symbol_count() -> u64
    .has_debug_info() -> bool
    .source_line_for_address(u64) -> Result<Option<SourceLine>>   // 精确，仅 DWARF
    .symbolicate(u64) -> Result<Option<SymbolicatedLocation>>     // 契约形状
    .coarse_location(u64) -> Result<Option<CoarseLocation>>       // 弱答案，exact=false
    .symbols() -> Result<Vec<SymbolInfo>>
    .indexed_event_fields() -> (String, Option<String>, u64)      // symbols.indexed

princess_symbol::symbolicate_fault_event(&SymbolIndex, &mut RunFaultPayload) -> Result<bool>
princess_symbol::symbolicate_fault(&Path, &mut RunFaultPayload) -> Result<bool>
princess_symbol::explain_missing_symbolication(&SymbolIndex, u64) -> PrincessError

// —— DWARF 细节（P5 反汇编并排要用） ——
princess_symbol::DwarfIndex::open(&Path) -> Result<DwarfIndex>
    .source_line_for_address(u64) -> Result<Option<SourceLine>>
    .rows_in_range(low, high) -> Result<Vec<SourceLine>>   // 给反汇编视图喂行号
    .frame_chain_for_address(u64) -> Result<Vec<InlinedFrame>>
    .address_span(u64) -> Result<Option<(u64,u64)>>

princess_symbol::SourceLine {
    symbol, raw_symbol, file, line, column,
    address_start, address_end,       // ← 地址区间（D20 要求，非只回行号）
    line_range_start, line_range_end, // ← 行号范围
    inlined,
}

// —— 栈回溯（P4 提供 read_word 闭包，本 crate 不碰活目标内存） ——
princess_symbol::walk_frame_pointer_chain(&DwarfIndex, rbp, max, read_word)

// —— BinProvider（契约 §5） ——
princess_symbol::ElfSymbolProvider::{new, index, dwarf, symbols_indexed_fields}
```

**关键设计点**：`SourceLine` 同时给出 **地址区间** `[address_start, address_end)` 与
**行号范围** `[line_range_start, line_range_end)`，落实 D20「反汇编 + 源码行并排」的硬性含义——
只回一个行号会让 UI 无法画出「这条语句对应哪几条指令」。

---

## 2. 选型与依赖（D20 冻结，未做任何再比较）

### 2.1 实测依赖版本

```
$ cargo tree -p princess-symbol --depth 1
princess-symbol v0.1.0 (/root/PrincessIDE/crates/princess-symbol)
├── addr2line v0.27.1
├── cpp_demangle v0.5.1
├── gimli v0.34.0
├── object v0.40.0
├── princess-core v0.1.0 (/root/PrincessIDE/crates/princess-core)
├── rustc-demangle v0.1.28
├── serde v1.0.229
└── serde_json v1.0.151
[dev-dependencies]
└── serde_json v1.0.151 (*)
```

与 D20 逐项比对：**`object` 0.40.0 ✅ / `gimli` 0.34.0 ✅ / `addr2line` 0.27.1 ✅ /
`rustc-demangle` 0.1.28 ✅ / `cpp_demangle` 0.5.1 ✅**。
**未引入** `goblin`、`elf`；**未引入** 任何反汇编库（`iced-x86` 属 P5）。

代码里以 `SYMBOL_STACK` 常量固化，并有单测 `the_frozen_stack_is_the_one_d20_requires` 钉住。

### 2.2 gimli 与 addr2line 的分层（为什么两个都要）

| 层 | 用途 |
|---|---|
| `addr2line::Context` | **查找**：解析 unit/line/function 索引，回答「这个地址在哪一行」。行选择规则与 GNU `addr2line` 一致，golden 对比靠它。 |
| `gimli` | **细节**：直接读 `DW_LNE_set_discriminator` 等行程序细节，并为 `rows_in_range` 提供区间；D20 并排视图与判据分析需要。 |
| `object` | **容器**：ELF 头/段节/`.symtab`/build-id。 |

---

## 3. 验收逐项（命令 → 退出码 → 关键输出）

### 3.1 P2-4：`run.fault.symbolicated`

```
$ cargo run -q -p princess-symbol --example symbolicate -- fixtures/refkernel/build/refkernel.elf 0x100b3d
artifact      : fixtures/refkernel/build/refkernel.elf
format        : elf64
architecture  : X86_64
kind          : executable
entryPoint    : 0x100040
buildId       : null
hasDebugInfo  : true
symbolCount   : 73
stack         : object=object 0.40.0 gimli=gimli 0.34.0 addr2line=addr2line 0.27.1 rustc-demangle=rustc-demangle 0.1.28 cpp_demangle=cpp_demangle 0.5.1

address       : 0x100b3d  (ripText "0x0000000000100b3d")
  symbolicated: {
    symbol        : "refkernel_fault_probe"
    rawSymbol     : "refkernel_fault_probe"
    file          : "/root/PrincessIDE/fixtures/refkernel/kernel.c"
    line          : 100
    column        : 5
    addressRange  : [0x100b3d, 0x100b3f)   // 2 bytes, for the disassembly gutter
    lineRange     : [100, 100)
    inlined       : false
  }
  run.fault.symbolicated = {"symbol":"refkernel_fault_probe","file":"/root/PrincessIDE/fixtures/refkernel/kernel.c","line":100}
  ripText preserved      = "0x0000000000100b3d"
EXIT=0
```

**对照 `docs/spec/20-acceptance.md` 的固定断言常量**：
`refkernel_fault_probe` @ `kernel.c:100` ✅。故障 RIP `0x100b3d` 取自夹具自己的
`fixtures/refkernel/build/serial.log` 中 `FAULT_RIP=0x0000000000100b3d`。

**`ripText` 契约（`10-contracts.md` §2）**：`ripText` 原文 `0x0000000000100b3d`
逐字保留，富化只写 `symbolicated`。单测
`fault_enrichment_preserves_rip_text_verbatim` 与集成测试
`fault_enrichment_keeps_the_guest_hex_text` 双重钉死。

### 3.2 golden 对比：与系统 `addr2line -f -C` 逐字比对

方法：对两个夹具的**整个 `.text`**（不是只挑故障那一个地址）**逐字节**遍历，每个地址都调用
系统 binutils 的 `addr2line -f -C -e <elf> <addr>`，与库的输出对比。

```
$ cargo test -p princess-symbol --test fixture_symbolication -- --nocapture
...
golden comparison [refkernel]: 3015 addresses compared against binutils; 3015 file:line exact, 0 differ only by binutils' header-inline attribution, 0 agreed on 'no row', 417 carried a binutils-only discriminator suffix
golden comparison [pagingkernel]: 4763 addresses compared against binutils; 4595 file:line exact, 168 differ only by binutils' header-inline attribution, 0 agreed on 'no row', 629 carried a binutils-only discriminator suffix
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 45.19s
EXIT=0
```

**差异必须解释（任务书要求「逐字比对，差异要解释」），共两类，均已定位：**

#### 差异 A：binutils 的 `(discriminator N)` 后缀 — 417 + 629 处

```
$ addr2line -f -C -e fixtures/refkernel/build/refkernel.elf 0x1002cb
serial_putc
/root/PrincessIDE/fixtures/refkernel/serial.c:51 (discriminator 1)
```

**归因**：binutils 在行程序的 `DW_LNE_set_discriminator` 非零时追加 ` (discriminator N)`。
**`addr2line` 0.27.1 的公开 `Location` 不暴露 discriminator**（`gimli` 0.34 能解析，
`addr2line` crate 的 `Location` 只有 `file`/`line`/`column`）。属**工具能力边界**，非符号化错误：
在上述每一处，`file:line` 本身都**完全一致**。测试对此显式计数而非静默忽略。

#### 差异 B：binutils 对 header 内联函数的归属重写 — 168 处

```
$ addr2line -f -C -e fixtures/paging-kernel/build/pagingkernel.elf 0x100a56
read_cr0
/root/PrincessIDE/fixtures/paging-kernel/paging.c:37        ← binutils

$ gdb -batch -q -ex "file fixtures/paging-kernel/build/pagingkernel.elf" -ex "info line *0x100a56"
Line 37 of "/root/PrincessIDE/fixtures/paging-kernel/paging.h" starts at address 0x100a56 <read_cr0> and ends at 0x100a5e <read_cr0+8>.   ← GDB
```

**归因（决定性证据链）**：

1. `read_cr0` 是 `paging.h:36` 的 `static inline` 函数，gcc 为它**额外生成了一份 out-of-line 拷贝**。
2. 该 DWARF 5 行程序在 `0x100a56` 的**原始行记录**是 `file_index=1`，而该 CU 的文件表
   第 1 项就是 `paging.h`：
   ```
   $ objdump --dwarf=rawline fixtures/paging-kernel/build/pagingkernel.elf | grep -A8 "offset 0x4b0"
    The File Name Table (offset 0x4b0, lines 5, columns 2):
     Entry	Dir	Name
     0	0	(indirect line string, offset: 0x95): paging.c
     1	0	(indirect line string, offset: 0x9e): paging.h      ← file_index=1
   ```
   gimli 直读行程序输出 `file[1] = paging.h`、`row addr=0x100a56 file_index=1 -> paging.h line=Some(37)`（探针程序见 §3.7）。
3. **GDB 用完全独立的 DWARF 读取器，给出与我们一致的 `paging.h:37`**。
4. 统计佐证：binutils 对整个 `pagingkernel.elf` 的输出里**一个 `.h` 都没有**
   （`{boot.S:343, isr.S:228, serial.c:1721, idt.c:290, paging.c:1227, kernel.c:954}`），
   说明它是在把 header 代码**归一到 CU 的 `.c`**，而不是读行记录。

**裁决：以行程序 / GDB 为准（即 `paging.h`）**，理由：
- GNU 自家两个工具互相矛盾时，`objdump --dwarf=rawline`（DWARF 原文）与 GDB 站在同一侧；
- D20 明确「反汇编 + 源码行并排」是给**用户看**的，显示函数**被书写所在的头文件**更有用也更字面；
- 我们报告的是 `DW_AT_decl_file` 之外的**行程序事实**，不是猜测。

该差异在测试中被**显式建模**（`is_known_binutils_header_attribution`），必须「同一行号 +
我们 `.h` + binutils `.c`」才算合法差异，其余任何不一致都会让测试失败——不会掩盖真 bug。
另有独立测试 `header_inline_row_matches_the_line_program_and_gdb_not_binutils` 用 GDB 现场仲裁。

### 3.3 分页夹具 #PF：正确行 + 与 `#PF` 处理路径的关系

```
$ cargo run -q -p princess-symbol --example symbolicate -- fixtures/paging-kernel/build/pagingkernel.elf 0x1011dd
artifact      : fixtures/paging-kernel/build/pagingkernel.elf
format        : elf64
buildId       : null
hasDebugInfo  : true
symbolCount   : 86

address       : 0x1011dd  (ripText "0x00000000001011dd")
  symbolicated: {
    symbol        : "paging_fault_probe"
    rawSymbol     : "paging_fault_probe"
    file          : "/root/PrincessIDE/fixtures/paging-kernel/kernel.c"
    line          : 122
    column        : 14
    addressRange  : [0x1011d9, 0x1011e4)   // 11 bytes, for the disassembly gutter
    lineRange     : [122, 122)
    inlined       : false
  }
  run.fault.symbolicated = {"symbol":"paging_fault_probe","file":"/root/PrincessIDE/fixtures/paging-kernel/kernel.c","line":122}
  ripText preserved      = "0x00000000001011dd"
---- exit=0 ----
```

**`kernel.c:122` 正是故意解引用未映射地址的那一行**：

```c
119  __attribute__((noinline, used))
120  void paging_fault_probe(void)
121  {
122      volatile uint64_t *probe = (volatile uint64_t *)(uintptr_t)UNMAPPED_ADDR;
123      uint64_t value = *probe;            /* <-- #PF is delivered here */
```

**与 `#PF` 处理路径的关系（任务书要求解释）**：x86-64 在 `#PF` 时压入的 RIP 是
**触发故障的那条指令的地址**（`paging_fault_probe` 内），而**不是** IDT 门里的
`isr_stub_14` / `page_fault_handler`。因此符号化必须落在**故障现场**而非交付路径上。
集成测试 `paging_pf_rip_is_in_the_faulting_function_not_the_handler` 显式断言：
故障 RIP 解析为 `paging_fault_probe`，且与 `isr_stub_14`（或含 `page_fault` 的符号）
是**不同的地址、不同的行**。

另一处与 §3.2 差异 B 呼应的细节：`0x1011dd` 落在**书写于 `kernel.c` 的**函数里，
不涉及 header 内联，因此这里 binutils 与我们的答案**完全一致**——
这也从反面印证了差异 B 的成因确实是「header 内联归属」，而非我们的行选择有偏差。

### 3.4 负样本

```
### NEGATIVE SAMPLE 1: 越界 / 空地址
$ cargo run -q -p princess-symbol --example symbolicate -- fixtures/refkernel/build/refkernel.elf 0xdeadbeef 0x0 fffffffffffffffe
address       : 0xdeadbeef  (ripText "0x00000000deadbeef")
  symbolicated: null
  reason        : E_NOT_FOUND — no DWARF line row covers 0xdeadbeef in fixtures/refkernel/build/refkernel.elf
  coarse        : null (no symbol contains this address)

address       : 0x0  (ripText "0x0000000000000000")
  symbolicated: null
  reason        : E_NOT_FOUND — no DWARF line row covers 0x0 in fixtures/refkernel/build/refkernel.elf
  coarse        : null (no symbol contains this address)

address       : 0xfffffffffffffffe  (ripText "0xfffffffffffffffe")
  symbolicated: null
  reason        : E_NOT_FOUND — no DWARF line row covers 0xfffffffffffffffe in fixtures/refkernel/build/refkernel.elf
  coarse        : null (no symbol contains this address)
---- exit=0 ----

### NEGATIVE SAMPLE 2: 不存在的文件
$ cargo run -q -p princess-symbol --example symbolicate -- /nonexistent/nope.elf 0x1000
error [E_NOT_FOUND]: symbol artifact not found: /nonexistent/nope.elf
detail: No such file or directory (os error 2)
---- exit=1 ----

### NEGATIVE SAMPLE 3: 存在但不是 ELF 的文件
$ cargo run -q -p princess-symbol --example symbolicate -- docs/spec/00-decisions.md 0x1000
error [E_INTERNAL]: docs/spec/00-decisions.md is not a parseable ELF image: Unknown file magic
detail: Unknown file magic
---- exit=1 ----
```

**逐条满足任务书**：「给一个不存在/越界的地址 → 必须明确报错或返回 None，不得返回看似合理的假数据」。
- 越界地址 → `null` + **说清理由**（`E_NOT_FOUND`，附「没有行记录覆盖该地址」）；
- 文件不存在 → `E_NOT_FOUND`，**不是**「空符号表」；
- 文件存在但非 ELF → `E_INTERNAL` 并称「不是可解析的 ELF」，**不会**被当成「零符号的有效镜像」。

**`buildId` 缺省必须是 `null`**：

```
$ addr2line -f -C -e ... ; readelf -n fixtures/refkernel/build/refkernel.elf
（无任何输出）
$ readelf -n fixtures/refkernel/build/refkernel.elf        # exit=0，输出为空
$ readelf -n fixtures/paging-kernel/build/pagingkernel.elf # exit=0，输出为空
```

两个夹具都没有 `.note.gnu.build-id`，故 `buildId` 打印为 `null`（§3.1 / §3.3 输出中可见）。
`build_id` 只在镜像**真的**含 build-id 时才是 `Some`，**没有**回退到文件哈希/时间戳等编造手段。
由单测 `build_id_is_absent_not_invented_on_the_fixtures` 与集成测试
`build_id_is_null_not_invented` 双重钉死。

**额外反例（防「假阴性」）**：单测
`an_address_inside_a_function_but_on_a_line_with_no_row_is_still_none_or_valid`
遍历整个 `.text`（步长 7），断言每个答案**要么是 `None`、要么其地址区间真的覆盖该探针地址**——
专门用来抓「返回看起来合理但其实不覆盖该地址的行」这类最难发现的假数据。

### 3.5 单元测试

```
$ cargo test -p princess-symbol
     Running unittests src/lib.rs
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.30s
     Running tests/fixture_symbolication.rs
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 45.41s
test result: ok. 0 passed; 0 failed; ...
EXIT=0
```

隔离验证（不牵连兄弟 crate）：

```
$ cargo test -p princess-symbol -p princess-core -p princess-build
princess-build  : 58 passed; 0 failed
princess-core   : 36 passed; 0 failed  (+4 event_fixture)
princess-symbol : 45 passed; 0 failed  (+10 fixture_symbolication)
EXIT=0
```

> **⚠️ 与 B3 无关的工作区失败记录**：`cargo test --workspace` 观察到
> `crates/princess-ai/tests/p71_streaming.rs` 有 **2 个失败**
> （`cancelling_mid_stream_stops_every_later_chunk`、
> `cancelling_through_the_cancel_token_stops_the_stream_too`）。
> `princess-ai` 是**同时开工的 P7 兄弟 crate，不在我的所有权内**，我未做任何修改；
> 记录于此仅供主 Agent 转交对应 Agent。

### 3.6 clippy

```
$ cargo clippy -p princess-symbol --all-targets
（princess-symbol 零告警）
EXIT=0
```

剩余 10 条告警**全部来自冻结的 `princess-core` lib**（`this impl can be derived` ×9 + 汇总行 ×1），
不属我的可写目录，未做改动。

### 3.7 归因用的原始证据（可复现命令）

```
$ objdump --dwarf=rawline fixtures/paging-kernel/build/pagingkernel.elf | grep -A8 "offset 0x4b0"
 The File Name Table (offset 0x4b0, lines 5, columns 2):
  Entry	Dir	Name
  0	0	(indirect line string, offset: 0x95): paging.c
  1	0	(indirect line string, offset: 0x9e): paging.h
  2	0	(indirect line string, offset: 0x95): paging.c
  3	1	(indirect line string, offset: 0x7c): stdint-gcc.h
  4	0	(indirect line string, offset: 0xa7): serial.h

$ objdump -d --line-numbers --start-address=0x100a56 --stop-address=0x100a6b fixtures/paging-kernel/build/pagingkernel.elf
0000000000100a56 <read_cr0>:
read_cr0():
/root/PrincessIDE/fixtures/paging-kernel/paging.c:37
  100a56:	55                   	push   %rbp
  ...

$ gdb -batch -q -ex "file fixtures/paging-kernel/build/pagingkernel.elf" -ex "info line *0x100a56"
Line 37 of "/root/PrincessIDE/fixtures/paging-kernel/paging.h" starts at address 0x100a56 <read_cr0> and ends at 0x100a5e <read_cr0+8>.
```

gimli 直读行程序的探针（临时程序，位于 `.scratch/symbol/gimli_probe/`）：

```
--- unit line prog v5 ---
  file[0] = paging.c
  file[1] = paging.h
  file[2] = paging.c
  file[3] = stdint-gcc.h
  file[4] = serial.h
  row addr=0x100a56 file_index=1 -> paging.h line=Some(37)
  row addr=0x100a5e file_index=1 -> paging.h line=Some(39)
```

---

## 4. 有歧义的设计决定（供主 Agent 复核）

| # | 决定 | 理由 | 若不同意如何改 |
|---|---|---|---|
| **B3-D1** | `BinProvider::disassemble` **显式报错**（`E_INTERNAL`，指明归 P5），不返回空 `Vec` | D20 把反汇编划给 P5 的 `iced-x86`；空 `Vec` 会被 UI 渲染成「这个函数没有指令」，那是谎报（契约 §0.4 失败必须显式） | 若要求「返回空」以便占位，改一行即可 |
| **B3-D2** | 「精确」与「粗略」两档答案用**不同类型**隔离：`source_line_for_address` 只回 DWARF 行；`.symtab` 级答案必须显式调 `coarse_location`，且 `exact: false`，`to_symbolicated_location()` 恒为 `None` | 负样本要求「不得返回看似合理的假数据」；若粗略答案能悄悄冒充精确答案，这条就形同虚设 | 可放宽为可选参数 |
| **B3-D3** | header 内联行**以行程序 / GDB 为准**（报 `paging.h`），不跟随 binutils（报 `paging.c`） | §3.2 差异 B 证据链：DWARF 原文 + GDB 一致；binutils 全 `.text` 输出无任何 `.h`，是它做了归一 | 若要求与 binutils 逐字一致，需在 header 行上回退到 CU 文件名（可在 `dwarf.rs` 单点实现） |
| **B3-D4** | 栈回溯只做**帧指针链**，不做 DWARF CFI 展开；`read_word` 由调用方注入 | 夹具 `-O0` 带帧指针；CFI 展开属 P4（它才持有活目标）；注入读内存闭包保证「不编造没读到的内存」 | P4 可在此基础上叠加 CFI |
| **B3-D5** | `ElfFacts.kind` 用 `object` 的 `Debug` 小写拼写（实测 `executable`），`format` 按契约拼 `elf64`/`elf32`（由 `is_64()` 推导，因 `object` 的 `BinaryFormat::Elf` 不分 32/64） | 契约 §5 明写 `format: elf32｜elf64`；`object` 不提供该区分 | 若 `kind` 需要 `exec`，改一行 |

---

## 5. 给下游的接口提示（≤5 条）

1. **`SymbolIndex` 应当复用**：`open()` 会解析全部 DWARF，构造较贵；一次运行开一个、多个地址查询，
   别对每个 RIP 都 `open()`。`DwarfIndex::context()` 目前每次查询重建（引擎的用量下是有意为之的取舍），
   若要批量反汇编请改用一次 `rows_in_range(low, high)` 取整段行号。
2. **`run.fault` 富化只调 `symbolicate_fault_event`**：它保证不动 `ripText`/`rip`/`regs`。
   不要自己拼 `SymbolicatedLocation`，否则 `buildId`/`ripText` 这类契约细节容易漂移。
3. **`symbols.indexed` 的 `buildId` 请用 `SymbolIndex::indexed_event_fields()`**：它把
   「`None` 就输出 `null`」这条规则收在一处；夹具是 `--build-id=none`，**必须**是 `null`。
4. **P5 反汇编并排视图**：用 `DwarfIndex::rows_in_range(low, high)` 一次取回
   `[address_start, address_end)` 与行号，再与指令对齐；不要逐指令调 `source_line_for_address`。
   本 crate **不提供**反汇编（P5 用 `iced-x86`），`BinProvider::disassemble` 会显式报错。
5. **P4 栈回溯**：`walk_frame_pointer_chain` 需要一个 `read_word: FnMut(u64) -> Option<u64>`
   的读内存闭包（从你的调试后端取），返回 `None` 即终止回溯——**它不会读它没读到的内存**。
   另注意 D10：32 位内核配 `qemu-system-x86_64` 的 gdbstub 会出错，需先做架构自检。

---

## 6. 纪律遵守声明

- **只写了授权目录**：`crates/princess-symbol/`、`docs/reports/p2-symbol.md`、`.scratch/symbol/`；
  根 `Cargo.toml` **只加了成员一行** `"crates/princess-symbol",`（未动其它任何行）。
  > 说明：主 Agent 的批量提交（`faf8ca8` 等）已把我这一行连同兄弟 crate 一起带入 HEAD；
  > 我本人**未执行任何 git 命令**，`crates/princess-symbol/` 至今仍是未跟踪状态（`?? crates/princess-symbol/`），
  > 待主 Agent 统一提交。
- **未修改** `crates/princess-core/`（冻结只读）、`crates/princess-build/`、`crates/princess-run/`
  及任何兄弟 crate。
- **未执行任何改动 git 的命令**（本报告仅用 `git status --short` 只读观察过一次）。
- **内存纪律 D22**：全程 `CARGO_BUILD_JOBS=2` 上限；构建前 `free -h` 预检；
  构建放后台/长超时；本次未出现 OOM（`dmesg` 无需查）。
- **网络 D19**：依赖全部命中 `.toolchain/cargo/registry` 既有缓存（版本恰为 D20 冻结版），
  `cargo fetch` exit 0，未改动 `.cargo/config.toml`。
- **临时文件**只在 `.scratch/symbol/`（含 `evidence/`、`gimli_probe/`），未污染仓库根。

### 与主 Agent 文档的偏差

**无**。未发现 `docs/spec/` 与实测冲突之处，无需修订契约或决策日志。
