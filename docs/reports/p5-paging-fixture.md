# P5/P4 前置夹具验收报告 —— 开启分页的 x86_64 参考内核 + QEMU monitor 录制样本

> 实现 Agent 报告。所有「通过」均由本工作区真实命令输出与退出码支撑；命令原文见各小节。
> 验收环境：QEMU 7.2.22 (Debian 1:7.2+dfsg-7+deb12u18+b3)、gcc 12.2.0、GNU ld 2.40、
> grub-mkrescue（宿主 `/usr/bin`）、无 KVM（TCG）、`source scripts/env.sh`。
> 遵守决策：D2（Multiboot2 + GRUB ISO，`-cdrom -boot d`，不走 `-kernel`）、
> D3（`fixtures/refkernel/` 只读未改动；不引用 `_work/`、`.research*/`、`_toolchain/`）、
> D9（`-display none -serial stdio -monitor none`，禁止 `-nographic`，monitor 走 QMP）、
> D15（临时文件在 `.scratch/paging/`）、D16（`-j2` 上限）。

---

## 1. 摘要

新增两个夹具，补上「P4 读 CR3/走页表」与「P5 页表/GDT-IDT 可视化、monitor 解析」在
验收上的空白：

1. **`fixtures/paging-kernel/`** —— 一个**真的开启分页**的 x86_64 Multiboot2 内核：
   建立完整 4 级页表 `PML4→PDPT→PD→PT`（含 4 KiB 页、2 MiB 大页、两个**故意未映射**的空洞），
   加载 GDT 与 IDT（`#PF`、`#UD` 有专门入口），打开 `CR0.PG=1` + `CR4.PAE` + `EFER.LME`，
   在 COM1 打印横幅与 **CR3、CR0、逐级页表项、GDTR/IDTR**，然后**故意解引用未映射地址
   `0x400000`**，触发**真实 `#PF`**（`CR2=0x400000`），打印错误码/CR2/故障 RIP 后停机。
2. **`fixtures/qemu-monitor/`** —— 用 **QMP（unix socket）** 在**分页开启之后**录制的
   真实 monitor 样本：`info registers`、`info mem`、`info tlb`、`info cpus`（原样保存，
   非手写），另附结构化 QMP 查询，以及一键再生成脚本 `regenerate.sh`。

关键结论：**分页确实开启**（串口 `CR0.PG=1` + `CR3=0x104000`；`-d int` 出现
`check_exception ... new 0xe` 且 `v=0e ... CR2=0000000000400000`），且 `info mem` / `info tlb`
**非退化**，真实反映了建立的映射（含两个空洞）。

---

## 2. 交付物目录树

```
docs/reports/p5-paging-fixture.md                 （本报告）
fixtures/paging-kernel/
├── .gitignore                build/ 忽略
├── README.md                 夹具说明（内存映射表、输出契约、坐标）
├── Makefile                  all / iso / run / symbols / check / clean
├── linker.ld                 1 MiB 基址，RX + RW 段
├── grub.cfg                  GRUB Multiboot2 菜单
├── boot.S                    Multiboot 头 + GDT + 4 级页表 + 长模式入口
├── isr.S                     向量 0..31 异常入口桩
├── idt.c / idt.h             IDT 构造（#PF/#UD 门）+ 导出的 IDTR
├── paging.c / paging.h       CR0/CR2/CR3/CR4 读取、按 CR3 走页表、GDT/IDT dump
├── serial.c / serial.h       轮询 COM1 + 迷你 printf
├── kernel.c                  kernel_main / exception_dispatch / paging_fault_probe
├── run.sh                    无头启动并断言 banner + #PF
├── princess.toml             工程描述（契约 §4 字段）
└── build/                    构建产物（.gitignore；P9 会删掉重建）
    ├── pagingkernel.elf
    ├── pagingkernel.iso
    └── serial.log

fixtures/qemu-monitor/
├── README.md                 每条样本的获取方式、可解析字段、版本相关字段、局限
├── regenerate.sh             一条命令重建全部样本（自 source env.sh、自定位仓库根）
├── qmp_capture.py            QMP 客户端（仅标准库）
├── info-registers.txt        HMP `info registers`（34 行）
├── info-mem.txt              HMP `info mem`（3 行，3 段映射）
├── info-tlb.txt              HMP `info tlb`（1021 行）
├── info-cpus.txt             HMP `info cpus`（1 行）
├── hmp-responses.jsonl       4 条 HMP 回复的原始 JSON（按 QMP return 原样）
├── qmp-query-version.json
├── qmp-query-status.json
├── qmp-query-cpus-fast.json
├── qmp-query-memory-size-summary.json
├── capture-serial.log        抓取时刻 guest 串口（证明分页已开）
├── capture-metadata.txt      argv / 时间 / 查询列表 / guest 状态
└── qemu-version.txt          QEMU 版本

.scratch/paging/              （临时/过程文件，非交付物）
├── PROGRESS.md               进度日志
├── verify.sh / verify.log    从零验收脚本与完整输出
├── capture-int.sh / qemu-int.log / serial-int.log   -d int 证据
```

