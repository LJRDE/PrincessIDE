# A1 — 把工具链补到「能承载 IDE 语言服务」（clangd-16 + bear）

| 项目 | 值 |
| --- | --- |
| 任务 | A1（Open Actions 表；决策 **D6** / **D7** / **D8**） |
| 宿主 | Debian GNU/Linux 12 (bookworm), x86_64 |
| 工作区 | `/root/PrincessIDE` |
| 执行时间 | 2026-09-10 16:17–16:30 UTC（宿主机本地时区 UTC+8：09-11 00:17–00:30，`date -u` 实测） |
| 目录所有权 | `scripts/bootstrap-toolchain.sh`、`scripts/env.sh`、`scripts/doctor.sh`、`docs/reports/a1-clangd16.md`、`.scratch/toolchain/`、`.toolchain/`（新增内容） |
| 未触碰 | `crates/`、`apps/`、`fixtures/`、`docs/spec/`、`docs/research/`、根 `Cargo.toml`、`scripts/smoke-boot.sh`、`scripts/symbolicate.sh`；**未执行任何改动 git 的命令** |
| 声明纪律 | 本文所有「通过」均有真实命令输出 + 退出码支撑，输出原样粘贴；未跑过的内容在 §7 显式标注 |

---

## 0. 摘要

| 项 | 结论 |
| --- | --- |
| **状态** | **完成**（A1-1 … A1-7 全部真实执行并通过；含一次隔离的**从零重跑**） |
| 新增工具 | `clangd-16` 16.0.6、`clang-16` 16.0.6、`bear` 3.1.1（＋ LLVM 16 运行库；`clangd-14`/`clang-14` 原样保留） |
| 安装方式 | 沿用 P0 已验证的 **`apt-get download` + `dpkg-deb -x` 解包到 `.toolchain/prefix/`**，**没有 `apt-get install`**、没有写 `/etc/ld.so.conf`、没有在工作区外写文件 |
| 幂等 | 连跑两次均 rc=0，第二次 `deb: nothing to download` / `unpacked 0 archive(s), skipped 73` |
| 回归 | `scripts/smoke-boot.sh` 仍 rc=0（改动 `env.sh` 前后各跑一次，两次都 PASS） |
| **D7 复核** | `-nostdlibinc` 路线 **不崩溃、0 诊断**（CLI 与 LSP 两种形态都验了） |
| **研究报告 §2.4 复现结论** | §2.4 的**实质结论成立**（clang 的 `-nostdinc` 会连内置 freestanding 头一起删，`-nostdlibinc` 才留）；但 **§2.4 的标题「`-nostdinc` 会让 clangd **直接崩掉**」在本环境<u>无法复现</u>** —— 实测 clangd-16 既不崩也不退出异常，而是**正常报 16 条诊断**。详见 §4 / §5.1 |

---

## 1. 这一步补的是什么（背景）

P0 装进 `.toolchain/` 的是 **clangd 14.0.6**（只够冒烟）。决策 **D6** 要求语言服务用 **clangd-16**（bookworm main 最新 `1:16.0.6-15~deb12u1`），决策 **D8** 要求 `compile_commands.json` 首选 **make + bear**。本次把这两件补齐，并保证 **不把两个 clangd 大版本搞混**。

选包依据（`apt-cache policy`，实测）：

```
clangd-16:  Candidate: 1:16.0.6-15~deb12u1   (bookworm/main, mirrors.tuna.tsinghua.edu.cn)
clang-16:   Candidate: 1:16.0.6-15~deb12u1
bear:       Candidate: 3.1.1-1
libspdlog1.10-fmt9: Candidate: (none)        ← bear 的 Depends 里写着它，但 bookworm 无此包名
                                               （与研究 §1.2 的实测一致；真正满足符号的是
                                                libspdlog1.10 1:1.10.0+ds-0.4）
```

---

## 2. 改动了什么（逐条）

### 2.1 `scripts/bootstrap-toolchain.sh`

| # | 改动 | 说明 |
| --- | --- | --- |
| 1 | 顶部目录清单新增 `.toolchain/bin/` | 工作区自有启动器目录 |
| 2 | 顶部新增「两套工具集」说明 | P0 集 vs A1 集；并写明 clangd 两个大版本**故意并存、不互换** |
| 3 | `PKGS` 追加 `clangd-16 clang-16 bear` | 三条各自写了为什么需要（clangd-16 是语言服务本体；clang-16 提供 resource-dir 里的内置 freestanding 头，是 `-nostdlibinc` 可行的前提；bear 是 D8 首选） |
| 4 | 新增函数 `install_bear_launcher()` | 见下 |
| 5 | `verify()` 的 PATH/LD_LIBRARY_PATH 增加 `$TOOLCHAIN/bin`、`llvm-16/bin`、`llvm-16/lib` | 与新布局一致 |
| 6 | `verify()` 检查清单新增 `clang-16` / `clangd-16` / `clangd-14` / `bear` | 每个都打印真实版本 |
| 7 | `verify()` 新增**版本断言** | `clangd-14` 必须是 14.x、`clangd-16` 必须是 16.x —— 「能跑」不算通过，「跑出来是不是那个版本」才算（这正是 D6 要防的混淆） |
| 8 | 主流程插入 `install_bear_launcher` | 在 `link_qemu_data` 之后、`verify` 之前 |

**为什么需要 bear 启动器（本次新增的关键点）**：Debian 的 bear 3.1.1 把 4 个运行期构件的位置**硬编码成安装路径**：

```
--bear-path   /usr/bin/bear
--library     /usr/$LIB/bear/libexec.so          (LD_PRELOAD 拦截库)
--wrapper     /usr/lib/x86_64-linux-gnu/bear/wrapper
--wrapper-dir /usr/lib/x86_64-linux-gnu/bear/wrapper.d
```

我们是解包到前缀而不是安装，这 4 个默认值**全是错的**，`bear -- make` 会失败；更糟的是 **libexec.so 找不到时的失败模式是「静默漏条目」**（研究 §1.2 已标为「bear 最危险的失败模式」）。所以新增 `.toolchain/bin/bear`：一个 810 字节的 `sh` 启动器，把这 4 个路径补上；`--version`/`--help`/无参数时**直接透传真二进制**，保证输出逐字节一致。真实二进制仍在 `$PREFIX/usr/bin/bear` 原处可访问。

