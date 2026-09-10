# PrincessIDE P0 — 工具链与端到端测试夹具报告

| 项目 | 值 |
| --- | --- |
| 阶段 | P0（环境与测试夹具） |
| 宿主 | Debian GNU/Linux 12 (bookworm), x86_64, 4 核, 3.8 GiB RAM, 根分区余 29 GiB |
| 背景 | 无显示器（`DISPLAY=none`），全部验证**无头**完成；不使用任何 GUI |
| 工作区 | `/root/PrincessIDE` |
| 本文档作者 | P0 执行 Agent（只负责 `.toolchain/`、`scripts/`、`fixtures/refkernel/`、本文档、`.gitignore`） |
| 所有声明 | 均由真实命令输出支撑，输出为原样粘贴；未验证的内容在「遗留缺口」中显式标注 |

---

## 1. 摘要

P0 的两项目标均已达成并**实测跑通**：

1. **自包含工具链**：Rust 1.98.1 工具链 + 10 个通过 `.deb` 解包获得的外部工具，全部落在
   `/root/PrincessIDE/.toolchain/` 内，**没有安装任何系统软件包**，**没有修改 `/etc/ld.so.conf`**，
   **没有在工作区外写入任何文件**。
2. **参考内核夹具 + 端到端验收闭环**：`fixtures/refkernel/` 产出带调试信息与符号表的 x86_64 ELF；
   `scripts/smoke-boot.sh` 无头启动 QEMU、断言横幅与异常与故障 RIP；
   `scripts/symbolicate.sh` 把故障 RIP 映射回 `fixtures/refkernel/kernel.c:100`。

三条硬证据（原样）：

```
PrincessIDE reference kernel booted
```

```
[refkernel] EXCEPTION: vector=0x06 (#UD invalid opcode)
[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 rflags=0x0000000000000046 error=0x0000000000000000
```

```
[symbolicate] PASS: 0x0000000000100b3d -> /root/PrincessIDE/fixtures/refkernel/kernel.c:100 (refkernel_fault_probe)
```

---

## 2. 交付物清单

| 路径 | 说明 |
| --- | --- |
| `scripts/bootstrap-toolchain.sh` | 幂等工具链安装脚本（rustup + .deb 解包） |
| `scripts/env.sh` | 可 `source` 的环境导出脚本（`PATH`/`LD_LIBRARY_PATH`/`RUSTUP_HOME`/`CARGO_HOME`） |
| `scripts/doctor.sh` | 打印每个工具的版本与绝对路径，缺任一必需工具则非零退出 |
| `scripts/smoke-boot.sh` | 无头 QEMU 冒烟测试：横幅 + 异常 + 故障 RIP 断言 |
| `scripts/symbolicate.sh` | `addr2line` / `objdump` 把故障 RIP 映射回 `源文件:行号` |
| `fixtures/refkernel/` | 参考内核夹具（C + 汇编，无第三方依赖） |
| `fixtures/refkernel/path-a-probe/` | 引导路径 A 的**证伪实验**（见 §4） |
| `docs/p0-toolchain-report.md` | 本文档 |
| `.gitignore` | 忽略 `.toolchain/`、`target/`、`*.o`、`*.elf`、`*.iso`、`*.img`、`*.deb` 等 |
| `.toolchain/` | 工具链本体（**不入库**） |

参考内核夹具源码：

| 文件 | 作用 |
| --- | --- |
| `boot.S` | Multiboot 1 + Multiboot 2 头、32 位入口、身份映射页表、进入 long mode |
| `isr.S` | 异常向量 0..31 的入口桩 + `isr_stub_table` |
| `idt.c` / `idt.h` | 构造并加载 IDT；定义 `struct exception_frame` |
| `serial.c` / `serial.h` | COM1 (0x3F8) 轮询串口 + 极简 `printf` |
| `kernel.c` | 横幅、`refkernel_fault_probe()`、`exception_dispatch()` panic 路径 |
| `linker.ld` | 单一 1 MiB 基址布局，`R E` 与 `RW` 两个 PT_LOAD |
| `Makefile` | `make` / `make iso` / `make symbols` / `make check` / `make clean` |
| `grub.cfg` | GRUB `multiboot2` 菜单项 |

---

## 3. 工具链清单（`scripts/doctor.sh` 真实输出，原样粘贴）

命令（在干净 shell 中执行，脚本自行定位并 source `env.sh`）：

```
$ cd / && bash /root/PrincessIDE/scripts/doctor.sh
```

输出：