`fixtures/refkernel/`、`crates/`、`apps/`、`scripts/`、`docs/spec/`、`docs/research/`、
`templates/`、根 `Cargo.toml`、`package.json` **均未改动**（本次只新建上述授权路径）。

---

## 3. 分页夹具设计要点

### 3.1 内存映射（`boot.S` 建立，`paging.c` 按 CR3 复走）

`PT[511]` 与 `PD[2]` 被有意留成 not-present，形成两个空洞；故障探针打的是 `PD[2]` 空洞：

| 虚拟地址范围 | 映射 | 页表层 |
|---|---|---|
| `0x0000_0000_0000_0000` – `0x0000_0000_001F_EFFF` | 4 KiB 页，P+RW | `PML4[0]→PDPT[0]→PD[0]→PT[0..510]` |
| `0x0000_0000_001F_F000` – `0x0000_0000_001F_FFFF` | **not present（空洞）** | `PT[511]=0` |
| `0x0000_0000_0020_0000` – `0x0000_0000_003F_FFFF` | 2 MiB 大页，P+RW | `PD[1]` |
| `0x0000_0000_0040_0000` – `0x0000_0000_005F_FFFF` | **not present（故障目标）** | `PD[2]=0` |
| `0x0000_0000_0060_0000` – `0x0000_0000_3FFF_FFFF` | 2 MiB 大页，P+RW | `PD[3..511]` |

内核自身（1 MiB 起）、栈、页表、IDT 全在首个 2 MiB 的 4 KiB 映射区内，均在 `PT[511]` 空洞之前，
所以分页一开就能继续执行。

### 3.2 输出契约（机器可解析）

串口固定行（P2/P3/P4/P6 的断言依据）：

```
PrincessIDE paging kernel booted
[pagingkernel] CR0=... CR2=... CR3=... CR4=...
[pagingkernel] CR0.PG=1 ...
[pagingkernel] PAGING_ENABLED cr3=0x... cr0=0x...
[pagingkernel] EXCEPTION: vector=0x0e (#PF page fault)
[pagingkernel] FAULT_RIP=0x................
[pagingkernel] FAULT_ERROR=0x................
[pagingkernel] FAULT_ADDR=0x................   ← CR2
[pagingkernel] PANIC: unhandled CPU exception, halting
```

横幅 `PrincessIDE paging kernel booted` **与既有夹具的 `PrincessIDE reference kernel booted` 不同**，
两者不可能互相冒充（`templates/verify-template.sh` 已经用「foreign banner」思路防过同类问题）。

### 3.3 与既有夹具的风格一致性

沿用 `fixtures/refkernel/` 的骨架：`gcc -m64 -O0 -g` 编译 C，`.S` 也交给 gcc；同一个
`linker.ld` 段布局；同样的异常帧结构与 `isr.S` 桩；`serial.c` 直接复用该夹具实现（仅改头注释）。
差别只在：横幅、页表层数（真 4 级 + PT）、故障目标（`#PF` 而非 `#UD`）、以及新增
`paging.c` 的 CR3 走表与 GDT/IDT dump。

