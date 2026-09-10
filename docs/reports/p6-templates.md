# P6 预研报告 —— 内核工程模板

| 项 | 值 |
| --- | --- |
| 阶段 | P6（模板与工具链向导）预研 |
| 任务 | 交付可用的 x86_64 + Multiboot2 内核工程模板 + 自检脚本 |
| 工作区 | `/root/PrincessIDE` |
| 日期 | 2026-09-11 |
| 本报告作者 | P6 实现 Agent（只写 `templates/`、`.scratch/templates/`、本报告） |
| 依据 | `docs/spec/00-decisions.md`（D2/D3/D9/D15/D16）、`docs/spec/10-contracts.md` §4、`docs/spec/20-acceptance.md` P6-1/P6-2、`docs/p0-toolchain-report.md`、`docs/research/B-qemu-panic-symbolization.md` §1.2/§2/§7 |
| 所有声明 | 均由真实命令输出支撑；输出为原样粘贴，未跑过的内容不会写成「通过」 |

---

## 0. 结论

模板**实测可用**：`templates/x86_64-multiboot2/` 能独立编译出 ELF64、生成 GRUB Multiboot2 ISO，
并在无头 QEMU（TCG，无 KVM）中启动，通过 COM1 打印自己的横幅：

```
PrincessIDE template kernel booted
```

该横幅与夹具横幅 `PrincessIDE reference kernel booted` 不同，两者不会互相冒充。模板**不依赖
`fixtures/` 的任何文件**（构建期零引用，见 §6.6）。

硬性验收 T1~T5 **全部实测通过**（详见 §3）。

---

## 1. 交付物清单

| 路径 | 说明 |
| --- | --- |
| `templates/x86_64-multiboot2/` | 可用的内核工程模板 |
| `templates/x86_64-multiboot2/boot.s` | Multiboot2 头 + 32 位入口 + GDT + 页表 + 进 long mode（GNU as 语法） |
| `templates/x86_64-multiboot2/kernel.c` | `kernel_main()`：横幅与诊断输出，随后停机 |
| `templates/x86_64-multiboot2/serial.c` / `serial.h` | COM1 轮询串口（无 libc） |
| `templates/x86_64-multiboot2/linker.ld` | ELF64 @ 1 MiB，`R E` + `RW` 两段 + 64 KiB 栈 |
| `templates/x86_64-multiboot2/grub.cfg` | GRUB `multiboot2` 菜单项 |
| `templates/x86_64-multiboot2/Makefile` | `make` / `make iso` / `make run` / `make clean` / `help` |
| `templates/x86_64-multiboot2/run.sh` | `make run` 使用的无头启动 + 横幅断言脚本 |
| `templates/x86_64-multiboot2/princess.toml` | 工程描述（§4 字段名逐字） |
| `templates/x86_64-multiboot2/README.md` | 用法、依赖、设计约束、限制 |
| `templates/x86_64-multiboot2/.gitignore` | 忽略模板自身的 `build/` 产物 |
| `templates/verify-template.sh` | 模板自检脚本（复制 → 构建 → 无头启动 → 断言横幅 → 校验 §4 字段） |
| `docs/reports/p6-templates.md` | 本报告 |

---

## 2. 模板目录树

```
templates/
├── verify-template.sh                  (755)
└── x86_64-multiboot2/
    ├── .gitignore
    ├── Makefile
    ├── README.md
    ├── boot.s
    ├── grub.cfg
    ├── kernel.c
    ├── linker.ld
    ├── princess.toml
    ├── run.sh                          (755)
    ├── serial.c
    └── serial.h
```

> `build/`、`build/kernel.elf`、`build/kernel.iso`、`build/serial.log` 均为构建期生成物，已由
> 模板内 `.gitignore` 忽略；交付的模板目录**不含**任何生成物（实测 `find templates -type d
> -name build` 结果为 0）。

---

## 3. 验收执行记录