```
PrincessIDE toolchain doctor
workspace : /root/PrincessIDE
prefix    : /root/PrincessIDE/.toolchain/prefix
RUSTUP_HOME=/root/PrincessIDE/.toolchain/rustup
CARGO_HOME =/root/PrincessIDE/.toolchain/cargo
-------------------------------------------------------------------------------
TOOL                   STATUS     VERSION / PATH
-------------------------------------------------------------------------------
cargo                  ok         cargo 1.98.1 (797e8a9bc 2026-08-05)
                                  /root/PrincessIDE/.toolchain/cargo/bin/cargo
rustc                  ok         rustc 1.98.1 (48a229cea 2026-09-01)
                                  /root/PrincessIDE/.toolchain/cargo/bin/rustc
rustfmt                ok         rustfmt 1.9.0-stable (48a229ceae 2026-09-01)
                                  /root/PrincessIDE/.toolchain/cargo/bin/rustfmt
clippy                 ok         clippy 0.1.98 (48a229ceae 2026-09-01)
                                  /root/PrincessIDE/.toolchain/cargo/bin/cargo-clippy
qemu-system-x86_64     ok         QEMU emulator version 7.2.22 (Debian 1:7.2+dfsg-7+deb12u18+b3)
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/qemu-system-x86_64
nasm                   ok         NASM version 2.16.01
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/nasm
clang                  ok         Debian clang version 14.0.6
                                  /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-14/bin/clang
clangd                 ok         Debian clangd version 14.0.6
                                  /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-14/bin/clangd
ld.lld                 ok         Debian LLD 14.0.6 (compatible with GNU linkers)
                                  /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-14/bin/ld.lld
gdb                    ok         GNU gdb (Debian 13.1-3) 13.1
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/gdb
xorriso                ok         xorriso 1.5.4 : RockRidge filesystem manipulator, libburnia project.
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/xorriso
mtools                 ok         mformat (GNU mtools) 4.0.32
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/mformat
grub-mkrescue          ok         grub-mkrescue (GRUB) 2.06-13+deb12u1
                                  /usr/bin/grub-mkrescue
gcc                    ok         gcc (Debian 12.2.0-14+deb12u1) 12.2.0
                                  /usr/bin/gcc
ld                     ok         GNU ld (GNU Binutils for Debian) 2.40
                                  /usr/bin/ld
objdump                ok         GNU objdump (GNU Binutils for Debian) 2.40
                                  /usr/bin/objdump
addr2line              ok         GNU addr2line (GNU Binutils for Debian) 2.40
                                  /usr/bin/addr2line
readelf                ok         GNU readelf (GNU Binutils for Debian) 2.40
                                  /usr/bin/readelf
make                   ok         GNU Make 4.3
                                  /usr/bin/make
cmake                  ok         cmake version 3.25.1
                                  /usr/bin/cmake
git                    ok         git version 2.39.5
                                  /usr/bin/git
node                   ok         v24.20.0
                                  /root/node-v24.20.0-linux-x64/bin/node
npm                    ok         11.19.0
                                  /root/node-v24.20.0-linux-x64/bin/npm
pnpm                   ok         12.3.4
                                  /root/node-v24.20.0-linux-x64/bin/pnpm
-------------------------------------------------------------------------------
doctor: all required tools present (24 resolved).
```

退出码（实测）：

```
$ bash scripts/doctor.sh >/dev/null 2>&1; echo "doctor exit=$?"
doctor exit=0
```

缺失检测的**负向实测**（把 `scripts/` 复制到一个没有 `.toolchain/` 的根目录下运行）：

```
doctor: 12 tool(s) present, MISSING REQUIRED: cargo rustc rustfmt clippy qemu-system-x86_64 nasm clang clangd ld.lld gdb xorriso mtools
doctor: run  bash /root/PrincessIDE/.tmp-negtest/scripts/bootstrap-toolchain.sh  to install them.
exit=1
```

### 3.1 安装方式与关键决策

| 工具 | 来源 | 决策理由 |
| --- | --- | --- |
| cargo / rustc / rustfmt / clippy | rustup，stable，minimal profile | `RUSTUP_HOME`/`CARGO_HOME` 均在工作区内；`--no-modify-path` 避免写 `~/.profile`（工作区外，沙箱会拒绝） |
| qemu-system-x86_64 + qemu-system-data + seabios + ipxe-qemu | `.deb` 解包 | 缺失；`-L` 指向工作区内 data 目录 |
| nasm | `.deb` 解包 | 缺失 |
| clang / clangd / lld（LLVM 14） | `.deb` 解包 | 缺失；放在 `usr/lib/llvm-14/bin` 使 clang 的相对 resource-dir 查找（`../lib/clang/14.0.0`）落在工作区内 |
| gdb | `.deb` 解包 | 缺失 |
| xorriso | `.deb` 解包 | 缺失；`grub-mkrescue` 生成 ISO 时需要 |
| mtools（`mformat` 等） | `.deb` 解包 | 缺失；`grub-mkrescue` 需要 |
| grub-mkrescue | **宿主已装**（未由 bootstrap 安装） | 见 §7 遗留缺口 |

