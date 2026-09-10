# 研究主题 B：QEMU 内核运行编排 + panic/异常符号化链路

> 本报告所有「已实测」结论均来自本环境（Debian 12 / x86_64 / 无头）真实执行命令的输出。
> 证据产物位于 `/root/PrincessIDE/_work/` 与 `/root/PrincessIDE/_toolchain/`。
> 三档标注约定：**[实测]** 本环境跑通；**[权威未实测]** 有可引用来源但本环境未验证；**[推测]** 经验推断。

---

## 0. 实测环境（本报告的证据底座）

| 项 | 值 | 来源 |
|---|---|---|
| QEMU | 7.2.22 (Debian 1:7.2+dfsg-7+deb12u18+b3) | `qemu-system-x86_64 --version` [实测] |
| GDB | 13.1-3 | `gdb --version` [实测] |
| NASM | 2.16.01 | `nasm -v` [实测] |
| GRUB | grub-mkrescue 2.06-13+deb12u1 | [实测] |
| xorriso | 1.5.4 | [实测] |
| gcc/ld/objdump/readelf/nm/addr2line | 系统自带 | [实测] |
| 固件 | seabios 1.16.2、OVMF_CODE_4M.fd / OVMF_VARS_4M.fd | 解包自 apt 包 [实测] |

**安装方式（关键，供 IDE 落地复刻）**：本环境禁止 `apt-get install`，全部工具用
`apt-get download <pkg> && dpkg-deb -x <pkg>.deb <prefix>` 解包到工作区内前缀
`/root/PrincessIDE/_toolchain/prefix`，运行时用 `LD_LIBRARY_PATH` + `PATH` 指向前缀。
环境变量封装见 `/root/PrincessIDE/_toolchain/env.sh`（`source` 后即可直接用
`qemu-system-x86_64` / `gdb` / `nasm` / `xorriso` / `grub-mkrescue`）。

最小 Multiboot 内核（NASM，6 个变体，用于制造各故障）在
`/root/PrincessIDE/_work/kernel/kernel.asm`；Multiboot2 内核在 `multiboot2.asm`；
链接脚本 `linker.ld`（`ENTRY(_start)`，`. = 0x100000`）。

---

## 1. QEMU 启动参数矩阵（面向内核、无头）

### 1.1 Multiboot1：`-kernel`（最简路径）——[实测]

```bash
qemu-system-x86_64 \
  -kernel kernel.bin \          # 含 multiboot1 头(magic 0x1BADB002)，ELF 入口 0x100000
  -m 64M -smp 4 \
  -display none -serial stdio -monitor none \
  -no-reboot -no-shutdown \
  -d int,cpu_reset,guest_errors -D qemu.log
```

实测输出（串口）：`[PrincessIDE] boot ok`。

要点：
- QEMU 7.2 的 `-kernel` 对 x86 支持：multiboot1 内核 / Linux bzImage / PVH ELF。
  **multiboot1 头必须在文件前 8192 字节内**（QEMU 在首 8KB 扫描 `0x1BADB002`）。
  本环境第一次构建失败原因即此：`-Ttext 0x100000` 且 `.multiboot` 单独成节时，magic
  落在文件偏移 8220（>8192），QEMU 报 `Error loading uncompressed kernel without PVH ELF Note`。
  用链接脚本把 `.multiboot` 放到 `.text` 最前、magic 落在偏移 4096 后即成功。[实测]
- `-kernel` 启动后 guest 状态：`EAX=0x2BADB002`（multiboot magic）、`EBX=multiboot info 指针`、
  32 位保护模式、分页关闭、中断关闭（`-d int` 的寄存器 dump 中 `EAX=2badb002` 可证）。[实测]
- QEMU 7.2 的 `-kernel` **不支持 multiboot2**，multiboot2 必须走 GRUB（见 1.2/1.3）。

### 1.2 Multiboot2 + GRUB：`-cdrom -boot d`（BIOS/SeaBIOS 路径）——[实测]

构建 ISO（grub-mkrescue 已在系统，xorriso/mtools 解包到前缀后 `source env.sh`）：