### 2.2 `scripts/env.sh`

| # | 改动 | 说明 |
| --- | --- | --- |
| 1 | 新增 `PRINCESSIDE_BIN="$PRINCESSIDE_TOOLCHAIN/bin"` | 启动器目录 |
| 2 | PATH 前插顺序改为：`.toolchain/bin` → `cargo/bin` → **`llvm-14/bin` → `llvm-16/bin`** → `prefix/usr/bin` → … | **llvm-14 在 llvm-16 之前**，于是**不带版本号的 `clang`/`clangd` 仍然是 14**（P0 行为零变化、零回归风险）；LLVM 16 通过**带版本号的名字**使用 |
| 3 | LD_LIBRARY_PATH 增加 `prefix/usr/lib/llvm-16/lib` | clangd 的 `RUNPATH` 不传递给间接依赖（研究 §0.2），必须靠 LD_LIBRARY_PATH |
| 4 | 新增导出：`PRINCESSIDE_CLANGD16` / `PRINCESSIDE_CLANGD14` / `PRINCESSIDE_CLANG16` / `PRINCESSIDE_BEAR` / **`PRINCESSIDE_LANG_SERVICE_CLANGD`（= clangd-16）** | IDE 端不必猜哪个大版本；按 D6，语言服务用 `$PRINCESSIDE_LANG_SERVICE_CLANGD` |
| 5 | 修 `LD_LIBRARY_PATH` / `PATH` 的**空元素**（顺带修的既有小坑） | 原来在前一值为空时会留下结尾 `:`，加载器把空元素当成「当前目录」→ 工程目录里一个同名 `libclang-cpp.so.16` 就可能被加载。现在不再产生空元素（实测见 `.scratch/toolchain/env-hygiene.log`：连 source 三次后 `empty PATH elements: 0`、`empty LD_LIBRARY_PATH elements: 0`，且条目数不叠加） |

**关于「不带版本号的 `clangd` 保持 14」这个取舍**：任务要求「保留 clangd-14 的可用性，不要把它们搞混」。实测确认三者互不干扰（`clangd`=14.0.6、`clangd-14`=14.0.6、`clangd-16`=16.0.6，见 §3）。**P3 语言服务请显式使用 `clangd-16` 或 `$PRINCESSIDE_LANG_SERVICE_CLANGD`**；`doctor.sh` 也会把 14/16 分别列出来做显式对照，不会出现「以为在跑 16、其实跑的是 14」的静默状态。

### 2.3 `scripts/doctor.sh`

| # | 改动 | 说明 |
| --- | --- | --- |
| 1 | 必需清单新增 4 行：`clangd-16 (LSP)`、`clang-16`、`clangd-14 (legacy)`、`bear (CDB)`，**每行打印真实版本 + 绝对路径** | 缺任一即非零退出（沿用原有 `missing[]` 机制） |
| 2 | 新增 `expect_version()` 断言 helper | 「能解析到」不够：`clangd-16` 必须自报 **16.x**、`clangd-14`/`clangd` 必须自报 **14.x**、`clang-16` 必须 **16.x**、`bear` 必须 **3.x**；不符则打印 `WRONG VER` 并计入 `missing[]`（非零退出） |

### 2.4 边界（没做的事）

- **没有** `apt-get install`，**没有**动系统包/`/etc/ld.so.conf`，**没有**在工作区外写文件。
- **没有**覆盖 `libc6`/`libgcc-s1`/`libstdc++6` 等核心运行库：沿用 `CORE_EXCLUDE` + `is_installed` 跳过 +「解包后删除宿主机也提供的库」三重保险（本次实测第二次运行 `removed 0 library file(s)`，说明没有重复清理）。
- **没有**改 `scripts/smoke-boot.sh`、`scripts/symbolicate.sh`，**没有**执行任何 git 命令。

---

## 3. 验收：命令原文 + 真实输出 + 退出码

全部输出取自一次性会话记录 `.scratch/toolchain/acceptance.log`（由 `.scratch/toolchain/run-acceptance.sh` 生成，含命令、原始输出、退出码）。

### A1-1 `source scripts/env.sh && clangd-16 --version` → **通过（rc=0）**

```
$ source scripts/env.sh && clangd-16 --version
Debian clangd version 16.0.6 (15~deb12u1)
Features: linux+grpc
Platform: x86_64-pc-linux-gnu
[exit code: 0]
```

同组一致性检查（**两个大版本没被搞混**）：

```
$ source scripts/env.sh && clangd-14 --version | head -1
Debian clangd version 14.0.6
[exit code: 0]

$ source scripts/env.sh && clangd    --version | head -1
Debian clangd version 14.0.6
[exit code: 0]

$ source scripts/env.sh && clang-16  --version | head -1
Debian clang version 16.0.6 (15~deb12u1)
[exit code: 0]
```

### A1-2 `source scripts/env.sh && bear --version` → **通过（rc=0）**

```
$ source scripts/env.sh && bear --version
bear 3.1.1
[exit code: 0]
```

`bear` 解析到 `.toolchain/bin/bear`（启动器）；真实二进制在 `.toolchain/prefix/usr/bin/bear`。启动器不只是摆设 —— 见 §3 附加验证 (c)。

### A1-3 `source scripts/env.sh && scripts/doctor.sh` → **通过（rc=0，28 个工具解析成功）**

```
$ source scripts/env.sh && scripts/doctor.sh
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
clangd-16 (LSP)        ok         Debian clangd version 16.0.6 (15~deb12u1)
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/clangd-16
clang-16               ok         Debian clang version 16.0.6 (15~deb12u1)
                                  /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-16/bin/clang-16
clangd-14 (legacy)     ok         Debian clangd version 14.0.6
                                  /root/PrincessIDE/.toolchain/prefix/usr/bin/clangd-14
bear (CDB)             ok         bear 3.1.1
                                  /root/PrincessIDE/.toolchain/bin/bear
clangd-16 (LSP)        ok         version matches /clangd version 16\./
clangd-14 (legacy)     ok         version matches /clangd version 14\./
clangd (P0 default)    ok         version matches /clangd version 14\./
clang-16               ok         version matches /clang version 16\./
bear (CDB)             ok         version matches /bear 3\./
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
doctor: all required tools present (28 resolved).
[exit code: 0]
```

