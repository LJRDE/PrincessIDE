# PrincessIDE 视图模型层规格 v0.1

> 依据：`docs/spec/00-decisions.md` **D30**（双前端选配）、**D20**（反汇编与源码行并排）
> 关联：`docs/spec/31-native-frontend.md`（P-E0 需求）、`docs/spec/30-modules.md`（M5/M12）
> 状态：**本轮不进入 §3 命令表**；由 P-E0b 将 spec §3 + TS + Rust 三处**原子迁移**

## 一、定位与设计原则

视图模型层是双前端架构的核心共享资产。它的存在使得：

> **逻辑归引擎，两个前端只换绘制。** (D30.3 第 2 条)

每个视图模型满足以下约束：

| 约束 | 含义 |
|---|---|
| **纯数据** | 构造器只吃内存数据，零副作用、无 I/O |
| **可 JSON 序列化** | `#[derive(Serialize)]`，`serde_json::to_string` 稳定 |
| **字段顺序稳定** | 用 `struct` 而非 `HashMap`，保证序列化顺序一致 |
| **逐字可比** | 同一输入两次序列化结果逐字相等（同构断言基础） |

## 二、四个视图模型

### 2.1 EventLog（事件日志）

**输入**：事件流（`princess-core` 的 `Event` 数组）

**模型要点**：每行一个事件，支持分页/窗口化（虚拟化滚动）

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `rows` | `Vec<EventLogRow>` | 当前窗口内的行 | - |
| `totalRows` | `usize` | 整个流的总行数 | 行 |
| `offset` | `usize` | 当前窗口的起始偏移（0-based） | 行 |

**EventLogRow 字段**：

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `seq` | `u64` | 事件序列号（单调递增） | - |
| `ts` | `String` | 时间戳（RFC 3339 毫秒） | - |
| `opId` | `Option<String>` | 关联的操作 ID | - |
| `stream` | `String` | 来源流（"build"、"serial.com1"、"ide" 等） | - |
| `text` | `String` | 显示文本 | - |
| `lossy` | `bool` | 是否有损编码（UTF-8 替换字符） | - |
| `severity` | `Option<Severity>` | 严重性（仅诊断/故障事件） | - |

**不变量**：
- `totalRows` 等于从输入事件产生的实际行数
- `rows.len()` ≤ `totalRows`
- `offset` + `rows.len()` ≤ `totalRows`
- `seq` 在模型内严格单调递增

### 2.2 Source（源码视图）

**输入**：源文件内容（字符串）+ 诊断列表

**模型要点**：逐行显示，诊断内联到对应行

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `file` | `String` | 源文件路径（绝对路径） | - |
| `rows` | `Vec<SourceRow>` | 逐行显示数据 | - |

**SourceRow 字段**：

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `lineNo` | `u32` | 行号（从 1 起） | 行 |
| `text` | `String` | 该行文本（不含换行符） | - |
| `diagnostics` | `Vec<SourceDiagnostic>` | 该行的诊断列表 | - |

**SourceDiagnostic 字段**：

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `col` | `Option<u32>` | 列号（1-based，编译器未报告时为 null） | 列 |
| `severity` | `DiagnosticDisplaySeverity` | "error" / "warning" / "note" | - |
| `message` | `String` | 诊断消息 | - |

**不变量**：
- 行号从 1 起，严格递增 1
- `rows[i].lineNo == i + 1`
- 诊断按列号排序（升序）

### 2.3 Hex（十六进制视图）

**输入**：原始字节数据

**模型要点**：经典 hex dump 格式，地址连续

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `rows` | `Vec<HexRow>` | hex 行，按地址排序 | - |
| `baseAddr` | `u64` | 第一行的基地址 | 字节 |
| `totalSize` | `usize` | 数据总大小 | 字节 |

**HexRow 字段**：

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `addr` | `u64` | 该行第一个字节的地址 | 字节 |
| `bytes` | `Vec<u8>` | 该行的字节（最多 16 个） | - |
| `ascii` | `String` | ASCII 表示（可打印字符原样，其他用 `.`） | - |