> **关于「既有夹具没有开分页」这一前提的核实（重要，避免验收分歧）**：
> 实际通读 `fixtures/refkernel/boot.S` 后确认，它**确实**在 32 位引导阶段用 2 MiB 大页
> 建了 1 GiB 恒等映射、并设置了 `CR4.PAE`/`EFER.LME`/`CR0.PG` 进入长模式（即分页其实是开的）。
> 但它**没有 4 级 `PT` 可供逐级走下、没有故意未映射的地址、没有 `#PF` 处理路径、也从不打印
> `CR3`**，因此 P4/P5 的「读 CR3 / 走页表 / 页表可视化 / #PF 归因」仍然无法验收。本夹具正是补上
> 这四点；`refkernel` 保持只读未改，继续承担 #UD 冒烟与符号化夹具。

---

## 4. 硬性验收 P1~P9

> 全部命令在一个 `source scripts/env.sh` 的 shell 内、`-j2` 上限下真实执行；
> 完整日志见 `.scratch/paging/verify.log`（284 行）。下面每条给出命令原文、关键输出、退出码。

### P9（先做）从零：删除全部构建产物

```
$ rm -rf fixtures/paging-kernel/build && ls fixtures/paging-kernel
boot.S grub.cfg idt.c idt.h isr.S kernel.c linker.ld Makefile paging.c paging.h
princess.toml README.md run.sh serial.c serial.h
[exit code: 0]
```

### P1 构建：带调试信息（`-g`）与符号表的 ELF

```
$ make -C fixtures/paging-kernel -j2
gcc -m64 -g -c boot.S -o build/boot.o
... (6 个 .o) ...
ld -m elf_x86_64 -T linker.ld --nostdlib --build-id=none -o build/pagingkernel.elf ...
[pagingkernel] built build/pagingkernel.elf
[exit code: 0]

$ file fixtures/paging-kernel/build/pagingkernel.elf
...: ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked,
     with debug_info, not stripped
[exit code: 0]

$ readelf -h ... | grep -E 'Class|Machine|Entry'
  Class:                             ELF64
  Machine:                           Advanced Micro Devices X86-64
  Entry point address:               0x100040
[exit code: 0]

$ readelf -S ... | grep -E '\.debug_(info|line|abbrev)'
  [ 7] .debug_line   ...   [ 8] .debug_line_str ...   [ 9] .debug_info ...   [10] .debug_abbrev ...
[exit code: 0]

$ nm fixtures/paging-kernel/build/pagingkernel.elf | grep -c .
87
[exit code: 0]
```

**P1 通过。**

### P2 无头启动：串口出现约定横幅

```
$ make -C fixtures/paging-kernel run
[pagingkernel] PASS: PrincessIDE paging kernel booted
[pagingkernel] PASS: #PF reported (paging on, fault taken)
[exit code: 0]
```

`build/serial.log` 首行：

```
PrincessIDE paging kernel booted
```

**P2 通过。**

### P3 分页确实开启

证据一（guest 自己打印，`build/serial.log`）：

```
[pagingkernel] CR0=0x0000000080000011 CR2=0x0000000000000000 CR3=0x0000000000104000 CR4=0x0000000000000020
[pagingkernel] CR0.PG=1 CR0.PE=1 CR4.PAE=1
[pagingkernel] PAGING_ENABLED cr3=0x0000000000104000 cr0=0x0000000080000011
```

* `CR0 = 0x80000011` → bit31 `PG=1`（bit0 `PE=1`，bit4 `ET=1`）。
* `CR3 = 0x104000`（非 0，指向 `boot_pml4`），`CR4 = 0x20` → `PAE=1`。
* 逐级走表结果（证明 CR3 指向的表链完整）：

```
[pagingkernel] PML4[0] = 0x0000000000105023 -> phys=0x0000000000105000 P=1 RW=1 US=0
[pagingkernel] PDPT[0] = 0x0000000000106023 -> phys=0x0000000000106000 P=1 RW=1 US=0
[pagingkernel] PD[0]   = 0x0000000000107023 -> phys=0x0000000000107000 P=1 RW=1 US=0
[pagingkernel] PD[1]   = 0x0000000000200083 -> phys=0x0000000000200000 P=1 RW=1 US=0   (2 MiB 大页)
[pagingkernel] PD[2]   = 0x0000000000000000 -> ... P=0                              (空洞)
[pagingkernel] PT[511] = 0x0000000000000000 -> ... P=0                              (空洞)
```