依赖闭包抓取方式（脚本内实现）：`apt-cache depends --recurse --no-recommends --no-suggests
--no-conflicts --no-breaks --no-replaces --no-enhances <pkgs>`，然后用 `dpkg-query` 过滤掉宿主
**已安装**的包，再用 `CORE_EXCLUDE` 列表显式排除核心运行时（`libc6`、`libgcc-s1`、`libstdc++6`、
`libtinfo6`、`zlib1g`、`perl`、`dpkg` 等 70 余项）。最终闭包 213 个包 → 实际下载并解包 **62 个**。

额外的安全网：解包完成后，脚本会扫描 `prefix/**/lib`，把**宿主已经提供的同名库文件删除**，
因此 `LD_LIBRARY_PATH` 只可能提供宿主真正缺失的库，绝不可能遮蔽宿主运行时库。
实测输出：

```
[bootstrap] deb: removed 1 library file(s) that the host already provides
```

`/etc/ld.so.conf` **未被修改**（`ldd` 验证见下）。

### 3.2 关键安装命令与真实输出

```
$ bash scripts/bootstrap-toolchain.sh
[bootstrap] rust: already installed, skipping
[bootstrap] deb: nothing to download
[bootstrap] deb: unpacked 0 archive(s), skipped 62 already unpacked
[bootstrap] deb: removed 0 library file(s) that the host already provides
[bootstrap] qemu: linked seabios data files into /root/PrincessIDE/.toolchain/prefix/usr/share/qemu
[bootstrap] qemu: linked ipxe option ROMs into /root/PrincessIDE/.toolchain/prefix/usr/share/qemu
[bootstrap] verifying toolchain
[bootstrap]   ok   cargo --version  ->  cargo 1.98.1 (797e8a9bc 2026-08-05)
[bootstrap]   ok   rustc --version  ->  rustc 1.98.1 (48a229cea 2026-09-01)
[bootstrap]   ok   rustfmt --version  ->  rustfmt 1.9.0-stable (48a229ceae 2026-09-01)
[bootstrap]   ok   cargo-clippy --version  ->  clippy 0.1.98 (48a229ceae 2026-09-01)
[bootstrap]   ok   qemu-system-x86_64 --version  ->  QEMU emulator version 7.2.22 (Debian 1:7.2+dfsg-7+deb12u18+b3)
[bootstrap]   ok   nasm -v  ->  NASM version 2.16.01
[bootstrap]   ok   clang --version  ->  Debian clang version 14.0.6
[bootstrap]   ok   clangd --version  ->  Debian clangd version 14.0.6
[bootstrap]   ok   ld.lld --version  ->  Debian LLD 14.0.6 (compatible with GNU linkers)
[bootstrap]   ok   gdb --version  ->  GNU gdb (Debian 13.1-3) 13.1
[bootstrap]   ok   xorriso --version  ->  xorriso 1.5.4 : RockRidge filesystem manipulator, libburnia project.
[bootstrap]   ok   mformat --version  ->  mformat (GNU mtools) 4.0.32
[bootstrap] all tools verified
[bootstrap] done.  Activate with:  source /root/PrincessIDE/scripts/env.sh
```

**幂等性实测**：连续执行第二次，解包全部跳过（`unpacked 0 archive(s), skipped 62 already unpacked`），
退出码 0。**冷启动实测**：删除 `.toolchain/prefix` 与 `.toolchain/.stamps` 后重跑，16.9 秒内
重新解包 62 个归档并全部验证通过，退出码 0。

`.toolchain/` 体积：`prefix` 618M，`debs` 73M，`cargo` 21M，`rustup` 602M，合计约 **1.4G**。

**网络实测（重要）**：

| 端点 | 实测速度 |
| --- | --- |
| `https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init` | **62 B/s**（15 秒仅收到 945 字节，不可用） |
| `https://mirrors.tuna.tsinghua.edu.cn/rustup/...` | **7.3 MB/s**（21 MB 文件秒下） |

因此 `bootstrap-toolchain.sh` 默认使用清华镜像作为 `RUSTUP_DIST_SERVER` / `RUSTUP_UPDATE_ROOT`，
可通过环境变量 `PRINCESSIDE_RUSTUP_DIST_SERVER` 覆盖回官方源。apt 源本身就是清华镜像。

### 3.3 解包工具的额外正确性验证

`clang` 不只是「能打印版本」——夹具源码的实测编译 + `lld` 链接均通过：

```
$ clang -print-resource-dir
/root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-14/lib/clang/14.0.6
$ ls "$(clang -print-resource-dir)/include/stddef.h"
/root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-14/lib/clang/14.0.6/include/stddef.h
$ clang -m64 -std=gnu11 -O0 -g -ffreestanding -fno-builtin -fno-stack-protector \
      -fno-pic -fno-pie -mno-red-zone -mgeneral-regs-only -c kernel.c -o kernel.clang.o
clang compile: OK
kernel.clang.o: ELF 64-bit LSB relocatable, x86-64, version 1 (SYSV), with debug_info, not stripped
$ ld.lld -m elf_x86_64 -T linker.ld --nostdlib --build-id=none -o refkernel.lld.elf ...
lld link: OK
```