### A1-4 `scripts/bootstrap-toolchain.sh` 连跑两次 → **通过（两次都 rc=0，第二次不重复下载）**

```
--- run 1 of 2 ---
[exit code: 0]
[bootstrap] rust: already installed, skipping
[bootstrap] deb: nothing to download
[bootstrap] deb: unpacked 0 archive(s), skipped 73 already unpacked
[bootstrap] bear: launcher written to /root/PrincessIDE/.toolchain/bin/bear
[bootstrap] all tools verified

--- run 2 of 2 ---
[exit code: 0]
[bootstrap] rust: already installed, skipping
[bootstrap] deb: nothing to download
[bootstrap] deb: unpacked 0 archive(s), skipped 73 already unpacked
[bootstrap] bear: launcher written to /root/PrincessIDE/.toolchain/bin/bear
[bootstrap] all tools verified
```

完整第二次日志（`verify()` 逐条版本）：

```
[bootstrap] rust: already installed, skipping
[bootstrap][warn] skip (core runtime, must come from host): libobjc-12-dev
[bootstrap][warn] skip (core runtime, must come from host): libobjc4
[bootstrap][warn] skip (core runtime, must come from host): libtextwrap1
[bootstrap] deb: nothing to download
[bootstrap] deb: unpacked 0 archive(s), skipped 73 already unpacked
[bootstrap] deb: removed 0 library file(s) that the host already provides
[bootstrap] qemu: linked seabios data files into /root/PrincessIDE/.toolchain/prefix/usr/share/qemu
[bootstrap] qemu: linked ipxe option ROMs into /root/PrincessIDE/.toolchain/prefix/usr/share/qemu
[bootstrap] bear: launcher written to /root/PrincessIDE/.toolchain/bin/bear
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
[bootstrap]   ok   clang-16 --version  ->  Debian clang version 16.0.6 (15~deb12u1)
[bootstrap]   ok   clangd-16 --version  ->  Debian clangd version 16.0.6 (15~deb12u1)
[bootstrap]   ok   clangd-14 --version  ->  Debian clangd version 14.0.6
[bootstrap]   ok   bear --version  ->  bear 3.1.1
[bootstrap]   ok   clangd-14 reports 14.x
[bootstrap]   ok   clangd-16 reports 16.x
[bootstrap] all tools verified
[bootstrap] done.  Activate with:  source /root/PrincessIDE/scripts/env.sh
```

> 第一次（增量）运行的原始日志另存于 `.scratch/toolchain/bootstrap-run1.log`：它记录了 A1 增量是**真的从「没有 clangd-16/bear」的状态**装的 —— `deb: downloading 11 package(s)`、`deb: unpacked 11 archive(s), skipped 62 already unpacked`。11 个包为：`bear clang-16 clangd-16 libclang1-16 libclang-common-16-dev libclang-cpp16 libear libfmt9 libllvm16 libspdlog1.10 llvm-16-linker-tools`。

### A1-5 回归 `source scripts/env.sh && scripts/smoke-boot.sh` → **通过（rc=0）**

```
$ source scripts/env.sh && scripts/smoke-boot.sh
[smoke-boot] building reference kernel and ISO
[smoke-boot] PASS
[smoke-boot]   banner    : PrincessIDE reference kernel booted
[smoke-boot]   exception : EXCEPTION: vector=0x06 (#UD invalid opcode)
[smoke-boot]   fault rip : 0x0000000000100b3d
[smoke-boot]   serial log : /root/PrincessIDE/fixtures/refkernel/build/smoke-boot.log
[exit code: 0]
```

改动 `env.sh`（PATH/LD_LIBRARY_PATH 顺序 + 空元素修复）**之后**又单独跑过一次，同样 `PASS` / rc=0（日志 `.scratch/toolchain/smoke-boot-after-env-change.log`）。

### A1-6 最小内核工程 + `.clangd`（`-nostdlibinc`）→ `clangd-16 --check` 不崩溃、诊断合理 → **通过（rc=0，0 errors）**

夹具（全部在 `.scratch/toolchain/a16/`，不污染仓库）：

```
.scratch/toolchain/a16/           .clangd                         ← D7 配置（-nostdlibinc）
  Makefile                        dot-clangd-nostdlibinc          ← A1-6 版本
  include/kernel/types.h          dot-clangd-nostdinc             ← A1-7 版本
  kernel/kmain.c                  dot-clangd-no-nostd             ← A1-7c 版本（只有 triple）
  kernel/hostleak.c               compile_commands.json           ← 由 bear 真实产出
  t/t1.c  t/t3.c
```

先用 **bear**（经启动器）真实产 CDB，证明 A1-2 的 bear 不是"只能打印版本号"：

```
$ bear --output compile_commands.json --force-preload -- make clean all
gcc -std=gnu11 -O2 -g3 -Wall -Wextra -ffreestanding -fno-stack-protector -fno-pic -fno-pie -mno-red-zone -mcmodel=kernel -mno-sse -m64 -nostdinc -isystem /usr/lib/gcc/x86_64-linux-gnu/12/include -Iinclude -c kernel/kmain.c -o build/kernel/kmain.o
[exit code: 0]
```

CDB 内容（节选，实测）：`arguments` 数组 + **绝对路径 `directory`**，与研究 §1.2 的产出一致。

```
[
  {
    "arguments": [ "/usr/bin/gcc", "-std=gnu11", ..., "-nostdinc", "-isystem",
                   "/usr/lib/gcc/x86_64-linux-gnu/12/include", "-Iinclude",
                   "-c", "-o", "build/kernel/kmain.o", "kernel/kmain.c" ],
    "directory": "/root/PrincessIDE/.scratch/toolchain/a16",
    "file": "/root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c",
    "output": "/root/PrincessIDE/.scratch/toolchain/a16/build/kernel/kmain.o"
  }
]
```

`.clangd`（A1-6，即 D7 要求的那套）：

```yaml
CompileFlags:
  Add:
    - -nostdlibinc
    - --target=x86_64-unknown-none
    - -ffreestanding
  Remove:
    - -nostdinc      # clang's -nostdinc also drops clang's own freestanding headers
    - -isystem       # removes the flag *and* its argument
    - -W*            # gcc-only warnings clangd would report as driver errors
Index:
  StandardLibrary: false
```