证据二（外部观察，`-d int` 寄存器 dump 中 `CR0`/`CR3`）：见 P4。

**P3 通过。**

### P4 `#PF` 真实发生（错误码 / CR2 / 故障 RIP + `-d int` 的 `v=0e`）

guest 串口：

```
[pagingkernel] dereferencing deliberately unmapped address 0x0000000000400000 ...
[pagingkernel] EXCEPTION: vector=0x0e (#PF page fault)
[pagingkernel] FAULT_RIP=0x00000000001011dd cs=0x0008 rflags=0x0000000000000002
[pagingkernel] FAULT_ERROR=0x0000000000000000
[pagingkernel] FAULT_ADDR=0x0000000000400000
[pagingkernel] PAGE_FAULT cause=not-present access=read ring=supervisor rsvd=0 instr_fetch=0
[pagingkernel] PAGE_FAULT cr3=0x0000000000104000 cr0=0x0000000080000011 cr4=0x0000000000000020
[pagingkernel] PANIC: unhandled CPU exception, halting
```

错误码 `0x0` 解码：P=0（not-present）、W/R=0（read）、U/S=0（supervisor）、RSVD=0、I/D=0。

QEMU 侧 `-d int,cpu_reset,guest_errors`（`bash .scratch/paging/capture-int.sh`，
日志 `.scratch/paging/qemu-int.log`）：

```
494:check_exception old: 0xffffffff new 0xe
495:     0: v=0e e=0000 i=0 cpl=0 IP=0008:00000000001011dd pc=00000000001011dd SP=0010:0000000000118fd0 CR2=0000000000400000
```

`v=0e`（向量 14 = #PF）、`CR2=0x400000`，且 `-d int` 的寄存器 dump 里 `CR0=80000011`、`CR3=0000000000104000`
（即**分页开启状态下**触发的故障，不是物理读）。

**P4 通过。**

### P5 `make` / `make iso` / `make run` 三个目标独立跑通

| 命令 | 退出码 | 关键输出 |
|---|---|---|
| `make -C fixtures/paging-kernel -j2` | 0 | `[pagingkernel] built build/pagingkernel.elf` |
| `make -C fixtures/paging-kernel -j2 iso` | 0 | `ISO image produced: 1813 sectors` / `[pagingkernel] built build/pagingkernel.iso` |
| `make -C fixtures/paging-kernel run` | 0 | `PASS: PrincessIDE paging kernel booted` + `PASS: #PF reported` |

`make symbols` 也通过：

```
0000000000100040 T _start
0000000000101036 T exception_dispatch
00000000001011c9 T paging_fault_probe
0000000000101204 T kernel_main
0000000000104000 B boot_pml4
[exit code: 0]
```

**P5 通过。**

### P6 符号化：`addr2line` 把故障 RIP 还原到 `源文件:行号`

```
$ addr2line -e fixtures/paging-kernel/build/pagingkernel.elf -f -C 0x00000000001011dd
paging_fault_probe
/root/PrincessIDE/fixtures/paging-kernel/kernel.c:122
[exit code: 0]

$ objdump -d --start-address=0x1011cd --stop-address=0x1011ed ... | tail -8
  1011d9:  48 8b 45 f8   mov  -0x8(%rbp),%rax
  1011dd:  48 8b 00      mov  (%rax),%rax     ← 故障指令（解引用 0x400000）
  1011e0:  48 89 45 f0   mov  %rax,-0x10(%rbp)
```

RIP 落在**稳定命名的 `paging_fault_probe()`** 内、正是那条未映射读，证明夹具对 P4 的
「RIP + 源文件:行号」链路可用。

**P6 通过。**

### P7 `regenerate.sh` 一条命令重建全部样本