`LD_LIBRARY_PATH` 生效且未破坏宿主（`qemu-system-x86_64` 能启动、`gdb`/`clang` 能运行即为证据）。

### 3.4 `env.sh` 幂等性

`env.sh` 会先剔除自身已注入的条目再前置，因此反复 `source` 不会堆叠：

```
$ source scripts/env.sh; p1="$PATH"; l1="$LD_LIBRARY_PATH"; source scripts/env.sh; source scripts/env.sh
PATH idempotent: OK
LD_LIBRARY_PATH idempotent: OK
PATH idempotent after 3rd source: OK
cargo resolves to: /root/PrincessIDE/.toolchain/cargo/bin/cargo
clang resolves to: /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-14/bin/clang
```

> 注：最初的 `env.sh` 版本此处是**有 bug 的**（会重复堆叠）；已修复并留下上述回归验证。

---

## 4. 引导路径选择

### 4.1 结论

**选定方案 B：x86_64 + Multiboot 2 + `grub-mkrescue` 生成 ISO + `-cdrom -boot d`。**

关键 QEMU 命令（`scripts/smoke-boot.sh` 中实际使用）：

```
qemu-system-x86_64 \
    -L "$PRINCESSIDE_QEMU_DATA" \
    -m 256M \
    -cdrom fixtures/refkernel/build/refkernel.iso \
    -boot d \
    -display none \
    -serial stdio \
    -no-reboot \
    -monitor none
```

### 4.2 方案 A 为什么没用 —— 有实测证据，不是推测

方案 A（`qemu-system-x86_64 -kernel` + Multiboot 1）**只能引导 32 位 ELF**：

```
$ qemu-system-x86_64 -L "$PRINCESSIDE_QEMU_DATA" -m 256M \
      -kernel fixtures/refkernel/build/refkernel.elf \
      -display none -serial stdio -no-reboot
qemu-system-x86_64: Cannot load x86-64 image, give a 32bit one.
exit=1
```

QEMU 明确拒绝了 ELF64 镜像，要求「a 32bit one」。

为了排除「是不是我的镜像本身有问题」这一可能，我写了一个最小的 32 位 Multiboot 1 内核
（`fixtures/refkernel/path-a-probe/probe32.S`，只打印一行并停机）做对照实验：

```
$ gcc -m32 -c probe32.S -o probe32.o
$ ld -m elf_i386 -T probe32.ld -nostdlib -o probe32.elf probe32.o
$ file probe32.elf
probe32.elf: ELF 32-bit LSB executable, Intel 80386, version 1 (SYSV)
$ qemu-system-x86_64 -L "$PRINCESSIDE_QEMU_DATA" -m 128M -kernel probe32.elf \
      -display none -serial stdio -no-reboot
PATH-A-32BIT-MULTIBOOT-BOOTED
```

**结论**：`-kernel` 路径在本机是**可用**的，拒绝的原因**纯粹是 ELF class**。
方案 A 要求内核是 32 位，而本项目夹具必须是真正的 x86_64（long mode）内核，
两者不可兼得，因此放弃方案 A。

（诚实说明：这个对照实验第一次跑是**没有输出**的，原因是我把等待发送的端口写成了
`0x3F8` 而不是 LSR 的 `0x3FD`，导致死循环；定位并修正后才得到上面的输出。此处记录以免误导。）

### 4.3 方案 B 的实现要点

- 同一份 `boot.S` 里同时放置 **Multiboot 1 头**与 **Multiboot 2 头**（前者留给将来可能的
  32 位路径，后者是 GRUB 实际使用的；GRUB 优先识别 MB2）。
- GRUB 在 BIOS 下以 **32 位保护模式**交接（`EAX=0x36D76289`，`EBX=mb2 info 物理地址`），
  内核自己装 GDT、建 1 GiB 的 2 MiB 大页身份映射、置 `CR4.PAE` / `EFER.LME` / `CR0.PG`，
  再远跳进入 64 位 long mode 调 `kernel_main()`。
- 运行时实测确认 (原样摘录自串口日志)：

```
[refkernel] multiboot: magic=0x36d76289 info=0x00119dc0 (multiboot 2)
[refkernel] cpu: long mode active, identity map 0x0..0x40000000
[refkernel] data segment image magic=0x1dea0c0ffee0beef
```

`magic=0x36d76289` 证明走的是 Multiboot 2；`data segment image magic=0x1dea0c0ffee0beef`
与源码初值一致，证明 RW 段确实被加载且初始化数据完好。