`clangd-16 --check` 完整输出：

```
$ cd .scratch/toolchain/a16 && source /root/PrincessIDE/scripts/env.sh && cp dot-clangd-nostdlibinc .clangd && clangd-16 --check=kernel/kmain.c --enable-config --log=info
I[00:25:37.705] Debian clangd version 16.0.6 (15~deb12u1)
I[00:25:37.706] Features: linux+grpc
I[00:25:37.706] PID: 310618
I[00:25:37.706] Working directory: /root/PrincessIDE/.scratch/toolchain/a16
I[00:25:37.706] argv[0]: clangd-16
I[00:25:37.706] argv[1]: --check=kernel/kmain.c
I[00:25:37.706] argv[2]: --enable-config
I[00:25:37.706] argv[3]: --log=info
I[00:25:37.710] Entering check mode (no LSP server)
I[00:25:37.710] Testing on source file /root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c
I[00:25:37.720] Loading compilation database...
I[00:25:37.727] Loaded compilation database from /root/PrincessIDE/.scratch/toolchain/a16/compile_commands.json
I[00:25:37.729] Compile command from CDB is: /usr/bin/gcc -std=gnu11 -O2 -g3 -ffreestanding -fno-stack-protector -fno-pic -fno-pie -mno-red-zone -mcmodel=kernel -mno-sse -m64 -Iinclude -c -o build/kernel/kmain.o -nostdlibinc --target=x86_64-unknown-none -ffreestanding -resource-dir=/root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-16/lib/clang/16 -- /root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c
I[00:25:37.730] Parsing command...
I[00:25:37.745] internal (cc1) args are: -cc1 -triple x86_64-unknown-none -fsyntax-only -disable-free -clear-ast-before-backend -disable-llvm-verifier -discard-value-names -main-file-name kmain.c -mrelocation-model static -mframe-pointer=all -fmath-errno -ffp-contract=on -fno-rounding-math -mconstructor-aliases -ffreestanding -mcmodel=kernel -target-cpu x86-64 -target-feature -sse -disable-red-zone -tune-cpu generic -mllvm -treat-scalable-fixed-error-as-warning -debug-info-kind=constructor -dwarf-version=5 -debugger-tuning=gdb -fcoverage-compilation-dir=/root/PrincessIDE/.scratch/toolchain/a16 -nostdsysteminc -resource-dir /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-16/lib/clang/16 -I include -O2 -std=gnu11 -fdebug-compilation-dir=/root/PrincessIDE/.scratch/toolchain/a16 -ferror-limit 19 -fgnuc-version=4.2.1 -vectorize-loops -vectorize-slp -no-round-trip-args -faddrsig -D__GCC_HAVE_DWARF2_CFI_ASM=1 -x c /root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c
I[00:25:37.747] Building preamble...
I[00:25:37.858] Indexing headers...
I[00:25:37.874] Built preamble of size 219692 for file /root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c version null in 0.12 seconds
I[00:25:37.874] Building AST...
I[00:25:37.950] Indexing AST...
I[00:25:37.951] Building inlay hints
I[00:25:37.954] Building semantic highlighting
I[00:25:37.959] Testing features at each token (may be slow in large files)
I[00:25:38.059] All checks completed, 0 errors
[exit code: 0]
```

诊断合理性三项独立取证：

1. **cc1 参数正确**：`-triple x86_64-unknown-none`、只加 `-nostdsysteminc`（**没有** `-nobuiltininc`）、`-resource-dir` 指向**我们前缀里的** `.../llvm-16/lib/clang/16` → 内置 freestanding 头保留。
2. **glibc 确实被挡在外面**（`.clangd` 生效的反证）：`kernel/hostleak.c`（`#include <string.h> <stdio.h>`）在 A1-6 配置下报
   `[pp_file_not_found] Line 5: 'string.h' file not found` + 2 条连带错误，`All checks completed, 3 errors`（rc=3）。**这是期望行为**（内核不该有 `string.h`）。
3. **`--enable-config` 在 clangd 16 上是可有可无的**（实测对照）：去掉它以后 `Compile command from CDB is:` 一行显示 `.clangd` 照样生效（`-nostdinc`/`-isystem` 被删、`-nostdlibinc`/`--target` 被加），结果同为 `0 errors`。→ 见 §5.2。

### A1-7 反例：把 `-nostdlibinc` 换成 `-nostdinc` → **未复现"崩溃"；复现的是"正常报错"**（rc=3，16 errors）