```
$ bash fixtures/qemu-monitor/regenerate.sh
[qemu-monitor] step 1/4: build .../pagingkernel.iso
[qemu-monitor] step 2/4: boot headless, QMP on unix socket
[qemu-monitor] step 3/4: capture HMP + QMP samples
[qmp] info-registers: 34 line(s)
[qmp] info-mem: 3 line(s)
[qmp] info-tlb: 1021 line(s)
[qmp] info-cpus: 1 line(s)
[qmp] query-version: saved | query-status: saved | query-cpus-fast: saved | query-memory-size-summary: saved
[qemu-monitor] step 4/4: record provenance
[qemu-monitor] OK: samples regenerated in fixtures/qemu-monitor
[exit code: 0]
```

脚本自 `source scripts/env.sh`、以自身位置定位仓库根、无交互；重复执行会覆盖为同一形态的样本
（`thread_id`、时间戳按设计变化，见 README 局限）。

**P7 通过。**

### P8 样本非退化（反映真实映射）

`info mem`（3 行，正好是内存映射表，含两处空洞）：

```
0000000000000000-00000000001ff000 00000000001ff000 -rw     ← 4 KiB 页，止于 0x1ff000 空洞
0000000000200000-0000000000400000 0000000000200000 -rw     ← 2 MiB 大页
0000000000600000-0000000040000000 000000003fa00000 -rw     ← 大页（0x400000..0x600000 空洞）
```

`info tlb`（1021 行）第一行与关键行：

```
0000000000000000: 0000000000000000 --------W
0000000000100000: 0000000000100000 ----A---W     ← 内核所在 4 KiB 页，A=1
0000000000200000: 0000000000200000 --P-----W     ← 2 MiB 大页
0000000000600000: 0000000000600000 --P-----W
```

`info registers` 关键行（证明抓取时 paging 已开）：

```
CR0=80000011 CR2=0000000000400000 CR3=0000000000104000 CR4=00000020
EFER=0000000000000500
```

`query-version.json` 记录版本 `7.2.22`，`query-memory-size-summary` 为 `268435456`（256 MiB）。
**info mem / info tlb 都非退化**，且 `info mem` 的区间边界精确对应夹具空洞。

**P8 通过。**

### P9 从零重跑

验收脚本的第一步就是 `rm -rf fixtures/paging-kernel/build`，随后在同一轮里依次重跑
P1（`make`）→ P5（`make iso` / `make run`）→ P6（`addr2line`）→ P4b（`-d int`）→
P7/P8（`regenerate.sh`）→ princess.toml 契约核对，全部退出码 0。完整日志
`.scratch/paging/verify.log`。

**P9 通过。**

---

## 5. `princess.toml` 全文与契约 §4 字段核对

`fixtures/paging-kernel/princess.toml`：

```toml
schema = 1

[project]
name = "paging-kernel"
language = "c"
arch = "x86_64"

[build]
backend = "make"
command = "make"
cwd = "."
targets = ["all"]
artifacts = ["build/pagingkernel.elf"]
compile_commands = "compile_commands.json"

[run]
backend = "qemu"
kernel = "build/pagingkernel.elf"
boot = "multiboot2"
args = ["-m", "256M", "-cdrom", "build/pagingkernel.iso", "-boot", "d", "-display", "none", "-serial", "stdio", "-monitor", "none", "-no-reboot"]
timeout_ms = 30000
serial = { device = "com1", tee_to_file = "build/serial.log" }

[debug]
backend = "gdb"
symbols = "build/pagingkernel.elf"
stub = { host = "127.0.0.1", port = 1234, mode = "launch" }

[toolchain]
cc = "gcc"
as = "gcc"
ld = "ld"
gdb = "gdb"

[ai]
provider = "openai-compatible"
base_url = ""
model = ""
```

核对脚本从 `docs/spec/10-contracts.md` §4 的 ```toml 块解析字段名与类型，与本文件逐项比对
（与 `templates/verify-template.sh` 的做法一致）：

```
ok schema / project.{name,language,arch} / build.{backend,command,cwd,targets,artifacts,compile_commands}
ok run.{backend,kernel,boot,args,timeout_ms,serial.device,serial.tee_to_file}
ok debug.{backend,symbols,stub.{host,port,mode}} / toolchain.{cc,as,ld,gdb} / ai.{provider,base_url,model}
missing [] unknown [] mismatch []
[exit code: 0]
```

**契约 §4 通过（38 个字段路径全部存在、0 未知、0 类型不符）。**
`crates/princess-core` 的 `ProjectConfig` 用 `#[serde(deny_unknown_fields)]`，字段名严格来自 §4。