```bash
mkdir -p iso/boot/grub
cp kernel2.bin iso/boot/           # multiboot2 内核(magic 0xE85250D6)
cat > iso/boot/grub/grub.cfg <<'EOF'
set timeout=0
serial --unit=0 --speed=115200
terminal_input serial
terminal_output serial
menuentry "MB2" { multiboot2 /boot/kernel2.bin; boot }
EOF
grub-mkrescue -o kernel_mb2.iso iso/
```

启动：

```bash
qemu-system-x86_64 -cdrom kernel_mb2.iso -boot d \
  -m 64M -display none -serial stdio -monitor none -no-reboot
```

实测串口输出：`Booting 'MB2'` → `[PrincessIDE] multiboot2 kernel booted via GRUB`。[实测]
（multiboot1 内核用 `multiboot` 命令替代 `multiboot2`，同样 `-cdrom -boot d` 跑通。[实测]）

### 1.3 UEFI：OVMF + pflash ——[实测]

```bash
# 首次把 VARS 固件复制为可写副本（OVMF 会写 NVRAM）
cp OVMF_VARS_4M.fd ovmf_vars.fd
qemu-system-x86_64 \
  -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=ovmf_vars.fd \
  -drive file=esp.img,format=raw \     # FAT 分区，内含 \EFI\BOOT\BOOTX64.EFI
  -m 128M -display none -serial stdio -monitor none -no-reboot
```

实测：OVMF 固件启动到 BDS 阶段，串口可见设备枚举：
`BdsDxe: loading Boot0002 "UEFI QEMU HARDDISK QM00001 " ... starting Boot0002 ...`。[实测]

**UEFI 全链路（OVMF → GRUB → multiboot2 → 内核）已在本环境端到端跑通**，方法是用
`grub-mkstandalone` 生成自包含 GRUB EFI 应用放到 ESP：

```bash
grub-mkstandalone -O x86_64-efi \
  -d "$PREFIX/usr/lib/grub/x86_64-efi" \
  -o BOOTX64.EFI \
  "boot/grub/grub.cfg=grub_uefi.cfg" "boot/kernel2.bin=kernel/mb2.bin"
# 用 mtools 放入 ESP: mcopy -i esp.img BOOTX64.EFI ::/EFI/BOOT/BOOTX64.EFI
```

实测串口输出（完整链）：
```
BdsDxe: starting Boot0002 "UEFI QEMU HARDDISK QM00001 " ...
Welcome to GRUB!
  Booting `MB2'