### 4.4 产物检查

```
$ file build/refkernel.elf
build/refkernel.elf: ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, with debug_info, not stripped

$ readelf -hW build/refkernel.elf | grep -E 'Class|Machine|Entry'
  Class:                             ELF64
  Machine:                           Advanced Micro Devices X86-64
  Entry point address:               0x100040

$ readelf -lW build/refkernel.elf
  Type           Offset   VirtAddr           PhysAddr           FileSiz  MemSiz   Flg Align
  LOAD           0x001000 0x0000000000100000 0x0000000000100000 0x001368 0x001368 R E 0x1000
  LOAD           0x003000 0x0000000000102000 0x0000000000102000 0x000008 0x016010 RW  0x1000
```

「带调试信息 + 保留符号表」已由 `with debug_info, not stripped`、`.debug_*` 段与
`.symtab` 段的存在证实。

---

## 5. 关键命令的真实输出

### 5.1 冒烟测试：横幅匹配 + 异常

串口原始日志（`fixtures/refkernel/build/smoke-boot.log`，完整粘贴）：

```
PrincessIDE reference kernel booted
[refkernel] multiboot: magic=0x36d76289 info=0x00119dc0 (multiboot 2)
[refkernel] cpu: long mode active, identity map 0x0..0x40000000
[refkernel] build: Sep 10 2026 23:49:38
[refkernel] data segment image magic=0x1dea0c0ffee0beef
[refkernel] idt installed, raising controlled fault in refkernel_fault_probe()

[refkernel] EXCEPTION: vector=0x06 (#UD invalid opcode)
[refkernel] FAULT_RIP=0x0000000000100b3d cs=0x0008 rflags=0x0000000000000046 error=0x0000000000000000
[refkernel] PANIC: unhandled CPU exception, halting
qemu-system-x86_64: terminating on signal 15 from pid 220658 (timeout)
```

> 最后一行是脚本在拿到故障 RIP 后主动结束 QEMU 产生的（内核按设计在 panic 后进入 `hlt` 死循环）。
> 断言只看串口输出，不看 QEMU 退出码，因此这一行不影响结论。

`scripts/smoke-boot.sh` 的输出：

```
$ cd /tmp && bash /root/PrincessIDE/scripts/smoke-boot.sh
[smoke-boot] building reference kernel and ISO
[smoke-boot] PASS
[smoke-boot]   banner    : PrincessIDE reference kernel booted
[smoke-boot]   exception : EXCEPTION: vector=0x06 (#UD invalid opcode)
[smoke-boot]   fault rip : 0x0000000000100b3d
[smoke-boot]   serial log: /root/PrincessIDE/fixtures/refkernel/build/smoke-boot.log
smoke-boot exit=0
```

**横幅匹配**（断言使用的确切模式，均为固定字符串）：

| 断言 | 模式 | 结果 |
| --- | --- | --- |
| 横幅 | `PrincessIDE reference kernel booted` | 命中 |
| 异常 | `EXCEPTION: vector=0x06` | 命中 |
| 故障 RIP | `FAULT_RIP=0x[0-9a-f]{16}` | 命中，`0x0000000000100b3d` |

### 5.2 符号化结果（含 `源文件:行号`）

```
$ cd /tmp && bash /root/PrincessIDE/scripts/symbolicate.sh
symbolicate: reference kernel fault report
  elf          : /root/PrincessIDE/fixtures/refkernel/build/refkernel.elf
  rip source   : serial log /root/PrincessIDE/fixtures/refkernel/build/smoke-boot.log
  fault RIP    : 0x0000000000100b3d
  addr2line -f : refkernel_fault_probe
  addr2line -e : /root/PrincessIDE/fixtures/refkernel/kernel.c:100
  nm symbol    : 0000000000100b39 T refkernel_fault_probe

--- objdump: faulting instruction and line mapping (0x100b1d .. 0x100b45) ---

/root/PrincessIDE/fixtures/refkernel/build/refkernel.elf:     file format elf64-x86-64


Disassembly of section .text:

0000000000100b1d <exception_dispatch+0x81>:
exception_dispatch():
/root/PrincessIDE/fixtures/refkernel/kernel.c:77
  100b1d:	00 00                	add    BYTE PTR [rax],al
  100b1f:	00 e8                	add    al,ch
  100b21:	3d f9 ff ff bf       	cmp    eax,0xbffffff9
/root/PrincessIDE/fixtures/refkernel/kernel.c:82
  100b26:	48 11 10             	adc    QWORD PTR [rax],rdx
  100b29:	00 b8 00 00 00 00    	add    BYTE PTR [rax+0x0],bh
  100b2f:	e8 2e f9 ff ff       	call   100462 <serial_printf>
/root/PrincessIDE/fixtures/refkernel/kernel.c:83
  100b34:	e8 5b ff ff ff       	call   100a94 <halt_forever>

0000000000100b39 <refkernel_fault_probe>:
refkernel_fault_probe():
/root/PrincessIDE/fixtures/refkernel/kernel.c:99
  100b39:	55                   	push   rbp
  100b3a:	48 89 e5             	mov    rbp,rsp
/root/PrincessIDE/fixtures/refkernel/kernel.c:100
  100b3d:	0f 0b                	ud2
/root/PrincessIDE/fixtures/refkernel/kernel.c:103
  100b3f:	bf 80 11 10 00       	mov    edi,0x101180
  100b44:	e8                   	.byte 0xe8

--- addr2line -f -C -e refkernel.elf 0x0000000000100b3d ---
refkernel_fault_probe
/root/PrincessIDE/fixtures/refkernel/kernel.c:100

[symbolicate] PASS: 0x0000000000100b3d -> /root/PrincessIDE/fixtures/refkernel/kernel.c:100 (refkernel_fault_probe)
```