> 说明（可能与直觉不符、故显式记录）：`[toolchain].as` 填的是 `gcc` 而不是契约示例里的 `nasm`。
> 本夹具（与 `refkernel` 相同的风格）把 `.S` 交给 gcc 汇编，全程不使用 nasm；契约冻结的是
> **字段名**，示例值是可替换的实现选择。若全项目要求 `as` 必须是汇编器名，可在主 Agent 裁决后
> 改为 `gcc` 保持（它就是实际的汇编器驱动）。

---

## 6. monitor 样本的抓取方法与局限

### 6.1 方法（D9：QMP unix socket）

`fixtures/qemu-monitor/regenerate.sh` 做的事：

1. `source scripts/env.sh`，`make -C fixtures/paging-kernel -j2 iso`；
2. 启动 QEMU：`-cdrom ISO -boot d -display none -serial stdio -monitor none
   -qmp unix:<tmp>/qmp.sock,server=on,wait=off -no-reboot`（串口仍是 stdio，重定向到文件；
   monitor 只走 QMP，不抢 stdio，也不使用 `-nographic`）；
3. **轮询串口**直到同时出现 `PAGING_ENABLED` 与 `PANIC`。此刻 guest 已开分页、取完
   `#PF`、在 `cli; hlt` 中停住且 CR3 仍加载 —— 状态稳定，适合作为确定性抓取点；
4. `qmp_capture.py` 连上 QMP，发 `qmp_capabilities`，对每条 HMP 命令发
   `human-monitor-command`，把 `return` 原文写入 `info-*.txt`，并保存 4 条结构化查询；
5. 落盘 `capture-serial.log`、`capture-metadata.txt`、`qemu-version.txt`。

QEMU 版本固定记录在 `qemu-version.txt`：`QEMU emulator version 7.2.22
(Debian 1:7.2+dfsg-7+deb12u18+b3)`。

### 6.2 重要勘误：`info tlb` 第 3 个标志位不是「present」

研究 B §4.2 把 `info tlb` 的标志位推断为 `present/accessed/writable`（标了 [推测]）。
本夹具的样本给出了**反例**：所有 4 KiB 叶 PTE 都是 present+RW，却打印 `--------W`；
只有 2 MiB 大页叶（PSE=1）打印 `--P-----W`。因此 9 个标志列应为：

| 列 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
|---|---|---|---|---|---|---|---|---|---|
| 含义 | NX | G | **PSE(页大小)** | D | A | PCD | PWT | US | RW |

本夹具 `info-tlb.txt` 的分布佐证：`--------W` ×499（未访问的 4 KiB 页）、
`----A---W` ×7、`---DA---W` ×5（已访问/已写）、`--P-----W` ×510（恰好是
`PD[1]` + `PD[3..511]` 共 510 个大页）。**把第 3 列当 present 的解析器会把所有 4 KiB 页判错。**

### 6.3 局限

* 只覆盖 **一个** QEMU 版本；`query-version` 与样本同存，供消费方检测漂移。
* 抓取时 guest 处于 `hlt` 停机态（为了确定性）；运行中的 guest 可能因访问更多页而变化。
  `query-status` 仍返回 `running: true`（它描述 VM 而非 vCPU；看 vCPU 是否停机请看
  `info registers` 的 `HLT=1`）。
* `info cpus` 的 `thread_id` / `query-cpus-fast` 的 `thread-id` 是宿主线程号，
  **每次运行都不同**，只能断言格式，不能当 golden 值。
* `info tlb` 在本版 QEMU（7.2）**不接受地址参数**（`info tlb 0x100000` 报
  `tlb: extraneous characters at the end of line`），输出可能很大（本夹具 1021 行），
  解析器需流式处理。

---

## 7. 已知问题与设计取舍