```
$ cd .scratch/toolchain/a16 && source /root/PrincessIDE/scripts/env.sh && cp dot-clangd-nostdinc .clangd && clangd-16 --check=kernel/kmain.c --enable-config --log=info
I[00:25:38.152] Debian clangd version 16.0.6 (15~deb12u1)
I[00:25:38.153] Features: linux+grpc
I[00:25:38.153] PID: 310626
I[00:25:38.153] Working directory: /root/PrincessIDE/.scratch/toolchain/a16
I[00:25:38.153] argv[0]: clangd-16
I[00:25:38.153] argv[1]: --check=kernel/kmain.c
I[00:25:38.153] argv[2]: --enable-config
I[00:25:38.153] argv[3]: --log=info
I[00:25:38.153] Entering check mode (no LSP server)
I[00:25:38.153] Testing on source file /root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c
I[00:25:38.155] Loading compilation database...
I[00:25:38.156] Loaded compilation database from /root/PrincessIDE/.scratch/toolchain/a16/compile_commands.json
I[00:25:38.156] Compile command from CDB is: /usr/bin/gcc -std=gnu11 -O2 -g3 -ffreestanding -fno-stack-protector -fno-pic -fno-pie -mno-red-zone -mcmodel=kernel -mno-sse -m64 -nostdinc -Iinclude -c -o build/kernel/kmain.o -nostdinc --target=x86_64-unknown-none -ffreestanding -resource-dir=/root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-16/lib/clang/16 -- /root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c
I[00:25:38.157] Parsing command...
I[00:25:38.158] internal (cc1) args are: -cc1 -triple x86_64-unknown-none -fsyntax-only -disable-free -clear-ast-before-backend -disable-llvm-verifier -discard-value-names -main-file-name kmain.c -mrelocation-model static -mframe-pointer=all -fmath-errno -ffp-contract=on -fno-rounding-math -mconstructor-aliases -ffreestanding -mcmodel=kernel -target-cpu x86-64 -target-feature -sse -disable-red-zone -tune-cpu generic -mllvm -treat-scalable-fixed-error-as-warning -debug-info-kind=constructor -dwarf-version=5 -debugger-tuning=gdb -fcoverage-compilation-dir=/root/PrincessIDE/.scratch/toolchain/a16 -nostdsysteminc -nobuiltininc -resource-dir /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-16/lib/clang/16 -I include -O2 -std=gnu11 -fdebug-compilation-dir=/root/PrincessIDE/.scratch/toolchain/a16 -ferror-limit 19 -fgnuc-version=4.2.1 -vectorize-loops -vectorize-slp -no-round-trip-args -faddrsig -D__GCC_HAVE_DWARF2_CFI_ASM=1 -x c /root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c
I[00:25:38.158] Building preamble...
I[00:25:38.175] Indexing headers...
I[00:25:38.176] Built preamble of size 202348 for file /root/PrincessIDE/.scratch/toolchain/a16/kernel/kmain.c version null in 0.02 seconds
E[00:25:38.176] [pp_file_not_found] Line 3: 'stdarg.h' file not found
I[00:25:38.176] Building AST...
E[00:25:38.207] [unknown_typename] Line 6: unknown type name 'uint16_t'
E[00:25:38.207] [unknown_typename] Line 6: unknown type name 'uint8_t'
E[00:25:38.207] [undeclared_var_use] Line 11: use of undeclared identifier 'uint8_t'
E[00:25:38.207] [undeclared_var_use] Line 16: use of undeclared identifier 'va_list'
E[00:25:38.207] [-Wimplicit-function-declaration] Line 17: call to undeclared function 'va_start'; ISO C99 and later do not support implicit function declarations
E[00:25:38.207] [undeclared_var_use] Line 17: use of undeclared identifier 'ap'
E[00:25:38.207] [undeclared_var_use_suggest] Line 18: use of undeclared identifier 'size_t'; did you mean 'sizeof'?
E[00:25:38.207] [typecheck_expression_not_modifiable_lvalue] Line 18: expression is not assignable
E[00:25:38.207] [undeclared_var_use] Line 18: use of undeclared identifier 'n'
E[00:25:38.207] [undeclared_var_use] Line 19: use of undeclared identifier 'n'
E[00:25:38.207] [undeclared_var_use] Line 20: use of undeclared identifier 'n'
E[00:25:38.207] [undeclared_var_use] Line 21: use of undeclared identifier 'n'
E[00:25:38.207] [-Wimplicit-function-declaration] Line 23: call to undeclared function 'va_end'; ISO C99 and later do not support implicit function declarations
E[00:25:38.207] [undeclared_var_use] Line 23: use of undeclared identifier 'ap'
E[00:25:38.207] [unknown_typename] Line 26: unknown type name 'uint64_t'
I[00:25:38.207] Indexing AST...
I[00:25:38.208] Building inlay hints
I[00:25:38.208] Building semantic highlighting
I[00:25:38.209] Testing features at each token (may be slow in large files)
I[00:25:38.288] All checks completed, 16 errors
[exit code: 3]
```

**关键点**：clangd **没有崩溃、没有异常退出、没有 sanitizer/断言输出**，进程正常走到 `All checks completed, 16 errors`，退出码是**文档 §2.9 里定义的「有诊断」值 3**。cc1 参数里清楚出现 `-nostdsysteminc -nobuiltininc` —— 这正是研究 §2.3 表格描述的机制，**机制被完整复现**。

---

## 4. A1-6 / A1-7 的独立复核（含 LSP 层与 clang driver 层）

为了不让结论只依赖 `--check` 这一种形态，我另外做了三层取证。

### 4.1 LSP 服务端形态（真正的 IDE 使用方式）

`.scratch/toolchain/lsp_probe.py`（本次新写）用 stdio LSP 起一个真的 clangd-16：`initialize` → `initialized` → `didOpen`（打开 `kernel/kmain.c`），收 `publishDiagnostics`，然后 `shutdown`/`exit`，报告存活与退出码。

**A1-6 配置（`-nostdlibinc`）：**

```
initialized reply received   : True
diagnostics pushed for file  : 0
server alive before shutdown : True
exit code after shutdown/exit: 0
crashed                      : NO
--- server stderr ---
(empty)
```

**A1-7 配置（`-nostdinc`）：**

```
initialized reply received   : True
diagnostics pushed for file  : 16
  - line 3    'stdarg.h' file not found
  - line 6    Unknown type name 'uint16_t' (fix available)
  - line 6    Unknown type name 'uint8_t' (fix available)
  - line 11   Use of undeclared identifier 'uint8_t' (fix available)
  - line 16   Use of undeclared identifier 'va_list'
  - line 17   Call to undeclared function 'va_start'; ISO C99 and later do not support implicit function declarations
  - line 17   Use of undeclared identifier 'ap'
  - line 18   Use of undeclared identifier 'size_t'; did you mean 'sizeof'? (fix available)
  - line 18   Expression is not assignable
  - line 18   Use of undeclared identifier 'n'
  - line 19   Use of undeclared identifier 'n'
  - line 20   Use of undeclared identifier 'n'
  - line 21   Use of undeclared identifier 'n'
  - line 23   Call to undeclared function 'va_end'; ISO C99 and later do not support implicit function declarations
  - line 23   Use of undeclared identifier 'ap'
  - line 26   Unknown type name 'uint64_t'
server alive before shutdown : True
exit code after shutdown/exit: 0
crashed                      : NO
--- server stderr ---
(empty)
```

→ **作为语言服务端，`-nostdinc` 也只是"满屏红字"，不是崩溃。**

### 4.2 clang driver 层：复现研究 §2.4 的三条对照