WARNING: no console will be available to OS
error: no suitable video mode found.
[PrincessIDE] multiboot2 kernel booted via GRUB
```
`error: no suitable video mode found` 是 `-display none` 下 GRUB 视频探测失败的告警，但因
`terminal_output serial` 仍能正常继续引导，可忽略。[实测]

**UEFI 注意事项（实测/权威）**：
- 必须 `readonly=on` 挂 CODE、可写挂 VARS，否则 OVMF 写 NVRAM 会失败；每次测试前重置
  `ovmf_vars.fd` 可避免旧 BootOrder 干扰。
- 手写 gnu-efi 应用：`gcc -fshort-wchar -mno-red-zone ... + ld -T elf_x86_64_efi.lds
  + objcopy --target=efi-app-x86_64` 可产出合法 `PE32+ EFI application`（`file` 验证通过），
  但在 OVMF 下执行未见预期串口输出（`starting Boot0002` 后 RIP 回到固件区），**判定为
  gnu-efi crt0/reloc 细节未解，端到端执行标「未验证」**。IDE 若走 UEFI，推荐直接用
  GRUB EFI（已验证），不要自己造 EFI 应用轮子。[实测+推测]

### 1.4 关键参数的实际作用与日志格式（均实测）

| 参数 | 作用（实测） |
|---|---|
| `-m 64M` / `-m 128M` | guest 内存；`query-memory-size-summary` 返回 `base-memory: 67108864`(64M) [实测] |
| `-smp 4` | `query-cpus-fast` 返回 4 个 CPU（cpu-index 0..3，各带 thread-id）[实测] |
| `-no-reboot` | guest 复位（含 triple fault）时 **QEMU 直接退出**而非重启（退出码 0）[实测] |
| `-no-shutdown` | guest *关机*（poweroff）时 QEMU 暂停而非退出；**不影响 triple fault**（三重故障是 reset，归 `-no-reboot` 管）[实测] |
| `-d int` | 输出异常/中断短格式 `check_exception` 行 + 完整寄存器 dump [实测] |
| `-d cpu_reset` | 输出 `CPU Reset (CPU 0)` 寄存器 dump + **`Triple fault` 文本行**（见 §3）[实测] |
| `-d guest_errors` | 输出 guest 非法访问，如 `Invalid read at addr 0xDEADBEEC, size 4, region '(null)', reason: rejected` [实测] |
| `-D <file>` | `-d` 日志写入文件（默认 stderr）；与 `-serial` 分流的关键手段 [实测] |

`-d help` 本版本完整可用项（`qemu-system-x86_64 -d help`，节选）[实测]：
`out_asm in_asm op op_opt op_ind int exec cpu fpu mmu pcall cpu_reset unimp
guest_errors page nochain plugin strace tid trace:PATTERN`。
说明文字（原文）：`int` = "show interrupts/exceptions in short format"；
`cpu_reset` = "show CPU state before CPU resets"；
`guest_errors` = "log when the guest OS does something invalid"。

---

## 2. 串口捕获的可靠性（均实测）

### 2.1 三种 `-serial` 后端取舍

| 后端 | 行为（实测） | 适用 |
|---|---|---|
| `-serial stdio` | guest 串口→当前进程 stdout | 交互调试、`tee` 落盘（见 2.2） |
| `-serial file:F` | 串口字节→文件 F（无实时流） | 纯落盘，事后 `tail -f F` 补流 |
| `-serial unix:sock,server=on,wait=off` | 串口→unix socket；**无客户端连接时早期输出丢失**（本环境实测：内核 t=0 的 boot 消息在 t=2s 连接后读不到） | 需要进程间消费时 |

`-serial unix:` 的 `wait=on` 会让 QEMU **阻塞直到有客户端连接**（避免丢早期输出，但会卡启动）。
本环境 `nc` 为传统版不支持 `-U`，读取 unix socket 用 python3：
`socket.socket(AF_UNIX) → connect(path) → recv()`。[实测]

### 2.2 推荐方案：「既落盘又实时流式」——[实测]

最简单可靠（串口与调试日志、QEMU 报错三路彻底分离）：

```bash
qemu-system-x86_64 -kernel kernel.bin -m 64M -display none \
  -serial stdio -monitor none \
  -d int,cpu_reset,guest_errors -D qemu_dbg.log \
  > serial_tee.log 2> qemu_stderr.log | tee serial_console.log
```

- 串口：stdout → `tee` 同时进控制台和 `serial_console.log`；
- `-D`：异常/复位/三击日志独立进 `qemu_dbg.log`；
- `2>`：QEMU 自身报错（如 "Triple fault"、firmware 告警）独立进 `qemu_stderr.log`。

若 IDE 用管道消费串口（非 `tee`），改用 socket 后端 + 一个常驻 reader 进程（读 socket 的同时
`tee` 落盘），可避免 stdio 与 IDE 自身的 stdin 冲突。

### 2.3 经典陷阱（实测复现）

1. **stdio 被多个字符设备抢占**：`-serial stdio -monitor stdio` 直接报错
   `cannot use stdio by multiple character devices`。[实测]
2. **`-nographic` 不等于干净的串口**：`-nographic` 会把 serial + monitor **多路复用**到 stdio
   （Ctrl-a c 切 monitor），且 BIOS/VGA 文本（SeaBIOS 版本、iPXE banner）会混入串口流。
   本环境实测 `-nographic` 出现 `SeaBIOS (version 1.16.2...)`、`iPXE ... Press Ctrl-B` 噪音；
   而 `-display none -serial stdio -monitor none` 只有纯净 guest 串口。**结论：内核串口捕获用
   `-display none -serial stdio`，不要用 `-nographic`**。[实测]
3. **管道无读者阻塞**：`-serial file:pipe` 指向 FIFO 时，QEMU 阻塞在 `open()` 等读者
   （本环境 `mkfifo` 后无读者，QEMU 停在 running 状态不执行 guest）。[实测]
   同理，`qemu ... -serial stdio | head -5` 会在 `head` 提前退出后使 QEMU 收到 SIGPIPE/阻塞。

---

## 3. 异常与三击故障检测（均实测，真实日志）

用同一内核 6 个变体制造故障，`-d int,cpu_reset,guest_errors -D dlog.txt` 抓取。

### 3.1 `-d int` 日志格式（真实样本）

```
check_exception old: 0xffffffff new 0xe
     0: v=0e e=0000 i=0 cpl=0 IP=0008:000000000010002e pc=000000000010002e SP=0010:0000000000103000 CR2=0000000040000000