1. **抓取时机**：monitor 样本是在 guest 故意 `#PF` 停机**之后**抓的，而不是「开分页但尚未故障」
   的瞬间。原因：serial 轮询粒度远粗于 guest 执行速度，无法稳定停在故障前；停机后 CR3/页表
   完全不变，样本仍是分页开启后的真实状态，且更确定。README 已如实标注。若 P5 需要「运行中」
   样本，可后续给夹具加一个 QMP 可唤醒的等待点，但那会让夹具不再是无依赖的自动停机内核。
2. **两个空洞**：除故障目标 `0x400000`（PD[2]，2 MiB）外，还留了 `0x1FF000`（PT[511]，4 KiB）。
   后者让 `info mem` 的第一段以 `0x1ff000` 结束，能同时验证「PT 级空洞」与「PD 级空洞」。
3. **GDT accessed 位**：串口 dump 的 `GDT[2]/GDT[4]` 是 `0x00cf93...` 而非源码里的 `0x00cf92...`，
   这是 CPU 加载段选择子时置了 accessed 位，属正常运行时修改；`info registers` 的段属性同样
   显示 `00cf9300`。
4. **`-d int` 日志噪音**：`-d int,cpu_reset,guest_errors` 会打印大量 BIOS/GRUB 阶段的
   `check_exception` 与 `Servicing hardware INT`，`v=0e` 在文件末尾。解析时应用
   `v=0e` + `CR2` 而不是「文件里有没有 check_exception」。
5. **权限位**：`run.sh` / `regenerate.sh` / `qmp_capture.py` 已 `chmod 755`。
6. **未做**：P4 的 GDB/DAP 联调（属 P4 本体范围）；本夹具只保证提供 `CR3`、页表、GDT/IDT
   与可符号化的故障，作为 P4/P5 的验收底座。

---

## 8. 从零重跑步骤

```bash
cd /root/PrincessIDE
source scripts/env.sh

# 0) 从零：删掉构建产物
rm -rf fixtures/paging-kernel/build

# 1) 构建 ELF（-g + 符号表）
make -C fixtures/paging-kernel -j2
file fixtures/paging-kernel/build/pagingkernel.elf           # with debug_info, not stripped

# 2) 生成 GRUB Multiboot2 ISO
make -C fixtures/paging-kernel -j2 iso

# 3) 无头启动，断言横幅 + 真实 #PF
make -C fixtures/paging-kernel run
cat fixtures/paging-kernel/build/serial.log                  # CR0.PG=1 / CR3 / FAULT_RIP / FAULT_ADDR

# 4) 符号化故障 RIP
RIP=$(grep -oE 'FAULT_RIP=0x[0-9a-f]+' fixtures/paging-kernel/build/serial.log | head -1 | cut -d= -f2)
addr2line -e fixtures/paging-kernel/build/pagingkernel.elf -f -C "$RIP"

# 5) 外部证明 #PF（-d int 出现 v=0e）
bash .scratch/paging/capture-int.sh
grep -nE 'check_exception|v=0e' .scratch/paging/qemu-int.log | tail

# 6) 重建 monitor 样本
bash fixtures/qemu-monitor/regenerate.sh
cat fixtures/qemu-monitor/info-mem.txt
wc -l fixtures/qemu-monitor/info-tlb.txt

# 7) 一键复跑全部验收（P1~P9）
bash .scratch/paging/verify.sh 2>&1 | tee .scratch/paging/verify.log
```

---

## 9. 结论

* P1~P9 **全部实测通过**，每一项都有真实命令输出与退出码（`.scratch/paging/verify.log`）。
* 最关键的前置条件 —— **分页确实开启（`CR0.PG=1`、`CR3=0x104000`）** —— 有 guest 串口、
  逐级走表、以及 QEMU `-d int` 三处独立证据；`#PF` 是分页开启后的真实页故障
  （`v=0e`、`CR2=0x400000`），不是物理读。
* `fixtures/qemu-monitor/` 的样本由真实 QEMU 在分页开启后录制，`info mem` / `info tlb`
  非退化，并纠正了 B 报告对 `info tlb` 标志列的一处推测（第 3 列是 PSE 而非 present）。