```
$ clang-16 -ffreestanding -nostdinc -fsyntax-only t/t1.c
[exit code: 1]
t/t1.c:1:10: fatal error: 'stdint.h' file not found
#include <stdint.h>
         ^~~~~~~~~~
1 error generated.

$ clang-16 -ffreestanding -nostdlibinc -fsyntax-only t/t1.c
[exit code: 0]

$ clang-16 -ffreestanding -nostdinc -isystem $(clang-16 -print-resource-dir)/include -fsyntax-only t/t1.c
[exit code: 0]
```

include 搜索路径对照（`clang-16 -E -v`，实测）：

```
A) -nostdlibinc
#include "..." search starts here:
#include <...> search starts here:
 /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-16/lib/clang/16/include
End of search list.

B) 不加任何 -nostd*
#include "..." search starts here:
#include <...> search starts here:
 /root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-16/lib/clang/16/include
 /usr/local/include
 /usr/include/x86_64-linux-gnu
 /usr/include
End of search list.

C) -nostdinc        ← 注意：`#include <...>` 段整个**不打印**（列表为空），只打印 "..." 段
#include "..." search starts here:
End of search list.

$ clang-16 -print-resource-dir
/root/PrincessIDE/.toolchain/prefix/usr/lib/llvm-16/lib/clang/16
```

→ §2.4 的**实质结论完全复现**：`-nostdinc` 连 clang 自带的 `stdint.h/stddef.h/stdarg.h` 都没了；`-nostdlibinc` 只砍 system include、保留 resource-dir include。

### 4.3 附加：只写 triple（不加 `-nostd*`）挡不住 glibc（D7 附加认知复核）

`.clangd` 只 `Add: [--target=x86_64-unknown-none, -ffreestanding]`、只 `Remove: [-nostdinc]`：

```
--- kernel/kmain.c ---
I [...] All checks completed, 0 errors          [exit code: 0]

--- kernel/hostleak.c (includes <string.h> <stdio.h>) ---
E [...] [pp_file_not_found] Line 5: in included file: 'bits/libc-header-start.h' file not found
E [...] [-Wimplicit-function-declaration] Line 9: call to undeclared function 'strlen'; ...
I [...] All checks completed, 2 errors          [exit code: 3]
```

→ **宿主机 glibc 的 `string.h` 被找到了**（于是才进到它内部的 `bits/libc-header-start.h`，而在 `x86_64-unknown-none` 下找不到该多架构子目录）。这独立复现了 **§2.5「target triple 本身不能阻止 glibc 头污染」** —— D7 要求"必须用 `-nostdlibinc`"是对的。

### 4.4 附加：`--check` 退出码表（§2.9）与 bear 的两个已知坑

```
$ clangd-16 --check=kernel/nope.c    → [exit code: 1]        # 目标文件不存在
$ clangd-16 --check=kernel/kmain.c   → 0 errors, [exit code: 0]
$ (A1-7)                             → 16 errors, [exit code: 3]
```

→ §2.9 的 0/3/1 三档退出码**完全复现**（可直接用于 IDE 的"工程配置自检"）。

```
$ bear --output cc_stale.json --force-preload -- make     # 树已是最新，make 无活可干
make: Nothing to be done for 'all'.
[exit code: 0]
cc_stale.json = []                                        ← §1.2 的坑，实测复现

$ make clean && bear --output cc_wrapper.json --force-wrapper -- make
gcc -std=gnu11 ... -c kernel/kmain.c -o build/kernel/kmain.o
[exit code: 0]
entries: 1                                                ← wrapper 模式在我们前缀下同样可用
```

→ **两条都对 IDE 有直接影响**：(1) 生成 CDB 前必须 `make clean`（或检测源码比上次新），否则会用**空数组覆盖**上一次的好 CDB；(2) `--force-wrapper` 模式在本前缀下同样可用（不依赖 LD_PRELOAD），可作为受限环境的退路。

---

## 5. 与调研报告不一致 / 需要更正之处

### 5.1 ⚠️ §2.4 标题的「clangd 直接崩掉」不能被复现（最重要的一条）

- **报告原文**：`### 2.4 ⭐ 本节最重要的实测结论：-nostdinc 会让 clangd 直接崩掉，要用 -nostdlibinc`
- **报告正文给出的证据**其实**全是 clang driver 层**的（`clang -fsyntax-only` → `fatal error: 'stdint.h' file not found` / `1 error generated`），**没有任何 clangd 崩溃的取证**（无 signal、无 stack trace、无 `--check` 崩溃记录）。
- **报告自己的 §6.1 第 1 行**又写成：「…clangd 直接 `pp_file_not_found`」——**这与 §2.4 标题互相矛盾**，而 `pp_file_not_found` 正是我实测到的现象。
- **我的实测（本次，四种形态）**：`--check` CLI 形态、真 LSP 服务端形态、无 `.clangd` 的裸 CDB 形态、以及 clang driver 形态，**都没有崩溃**：
  - `--check` + `-nostdinc` → `All checks completed, 16 errors`，rc=3（§2.9 定义的"有诊断"档）；
  - LSP 服务端 + `-nostdinc` → 正常推送 16 条诊断，`shutdown`/`exit` 干净退出 rc=0，`crashed: NO`；
  - 裸 CDB（`-nostdinc -isystem $(GCCINC)`，无 `.clangd`）→ 反而 **0 errors / rc=0**（gcc 的 `-isystem` 目录把内置 freestanding 头补回来了）。
- **结论**：**§2.4 的实质技术结论成立且重要**（`-nostdinc` 在 clang 下比 GCC 更狠、会连 `-nobuiltininc` 一起加、内置头全没；内核正解是 `-nostdlibinc`）——**但"会让 clangd 直接崩掉"这个措辞是错的/过度断言**。正确表述应为：「`-nostdinc` 会让 clangd 产生**满屏假错误（`pp_file_not_found` 及其连锁 16 条）**，语言服务等于不可用」，而不是"崩溃"。
  - 对决策的影响：**D7 的结论不变**（仍然必须 `Remove: [-nostdinc]` + `Add: [-nostdlibinc]`），只是理由从"防崩溃"改为"防假错误/防语言服务不可用"。若后续有 Agent 引用"会崩溃"来论证，请改为引用本节。

### 5.2 次要更正：`--enable-config` 在 clangd 16 下不是必需开关