EAX=... EBX=... ... EIP=0010002e EFL=... CR0=... CR2=... CR3=...
```

字段语义（结合 6 个变体交叉验证）：
- `check_exception old: 0xN new 0xM`：`old` 为已挂起未投递的异常（`0xffffffff`=无），`new` 为新异常向量。
- `NNN: v=XX e=YYYY i=Z cpl=Z IP=SS:RRRR... pc=... SP=...`：`v=`异常号，`e=`错误码
  （`#PF` 才有意义，`#GP` 为选择子+IDT 位），`IP=选择子:64位RIP`（**符号化取 offset 部分，忽略选择子**），
  `#PF` 时附带 `CR2=故障数据地址`，其它异常附带 `env->regs[R_EAX]=...`。
- 其后紧跟完整 CPU 寄存器 dump（含 `EIP/EFL/CR0/CR2/CR3/GDT/IDT`）。

异常向量实测对照：`v=00` #DE（除零）、`v=06` #UD（ud2）、`v=0d` #GP、`v=08` #DF、
`v=0e` #PF。[实测]

### 3.2 各故障的真实判定信号

**#UD（ud2，未装 IDT）** [实测]：
```
check_exception old: 0xffffffff new 0x6
     0: v=06 e=0000 i=0 cpl=0 IP=0008:0000000000100029 ... env->regs[R_EAX]=000000002badb002
```
（随后因无 IDT 处理器：`new 0xd` → `v=0d e=0032`（IDT 取门失败，#GP，错误码 0x32 =
(6<<3)|2，即 vector6 的 IDT 选择子+IDT位）→ `new 0xd`/`v=08`（#DF）→ `Triple fault`。）[实测]

**#DE（除零）** [实测]：`check_exception old: 0xffffffff new 0x0` → `v=00 e=0000 ...`。

**#PF（开分页后读 0x40000000）** [实测]：
```
check_exception old: 0xffffffff new 0xe
     0: v=0e e=0000 i=0 cpl=0 IP=0008:000000000010002e ... CR2=0000000040000000
```
CR2=0x40000000 即故障数据地址，与指令 RIP=0x10002e 分离——**符号化代码位置用 RIP，数据位置用 CR2**。

**关键反例（极重要）**：#PF 只在 `CR0.PG=1`（分页开启）时才可能。本环境变体 2 在分页关闭下读
`[0xDEADBEEF]`，**不产生 #PF**（`-d int` 无 check_exception），只在 `-d guest_errors` 里出现：
`Invalid read at addr 0xDEADBEEC, size 4, region '(null)', reason: rejected`（一条 4 字节读因未对齐
拆成 0xDEADBEEC/0xDEADBEF0 两条）。[实测]

### 3.3 triple fault 判定——[实测]

**从 `-d` 日志**：triple fault 文本行 **只由 `-d cpu_reset` 产生，`-d int` 不产生**。实测对照
（同一 triple fault 内核，分别 `-d int` / `-d cpu_reset` / `-d guest_errors`）：

| `-d` 值 | `check_exception` 行数 | `Triple fault` 行数 |
|---|---|---|
| `int` | 4 | 0 |
| `cpu_reset` | 0 | 1 |
| `guest_errors` | 0 | 0 |

所以**可靠的三击检测必须同时开 `-d int`（看异常级联）与 `-d cpu_reset`（看 `Triple fault` 行）**。
`-d int` 的末两行是 `check_exception old: 0x8 new 0xd`（#DF 投递期间再异常 → 触发三击）——这是
`-d int` 侧的强信号，但若只有 `-d int` 拿不到那句 `Triple fault` 文本确认。[实测]