所有命令均在本工作区真实执行；`$` 后的命令为原文，紧随其后的代码块为真实输出片段。

### 3.1 T1 + T2 —— 自检脚本退出 0，串口出现模板横幅

命令原文：

```bash
$ cd /root/PrincessIDE
$ bash templates/verify-template.sh
```

真实输出（完整，退出码 0）：

```
[verify] template : /root/PrincessIDE/templates/x86_64-multiboot2
[verify] workdir  : /root/PrincessIDE/.scratch/templates/verify.IsVcvs
[verify] step 1/5: copy template -> /root/PrincessIDE/.scratch/templates/verify.IsVcvs/x86_64-multiboot2
[verify] step 2/5: make -j2   (compile + link)
[verify] step 3/5: make -j2 iso   (GRUB Multiboot2 ISO)
[verify] step 4/5: headless QEMU boot (timeout 45s)
[verify] step 5/5: princess.toml fields vs contracts §4
[verify]   ok         ai
[verify]   ok         ai.base_url
[verify]   ok         ai.model
[verify]   ok         ai.provider
[verify]   ok         build
[verify]   ok         build.artifacts
[verify]   ok         build.backend
[verify]   ok         build.command
[verify]   ok         build.compile_commands
[verify]   ok         build.cwd
[verify]   ok         build.targets
[verify]   ok         debug
[verify]   ok         debug.backend
[verify]   ok         debug.stub
[verify]   ok         debug.stub.host
[verify]   ok         debug.stub.mode
[verify]   ok         debug.stub.port
[verify]   ok         debug.symbols
[verify]   ok         project
[verify]   ok         project.arch
[verify]   ok         project.language
[verify]   ok         project.name
[verify]   ok         run
[verify]   ok         run.args
[verify]   ok         run.backend
[verify]   ok         run.boot
[verify]   ok         run.kernel
[verify]   ok         run.serial
[verify]   ok         run.serial.device
[verify]   ok         run.serial.tee_to_file
[verify]   ok         run.timeout_ms
[verify]   ok         schema
[verify]   ok         toolchain
[verify]   ok         toolchain.as
[verify]   ok         toolchain.cc
[verify]   ok         toolchain.gdb
[verify]   ok         toolchain.ld
[verify]   all 37 contract field paths present, 0 unknown
[verify] PASS
[verify]   banner    : PrincessIDE template kernel booted
[verify]   serial log: /root/PrincessIDE/.scratch/templates/verify.IsVcvs/x86_64-multiboot2/build/serial.log
```

退出码实测：

```
$ bash templates/verify-template.sh >/dev/null 2>&1; echo "verify exit=$?"
verify exit=0
```

被断言的真实串口日志（`make run`，同样内容由 `verify-template.sh` 的 QEMU 启动产生）：

```
PrincessIDE template kernel booted
[template] x86_64 long mode reached via Multiboot2
[template] handoff magic=0x36d76289 info=0x00116590 (multiboot2)
[template] image magic=0x07e3f00dbeef1234
[template] COM1 polled serial ready; halting.
```

> `magic=0x36d76289` 证明引导协议确为 Multiboot2；`image magic` 回读证明 RW 段已加载。

### 3.2 T3 —— `princess.toml` 字段名与契约 §4 逐字一致