要点：故障 RIP 精确落在 `ud2` 指令（`0f 0b`）上，`addr2line` 解析为
**`kernel.c:100`**，所属函数为 **`refkernel_fault_probe`** —— 正是夹具契约要求的命名稳定函数。

### 5.3 负向测试（脚本的失败路径确实会失败）

```
$ bash scripts/symbolicate.sh 0xdeadbeef
[symbolicate] FAIL: addr2line could not map 0xdeadbeef to a source location
exit=1

$ bash scripts/symbolicate.sh 0x0000000000100b4e      # kernel_main 的地址
[symbolicate] FAIL: expected the fault RIP inside refkernel_fault_probe(), got kernel_main
exit=1
```

### 5.4 从干净 shell 可重复运行

`smoke-boot.sh` 与 `symbolicate.sh` 均不依赖调用者 cwd 或 PATH；它们自行定位并 source `env.sh`。
上述所有验证都是 `cd /tmp && bash /root/PrincessIDE/scripts/...` 或 `cd / && bash ...` 的形式，
并重复执行多次均得到相同结果。

---

## 6. 夹具契约（供后续阶段依赖，勿随意改动）

`fixtures/refkernel/kernel.c` 中的输出格式是**测试脚本与夹具之间的接口**：

| 契约 | 值 |
| --- | --- |
| 横幅（必须精确匹配） | `PrincessIDE reference kernel booted` |
| 异常行 | `[refkernel] EXCEPTION: vector=0x06 (#UD invalid opcode)` |
| 故障 RIP 行 | `[refkernel] FAULT_RIP=0x<16 位十六进制> cs=0x.... rflags=0x.... error=0x....` |
| panic 行 | `[refkernel] PANIC: unhandled CPU exception, halting` |
| 故障函数 | `refkernel_fault_probe()`，`__attribute__((noinline, used))`，内含 `ud2` |
| 故障向量 | `#UD`（6），无错误码（`error=0`） |

选 `ud2` 而非除零的原因：`ud2` 是**确定性**的 —— 不会被优化掉、不依赖优化级别、
必然产生 `#UD` 且错误码恒为 0，适合机器化断言。

`refkernel_fault_probe` 必须保持 `noinline` 且不被内联进 `kernel_main`，否则符号化断言会失败。

---

## 7. 遗留缺口

### 7.1 故意未装：Tauri 所需的 GUI 库

按 P0 范围，**GUI 依赖本阶段故意不安装**（无显示器，且 Tauri 属于后续阶段）。将来需要补的
（以下候选版本已用 `apt-cache policy` 在 bookworm 上核实**可获取**）：

| 包 | bookworm 候选版本 | 用途 |
| --- | --- | --- |
| `libwebkit2gtk-4.1-dev` | 2.50.6-1~deb12u2 | Tauri v2 的 WebView |
| `libwebkit2gtk-4.0-dev` | 2.50.6-1~deb12u2 | Tauri v1 的 WebView |
| `libjavascriptcoregtk-4.1-dev` | 2.50.6-1~deb12u2 | WebKit JS 引擎头文件 |
| `libgtk-3-dev` | 3.24.38-2+deb12u3 | GTK3 |
| `libsoup-3.0-dev` | 3.2.3-0+deb12u2 | 网络栈 |
| `librsvg2-dev` | 2.54.7+dfsg-1~deb12u1 | 图标渲染 |
| `libayatana-appindicator3-dev` | 0.5.92-1 | 托盘图标 |
| `libxdo-dev` | 1:3.20160805.1-5 | Tauri v2 输入模拟 |
| `libssl-dev` | 3.0.20-1~deb12u2 | TLS |
| `pkg-config` | 1.8.1-1 | 构建发现 |