**从 QEMU 退出行为**（`-no-reboot` 时）：
- 默认（无 `-no-reboot`）：三击 → 复位重启 → 死循环（`timeout` 才能杀掉，退出码 124）。[实测]
- `-no-reboot`：三击 → QEMU 退出，退出码 **0**。[实测]
- 仅 `-no-shutdown`（无 `-no-reboot`）：三击仍复位重启，QEMU 存活（`query-status`=running）。[实测]

**误报/漏报**：
- 退出码 **不能**区分三击与普通复位（`-no-reboot` 下任何 reset 都退出码 0），必须结合日志文本。[实测推断]
- `-no-reboot` 还会在**正常 reboot**（如 guest 主动 `sys_reset`/键盘 Ctrl-Alt-Del）时退出——IDE
  不能把「QEMU 退出」直接当成 panic，需以 `Triple fault` 文本或异常级联为准。[权威未实测]

---

## 4. QEMU monitor 的机器可解析接口（均实测）

### 4.1 三条独立通道（分离是关键）

```bash
qemu-system-x86_64 -kernel kernel.bin -m 64M -display none \
  -serial file:serial.txt \
  -monitor unix:mon.sock,server=on,wait=off \   # HMP 人类可读控制台
  -qmp     unix:qmp.sock,server=on,wait=off \   # QMP 机器接口（JSON）
  -no-reboot
```

### 4.2 HMP 文本输出真实样本

`info registers`（QMP `human-monitor-command` 的 `return` 字段原文）[实测]：
```
CPU#0
EAX=2badb002 EBX=00009500 ECX=00100010 EDX=00000511
ESI=00100069 EDI=00001000 EBP=00000000 ESP=00103000
EIP=00100021 EFL=00000046 [---Z-P-] CPL=0 II=0 A20=1 SMM=0 HLT=1
CS =0008 00000000 ffffffff 00cf9a00 DPL=0 CS32 [-R-]
...
CR0=00000011 CR2=00000000 CR3=00000000 CR4=00000000
```
`info cpus`：`* CPU #0: thread_id=213860`。[实测]
`info mem`（分页关）：`PG disabled`；分页开后：`0000000000000000-0000000000800000 0000000000800000 -rw`。[实测]
`info tlb`（分页开后）[实测]：
```
0000000000000000: 0000000000000000 --P-A---W
0000000000400000: 0000000000400000 --P-----W
```
格式 `vaddr: paddr <10字符标志>`；本版本 `info tlb` **不带地址参数**（`info tlb 0x100000` 报
`tlb: extraneous characters at the end of line`），旧教程里的 `info tlb <addr>` 语法不适用 7.2。[实测]
标志位字母语义 P/A/W = present/accessed/writable 是**根据两行差异推断**（页 0 有代码/栈故 A=1，
4MB 页未被访问故 A=0），其余位未逐一确认。[推测]

### 4.3 比解析文本更稳的方式：QMP 结构化命令 ——[实测，推荐]

QMP 握手（unix socket，QEMU 7.2.22 实测报文）：
```
→ {"execute":"qmp_capabilities"}          ← {"return": {}}
→ {"execute":"query-version"}             ← {"return": {"qemu": {"major":7,"minor":2,"micro":22},...}}
→ {"execute":"query-cpus-fast"}           ← {"return": [{"thread-id":...,"cpu-index":0,"target":"x86_64",...}]}
→ {"execute":"query-memory-size-summary"} ← {"return": {"base-memory":67108864,"plugged-memory":0}}
```
Greeting 首行（真实）：`{"QMP": {"version": {"qemu": {...}}, "capabilities": ["oob"]}}`。

- 结构化命令（`query-cpus-fast`、`query-status`、`query-registers`(需 7.x?)、
  `query-memory-size-summary`）返回 JSON，**远比 `info registers` 文本好解析**。[实测]
- 兜底 `human-monitor-command`：`{"execute":"human-monitor-command","arguments":{"command-line":"info registers"}}`
  把任意 HMP 命令的文本塞进 `return` 字段（见 4.2 样本）。[实测]
- `query-registers` 在本 7.2.22 是否可用未单独验证（本报告寄存器用 `info registers`/`-d` 获取）。[未验证]

**IDE 建议**：优先 QMP 结构化命令；仅当需要 `info tlb`/`info mem` 这类无结构化替代的项时才
`human-monitor-command` + 正则解析（格式稳定，见上）。

---