自动核对命令（内嵌于 `verify-template.sh` 第 5 步）：直接用 Python `tomllib` 解析
`docs/spec/10-contracts.md` §4 的 ```toml 代码块，与生成物 `princess.toml` 做**递归键路径 + 值类型**
比对。真实输出见 §3.1 的 37 行 `ok`，结论行：

```
[verify]   all 37 contract field paths present, 0 unknown
```

逐项核对表（左：§4 冻结 schema 字段；右：模板实际字段）：

| §4 冻结字段路径 | 模板 | 类型 |
| --- | --- | --- |
| `schema` | `schema = 1` | int |
| `project.name` | `name = "x86_64-multiboot2"` | str |
| `project.language` | `language = "c"` | str |
| `project.arch` | `arch = "x86_64"` | str |
| `build.backend` | `backend = "make"` | str |
| `build.command` | `command = "make"` | str |
| `build.cwd` | `cwd = "."` | str |
| `build.targets` | `targets = ["all"]` | list |
| `build.artifacts` | `artifacts = ["build/kernel.elf"]` | list |
| `build.compile_commands` | `compile_commands = "compile_commands.json"` | str |
| `run.backend` | `backend = "qemu"` | str |
| `run.kernel` | `kernel = "build/kernel.elf"` | str |
| `run.boot` | `boot = "multiboot2"` | str |
| `run.args` | `args = [ ... ]`（见 §4） | list |
| `run.timeout_ms` | `timeout_ms = 30000` | int |
| `run.serial` | `serial = { ... }` | dict |
| `run.serial.device` | `device = "com1"` | str |
| `run.serial.tee_to_file` | `tee_to_file = "build/serial.log"` | str |
| `debug.backend` | `backend = "gdb"` | str |
| `debug.symbols` | `symbols = "build/kernel.elf"` | str |
| `debug.stub` | `stub = { ... }` | dict |
| `debug.stub.host` | `host = "127.0.0.1"` | str |
| `debug.stub.port` | `port = 1234` | int |
| `debug.stub.mode` | `mode = "launch"` | str |
| `toolchain.cc` | `cc = "gcc"` | str |
| `toolchain.as` | `as = "as"` | str |
| `toolchain.ld` | `ld = "ld"` | str |
| `toolchain.gdb` | `gdb = "gdb"` | str |
| `ai.provider` | `provider = "openai-compatible"` | str |
| `ai.base_url` | `base_url = ""` | str |
| `ai.model` | `model = ""` | str |

结论：**字段名逐字一致，无缺失、无自创、无类型不符**。字段的*取值*按模板实际情况填写
（见 §6 的设计决定）。

### 3.3 T4 —— `make` / `make iso` / `make run` 三个目标独立跑通

为严格证明「独立」，每个目标都从**删除 `build/` 后的干净副本**开始。

命令原文与真实输出：

```bash
$ cp -a templates/x86_64-multiboot2 .scratch/templates/t4
$ make -C .scratch/templates/t4 -j2            # (a) 只构建
$ rm -rf .scratch/templates/t4/build
$ make -C .scratch/templates/t4 -j2 iso        # (b) 只产 ISO（内部会先构建 ELF）
$ rm -rf .scratch/templates/t4/build
$ make -C .scratch/templates/t4 -j2 run BOOT_TIMEOUT=45   # (c) 只运行
```

(a) `make`：退出码 `0`

```
as --64 -g -o build/boot.o boot.s
gcc -m64 -std=gnu11 -O0 -g ... -c serial.c -o build/serial.o
gcc -m64 -std=gnu11 -O0 -g ... -c kernel.c -o build/kernel.o
ld -m elf_x86_64 -T linker.ld --nostdlib --build-id=none -o build/kernel.elf build/boot.o build/serial.o build/kernel.o
[template] built build/kernel.elf
```

产物：`build/kernel.elf: ELF 64-bit LSB executable, x86-64, ... with debug_info, not stripped`
（`readelf -hW`：`Class: ELF64`，`Machine: Advanced Micro Devices X86-64`，`Entry point address: 0x100030`）。

(b) `make iso`：退出码 `0`

```
grub-mkrescue --compress=xz -o build/kernel.iso build/iso
...
ISO image produced: 1806 sectors
Written to medium : 1806 sectors at LBA 0
Writing to 'stdio:build/kernel.iso' completed successfully.
[template] built build/kernel.iso
```

产物：`build/kernel.iso`（3,698,688 字节）。

(c) `make run`：退出码 `0`

```
BOOT_TIMEOUT=45 MEMORY=256M BANNER="PrincessIDE template kernel booted" bash run.sh
[template] PASS: PrincessIDE template kernel booted
[template] serial log: /root/PrincessIDE/.scratch/templates/t4/build/serial.log
```

### 3.4 T5 —— 从零重跑仍退出 0

命令原文与真实输出：

```bash
$ rm -rf .scratch/templates/verify.* .scratch/templates/t4 .scratch/templates/t5-neg .scratch/templates/t4-*.out
$ find templates -type d -name build | wc -l
0
$ bash templates/verify-template.sh
[verify] step 1/5: copy template -> /root/PrincessIDE/.scratch/templates/verify.1Dt3Zz/x86_64-multiboot2
[verify] step 2/5: make -j2   (compile + link)
[verify] step 3/5: make -j2 iso   (GRUB Multiboot2 ISO)
[verify] step 4/5: headless QEMU boot (timeout 45s)
[verify] step 5/5: princess.toml fields vs contracts §4
[verify]   all 37 contract field paths present, 0 unknown
[verify] PASS
[verify]   banner    : PrincessIDE template kernel booted
[verify]   serial log: /root/PrincessIDE/.scratch/templates/verify.1Dt3Zz/x86_64-multiboot2/build/serial.log
$ echo "T5 verify exit=$?"
T5 verify exit=0
```

`verify-template.sh` 每次都把模板复制到全新 `mktemp -d` 目录再构建，因此天然是「从零重跑」；
本节另外显式删除了此前所有生成物，结果仍为退出 0。

**cwd 无关性**（脚本自定位仓库根并自行 `source scripts/env.sh`）：从 `/` 用干净 shell 调用同样通过：

```bash
$ cd / && bash /root/PrincessIDE/templates/verify-template.sh
[verify] template : /root/PrincessIDE/templates/x86_64-multiboot2
[verify] step 1/5: copy template -> /root/PrincessIDE/.scratch/templates/verify.ycwiDm/x86_64-multiboot2
...
[verify] PASS
[verify]   banner    : PrincessIDE template kernel booted
$ echo $?
0
```

### 3.5 负样本 —— 断言不是恒真（D14）

把副本 `kernel.c` 的横幅改成 `PrincessIDE WRONG banner`，重新构建后 `make run` 必须失败：

```bash
$ sed -i 's/PrincessIDE template kernel booted/PrincessIDE WRONG banner/' .scratch/templates/t5-neg/kernel.c
$ make -C .scratch/templates/t5-neg -j2 iso   # 退出 0
$ make -C .scratch/templates/t5-neg run BOOT_TIMEOUT=20
[template] FAIL: banner not found in serial output
PrincessIDE WRONG banner
[template] x86_64 long mode reached via Multiboot2
[template] handoff magic=0x36d76289 info=0x00116590 (multiboot2)
[template] image magic=0x07e3f00dbeef1234
[template] COM1 polled serial ready; halting.
make: *** [Makefile:81: run] Error 1
```

退出码实测：`make run` 为 `2`（make 对配方失败的传播），`run.sh` 直接调用为 `1`：

```
$ (cd .scratch/templates/t5-neg && BOOT_TIMEOUT=20 bash run.sh >/dev/null 2>&1); echo "run.sh direct exit=$?"
run.sh direct exit=1
```

`verify-template.sh` 还额外断言**夹具横幅不得出现**在模板串口输出中（防止冒充）：

```bash
grep -qF "PrincessIDE template kernel booted" "$LOG" || fail ...
if grep -qF "PrincessIDE reference kernel booted" "$LOG"; then fail ...; fi
```

---

## 4. `princess.toml` 全文

```toml
# PrincessIDE project descriptor — x86_64 + Multiboot2 + GRUB ISO template.
#
# The field names below follow docs/spec/10-contracts.md §4 verbatim; the engine
# rejects unknown keys (fail loud), so do not invent new ones.  Relative paths
# are resolved against the project root.