**注意事项（给后续阶段的警告）**：这批包依赖闭包远比 P0 的大（含大量 `-dev` 包与
`libgtk`/`glib` 生态），用 `.deb` 解包到工作区前缀的方式会变得很脆弱 —— `pkg-config` 的
`.pc` 文件里写死了 `/usr/lib/x86_64-linux-gnu` 路径，`pkg-config` 需要 `PKG_CONFIG_PATH`
重定向，而 GTK/WebKit 的构建脚本还会去找 `/usr/include`。**建议后续阶段改为真正的系统级
安装（`apt-get install`），届时需要授权提权**。P0 阶段无授权人，故不做。

### 7.2 grub-mkrescue 依赖宿主，bootstrap 不安装它

`grub-mkrescue`（`/usr/bin/grub-mkrescue`，来自宿主已装的 `grub-pc-bin` + `grub2-common`）是
**唯一一个 P0 运行时依赖但不由 `bootstrap-toolchain.sh` 安装**的工具。原因是它把平台模块目录
硬编码为 `/usr/lib/grub/i386-pc`，`.deb` 解包到工作区前缀无法把该路径重定向，
解包出来的 `grub-mkrescue` 会找不到 `boot_hybrid.img` 等模块。

**影响**：在缺少 `grub-pc-bin` 的机器上，「从零重跑」的 `make iso` / `smoke-boot.sh` 会失败。
`doctor.sh` 会把 `grub-mkrescue` 报为 MISSING REQUIRED 而不是静默出错。
**缓解**：宿主已有该包（Debian 12 的最小镜像通常不含，需确认）；如缺失需系统级
`apt-get install grub-pc-bin grub2-common` 或提供替代引导路径。

### 7.3 KVM 不可用 —— 只能软件模拟（TCG）

`/dev/kvm` 存在但不可访问：

```
$ qemu-system-x86_64 -enable-kvm -cpu host ...
Could not access KVM kernel module: Permission denied
qemu-system-x86_64: failed to initialize kvm: Permission denied
exit=1
```

这是容器/沙箱权限限制，不是 QEMU 的问题。因此所有 QEMU 验证都在 **TCG（纯软件模拟）** 下完成，
速度较慢；`smoke-boot.sh` 因此采用「轮询到 panic 就提前结束」而非固定等待超时，
一次完整冒烟约数秒。若将来能拿到 `/dev/kvm`，`smoke-boot.sh` 可加 `-enable-kvm` 提速。

### 7.4 未做（超出 P0 范围）

- **内核侧 panic 栈回溯（backtrace）**：夹具只报告故障 RIP，没有做栈展开。P0 的要求是
  「RIP 可符号化」，已满足；带栈回溯的 panic 输出属于后续阶段。
- **Rust 侧 `x86_64-unknown-none` 等裸机 target**：未安装，本阶段不需要。
- **CI 配置文件**：宿主未指定 CI 系统，因此只提供可脚本化调用的入口
  （`bootstrap-toolchain.sh` / `doctor.sh` / `smoke-boot.sh` / `symbolicate.sh`，全部有明确退出码），
  未写 `.github/workflows` 或其它 CI 描述文件。
- **32 位工具链**：`gcc -m32` 可编译 freestanding 代码（路径 A 的对照实验已证明），
  但缺少 32 位 glibc 开发包，无法编译 hosted 32 位程序。既然不用路径 A，未安装。

### 7.5 共享工作区的并发情况（需要上游决策，非缺陷）

本工作区同时有**多个 Agent 在写**，我在执行期间观察到：

| 路径 | 观察 | 归属 |
| --- | --- | --- |
| `_toolchain/` | 23:41 出现的另一份工具链解包树 | 非本 Agent |
| `_work/` | 23:52，含 `kernel_mb1.iso`/`kernel_mb2.iso`/`ovmf_vars.fd`/`qmp_test.sh`/`serial6.txt` 等，是**同一个 P0 任务的另一份内核/ISO/QEMU 实验** | 非本 Agent |
| `.researchA/` | 23:50 的研究目录 | 非本 Agent |
| `docs/research/`、`docs/spec/` | 其它 Agent 的文档（`clangd-freestanding-kernel.md`、`10-contracts.md`、`20-acceptance.md`） | 非本 Agent |

本 Agent **完全没有创建或修改**上述任何路径。但请注意：

1. `.toolchain/prefix/` 曾被并发写入（出现了 `clang-16`/`clangd-16`/`libllvm16` 等 backports 包）。
   我接管后用 `.stamps` 机制 + 直接解包重建了该前缀，现在其中只有本 P0 声明的
   **LLVM 14（bookworm main）** 这一套，版本自洽。
2. `_work/` 中的内核实验与本 Agent 的 `fixtures/refkernel/` 是**重复劳动**；
   建议上游确认是否只需要一份，避免后续阶段引用到两份不同的夹具。
3. 初始 git 提交**只包含本 Agent 拥有的路径**（见 §9），其它 Agent 的目录保持 untracked，
   由各自或上游统一提交。`_work/`、`.researchA/`、`docs/research/`、`docs/spec/` 均未被 `git add`。