## 5. 符号化链路：故障 RIP → `源文件:行号`（均实测）

以 #PF 变体（`-g -F dwarf` 重编出 `kernel_5_dbg.bin`）的故障 RIP `0x10002e` 为例。

### 5.1 三种工具对比

```bash
# 1) addr2line —— 单地址/批量最快
addr2line -e kernel_5_dbg.bin -f -C 0x10002e
#   → _start
#   → /root/PrincessIDE/_work/kernel/kernel.asm:44

# 2) objdump —— 反汇编+行号交织（人工核对指令级位置）
objdump -d -l kernel_5_dbg.bin | grep -A3 10002e
#   → 10002e: a1 00 00 00 40  mov 0x40000000,%eax   ← 上一行 /kernel.asm:44

# 3) gdb batch —— 带符号偏移 + 行范围
gdb -batch -ex 'info line *0x10002e' kernel_5_dbg.bin
#   → Line 44 of "kernel.asm" starts at address 0x10002e <_start+30> and ends at 0x100033 <halt>.
```

三者结论一致：`0x10002e → kernel.asm:44`（正是 `mov eax,[0x40000000]` 那行）。[实测]

### 5.2 内核场景的特殊问题

1. **链接地址 vs 加载地址**：本环境 `-Ttext 0x100000` 链接地址=加载地址，RIP 直接可用。
   高半核（如链接 0xFFFFFFFF80000000、物理加载 0x100000）时，**RIP 是虚拟地址，恰好等于 ELF
   的 VMA**，addr2line 无需换算；但若用 `objcopy -O binary` 输出裸二进制再被 bootloader 加载到
   非链接地址，则必须 `RIP - 实际加载基址 + ELF 链接基址` 换算后再符号化。[权威未实测，OSDev 共识]
2. **重定位**：`ET_EXEC`（绝对链接）无重定位问题；`ET_DYN`/`-pie` 内核需按运行时基址换算。
   x86_64 内核若走 KASLR 需从引导参数/符号表取实际基址。[权威未实测]
3. **`-g` 与优化等级**：本环境汇编 `-g -F dwarf` 产出 `.debug_line/.debug_info/.symtab`（readelf
   验证），addr2line 即可工作。C 侧实测（`-O2 -g`）：
   内联函数 `add()` 被折叠进 `compute()`（反汇编只剩 `lea 0x3(%rdi,%rdi,1),%eax; ret`，无独立
   `add` 符号），addr2line 把地址归属到**内联函数自身所在行（line 3）而非调用点（line 4）**。
   即：**-O2 下「崩溃行号」可能落在被内联的函数体内，而非调用处**；需 `addr2line -i`（内联链）
   或同时看反汇编核对。本环境 `-i` 仅回一层（该例 `static inline` 被完全折叠），内联链不可盲信。[实测]
4. **符号表来自 ELF、运行在裸机**：这是边界但**不是障碍**——故障 RIP 与 ELF 符号/VMA 是同一
   地址空间，只要「链接地址 = 实际运行地址」就一一对应。真正的坑是：①RIP 是 64 位而内核链接为
   32 位（§5.3 架构错配）；②内核加载后自行重映射（分页）导致虚拟≠物理时，QEMU `-d int` 的 IP
   是**虚拟** RIP，仍应拿 ELF VMA 符号化（正确），但 `info tlb` 看到的 paddr 不能拿来做源码符号化。[实测推断]
5. **`-d int` 的 IP 字段解析**：`IP=0008:000000000010002e`，取 `:` 后 16 位十六进制（`0x10002e`），
   忽略段选择子 `0008`。[实测]

### 5.3 GDB 远程（gdbstub）与架构错配——[实测，IDE 图形调试必踩]

```bash
qemu-system-x86_64 -kernel kernel_5_dbg.bin -m 64M -display none -s -S ...   # -s:gdb stub :1234, -S:暂停等连接
gdb -batch -ex 'target remote :1234' -ex 'break serial_putc' -ex 'continue' -ex 'bt' kernel_5_dbg.bin
```