- **报告 §4.9** 把 `--enable-config` 列为「才读 `.clangd` / `config.yaml`」的可选参数。
- **实测**：`clangd-16 --check=kernel/kmain.c`（**不加** `--enable-config`）与加上它，产出的 `Compile command from CDB is:` **完全一致**（`-nostdinc`/`-isystem` 被删、`-nostdlibinc`/`--target` 被加），同为 `0 errors`。→ 项目 `.clangd` **默认就会被读取**；`--enable-config` 传了也无害。

### 5.3 次要更正：`-E -v` 在 `-nostdinc` 下不打印 `#include <...>` 段

- 报告 §2.4 的 C 组写的是：
  ```
  C) -nostdinc
  #include <...> search starts here:
  End of search list.          ← 空的
  ```
- **实测**：列表为空时 clang **整个 `#include <...>` 段都不打印**，只打印 `#include "..." search starts here:` / `End of search list.`。语义相同（空），但逐字转录时会对不上。

### 5.4 次要更正 + 一条 D7 的实施建议：`Remove: [-W*]` 会把 `-Wall -Wextra` 一起删掉

研究 §4.9 的 `.clangd` 模板里有 `Remove: [-W*]`（本报告 A1-6 也照抄了）。**实测副作用**（看 A1-6 的 `Compile command from CDB is:` 一行）：

```
CDB 原始:  ... -O2 -g3 -Wall -Wextra -ffreestanding ... -nostdinc -isystem <gccinc> -Iinclude ...
应用后:    ... -O2 -g3          -ffreestanding ...                    -Iinclude -nostdlibinc --target=x86_64-unknown-none ...
```

→ `-Wall -Wextra` **也消失了**，语言服务会丢掉一批本该有的告警诊断。D7 说「在 `CompileFlags.Remove` 里删掉 gcc 专用 flag」，**建议实现时不要用 `-W*` 通配**，而是列出研究 §2.6 实测的那张具体黑名单：`-fno-tree-loop-distribute-patterns`、`-fconserve-stack`、`-mpreferred-stack-boundary=*`、`-fno-var-tracking-assignments`、`-fno-ipa-icf`、`-mno-direct-extern-access`（这些才是 `drv_unknown_argument` 的来源，且 `Diagnostics.Suppress` 压不掉）。这条对 P3 直接可用。

### 5.5 与研究一致、值得强调的实测确认

- `libspdlog1.10-fmt9` 在 bookworm **确实没有这个包名**（`Candidate: (none)`）；bear 靠 `libspdlog1.10` 满足符号，我这边 `bear -- make` 实测可跑。
- 研究 §0.2 的两个 `.deb` sha256 与本次下载**逐字节相同**（见附录），说明"解包安装步骤"可复现。
- bear 3.1.1 确实是 `--force-preload` / `--force-wrapper` 两种模式都能用（各实测一次）。

---

## 6. 从零重跑步骤

### 6.1 标准步骤

```bash
cd /root/PrincessIDE

# 1) 彻底清空（会重下 rust 工具链 + 全部 .deb，约 1.2 GB 下载量）
rm -rf .toolchain

# 2) 一键重建（幂等；rust 走清华镜像）
bash scripts/bootstrap-toolchain.sh          # 期望 rc=0，末尾 "all tools verified"

# 3) 激活并自检
source scripts/env.sh
clangd-16 --version                          # 期望 Debian clangd version 16.0.6 (15~deb12u1)
bear --version                               # 期望 bear 3.1.1
scripts/doctor.sh                            # 期望 rc=0，28 个工具，含 clangd-16 / clang-16 / clangd-14 / bear

# 4) 回归
scripts/smoke-boot.sh                        # 期望 rc=0, "PASS"

# 5) 语言服务自检（夹具在 .scratch/toolchain/a16，随会话产物；重建见 6.3）
cd .scratch/toolchain/a16
bear --output compile_commands.json --force-preload -- make clean all
cp dot-clangd-nostdlibinc .clangd
clangd-16 --check=kernel/kmain.c --enable-config     # 期望 rc=0, "All checks completed, 0 errors"
```

### 6.2 本次**实际执行过**的从零验证（隔离目录，未动真 `.toolchain/`）

由于 `bootstrap-toolchain.sh` 用自身路径推导 `ROOT`，我把三条脚本**原样复制**到
`.scratch/toolchain/fromzero/scripts/`（sha256 与仓库内版本**逐字节相同**，见附录），从而在一个空
`ROOT` 下做了一次真正的从零安装，**不触碰真 `.toolchain/`**（失败也不会破坏环境）。
（下面两段中的 `...` 是省略的长路径前缀，其余逐字原样；完整日志见 `.scratch/toolchain/fromzero-run.log`。）

```
$ cd /root/PrincessIDE/.scratch/toolchain/fromzero && bash scripts/bootstrap-toolchain.sh
[bootstrap] rust: installing stable toolchain (profile=minimal, +rustfmt +clippy)     ← 真从零装 rust
[bootstrap] deb: downloading 67 package(s) into .../fromzero/.toolchain/debs
[bootstrap] deb: unpacked 67 archive(s), skipped 0 already unpacked                   ← 全新解包 67 个
[bootstrap] removed 1 library file(s) that the host already provides
[bootstrap] bear: launcher written to .../fromzero/.toolchain/bin/bear
[bootstrap]   ok   clangd-16 --version  ->  Debian clangd version 16.0.6 (15~deb12u1)
[bootstrap]   ok   bear --version  ->  bear 3.1.1
[bootstrap] all tools verified
[exit code: 0]
```

随后把**最终版**脚本再复制进去并自检：

```
$ cd .scratch/toolchain/fromzero && source scripts/env.sh && clangd-16 --version | head -1
Debian clangd version 16.0.6 (15~deb12u1)
$ bear --version
bear 3.1.1
$ scripts/doctor.sh | grep -E "clangd-16|clangd-14|bear|clang-16|all required"
clangd-16 (LSP)        ok         Debian clangd version 16.0.6 (15~deb12u1)
                                  .../fromzero/.toolchain/prefix/usr/bin/clangd-16
clang-16               ok         Debian clang version 16.0.6 (15~deb12u1)
                                  .../fromzero/.toolchain/prefix/usr/lib/llvm-16/bin/clang-16
clangd-14 (legacy)     ok         Debian clangd version 14.0.6
                                  .../fromzero/.toolchain/prefix/usr/bin/clangd-14
bear (CDB)             ok         bear 3.1.1
                                  .../fromzero/.toolchain/bin/bear
doctor: all required tools present (28 resolved).
[exit code: 0]
```