**不变量**：
- 行地址连续：`rows[i+1].addr == rows[i].addr + 16`（最后一行可能不满 16 字节）
- `bytes.len()` ≤ 16
- `ascii.len() == bytes.len()`
- 地址值单调递增

### 2.4 Disassembly（反汇编视图）— D20 硬要求

**输入**：`princess-bin` 的 `DisassembledInstruction` 数组

**模型要点**：**每行必须有 `sourceFile` 和 `sourceLine` 字段**（可以是 null），不允许让渲染器自己去查

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `rows` | `Vec<DisassemblyRow>` | 反汇编行，按地址排序 | - |
| `baseAddr` | `u64` | 第一条指令的地址 | 字节 |
| `totalInstructions` | `usize` | 指令总数 | 条 |

**DisassemblyRow 字段**：

| 字段 | 类型 | 含义 | 单位 |
|---|---|---|---|
| `addr` | `u64` | 指令地址 | 字节 |
| `bytes` | `Vec<u8>` | 原始指令字节 | - |
| `mnemonic` | `String` | 指令助记符（如 "mov"、"call"） | - |
| `operands` | `String` | 操作数文本 | - |
| `sourceFile` | `Option<String>` | 源文件路径（无调试信息时为 null） | - |
| `sourceLine` | `Option<u32>` | 源码行号（无调试信息时为 null） | 行 |
| `symbol` | `Option<String>` | 覆盖该地址的符号名 | - |

**不变量**：
- **D20 硬要求**：每行必须有 `sourceLine` 或显式 `null`——前端不需要自己查
- `addr` 值单调递增
- `sourceFile` 和 `sourceLine` 同时存在或同时为 null

## 三、两个前端如何消费

| 前端 | 职责 | 不允许做的事 |
|---|---|---|
| **Web（Tauri）** | 只读、只画 | 不得自己解析 ELF/DWARF |
| **原生（Vulkan）** | 只读、只画 | 不得自己解析 ELF/DWARF |

两个前端消费**同一份**视图模型 JSON。引擎计算视图模型，前端只负责渲染。

## 四、与契约命令表的关系

**本轮（P-E0a）不将 `princess:view:*` 写入 `docs/spec/10-contracts.md` §3。**

理由：`princess:view:*` 一旦写进 §3，`check-contract.mjs` 会要求 spec + TS + Rust 三处同时存在，而 TS/Rust 两处在别路 Agent 的所有权内。

**P-E0b 的职责**：将 spec §3 + TS + Rust 三处**原子迁移**，同时保证：
1. `check-contract.mjs` 仍然 ALIGNED
2. 现有 24 个命令不被破坏

## 五、黄金夹具

黄金夹具位于 `fixtures/view/`，每个视图一个 JSON 文件：

| 文件 | 内容 | 来源 |
|---|---|---|
| `eventlog.json` | 事件日志模型 | `fixtures/events/refkernel-run.ndjson` |
| `source.json` | 源码视图模型 | `fixtures/refkernel/kernel.c` |
| `hex.json` | hex 视图模型 | `fixtures/refkernel/build/refkernel.elf` 前 48 字节 + `0x100b30` 区域 |
| `disassembly.json` | 反汇编视图模型 | `refkernel_fault_probe` 函数（`0x100b39`） |

夹具的 key 必须与本规格一致。夹具是"契约的可执行副本"，以后 P-E0b/前端/原生端都以它为准。

## 六、验证门

| 门 | 命令 | 期望 |
|---|---|---|
| 构建 | `CARGO_BUILD_JOBS=2 cargo build -p princess-view` | exit 0 |
| 测试 | `CARGO_BUILD_JOBS=2 cargo test -p princess-view` | exit 0，所有测试通过 |
| 契约校验 | `node apps/desktop/scripts/check-contract.mjs` | ALIGNED（本轮不改命令表） |
| 黄金文件存在 | `ls fixtures/view/*.json` | 4 个文件 |
| D20 落地 | `grep -c 'sourceLine' fixtures/view/disassembly.json` | > 0 |