**坑**：32 位 ELF 内核跑在 `qemu-system-x86_64` 上时，gdb 报
`Selected architecture i386 is not compatible with reported target architecture i386:x86-64`
+ `Remote 'g' packet reply is too long`。**修复**：连接前 `set architecture i386:x86-64`。
即便这样，`bt` 只有 `#0 serial_putc () at kernel.asm:92` 正确，`#1/#2` 是乱地址（32 位栈被按
64 位解析）。[实测]

**更干净的做法**：用 32 位机 `qemu-system-i386`（本环境该包是 wrapper，硬编码 `/usr/libexec/...`
绝对路径，直接调用 `$PREFIX/usr/libexec/qemu-system-i386` 才可用），无需 `set architecture`，
`bt` 干净。[实测]

**回溯需要帧信息**：本环境汇编内核未写 `.cfi_*` 也未 `push ebp; mov ebp,esp`，所以 gdb `bt`
只有 `#0`。**IDE 要拿到多帧回溯，内核必须：C 代码用 `-fno-omit-frame-pointer` 编译，或汇编用
DWARF CFI 指令**（否则只能靠「地址→addr2line」的文本回溯，见 §6）。[实测]

---

## 6. 内核自己打印的栈回溯文本：IDE 如何解析

三种常见格式与解析策略（`0x…` 十六进制地址是共同核心）：

1. **`0x地址` 裸地址列表**（最常见，如 Rust panic 的 `0x...` 帧、自研 `bt` 打印）：
   正则 `0x[0-9a-fA-F]+`（或 `[0-9a-f]{8,16}`）提取，**去重、按 ELF 基址换算**（§5.2.1），
   再**批量**喂 addr2line：`addr2line -e kernel -f -C -i addr1 addr2 addr3 ...`（stdin 逐行）。
   addr2line 批量模式输出格式：每个地址两行（`函数名` / `文件:行号`），可直接按 `addr → (file:line)`
   建表。[实测（单地址），批量格式权威未实测]
2. **`#N 0xaddr in func at file:line`（GDB bt 风格）**：本报告 §5.3 已产出真实样本
   `#0  serial_putc () at kernel.asm:92`。解析：若文本已含 `at file:line` 直接取用；否则提取
   `0xaddr` 走 addr2line。注意 gdb 帧里 `func+offset`（如 `<_start+30>`）表明是函数内偏移，
   可反推符号。[实测]
3. **`func+0xoffset/0xsize`（Linux oops/Rust backtrace）**：`RIP: 0010:ffffffff81001234` 或
   `kernel::foo+0x1a/0x40`。解析：`0x` 后地址交给 addr2line；`func+0x1a` 需先在 ELF 里
   `nm`/`readelf -s` 找 `func` 起始地址 + offset 再符号化。[权威未实测]

**解析策略（给 IDE）**：统一归一化为「十六进制地址」→ 一次性 addr2line 批量解析 → 回填源码位置；
对带 `+offset` 的符号先做符号基址+偏移换算。地址需先按 §5.2.1 做链接/加载基址归一。[推测（基于实测工具行为）]

---

## 7. 坑与反例（会浪费一整天的）

1. **`-kernel` 报 `Error loading uncompressed kernel without PVH ELF Note`**：不是内核坏了，是
   multiboot 头没落在文件前 8192 字节（`-Ttext` + 单独 `.multiboot` 节时常见）。用链接脚本把
   `.multiboot` 放 `.text` 最前即可。本环境亲历，偏移 8220 → 4096 后才通。[实测]
2. **`-d int` 里根本没有 `Triple fault` 文本**：那句是 `-d cpu_reset` 打的。只开 `-d int` 会
   漏判三击（只有 `check_exception old: 0x8 new 0xd` 这个隐晦前兆）。同理，`-no-reboot` 退出码
   是 0，不能当「发生了 panic」的凭据。[实测]
3. **用 `-nographic` 抓串口**：BIOS/iPXE 噪音 + monitor 多路复用（Ctrl-a c）全混进流里，而且
   `-serial stdio -monitor stdio` 直接报 `cannot use stdio by multiple character devices`。
   正确姿势是 `-display none -serial stdio -monitor none`（或 monitor 走 unix socket）。[实测]
4. **32 位内核 + `qemu-system-x86_64` 的 gdbstub**：`Remote 'g' packet reply is too long` +
   回溯乱码。要么 `set architecture i386:x86-64`（只救寄存器/断点，回溯仍乱），要么用
   `qemu-system-i386`（本环境 wrapper 有 `/usr/libexec` 硬编码 bug，直接调 libexec 二进制）。[实测]