（验证完成后已删除该隔离树的 `.toolchain/` 以省磁盘，日志保留在 `.scratch/toolchain/fromzero-run.log`。）

### 6.3 语言服务夹具的重建

`.scratch/toolchain/a16/` 里的文件都是纯文本（5 个源文件 + 3 个 `.clangd` 变体 + Makefile），内容见 §3 A1-6；`compile_commands.json` 由第 5 步的 `bear` 现场产出，不需要保存。

---

## 7. 遗留 / 未验证（诚实清单）

1. **未做上游 clangd zip 的对比**：研究 §0.1/§0.3 建议打包 IDE 时自带上游 zip（不带 grpc、体积小得多）。本次只装了 Debian 包，**上游 zip 仍未验证**（本次也没尝试访问 GitHub API）；`.toolchain/prefix` 已 612 MB，其中 `libLLVM-16.so.1` 123 MB、`libclang-cpp16` 11 MB、grpc/protobuf/absl 若干 —— **打包阶段应重新评估这条**。
2. **`--query-driver` 未用**：按 D7「不依赖自动推断」，本次全部显式写死 triple，未再验证 `--query-driver`。
3. **未验证 `compile_commands.json` 规模上量后的性能**（单文件夹具；背景索引内存/耗时数据见研究 §2.10）。
4. **无 KVM / 无显示器**：QEMU 走 TCG，`smoke-boot` 已验证通过，但"启动耗时"不做性能声明。
5. **未执行任何 git 命令**（提交由主 Agent 统一做）。
6. **`libclang-cpp.so.16` 等 33 字节的条目是符号链接**（指向 `libclang-cpp.so.16.1`），未单独校验链接目标存在性——`clangd-16 --version` + 真实 `--check` + LSP 三种运行形态都已证明加载正常。

---

## 附录 A：本次新增的构件与校验和

新增 11 个 `.deb`（`.toolchain/debs/`，共 47 MB）：

```
bear_3.1.1-1_amd64.deb                         360 kB
clang-16_1%3a16.0.6-15~deb12u1_amd64.deb       110 kB
clangd-16_1%3a16.0.6-15~deb12u1_amd64.deb      4.7 MB
libclang-common-16-dev_..._all.deb             655 kB
libclang-cpp16_..._amd64.deb                    11 MB
libclang1-16_..._amd64.deb                     6.6 MB
libear_3.1.1-1_amd64.deb                       172 kB
libfmt9_9.1.0+ds1-2_amd64.deb                  113 kB
libllvm16_..._amd64.deb                         23 MB
libspdlog1.10_1%3a1.10.0+ds-0.4_amd64.deb      130 kB
llvm-16-linker-tools_..._amd64.deb             1.2 MB
```

sha256 与研究 §0.2 报告的**逐字节一致**：

```
e1ae5d7494dff11a419eddcd47ec38314bdcf8b395ca9fdd459573474abc849c  clangd-16_1%3a16.0.6-15~deb12u1_amd64.deb
da759ba63b691f8a0ce1b4b61136effc9d60386e8cc29cbf0bb0e8d1d691080d  libclang-common-16-dev_1%3a16.0.6-15~deb12u1_all.deb
```

体积（实测）：

```
.toolchain/prefix        612 MB   （llvm-16 树 101 MB，llvm-14 树 99 MB，libLLVM-16.so.1 123 MB）
.toolchain/debs          120 MB
.toolchain                696 MB
```

本次改动的脚本 sha256（**最终版**，三份都在 `bash -n` 语法检查通过后计算）：

```
f623e4bbcd0243fe4c5e02ff08f86c3eba3566beb3f436008bedf33722a83356  scripts/bootstrap-toolchain.sh
4b4f32d32d12f8d24faaf1a7a5a2d1e7d113f8e1004c6a641cef23007bfc566a  scripts/env.sh
7f99061def93e0f2cd5f77eac28e5b5271263df062f1abb7276bee5a77b2d5e1  scripts/doctor.sh

（与 §6.2 隔离目录 .scratch/toolchain/fromzero/scripts/ 下的三份副本逐字节相同）
```

## 附录 B：本次会话产物（均在授权目录内）

| 路径 | 内容 |
| --- | --- |
| `scripts/bootstrap-toolchain.sh` | 改动后的幂等安装脚本 |
| `scripts/env.sh` | 改动后的激活脚本 |
| `scripts/doctor.sh` | 改动后的自检脚本 |
| `docs/reports/a1-clangd16.md` | 本文 |
| `.scratch/toolchain/run-acceptance.sh` | 验收 transcript 生成器 |
| `.scratch/toolchain/acceptance.log` | §3 全部命令 + 原始输出 + 退出码 |
| `.scratch/toolchain/bootstrap-run1.log` | A1 增量首次安装日志（11 个包） |
| `.scratch/toolchain/bootstrap-run2.log` / `run3.log` / `accept-run1.log` / `accept-run2.log` | 幂等复跑日志 |
| `.scratch/toolchain/fromzero-run.log` | 隔离目录的从零安装日志 |
| `.scratch/toolchain/smoke-boot.log` / `smoke-boot-after-env-change.log` | 回归日志 |
| `.scratch/toolchain/lsp_probe.py` | LSP 层探针（§4.1） |
| `.scratch/toolchain/lsp-probe.log` | §4.1 的原始输出（两种配置各一次） |
| `.scratch/toolchain/extra-evidence.log` | §4.2 / §4.3 / §4.4 的原始输出 |
| `.scratch/toolchain/env-hygiene.log` | `source` 三次的幂等性 + 空元素检查 + `$PRINCESSIDE_LANG_SERVICE_CLANGD` |
| `.scratch/toolchain/a16/` | 内核风格夹具 + 三份 `.clangd` 变体 + bear 产出的 `compile_commands.json` |
| `.toolchain/bin/bear` | 新增的 bear 启动器（由 bootstrap 生成） |
| `.toolchain/debs/`、`.toolchain/prefix/`、`.toolchain/.stamps/` | 新增 11 个包的缓存/解包/戳记 |