schema = 1

[project]
name = "x86_64-multiboot2"
language = "c"
arch = "x86_64"

[build]
backend = "make"
command = "make"
cwd = "."
targets = ["all"]
artifacts = ["build/kernel.elf"]
compile_commands = "compile_commands.json"

[run]
backend = "qemu"
kernel = "build/kernel.elf"
boot = "multiboot2"
# D9 serial contract: -display none -serial stdio -monitor none.
# D2 boot path: GRUB ISO via -cdrom + -boot d (never `-kernel`).
args = ["-m", "256M", "-cdrom", "build/kernel.iso", "-boot", "d", "-display", "none", "-serial", "stdio", "-monitor", "none", "-no-reboot"]
timeout_ms = 30000
serial = { device = "com1", tee_to_file = "build/serial.log" }

[debug]
backend = "gdb"
symbols = "build/kernel.elf"
stub = { host = "127.0.0.1", port = 1234, mode = "launch" }

[toolchain]
cc = "gcc"
as = "as"
ld = "ld"
gdb = "gdb"

[ai]
provider = "openai-compatible"
base_url = ""
model = ""
```

---

## 5. 自检脚本 `templates/verify-template.sh`

行为（无交互、可重复、自行定位仓库根并 `source scripts/env.sh`，不依赖调用者 cwd/PATH）：

1. 把 `templates/x86_64-multiboot2/` 复制到 `$ROOT/.scratch/templates/verify.<mktemp>`；
2. `make -j2`（编译 + 链接）；
3. `make -j2 iso`（GRUB Multiboot2 ISO）；
4. 无头启动 QEMU（`-cdrom -boot d -display none -serial stdio -monitor none -no-reboot`），
   轮询串口日志直到出现横幅或超时，然后结束 QEMU；
5. 用 Python `tomllib` 从 `docs/spec/10-contracts.md` §4 提取冻结 schema，与生成物
   `princess.toml` 递归比对字段路径与值类型。

退出码：全部通过 `0`；任一步失败**非 0**并打印串口日志与 QEMU stderr。`--keep` 可保留临时目录。
环境变量 `PRINCESSIDE_TEMPLATE_BOOT_TIMEOUT`（默认 45 秒）、`PRINCESSIDE_TEMPLATE_MEMORY`
（默认 256M）可覆盖。

> 第 5 步需要 `python3` + `tomllib`（本机 3.11.2 具备，实测执行）；若宿主缺少，脚本会打印
> `SKIP princess.toml schema check` 并**仍由第 1~4 步的构建/启动/横幅断言决定成败**，
> 使 T1/T2 不依赖 Python。

---

## 6. 设计决定与可能被质疑的点

### 6.1 `[toolchain]` 取值用宿主 `gcc`/`as`/`ld`，而非契约示例的 `x86_64-elf-*` / `nasm`
§4 冻结的是**字段名**，示例值只是示例。本模板的 `Makefile` 实际调用 `gcc`（C）、`as --64`
（GNU 汇编，`boot.s`）与 `ld`（`-m elf_x86_64`），因此 `[toolchain]` 如实写
`cc="gcc"` / `as="as"` / `ld="ld"` / `gdb="gdb"`。**没有**使用 nasm，避免声明与构建不一致
（若写 `as="nasm"` 而 Makefile 实际用 GNU as，反而违和）。若主 Agent 要求模板演示 nasm 路径，
需重写 `boot.s` 为 NASM 语法并回归，属于变更请求。

### 6.2 `[run].args` 里含 `-cdrom` / `-boot d`
契约示例的 `args` 未含 CD 引导参数。这是刻意的：D2 规定引导路径是 GRUB ISO，模板的
`args` 必须能直接表达完整可运行命令（`-cdrom build/kernel.iso -boot d`），否则 `[run]` 对
本模板不可直接执行。`kernel` 字段仍指向 ELF、`boot="multiboot2"`，语义不冲突。

### 6.3 `-L $PRINCESSIDE_QEMU_DATA` 未写进 `args`
QEMU 的固件/option ROM 数据目录是本环境特有的绝对路径（`.toolchain/prefix/usr/share/qemu`），
由 `scripts/env.sh` 注入。写死进工程配置会破坏可移植性，因此由 `run.sh` 在运行时从环境取，
并在缺失时给出明确报错。引擎侧未来应同样从工具链探测结果注入 `-L`。

### 6.4 `compile_commands = "compile_commands.json"` 已声明但模板不生成
该键是 §4 冻结 schema 的一部分，故照抄。`compile_commands.json` 的生成归 `crates/princess-build`
（D8/D17，P2-B/B1）；P6 模板不越权生成，README 已注明。

### 6.5 `timeout_ms = 30000`
夹具冒烟用 25s；模板同样因 TCG 无 KVM 较慢。实测本模板从 QEMU 起到横幅约 1 秒，30s 余量充足。

### 6.6 与夹具的隔离
模板源码、Makefile、链接脚本、GRUB 配置**零引用** `fixtures/`、`_work/`、`.research*`、`_toolchain/`。
全仓扫描只在 `README.md` 出现一次 `fixtures/refkernel/` 字样，且是**说明横幅为何不同**的散文，
不参与构建。横幅字符串互相独立（`template` vs `reference`）。

### 6.7 模板附带 `run.sh`
`make run` 需要一个「启动→等横幅→杀 QEMU→按退出码表达成败」的驱动器，写在 Makefile 配方里
（轮询 + `$$` 转义）反而更脆。故放一个 40 行 `run.sh`，保持 Makefile 可读。它不改变
`make`/`make iso` 的行为。

### 6.8 权限位
交付文件 644、脚本 755（首次以 `umask 007` 创建时曾出现 600/711，已显式 `chmod` 修正）。

---

## 7. 已知限制

1. **KVM 不可用**：QEMU 走 TCG 软件模拟（`docs/p0-toolchain-report.md` §7.3），启动比硬件虚拟化慢；
   本次实测约 1.3 秒即出横幅，已给足超时。
2. **`grub-mkrescue` 依赖宿主包**：来自宿主 `grub-pc-bin` + `grub2-common`（P0 遗留缺口 §7.2）。
   缺少它时 `make iso` / `verify-template.sh` 会显式失败（脚本有前置检查），不会静默降级。
3. **`make` / `make iso` / `make run` 依赖已 `source scripts/env.sh`**：提供 `qemu-system-x86_64`、
   `xorriso`/`mtools`、`$PRINCESSIDE_QEMU_DATA`。Makefile 头部与 README 均已注明。
4. **模板刻意极简**：无中断、无 APIC、无分页管理、无分配器、单核、`-O0 -g`。只保证「能引导并
   输出横幅」，这是 P6-1/P6-2 的范围。
5. **负样本覆盖**：本轮验证了「横幅缺失 → 失败」与「夹具横幅不得出现」。未覆盖构建失败/超时，
   那些属于 P2 的 run/build 归因验收（P2-6/P2-7），不在 P6 模板自检范围。
6. **串口与 QEMU stderr 分离**：`run.sh`/`verify-template.sh` 把 COM1 写 `build/serial.log`、
   QEMU 自身消息写 `build/qemu.stderr.log`，避免污染串口流（研究 B §2.2）。

---

## 8. 从零复现步骤

```bash
cd /root/PrincessIDE
source scripts/env.sh                 # 每个新 shell 都要 source

# 交付物自检（T1/T2/T3/T5）：复制 → make → make iso → 无头启动 → 断言横幅 → 校验 §4 字段
bash templates/verify-template.sh

# 单独跑三个目标（T4）
cp -a templates/x86_64-multiboot2 /tmp/mykernel && cd /tmp/mykernel
make -j2          # build/kernel.elf
make -j2 iso      # build/kernel.iso
make run          # 无头启动并断言横幅；串口日志 build/serial.log
```