---

## 8. 「从零重跑」操作步骤

### 8.1 前置条件

- Debian 12 (bookworm) x86_64，可用的 `apt`（已配置任意可用镜像）。
- 宿主已有：`gcc`、`ld`、`make`、`binutils`（`objdump`/`addr2line`/`readelf`）、
  `git`、`curl`、`dpkg`/`dpkg-deb`、`apt-get`，
  以及 **`grub-mkrescue`（来自 `grub-pc-bin` + `grub2-common`）** ← 见 §7.2。
- 磁盘 ≥ 2 GB 可用空间（`.toolchain/` 约 1.4 GB）。
- 网络可达：crates.io / rustup 镜像 / Debian apt 源。
- 无需显示器；无需 root 安装软件包（bootstrap 不会调用 `apt-get install`）。

### 8.2 步骤

```bash
# 0) 取得代码
cd /root && git clone <repo-url> PrincessIDE && cd PrincessIDE

# 1) 一次性安装工作区内工具链（幂等，可反复执行；已装则跳过）
#    rustup 走清华镜像（官方源实测 62 B/s 不可用），约 600 MB 下载
bash scripts/bootstrap-toolchain.sh

# 2) 校验工具链：打印每个工具的真实版本与绝对路径，缺任一必需工具则退出码非 0
bash scripts/doctor.sh

# 3) 构建参考内核并做无头冒烟测试
#    断言：横幅出现 + 触发 #UD + 输出含故障 RIP；成功退出 0，失败打印完整串口输出
bash scripts/smoke-boot.sh

# 4) 符号化：把故障 RIP 映射回 fixtures/refkernel 的具体 源文件:行号
bash scripts/symbolicate.sh

# 5) 交互式开发时激活环境（每个新 shell 都要 source 一次）
source scripts/env.sh

# 6)（可选）单独构建 / 查看夹具
make -C fixtures/refkernel            # 只产 ELF
make -C fixtures/refkernel iso        # 产 GRUB ISO
make -C fixtures/refkernel symbols    # 打印关键符号表项
```

### 8.3 预期结果

| 步骤 | 预期 |
| --- | --- |
| 1 | 最后输出 `[bootstrap] all tools verified` 与 `[bootstrap] done.`，退出码 0 |
| 2 | 全部 `ok`，最后一行 `doctor: all required tools present (24 resolved).`，退出码 0 |
| 3 | `[smoke-boot] PASS`，`fault rip : 0x0000000000100b3d`，退出码 0 |
| 4 | `[symbolicate] PASS: 0x... -> /root/PrincessIDE/fixtures/refkernel/kernel.c:100 (refkernel_fault_probe)`，退出码 0 |

> 注意：故障 RIP 的**绝对值**会随源码行数/编译结果漂移（本次为 `0x100b3d`），
> 因此脚本断言的是**格式**（`FAULT_RIP=0x<16 位十六进制>`）与**符号归属**
> （必须落在 `refkernel_fault_probe`），而不是写死地址。行号 `kernel.c:100` 也会随源码调整而变。

### 8.4 如果只想重来工具链

```bash
rm -rf .toolchain/prefix .toolchain/.stamps   # 只重建解包树（保留 .deb 缓存，约 17 秒）
bash scripts/bootstrap-toolchain.sh

rm -rf .toolchain                             # 彻底重来（会重新下载 rustup ~600 MB）
bash scripts/bootstrap-toolchain.sh
```

---

## 9. 仓库基础

- 已执行 `git init`（分支 `master`），并配置了提交身份
  `PrincessIDE P0 Agent <p0-agent@princesside.local>`。
- `.gitignore` 覆盖任务要求的全部模式：`.toolchain/`、`target/`、`*.o`、`*.elf`、`*.iso`、
  `*.img`、`*.deb`，另加 `*.a`、`*.so`、`*.d`、`*.bin`、`*.map`、`*.log`、
  `fixtures/refkernel/build/`、`node_modules/`、`dist/`、`.pnpm-store/` 与编辑器噪声。
  另外把 `_toolchain/`（并发的另一份工具链解包树，约数百 MB）也列入忽略，避免误入库。
- 初始提交**只包含本 Agent 拥有的路径**：`.gitignore`、`scripts/`、
  `fixtures/refkernel/`（源码与脚本，构建产物按 `.gitignore` 排除）、
  `docs/p0-toolchain-report.md`。其它 Agent 的目录（`_work/`、`_toolchain/`、`.researchA/`、
  `docs/research/`、`docs/spec/`）**未加入索引**，保持 untracked，交由各自或上游处理。

> 说明：`apps/` 与 `crates/` 属于后续阶段的其他 Agent，本 Agent 未创建、未修改，符合职责边界。