5. **`-serial file:` 指向 FIFO 且无读者**：QEMU 阻塞在 open，guest 根本不跑，看起来像「内核没
   输出」。[实测]
6. **分页没开就断言 #PF**：CR0.PG=0 时读任意「非法」地址只是物理读，不产生页故障（本环境变体 2
   实测：无 check_exception，只有 guest_errors 的 `Invalid read`）。panic 检测逻辑必须先确认
   PG=1 再谈 #PF。[实测]

---

## 8. 附录：可直接抄用的命令汇总

```bash
# —— 环境 ——
source /root/PrincessIDE/_toolchain/env.sh    # 注入 qemu/gdb/nasm/xorriso 前缀

# —— 1. Multiboot1 内核启动（推荐基座）——
qemu-system-x86_64 -kernel kernel.bin -m 64M -smp 4 \
  -display none -serial stdio -monitor none \
  -no-reboot -no-shutdown \
  -d int,cpu_reset,guest_errors -D qemu_dbg.log \
  2> qemu_stderr.log | tee serial.log

# —— 2. 符号化故障 RIP ——
addr2line -e kernel_dbg.bin -f -C -i <rip>        # 单地址
addr2line -e kernel_dbg.bin -f -C -i addr1 addr2  # 批量
gdb -batch -ex 'info line *0x<rip>' kernel_dbg.bin

# —— 3. QMP 结构化查询（机器接口）——
# python3: connect(unix socket) → 读 greeting → {"execute":"qmp_capabilities"}
#   → {"execute":"query-cpus-fast"} / {"execute":"query-status"} / {"execute":"query-memory-size-summary"}
#   兜底: {"execute":"human-monitor-command","arguments":{"command-line":"info registers"}}

# —— 4. GRUB Multiboot2 ISO ——
grub-mkrescue -o kernel.iso iso/            # iso/boot/grub/grub.cfg + iso/boot/kernel2.bin
qemu-system-x86_64 -cdrom kernel.iso -boot d -m 64M -display none -serial stdio -monitor none -no-reboot

# —— 5. UEFI(OVMF) + GRUB ——
grub-mkstandalone -O x86_64-efi -d "$PREFIX/usr/lib/grub/x86_64-efi" \
  -o BOOTX64.EFI "boot/grub/grub.cfg=grub_uefi.cfg" "boot/kernel2.bin=kernel2.bin"
# mformat/mcopy 写入 esp.img 的 \EFI\BOOT\BOOTX64.EFI 后：
qemu-system-x86_64 \
  -drive if=pflash,format=raw,readonly=on,file=OVMF_CODE_4M.fd \
  -drive if=pflash,format=raw,file=ovmf_vars.fd \
  -drive file=esp.img,format=raw -m 128M -display none -serial stdio -monitor none -no-reboot
```

### 引用
- QEMU 调用参数：https://www.qemu.org/docs/master/system/invocation.html （`-kernel`/`-cdrom`/`-boot`/`-d`/`-serial`/`-monitor`/`-no-reboot`/`-no-shutdown`/`-s`）
- QEMU monitor（HMP）：https://www.qemu.org/docs/master/system/monitor.html
- QMP 参考：https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html
- Multiboot1/2 规范：https://www.gnu.org/software/grub/manual/multiboot/ 与 https://www.gnu.org/software/grub/manual/multiboot2/
- OSDev（Bare Bones / 高半核 / 链接脚本）：https://wiki.osdev.org/Bare_Bones
- addr2line/binutils：https://sourceware.org/binutils/docs/binutils/addr2line.html
- GDB：https://sourceware.org/gdb/current/onlinedocs/gdb/

> 说明：`-d help` 的 flag 列表、QMP 报文、各故障日志、`info registers/mem/tlb/cpus` 输出均为本环境
> QEMU 7.2.22 / GDB 13.1 实测原文；OVMF 固件启动到 BDS 及 OVMF→GRUB→multiboot2 全链路已实测；
> 手写 gnu-efi 应用的执行、`addr2line -i` 内联链、QMP `query-registers` 标记为未验证/受限。
