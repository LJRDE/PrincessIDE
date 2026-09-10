# 主题 A：让 clangd 在「freestanding 交叉编译内核项目」上真正可用

> 基线环境：Debian 12 (bookworm)、x86_64、root、4 核 / 3.8G 内存（可用约 2G）、无显示器。
> 所有「实测」结论均在本工作区（`/root/PrincessIDE`，workspace-write 沙箱）内真实执行命令得到，
> 未安装任何系统包（只用 `apt-get download` + `dpkg-deb -x` 解包到工作区内前缀）。
>
> **证据分级（全文统一）**
> - **【实测】** = 已在本环境实际执行命令并保留输出
> - **【有源】** = 有权威来源（官方文档/官方仓库/发行版元数据）但本环境未实测
> - **【推测】** = 我的推断，未经证实
>
> 复现用的全部临时资产在 `/root/PrincessIDE/.researchA/`（非交付目录，可删）。
> 报告本身只写入 `docs/research/`，未触碰 `docs/spec/`、`apps/`、`crates/`、`fixtures/`、`scripts/`。

---

## 0. 前置：在「不能装系统包」的沙箱里拿到 clangd（实测）

### 0.1 可选的 clangd 版本（发行版元数据，实测）

```
$ apt-cache madison clangd-14 clangd-15 clangd-16 clangd-13 clangd
 clangd-14 | 1:14.0.6-12          | ... bookworm/main amd64 Packages
 clangd-15 | 1:15.0.6-4+b1        | ... bookworm/main amd64 Packages
 clangd-16 | 1:16.0.6-15~deb12u1  | ... bookworm/main amd64 Packages
 clangd-13 | 1:13.0.1-11+b2       | ... bookworm/main amd64 Packages
 clangd    | 1:14.0-55.7~deb12u1  | ... bookworm/main amd64 Packages
```

**结论**：bookworm main 里最新的是 **clangd-16 = 1:16.0.6-15~deb12u1**。选它。

> 【有源】clangd 官方安装页明确建议不要用发行版包，而是用上游 zip：
> "For best results, use the [most recent version](https://github.com/clangd/clangd/releases/latest) of clangd."
> 以及 Standalone .zip releases: <https://github.com/clangd/clangd/releases/latest>
> —— 这一点对 PrincessIDE 很重要：**打包 IDE 时应自带上游 clangd zip，而不是依赖用户装 clangd-16**（见 §0.3）。
> 本次因沙箱网络代理对 `api.github.com` 返回 404（实测），**未能下载上游 zip 做对比实测**，标为未验证。

### 0.2 解包安装（实测，可直接复用）

> ⚠️ **共享目录坑（本次真实踩到）**：我最初把 prefix 解到 `/root/PrincessIDE/.toolchain/prefix`，
> 但 `.toolchain/` 是**多个 Agent 共用的目录**，中途被别的 Agent 覆盖清理，clangd 可执行文件消失
> （`exec: …/bin/clangd: not found`，`rc=127`）。
> **教训：解包前缀必须放在自己独占的目录下。** 本报告最终使用
> **`/root/PrincessIDE/.researchA/prefix`**（286 MB），下面的命令已按此修正。

```bash
mkdir -p /root/PrincessIDE/.researchA/aptdl /root/PrincessIDE/.researchA/prefix
cd /root/PrincessIDE/.researchA/aptdl
apt-get download clangd-16 libclang-cpp16 libllvm16 libclang-common-16-dev \
                 libgrpc++1.51 libgrpc29 libprotobuf32 libc-ares2 libre2-9
# 另外为了拿到真正的 clang driver（用于 -MJ / 交叉编译对比实验）
apt-get download clang-16 libclang1-16 llvm-16-linker-tools
for f in *.deb; do dpkg-deb -x "$f" /root/PrincessIDE/.researchA/prefix/; done
```

实测产物（校验和，便于复现）：

```
e1ae5d7494dff11a419eddcd47ec38314bdcf8b395ca9fdd459573474abc849c  clangd-16_1%3a16.0.6-15~deb12u1_amd64.deb
da759ba63b691f8a0ce1b4b61136effc9d60386e8cc29cbf0bb0e8d1d691080d  libclang-common-16-dev_1%3a16.0.6-15~deb12u1_all.deb
```

```
$ .researchA/prefix/usr/lib/llvm-16/bin/clangd --version
Debian clangd version 16.0.6 (15~deb12u1)
Features: linux+grpc
Platform: x86_64-pc-linux-gnu
```

解包后 prefix 体积约 **286 MB**（另一次实测中带 symlink 的目录统计为 618 MB），其中 `clangd` 可执行文件 24 MB、`libclang-cpp.so.16` 63 MB。
（Debian 的 clangd 带 `linux+grpc` 特性，所以额外拖进 grpc/protobuf/absl 依赖 —— 这是打包体积的主要浪费。）

**关键坑（实测）**：解包后**必须设 `LD_LIBRARY_PATH`**。clangd 有 `RUNPATH = $ORIGIN/../lib`，
但 `DT_RUNPATH` 只对可执行文件的**直接**依赖生效、不传递，因此 grpc 的间接依赖找不到：

```
$ readelf -d usr/lib/llvm-16/bin/clangd | grep -i runpath
 0x000000000000001d (RUNPATH)  Library runpath: [$ORIGIN/../lib]

$ env -u LD_LIBRARY_PATH ldd usr/lib/llvm-16/bin/clangd | grep "not found"
	libprotobuf.so.32 => not found
	libgpr.so.29 => not found
	libgrpc.so.29 => not found
	libgrpc++.so.1.51 => not found
	libLLVM-16.so.1 => not found
# 把这些直接依赖 symlink 进 $ORIGIN/../lib 之后，仍有 4 个找不到：
	libaddress_sorting.so.29 => not found
	libupb.so.29 => not found
	libre2.so.9 => not found
```

**可用的启动器（实测可跑）**：

```bash
#!/bin/sh
# /root/PrincessIDE/.researchA/clangd-env.sh  —— 实测可用
PREFIX=/root/PrincessIDE/.researchA/prefix
export LD_LIBRARY_PATH="$PREFIX/usr/lib/x86_64-linux-gnu:$PREFIX/usr/lib/llvm-16/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$PREFIX/usr/lib/llvm-16/bin/clangd" "$@"
```

### 0.3 给 PrincessIDE 的打包建议

- **【实测】** Debian 解包方案在无 root 目标机上完全可行，但需要 `LD_LIBRARY_PATH` 包装器，且体积 618 MB。
- **【有源】** 更好路径是上游 clangd zip（<https://github.com/clangd/clangd/releases/latest>），官方即为「standalone 可直接下载运行」的定位；**具体依赖是否静态、体积多少：未验证**（代理挡住了 GitHub API）。
- **【推测】** 上游 zip 不带 grpc，体积和依赖都会小很多；PrincessIDE 首发应只自带 C/C++ 用的 clangd，不要带 Debian 的 grpc 变体。

### 0.4 实测用到的其他构件

| 构件 | 版本 | 来源 |
|---|---|---|
| bear | **3.1.1-1**（Debian bookworm；上游已到 **4.2.2 / 2026-09-05**，Rust 重写） | 【实测】`apt-cache policy bear` → `Candidate: 3.1.1-1`；成功解包并运行；【有源】<https://github.com/rizsotto/Bear/releases.atom> |
| clang (driver) | **16.0.6 (15~deb12u1)** | 【实测】`clang --version` |
| cmake | **3.25.1** | 【实测】`cmake --version` |
| ninja | **缺失** | 【实测】`cmake -G Ninja` 报 `unable to find a build program corresponding to "Ninja"` |
| nasm | **2.16.01-1**（未装） | 【实测】`apt-cache policy nasm` → `Candidate: 2.16.01-1` |
| compiledb (PyPI) | **0.10.7**（2025-03-23） | 【实测】`pip3 download --no-deps compiledb` → `compiledb-0.10.7-py3-none-any.whl`；Debian **未打包**（`apt-cache search compiledb` 为空）；【有源】<https://pypi.org/project/compiledb/> |
| rustc / cargo | **1.63.0+dfsg1-2**（bookworm，未装） | 【实测】`apt-cache policy rustc` |
| rust-analyzer / zig / zls | **Debian 未打包** | 【实测】`apt-cache policy` 无 Candidate |
| 当前日期（用于判断"最新版"） | **2026-09-10** | 【实测】`date -u` |

---

## 1. 为内核工程生成 `compile_commands.json`：四条路径

### 1.1 造一个「内核风格」测试工程（实测夹具）

`/root/PrincessIDE/.researchA/proj/`：

```
include/kernel/types.h      # 位域页表项 + uint64_t 等（依赖 <stdint.h>/<stddef.h>）
include/kernel/panic.h      # trap_frame / panic() 声明
kernel/kmain.c              # kmain + PMM 假实现 + inline asm outb
arch/x86_64/boot.S          # GAS: .multiboot 头 / 栈 / _start
arch/x86_64/isr.asm         # NASM 语法: BITS 64 / %macro / push qword
cxx/klass.cpp               # 内核 C++: namespace/class/template/placement new
Makefile  linker.ld  compile_commands.json  .clangd
```

Makefile 最终版（**这是"能真正编译过"的版本**，见 §2.4 的反例）：

```make
CC      := gcc
AS      := gcc
NASM    := nasm
GCCINC  := $(shell $(CC) -print-file-name=include)
CFLAGS  := -std=gnu11 -O2 -g3 -Wall -Wextra -ffreestanding -fno-stack-protector \
           -fno-pic -fno-pie -mno-red-zone -mcmodel=kernel -mno-sse -m64 \
           -nostdlib -nostdinc -isystem $(GCCINC) -Iinclude
ASFLAGS := -ffreestanding -fno-pic -mno-red-zone -mcmodel=kernel -m64
OBJDIR  := build
# ... wildcard 收集 kernel/*.c arch/x86_64/*.S arch/x86_64/*.asm ...
$(OBJDIR)/%.o: %.c
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS) -c $< -o $@
$(OBJDIR)/%.S.o: %.S
	@mkdir -p $(dir $@)
	$(AS) $(ASFLAGS) -c $< -o $@
$(OBJDIR)/%.asm.o: %.asm
	@mkdir -p $(dir $@)
	$(NASM) -f elf64 -g -F dwarf $< -o $@
```

### 1.2 路径 ①：make + bear 3.1.1 —— **本环境实测成功，首选**

**【实测】** bear 真的在本沙箱跑起来了。因为不在 `/usr`，需要显式覆盖 bear 的 4 个路径参数：

```bash
P=/root/PrincessIDE/.researchA/prefix
export LD_LIBRARY_PATH=$P/usr/lib/x86_64-linux-gnu
make clean
$P/usr/bin/bear --output /tmp/cc_bear.json --force-preload \
  --bear-path $P/usr/bin/bear \
  --library   $P/usr/lib/x86_64-linux-gnu/bear/libexec.so \
  --wrapper   $P/usr/lib/x86_64-linux-gnu/bear/wrapper \
  --wrapper-dir $P/usr/lib/x86_64-linux-gnu/bear/wrapper.d \
  -- make build/kernel/kmain.o build/arch/x86_64/boot.S.o
# → 生成 2 条正确条目
```

`bear --version` → `bear 3.1.1`；`bear --help` 的关键节选（**原文实测**）：

```
Usage: bear <command> [--output <arg>] [--verbose] -- ...

commands
  intercept
  citnames

  --output <arg>       path of the result file (default: compile_commands.json)
  --append             append result to an existing output file
  --config <arg>       path of the config file
  --force-preload      force to use library preload
  --force-wrapper      force to use compiler wrappers
  --bear-path <arg>    path to the bear executable (default: /usr/bin/bear)
  --library <arg>      path to the preload library (default: /usr/$LIB/bear/libexec.so)
  --wrapper <arg>      path to the wrapper executable (default: /usr/lib/x86_64-linux-gnu/bear/wrapper)
  --wrapper-dir <arg>  path to the wrapper directory (default: /usr/lib/x86_64-linux-gnu/bear/wrapper.d)
```

实测产出的 JSON（节选，`python3 -m json.tool` 输出）：

```json
[
  {
    "arguments": [
      "/usr/bin/gcc", "-std=gnu11", "-O2", "-g3", "-Wall", "-Wextra",
      "-ffreestanding", "-fno-stack-protector", "-fno-pic", "-fno-pie",
      "-mno-red-zone", "-mcmodel=kernel", "-mno-sse", "-m64",
      "-nostdinc", "-isystem", "/usr/lib/gcc/x86_64-linux-gnu/12/include",
      "-Iinclude", "-c", "-o", "build/kernel/kmain.o", "kernel/kmain.c"
    ],
    "directory": "/root/PrincessIDE/.researchA/proj",
    "file": "/root/PrincessIDE/.researchA/proj/kernel/kmain.c",
    "output": "/root/PrincessIDE/.researchA/proj/build/kernel/kmain.o"
  },
  { "arguments": ["/usr/bin/gcc", "-ffreestanding", "-fno-pic", "-mno-red-zone",
                  "-mcmodel=kernel", "-m64", "-c", "-o",
                  "build/arch/x86_64/boot.S.o", "arch/x86_64/boot.S"], ... }
]
```

**两种模式都实测可用**：
- `--force-preload`（默认路径）：2 条条目 ✅
- `--force-wrapper`：1 条条目 ✅（不带 LD_PRELOAD，靠 wrapper 目录劫持 —— 适合容器/受限环境）

**`--query-driver` 无关**：bear 记录的是原始 `gcc` 命令行，所以后续 clangd 仍需自己的 include 修正（§2）。

**⚠️ 版本分野（很重要，容易踩）：Debian bookworm 装的是 bear 3.1.1，而上游已经是 4.2.x。**

| | Debian bookworm（**实测**） | 上游最新（**有源**） |
|---|---|---|
| 版本 | `3.1.1-1` | **4.2.2（2026-09-05）**，前序 4.2.1(2026-08-16)/4.2.0(2026-08-01)/4.1.1(2026-04-04)/4.1.0(2026-03-28)/4.0.4(2026-02-28) |
| 语言 | C++（`libear` + `libexec.so`） | **Rust 重写**（crate：`bear-driver`/`intercept-supervisor`/`intercept-preload`/`bear-wrapper`/`crates/semantic`） |
| 切换 preload / wrapper | **CLI flag**：`--force-preload` / `--force-wrapper`（实测存在于 `--help`） | **只能靠配置文件** `intercept.mode: preload\|wrapper`；命令行表里已无这两个 flag |
| 配置文件 | 帮助里只有 `--config <arg>`，未给默认名 | **`bear.yml`（不是 `bear.config`）**，且顶层 `schema: "4.2"` 是必填键（"It is the one key with no default"） |
| 输出默认名 | `compile_commands.json`（实测 `--help` 原文） | 4.2.0 起 `bear intercept` 不再默认 `events.json`，`--output` 变必填；4.2.2 新增 `--overwrite` |

【有源】来源：<https://github.com/rizsotto/Bear/releases.atom>（版本与日期）、
<https://rizsotto.github.io/Bear/reference/configuration.html>（`bear.yml` / `schema` / `intercept.mode`）、
<https://rizsotto.github.io/Bear/reference/command-line.html>（4.x 命令行表）、
<https://rizsotto.github.io/Bear/understanding/how-it-works.html>（两种拦截机制）。

**bear 的坑（实测 + 有源，逐条标注）**：
- **【实测】** Debian 的依赖里写着 `libspdlog1.10-fmt9`，但 `apt-cache policy libspdlog1.10-fmt9` → **`Candidate: (none)`**（bookworm main 没这个包名）。
  实际能跑是因为 `libspdlog1.10`（`1:1.10.0+ds-0.4`）能满足符号需求。→ **离线部署时别照抄 Depends 字段，直接抓 `libspdlog1.10`。**
- **【实测】** bear 只记录**真正被执行**的编译命令。本环境 `--force-wrapper` 实验里只编了 1 个文件 → 只 1 条条目。
- **【有源】** 官方文档把这个坑写得很直白（<https://rizsotto.github.io/Bear/guides/recipes/compile-commands-for-makefile.html>）：
  > "Make only rebuilds what is out of date, and Bear only records commands the build actually executes, so a `make` with nothing to do records nothing: running `bear -- make` a second time in a row against an unchanged tree **overwrites `compile_commands.json` with an empty `[]`**."
  → 修法：先 `make clean`，或用 `--append`。**PrincessIDE 生成 CDB 前必须强制 clean（或检测"源码比上次新"）。**
- **【有源】preload 模式的硬限制**："It cannot see into a statically linked executable, because such a binary never goes through the dynamic linker in the first place."（同上 how-it-works 页）
  → 如果内核工具链里有**静态链接**的编译器包装器，preload 模式会**静默漏条目**。
- **【有源】wrapper 模式的硬限制**："only for compilers the build actually reaches through `PATH` or a variable Bear resolved at startup: a build step that discovers the compiler on its own (a `./configure` probing for `cc`) needs to run under Bear too"（同上）。
- **【有源】两种模式不能互相兜底**（与 3.1.1 可强制切换不同）：
  > "Neither mechanism is a fallback for the other. Where preload is unavailable, Bear does not quietly switch to wrapper mode; forcing preload there is a startup error that names wrapper mode as the alternative, and the build does not run."
- **【有源】容器**："Bear must run inside the container, as part of the build it observes; `bear -- docker exec ...` from the host does not work."（<https://rizsotto.github.io/Bear/platforms/linux.html>）
  → **对 PrincessIDE 的直接影响**：如果将来支持"在 Docker 里构建内核模板"，必须把 bear 放进容器内、在容器里跑 `bear -- make`。
- **【有源】WSL2**：镜像网络模式（mirrored networking）"breaks, producing an empty or short `compile_commands.json` with no other error"（<https://rizsotto.github.io/Bear/guides/troubleshooting.html>）。
- **【有源】交叉工具链**：libexec.so 报 `version 'GLIBC_2.33' not found` 时，"The intercepted invocation fails outright, so that command is **silently missing** from the database"（同上）。
  → **silently missing 是 bear 最危险的失败模式**：IDE 必须做"条目数 vs 源文件数"的健全性校验，不能只看 bear 退出码。
- **【有源】dry-run 也内建在 4.x 里**：`make -n | bear parse-sh`；递归 make 用 `make -nw | bear parse-sh`。
  官方明说保真度更低：递归 make 不总把 `-n` 传下去、依赖生成文件的目标不会打印、只理解 POSIX shell 的一个子集
  （子 shell、命令替换、here-doc 不支持，未解析的行会被跳过并在 stderr 报行号）。
（3.1.1 是否有 `parse-sh` 子命令：**未实测**。）

**配置项（有源，对 CDB 质量很关键）**（<https://rizsotto.github.io/Bear/reference/configuration.html>）：
- `format.paths.directory` / `format.paths.file`：可取 `as-is|canonical|relative|absolute`
  → 对 §2.9 里"`directory` 写错导致相对 `-I` 全废"这个坑，**建议设成 `absolute`**。
- `duplicates.match_on`：默认 `[directory, file]`，可选 `file/arguments/directory/command/output`。
- `format.entries.use_array_format`（默认 true，即 `arguments` 数组）、`format.entries.include_output_field`。
- `format.arguments.from_response_files`、`format.arguments.from_environment`、`compilers[].as/ignore`、
  `sources.directories[].path/action`、`headers.enabled`（默认 false）/`headers.strategy`。
- 配置文件搜索顺序：**先 cwd，再 XDG/%LOCALAPPDATA%；"The first file found is loaded; the rest are not consulted."**
  用 `RUST_LOG=info bear -- true` 打印最终生效的配置（Rust 版特性）。

**手工命令行（不依赖 bear）的官方背书**：
- 【有源】<https://clang.llvm.org/docs/JSONCompilationDatabase.html>：
  > "Clang has the ability to generate compilation database fragments via `-MJ argument`. You can concatenate those fragments together between `[` and `]` to create a compilation database."
  （这与我在 §1.5 实测的"必须自己包 `[` `]`、多次追加会产出非法 JSON"完全一致。）
- 【有源】同页对条目格式的建议：优先用 `arguments` 而非 `command` —— "**arguments** is preferred, as shell (un)escaping is a possible source of errors"；约定把文件命名为 `compile_commands.json` 并放在构建目录顶部。
- 【有源】来自 Bear 文档的其它 CDB 来源：`ninja -t compdb > compile_commands.json`、Qbs `qbs generate --generator clangdb`、Waf `clangdb`、Bazel 需 Hedron extractor（<https://rizsotto.github.io/Bear/guides/recipes/cmake.html>）。
  → **`ninja -t compdb` 是 Ninja 用户的零依赖路径**（本环境无 ninja，**未实测**）。

### 1.3 路径 ②：make + compiledb

- **【实测】** Debian bookworm **没有打包** `compiledb`（`apt-cache search compiledb` 无输出）。
- **【实测】** PyPI 有 **0.10.7**：`pip3 download --no-deps compiledb` → `compiledb-0.10.7-py3-none-any.whl (26 kB)`。
  → 可以在 IDE 的离线依赖里 vendored 一个 wheel，或 `pip install --target <workspace>` 到工作区。
- **【有源】版本与元数据**（<https://pypi.org/project/compiledb/>）：最新 **0.10.7，Released Mar 23, 2025**；
  `Requires: Python >=3.3`；License GPLv3；Development Status "4 - Beta"。
  上一版是 0.10.1（2019-07-11）—— 说明这个工具**维护不活跃**（6 年只发过 0.10.x 补丁）。
- **【有源】⚠️ 命令行形式容易记错**：`-n` **不是 make 的 `--dry-run`**，而是 compiledb 自己的 `--no-build`，
  必须写在 `make` **之前**。逐字：
  > "the build step can be skipped using the `-n` or `--no-build` options. `$ compiledb -n make`"
  其他实测无关但有用的形式（同页逐字）：
  ```
  $ compiledb make                                  # 执行构建并记录
  $ compiledb -n make                               # 只 dry-run
  $ compiledb make -f core/main.mk -C build         # 透传给 make
  $ compiledb --parse build-log.txt                 # 解析已有日志
  $ make -Bnwk | compiledb -o-                      # 从 stdin 解析
  $ compiledb --command-style make                  # 输出 "command" 字符串而非 "arguments" 数组
  ```
- **【有源】原理与自述的优势**（同页逐字）：
  > "since in most cases it **doesn't need a clean build** (as the mentioned tools do) to generate the compilation database file, to achieve this it uses the make options such as `-n`/`--dry-run` and `-k`/`--keep-going` to extract the compile commands"

  → 对内核这种全量构建很慢的项目，"不需要 clean build" 是个真实优点（对比 Bear 的 §1.2 空数组坑）。
- **【有源】`make -n` 的语义限制（来自 GNU make 官方手册，权威定义）**：
  - 只打印不执行：<https://www.gnu.org/software/make/manual/html_node/Instead-of-Execution.html>
    > "'-n' … 'No-op'. Causes `make` to print the recipes that are needed to make the targets up to date, but not actually execute them. **Note that some recipes are still executed, even with this flag**"
  - 递归 make / `+` 开头的行**仍会执行**：
    > "The '-n', '-t', and '-q' options do not affect recipe lines that begin with '+' characters or contain the strings '$(MAKE)' or '${MAKE}'."
  - included makefile 的规则会被重做：
    > "Also any recipes needed to update included makefiles are still executed"
  - **`$(shell ...)` 不受 `-n` 抑制**（<https://www.gnu.org/software/make/manual/html_node/Shell-Function.html>）：
    > "The commands run by calls to the `shell` function are run when the function calls are expanded."
  → **这四条正是 "make -n 解析法" 的根本脆弱之处**：命令会被重复执行（副作用）、递归子 make 的命令可能拿不到、`$(shell)` 的副作用挡不住。
- **【未找到权威来源】** compiledb 对 `ccache` 包装、`$(CC)` 被覆盖（内核常见的 `CROSS_COMPILE` + `CC=ccache clang`）、
  多配置重复命令的具体失败模式 —— 官方文档没写，**本次未实测，故不列为结论**。

**给 PrincessIDE 的建议**：compiledb 的"纯文本解析"性质意味着**它不需要执行构建**（对内核这种重构建项目省时间），
但也意味着容易静默漏条目。做法：compiledb 生成后**校验条目数 ≈ 源文件数**，不达标就 fallback 到 bear。

### 1.4 路径 ③：手写拦截（wrapper shim）—— 已实测，最可控

这是本次唯一「完全自己掌控」的路径，实测可用：

```python
#!/usr/bin/env python3
# /root/PrincessIDE/.researchA/shim/cdb-shim.py  —— 实测可用（JSONL 追加，天然并发安全）
import json, os, sys
REAL = os.environ.get("CDB_REAL_PREFIX", "/usr/bin")
name = os.path.basename(sys.argv[0]); real = os.path.join(REAL, name)
argv = sys.argv[1:]
out  = os.environ.get("CDB_OUT", "compile_commands.json")
entry = {"directory": os.getcwd(), "file": None, "arguments": [real] + argv}
SRC_EXT = (".c", ".cc", ".cpp", ".cxx", ".S", ".s", ".asm")
for a in argv:
    if a.startswith("-"): continue
    if a.endswith(SRC_EXT): entry["file"] = os.path.abspath(a); break
if entry["file"]:
    with open(out, "a") as f:            # 追加一行 = 一个 JSON 对象
        f.write(json.dumps(entry) + "\n")
os.execv(real, [real] + argv)
```

```bash
for t in gcc g++ cc; do ln -sf cdb-shim.py .researchA/shim/$t; done
PATH=/root/PrincessIDE/.researchA/shim:$PATH \
  CDB_OUT=$PWD/cdb-raw.jsonl make build/kernel/kmain.o
```

实测捕获结果：

```json
{"directory": "/root/PrincessIDE/.researchA/proj", "file": "/root/PrincessIDE/.researchA/proj/kernel/kmain.c",
 "arguments": ["/usr/bin/gcc", "-std=gnu11", ..., "-c", "kernel/kmain.c", "-o", "build/kernel/kmain.o"]}
```

再做一次 JSONL → `compile_commands.json` 的转换（`json.dump(recs, ...)`）即可。clangd 实测能加载。

**为什么推荐 JSONL 而不是直接写 JSON 数组**：
- 不依赖收尾钩子：`make -k`、`Ctrl-C`、某个 TU 编译失败都不会留下半截坏 JSON；
- `make -jN` 并行时多进程追加（`O_APPEND` 单行 < PIPE_BUF 的写入是原子的）不会互相覆盖；
- 转换成正式文件是最后一步，可以顺便排序、去重、裁剪。

**shim 路径的坑**：
- **PATH 必须前置**，而且 Makefile 里若写了 `CC := /usr/bin/gcc` 这类绝对路径，shim 完全失效。
- `@response-file` 参数（`gcc @args.rsp`）需要展开才能拿到真实源文件名，本 shim 未处理。
- `ccache gcc ...` / `distcc` 会把真实编译器变成 shim 的"下一个参数"，需要额外解包。
- 交叉工具链前缀（`x86_64-elf-gcc`）要为每个名字建一个符号链接。

### 1.5 路径 ④：`clang -MJ`（原生编译数据库输出）—— 已实测，坑很深

**【实测】** `clang --help` 确认该选项存在：

```
  -MJ <value>             Write a compilation database entry per input
```

`gcc` **没有**这个选项：

```
$ gcc -MJ /tmp/mj.json -c kernel/kmain.c ...
gcc: error: unrecognized command-line option '-MJ'; did you mean '-J'?
```

实测输出（一个 GAS `.S` 输入）：

```json
{ "directory": "/root/PrincessIDE/.researchA/proj", "file": "arch/x86_64/boot.S",
  "output": "/tmp/boot-9e52ac.s",
  "arguments": ["/.../clang", "-xassembler-with-cpp", "arch/x86_64/boot.S", "-o", "/tmp/boot-9e52ac.s",
                "-std=gnu11", "-O2", "-ffreestanding", "-nostdlibinc",
                "--target=x86_64-unknown-none", "-I", "include", "-c", "--target=x86_64-unknown-none"]}
```

**三个必须知道的坑（全部实测）**：

1. **编译失败则什么都不写**。我给 `-MJ` 的文件因为 inline asm 报错，`/tmp/mj1.json` 是**空文件**（0 字节）。
   → 用 `-MJ` 生成数据库必须保证全绿编译，或者接受部分缺失。
2. **不能一次多个输入 + `-o`**：
   ```
   $ clang -c /tmp/ok.c /tmp/outb.c -o /dev/null -MJ /tmp/mj_multi.json
   clang: error: cannot specify -o when generating multiple output files
   ```
   → 一次调用一个输入。
3. **多次调用写同一个 `-MJ` 文件会产出非法 JSON**（对象之间用 `,\n` 拼接、没有 `[ ]`）：
   ```
   $ clang ... -MJ /tmp/mj_multi2.json && clang ... -MJ /tmp/mj_multi2.json
   $ python3 -c "import json;json.load(open('/tmp/mj_multi2.json'))"
   json.decoder.JSONDecodeError: Extra data: line 1 column 325 (char 324)
   ```
   → 必须自己包 `[` `]` 并去掉最后一个逗号（或改成 per-file `-MJ` 再合并）。
4. `"output"` 字段是**临时 `.s` 路径**（预处理产物），对 IDE 无意义 —— clangd 会忽略它，但自己写工具时不要读它。
5. 【有源】`-MJ` 是 clang 特有；`compile_commands.json` 本身的字段定义见
   <https://clang.llvm.org/docs/JSONCompilationDatabase.html>。

**适用判断**：内核项目若已经用 clang 做交叉编译（`--target=x86_64-unknown-none-elf`），`-MJ` 是**零额外依赖**的方案，
只需在 Makefile 的每个编译规则上加 `-MJ $@.json`，最后 `jq -s . *.json > compile_commands.json`。
若用 gcc 交叉工具链，此路不通。

### 1.6 路径 ⑤：CMake —— 本环境实测通过

**【有源】权威语义**（<https://cmake.org/cmake/help/latest/variable/CMAKE_EXPORT_COMPILE_COMMANDS.html>，页面为 CMake **4.4.3** 文档）：
- 逐字：**"Added in version 3.5."**
- 逐字（**回答"哪些生成器支持"**）：**"This option is implemented only by Makefile Generators and Ninja Generators. It is ignored on other generators."**
- 逐字：**"This option currently does not work well in combination with the `UNITY_BUILD` target property or the `CMAKE_UNITY_BUILD` variable."**
- 逐字（**Visual Studio / Xcode 完全无效**）："The Visual Studio and Xcode generators fall into 'other generators': setting the variable with either of those has no effect, and no `compile_commands.json` appears no matter how the project builds."
- 逐字：由同名**环境变量**初始化，并初始化所有 target 的 `EXPORT_COMPILE_COMMANDS` 属性。
- ⚠️ **口径冲突提醒**：clang 官方文档 <https://clang.llvm.org/docs/JSONCompilationDatabase.html> 至今还写着
  "Currently CMake (**since 2.8.5**) supports generation of compilation databases for Unix Makefile builds (**Ninja builds in the works**)"
  —— 这**明显过时**（Ninja 早已支持）。**只引 CMake 官方那句，不要引 clang 那句。**

**【实测】** cmake 3.25.1，`-G "Unix Makefiles"`：

```cmake
cmake_minimum_required(VERSION 3.25)
project(kernel C)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)
execute_process(COMMAND gcc -print-file-name=include OUTPUT_VARIABLE GCC_INCLUDE OUTPUT_STRIP_TRAILING_WHITESPACE)
add_compile_options(-std=gnu11 -O2 -g3 -Wall -ffreestanding -fno-stack-protector -fno-pic
                    -fno-pie -mno-red-zone -mcmodel=kernel -mno-sse -m64 -nostdlib -nostdinc
                    -isystem ${GCC_INCLUDE} -I${CMAKE_SOURCE_DIR}/include)
add_library(kernel STATIC src/mmap.c)
```

```
$ cmake -S . -B build-make -G "Unix Makefiles" -DCMAKE_TOOLCHAIN_FILE=tools.cmake
$ ls build-make/compile_commands.json      # 存在，466 字节，1 条条目
```

生成的条目：

```json
[{"directory": ".../build-make",
  "command": "/usr/bin/gcc -std=gnu11 ... -nostdinc -isystem /usr/lib/gcc/x86_64-linux-gnu/12/include -I.../include -o CMakeFiles/kernel.dir/src/mmap.c.obj -c .../src/mmap.c",
  "file": ".../src/mmap.c", "output": "CMakeFiles/kernel.dir/src/mmap.c.obj"}]
```

clangd 加载后 **0 errors**（配合 `.clangd` 的 `Remove: [-nostdinc, -isystem]` + `Add: [-nostdlibinc, --target=...]`）。

**CMake 的坑（实测）**：

1. **`directory` 是 build 目录，不是源码目录。** 生成的 `-I` 恰好是绝对路径所以能工作；
   若项目用相对 `-I`，把 `compile_commands.json` 拷到源码根会因为 `directory` 不匹配而全面报 `'xxx.h' file not found`（§2.7 复现了同类故障）。
2. **官方做法**（【有源】<https://clangd.llvm.org/installation>）：
   > "`compile_commands.json` will be written to your build directory. If your build directory is `$SRC` or `$SRC/build`, clangd will find it. Otherwise, symlink or copy it to `$SRC`, the root of your source tree."
   ```bash
   ln -s ~/myproject-build/compile_commands.json ~/myproject/
   ```
3. **`-G Ninja` 在本环境直接失败**（ninja 未安装，且不能 apt install）：
   ```
   CMake Error: CMake was unable to find a build program corresponding to "Ninja".
   ```
   → PrincessIDE 的 CMake 模板必须默认 `-G "Unix Makefiles"`，或在向导里检测 `ninja` 是否存在再切。
4. **一个"空变量吞掉下一个 flag"的真实反例（实测）**：我最初把 `-isystem ${GCC_INCLUDE}` 写在 `execute_process` **之前**，
   `${GCC_INCLUDE}` 为空，于是命令行变成 `-isystem -I/root/.../include`，clangd 把它解析成
   `-isystem` 的参数是 `-I/root/.../include`：`-isystem -I/root/.../include`（日志原文），
   结果 `kernel.h' file not found`。**生成 CDB 时务必校验每条命令里没有裸露在末尾/紧跟 `-I`/`-isystem` 的空值。**
5. **【有源】导出的命令 ≠ 实际执行的命令**（<https://rizsotto.github.io/Bear/guides/recipes/cmake.html>，逐字）：
   > "CMake's export records the command it intends to run, **generated at configure time**. Three situations make that different from what the build actually executes: a compiler launcher in front of the compiler (`CMAKE_C_COMPILER_LAUNCHER`, `CMAKE_CXX_COMPILER_LAUNCHER`, or a `ccache`/`distcc` setup **baked into the toolchain file**); a wrapper script standing in for the compiler; a custom command or custom target that compiles something outside CMake's own compile rules."

   → **对交叉编译内核工程很关键**：如果把 `CROSS_COMPILE`/`-isystem <sysroot>`/`ccache` 塞进 toolchain file，
   CMake 导出的命令可能与真实编译不一致。官方给的替代做法是 `bear -- cmake --build build`。
   （**【未找到权威来源】** "toolchain file + `CMAKE_EXPORT_COMPILE_COMMANDS` 记录的就是 target flag" 这句字面表述，
   CMake 官方文档里没有；只能引它"导出 exact compiler calls"的原话 + clangd 的 `CompileFlags.Compiler`/`--query-driver` 来论证，
   **不要凭空断言 CMake 会写入 sysroot/freestanding flag**。）
6. **【有源】其它生成器自带导出**（同页逐字）：
   - "Ninja: **`ninja -t compdb > compile_commands.json`**"
   - "Qbs: `qbs generate --generator clangdb`"
   - "Waf: load the `clang_compilation_database` tool from `wscript`, then run `waf clangdb`"
   - "Bazel: no native flag" → 需 <https://github.com/hedronvision/bazel-compile-commands-extractor>
     （Bazel 官方对 Bear 这类拦截技术"resistant"，clang 文档逐字）
   - "Clang itself: pass `-MJ <fragment>.json` to every invocation and concatenate the fragments into a single JSON array"
   → **`ninja -t compdb` 是 Ninja 用户的零依赖路径**（本环境无 ninja，**未实测**）。

### 1.7 路径 ⑥：cargo / zig（**本环境完全未实测**，下面全部是【有源】结论）

**【实测】本环境的可用性现状**：

```
$ apt-cache policy rustc cargo rust-analyzer zig zls
rustc          Candidate: 1.63.0+dfsg1-2   (bookworm/main)
cargo          (无 Candidate 行)
rust-analyzer  (无 Candidate)
zig            (无 Candidate)
zls            (无 Candidate)
```

→ **bookworm 里没有 rust-analyzer、没有 zig、没有 zls**；rustc/cargo 只有 1.63（2022 年）。
因此 **cargo / zig 两条路径本次完全是纸上调研，没有任何本机行为证据。**

#### Cargo：**没有**任何稳定的内置 compile_commands 输出

**【有源】三条独立证据**：

1. `cargo-build(1)` 的完整选项表里**没有** compile database 相关选项：
   <https://doc.rust-lang.org/cargo/commands/cargo-build.html>
   `--message-format <fmt>` 的合法取值逐字为
   "`human` (default) … `short` … `json` … `json-diagnostic-short` … `json-diagnostic-rendered-ansi` … `json-render-diagnostics`"。
   `-Z` 逐字："Unstable (nightly-only) flags to Cargo."
2. nightly 的 unstable 特性表里**已经没有 `build-plan`**：
   <https://doc.rust-lang.org/nightly/cargo/reference/unstable.html>
   （Information and metadata 一类只剩 `unit-graph`、`cargo rustc --print`、Build analysis、`rustc-unicode`）
3. **`-Z build-plan` 已被彻底移除** —— Cargo **1.93 (2026-01-22)** changelog，Nightly only 段逐字：
   > "`build-plan`: Remove the unstable feature `build-plan` entirely. The Cargo team are looking forward to other alternatives like plumbing commands, --unit-graph, and structured logging helping to fill the gap. [#16212]"
   <https://doc.rust-lang.org/nightly/cargo/CHANGELOG.html>

**【有源】所以"Cargo 会不会顺便产出 compile_commands.json"的答案是：不会，而且曾经最接近的 nightly 特性已被删除。**
（顺带记录当前版本线：nightly **Cargo 1.100 (2026-11-12)**；stable 1.99 (2026-10-01)、1.98 (2026-08-20)、1.97 (2026-07-09)。）

**【有源】`rustc` 也不能产出编译数据库**：<https://doc.rust-lang.org/rustc/command-line-arguments.html>
`--emit` 的合法取值逐字只有
"`asm` … `dep-info` … `link` … `llvm-bc` … `llvm-ir` … `metadata` … `mir` … `obj`" —— **没有 compilation database**。
（`dep-info` 逐字："Generates a file with Makefile syntax that indicates all the source files that were loaded to generate the crate." —— 这是
Makefile 风格的依赖文件，**不是** CDB。）
替代的机器可读途径只有 plumbing 类命令，unstable 页逐字："Low, level commands that act as APIs for Cargo, like `cargo metadata`"。

**【有源】`bear -- cargo build` 不是官方推荐做法**：Bear 的 recipes 索引
（<https://rizsotto.github.io/Bear/guides/recipes/index.html>）**没有 Cargo 条目**（收录的是 Makefile / CMake / dry-run capture /
clangd setup / headers / ccache-distcc-icecc / docker，工具链是 cross-compilation / multilib / embedded-arm / …）。
且 Bear 4.2.0 新增识别编译器里列的是 MPI wrappers、Cray CCE、AMD ROCm、QNX `qcc`、Emscripten、TI、Microchip XC8、
**the Swift driver**、NASM/YASM/FASM —— **`rustc` 不在其中**。
→ 只能说"官方没有推荐"，**不能反推为"不可用"**（未验证）。

**【未找到权威来源】以下与 Rust 内核相关的说法一律不得写进结论**：
`RUSTC_BOOTSTRAP` 的官方语义；rust-analyzer 对 C/C++ 文件的支持范围；
Linux kernel 的 `make compile_commands.json` / `scripts/clang-tools/gen_compile_commands.py`
（我抓取的 kernel 官方文档 **7.3.0-rc2** 的 `kbuild.html` 与 `kbuild/llvm.html` **两页都没有 `compile_commands` 字样**，
只有 LKML 补丁与下游源码树提到 "Currently, you need to manually run scripts/gen_compile_commands"）。
`cargo-compile-commands` 这个 crate：docs.rs 返回 404 且正文为 "The requested crate does not exist / no such crate"
（<https://docs.rs/crate/cargo-compile-commands>）→ 只能写"**未发现该 crate 存在**"，不能写"确定不存在"。

#### Zig：**没有**官方 compile_commands 导出

**【有源】** 官方 Build System 页内嵌的 `zig build --help`（来自 `zig-x86_64-linux-0.16.0`）：
<https://ziglang.org/learn/build-system/>
- 与 C 编译相关的选项只有：
  `--verbose-cc  Enable compiler debug output for C compilation`、`--verbose-cimport  Enable compiler debug output for C import`。
- **整个 `--help` 里没有任何 compile_commands / compilation database 选项**（`--cache-dir` / `--global-cache-dir` 只是缓存目录，无关）。
- target 指定方式逐字：`-Dtarget=[string]  The CPU architecture, OS, and ABI to build for`、
  示例 `zig build -Dtarget=x86_64-windows -Doptimize=ReleaseSmall`；多目标用 `std.Target.Query`。
- **注意**：官方 Build System 教程页 grep `addCSourceFile` **零匹配** → "官方文档介绍了 `b.addCSourceFile`"这个说法不成立，
  应改成"API 存在（见下），但该教程页未涉及"。

**【第三方，非官方】** 需要 CDB 时的社区做法是 `gen-compile-commands` 这个 Zig 库
（<https://codeberg.org/smithcol11/gen-compile-commands>），README 逐字给出用法：

```zig
const gen_cc = @import("gen_compile_commands");
exe.addCSourceFile(.{ .file = .{ .path = "src/lib.c" }, .flags = &[_][]const u8{"-Wall"} });
b.installArtifact(exe);
try gen_cc.generateCompileCommands(b, &.{exe});      // 运行 zig build 时生成 compile_commands.json
```
API 逐字：`pub fn generateCompileCommands(b: *std.Build, targets: []*std.Build.Step.Compile) !void`。
（该库 CHANGELOG 有提交 "Migrate zig to 0.15.2."，2026-02-13 —— **生态很新，风险自担**。）

**【未找到权威来源】Zig 的 `zig cc` drop-in 语义、`zig cc -target x86_64-freestanding` 语法、Zig 版 `-fno-sanitize`**：
官方页面没有逐字给出；最常被引用的说明在作者个人博客
<https://andrewkelley.me/post/zig-cc-powerful-drop-in-replacement-gcc-clang.html>（**个人博客，非官方文档，本次未抓正文**）。
而 `https://ziglang.org/blog/zig-cc/` **返回 404**。
→ **报告中不得写"Zig 官方文档说 `zig cc` 是 clang 的 drop-in 替代"**。这些都是未证实。

**【推测，明确标注】** 如果 C 文件是用 `zig cc` 逐个编译的，理论上可以给每次 `zig cc` 调用加 `-MJ <fragment>.json`
（前提是 `zig cc` 确实转发该 clang flag —— **这一点我既没有来源、也没有实测**，必须实测后才能写进实现）。

---

## 2. clangd 处理交叉工具链的正确配置

### 2.1 `--query-driver` 的真实语义（**实测，含一次真实的"执行"取证**）

`clangd --help` 原文（**实测**）：

```
  --compile-commands-dir=<string>     - Specify a path to look for compile_commands.json. If path is invalid,
                                        clangd will look in the current directory and parent paths of each source file
  --query-driver=<string>             - Comma separated list of globs for white-listing gcc-compatible drivers
                                        that are safe to execute. Drivers matching any of these globs will be used
                                        to extract system includes. e.g. /usr/bin/**/clang-*,/path/to/repo/**/g++-*
```

注意原文用词：**"safe to execute"** —— 白名单的对象是"允许执行的程序"。

**取证实验**：我写了一个假 driver，把接到的 argv 记到日志再转发给真 gcc：

```sh
#!/bin/sh
# /root/PrincessIDE/.researchA/fakedriver/mygcc
echo "INVOKED: $0 $*" >> /root/PrincessIDE/.researchA/fakedriver/calls.log
exec /usr/bin/gcc "$@"
```

把 CDB 里的 `arguments[0]` 换成它，再 `--query-driver=/root/PrincessIDE/.researchA/fakedriver/*`：

```
I[…] System includes extractor: successfully executed /root/PrincessIDE/.researchA/fakedriver/mygcc
I[…] Compile command from CDB is: /root/…/mygcc -std=gnu11 … -nostdinc -Iinclude … --target=x86_64-linux-gnu
```

`calls.log` 里的**唯一一行、逐字**：

```
INVOKED: /root/PrincessIDE/.researchA/fakedriver/mygcc -E -x c - -v -nostdinc
```

**结论（全部实测）**：
1. clangd 只会执行**同时满足**两个条件的程序：① 它是 CDB 里那条命令的 `argv[0]`；② 它的路径匹配 `--query-driver` 的某个 glob。
   两者缺一，clangd 不会执行它（我第一版实验把 glob 指向假 driver、但 CDB 里写的是 `/usr/bin/gcc`，`calls.log` 完全没有被创建）。
2. clangd 传给 driver 的参数是**固定**的：`<driver> -E -x c - -v -nostdinc`（并把这个命令里原有的编译 flag 原样附上）。
3. clangd 从它的 **stderr** 里解析两样东西：
   - `Target:` 行 → target triple（下面实测里 gcc 报 `Target: x86_64-linux-gnu`，clangd 就给编译命令加了 `--target=x86_64-linux-gnu`，cc1 里变成 `-triple x86_64-unknown-linux-gnu`）；
   - `#include <...> search starts here:` … `End of search list.` 之间的路径列表 → system include 路径。
4. **安全提醒**：因为 `-nostdinc` 被附加在末尾，用 `-nostdinc` 的交叉工具链**返回空 include 列表**。
   我实测 `gcc -E -x c - -v -nostdinc < /dev/null` 的输出就是：
   ```
   #include "..." search starts here:
   #include <...> search starts here:
   End of search list.
   ```
   → 也就是说，**对一个用 `-nostdinc -isystem ...` 的内核工具链，`--query-driver` 只能帮你拿到 triple，拿不到 include（因为内核本来就不该有 system include）**。
5. `--query-driver` 是**执行任意代码**：glob 写太宽（如 `/usr/bin/*` 甚至 `/**`）等于把用户任意可写目录里的可执行文件交给 clangd 执行。
   PrincessIDE 应把它收敛到 IDE **自己管理的工具链目录**，例如
   `--query-driver=<ide-toolchains-dir>/**/x86_64-elf-gcc,<ide-toolchains-dir>/**/clang`。
   **绝不**从工程文件（`.clangd`/CDB）里读取 glob。

`--query-driver` 引入版本：**未验证**（本次只用了 clangd 16.0.6，未做跨版本对照）。

**相关的另一个配置项：`CompileFlags.BuiltinHeaders`（有源，且带官方警告）**
（<https://clangd.llvm.org/config>，逐字）：
> `Clangd`: Use builtin headers from `clangd`. This is the default.
> `QueryDriver`: Use the headers extracted from the compiler via the `--query-driver` command line argument. If a query driver is not supplied or does not match the compiler, then the `Clangd` builtin headers will be the fallback.

> **Note**: if the driver is not clang, `BuiltinHeaders: QueryDriver` will result in the clang frontend (embedded in clangd) processing the builtin headers of another compiler, which could lead to unexpected results such as false positive diagnostics.

→ **对内核工程的取态**：保持默认 `Clangd`（用 clang 自己的 `<stdint.h>/<stddef.h>/<stdarg.h>` 等 freestanding 头），
**不要**设成 `QueryDriver` 去用 GCC 的 `stdint.h` —— 官方明说可能产生假诊断。
这也解释了为什么 `.clangd` 里应该用 `-nostdlibinc`（保留 clang 内置头）而不是 `-nostdinc`（清空一切）。

### 2.2 target triple 从哪来（实测，四条来源）

| 来源 | 实测证据 |
|---|---|
| driver 的 `Target:` 行（`--query-driver` 路径） | 见 §2.1；`--target=x86_64-linux-gnu` 被自动注入 |
| CDB 里的 `--target=` / `-target` | `--target=x86_64-unknown-none` → cc1 `-triple x86_64-unknown-none` |
| `CompileFlags.Compiler` 的名字 | 【有源】<https://clangd.llvm.org/config>："The name controls flag parsing (clang vs clang-cl), target inference (gcc-arm-noneabi) etc." |
| 全都没有时 | 落到 clangd 自身平台：`-triple x86_64-pc-linux-gnu`（实测，见 §2.5 的初始日志） |

**推荐**：内核工程显式在 `.clangd` 里写死 triple（`--target=x86_64-unknown-none`），**不要依赖 `--query-driver` 的自动推断** ——
显式写法可复现、跨机器一致、且不需要 clangd 执行任何外部程序。

### 2.3 `-ffreestanding` / `-nostdlib` / `-mno-red-zone` 等在 clangd 下的行为（**实测，逐条**）

以 `kernel/kmain.c` 的 CDB 条目为输入，`clangd --check --log=info` 打印的 `internal (cc1) args` 里（**实测逐字**）：

```
-ffreestanding  -mcmodel=kernel  -target-cpu x86-64  -target-feature -sse  -disable-red-zone
-nostdsysteminc  -nobuiltininc  -resource-dir <prefix>/usr/lib/llvm-16/lib/clang/16
-I include  -I include/kernel  -O2  -Wall  -Wextra  -std=gnu11
```

| 编译 flag | clangd/cc1 的对应行为 | 判定 |
|---|---|---|
| `-ffreestanding` | cc1 收到 `-ffreestanding`；宏实测 `__STDC_HOSTED__` 从 `1` 变 `0` | ✅ 完全生效 |
| `-mcmodel=kernel` | cc1 收到 `-mcmodel=kernel` | ✅ |
| `-mno-red-zone` | cc1 收到 `-disable-red-zone` | ✅ |
| `-mno-sse` | cc1 收到 `-target-feature -sse`（注意：把 `-mno-sse` 从 CDB 里 Remove 掉后这一项消失，同时 `-mframe-pointer=none` 变成 `-mframe-pointer=all`） | ✅ |
| `-nostdlib` | **被 clangd 完全丢弃**（它是链接期 flag，clangd 只做 `-fsyntax-only`） | ✅ 无害，无需处理 |
| `-nostdinc` | cc1 收到 `-nostdsysteminc` **且 `-nobuiltininc`** —— 连 clang 自带的 `stdint.h/stddef.h/stdarg.h` 都没了 | ❌ **致命**，见 §2.4 |
| `-nostdlibinc` | 只加 `-nostdsysteminc`；`-isystem <resource-dir>/include` 保留 | ✅ 内核正解 |
| `-g3` | `-debug-info-kind=constructor -dwarf-version=5` | ✅（对 clangd 无影响） |
| `-std=gnu11` | `-std=gnu11`，`__STDC_VERSION__` = `201112L` | ✅ |
| `-m64` | `-target-cpu x86-64`；`__SIZEOF_LONG__=8 __SIZEOF_POINTER__=8` | ✅ |

### 2.4 ⭐ 本节最重要的实测结论：`-nostdinc` 会让 clangd 直接崩掉，要用 `-nostdlibinc`

大多数内核 Makefile 写的是 `-nostdinc`（GCC 传统）。**clang 的 `-nostdinc` 语义比 GCC 更狠**：

```
$ clang -ffreestanding -nostdinc -fsyntax-only t1.c      # t1.c: #include <stdint.h>
t1.c:1:10: fatal error: 'stdint.h' file not found
1 error generated.

$ clang -ffreestanding -nostdlibinc -fsyntax-only t1.c
（无输出，rc=0）

$ clang -ffreestanding -nostdinc -isystem $(clang -print-resource-dir)/include -fsyntax-only t1.c
（无输出，rc=0）
```

三条 include 搜索路径的**实测对照**：

```
A) -nostdlibinc
#include <...> search starts here:
 /…/lib/clang/16/include
End of search list.

B) 不加任何 -nostd*
#include <...> search starts here:
 /…/lib/clang/16/include
 /usr/local/include
 /usr/include/x86_64-linux-gnu
 /usr/include
End of search list.

C) -nostdinc
#include <...> search starts here:
End of search list.          ← 空的
```

`-H` 取证，同一个文件同时 `#include <stdint.h> <stddef.h> <stdarg.h> <string.h> <stdio.h>`：

```
# 默认（不加 -nostd*）：
. /…/lib/clang/16/include/stdint.h
. /…/lib/clang/16/include/stddef.h
. /…/lib/clang/16/include/stdarg.h
. /usr/include/string.h          ← 宿主机 glibc 进来了
. /usr/include/stdio.h           ← 宿主机 glibc 进来了

# -nostdlibinc：
. /…/lib/clang/16/include/stdint.h
. /…/lib/clang/16/include/stddef.h
. /…/lib/clang/16/include/stdarg.h
t3.c:4:10: fatal error: 'string.h' file not found     ← 正确：内核不该有 string.h
```

**GCC 侧对照（实测）**：`gcc -nostdlibinc` 不存在，GCC 世界的内核正解是
`-nostdinc -isystem $(gcc -print-file-name=include)`：

```
$ gcc -ffreestanding -nostdinc -fsyntax-only t1.c
t1.c:1:20: error: no include path in which to search for stdint.h
$ gcc -ffreestanding -nostdinc -isystem "$(gcc -print-file-name=include)" -fsyntax-only t1.c
（rc=0）
$ gcc -print-file-name=include
/usr/lib/gcc/x86_64-linux-gnu/12/include
$ gcc -nostdlibinc -fsyntax-only t1.c
gcc: error: unrecognized command-line option '-nostdlibinc'; did you mean '-nostdinc'?
```

并且 **GCC 的 `-ffreestanding` 完全不改 include 搜索路径**（实测对照：加/不加 `-ffreestanding`
的 `gcc -E -v` 输出 **完全相同**，都包含 `/usr/include`）：

```
# gcc -ffreestanding -E -v        # gcc -E -v
 /usr/lib/gcc/x86_64-linux-gnu/12/include
 /usr/local/include
 /usr/include/x86_64-linux-gnu
 /usr/include
```

**因此有两条彼此独立的修法，PrincessIDE 应该同时做**：

1. **修工程（推荐）**：Makefile 用 `-nostdinc -isystem $(CC) -print-file-name=include)`，
   这样 **gcc 真实构建也能过**。
   ⚠️ 反例实测：我最初的夹具 Makefile 只写 `-ffreestanding ... -nostdinc -Iinclude`，
   **`make` 本身就直接失败**：
   ```
   include/kernel/types.h:3:10: fatal error: stdint.h: No such file or directory
   make: *** [Makefile:22: build/kernel/kmain.o] Error 1
   ```
2. **修 clangd（必须，因为 clang 不认 gcc 的 `-isystem` 语义差异且 gcc 头文件与 clang 内置头可能冲突）**：
   用 `.clangd` 把 `-nostdinc` 换成 `-nostdlibinc`：

```yaml
CompileFlags:
  Add:
    - -nostdlibinc
    - --target=x86_64-unknown-none
  Remove:
    - -nostdinc      # 移除；clang 的 -nostdinc 会连内置 freestanding 头一起干掉
    - -isystem       # 连同它的参数一起移除（实测：Remove 会连带删掉 flag 的实参）
    - -W*            # 顺手去掉 GCC 专用告警（见 §2.6）
```

实测效果（CDB = bear 产出的真实 gcc 命令行）：

```
Compile command from CDB is: /usr/bin/gcc -std=gnu11 -O2 -g3 -ffreestanding … -isystem /usr/lib/gcc/.../12/include -Iinclude -c -o … 
   ↓ 应用 .clangd 后
/usr/bin/gcc -std=gnu11 -O2 -g3 -ffreestanding … -Iinclude -o build/kernel/kmain.o -nostdlibinc --target=x86_64-unknown-none -resource-dir=…
All checks completed, 0 errors
```

### 2.5 ⭐ target triple 本身**不能**阻止 glibc 头污染（实测反直觉结论）

即使指定了裸机 triple，clang 仍然把 `/usr/include` 放进搜索路径：

```
$ clang --target=x86_64-unknown-none -ffreestanding -E -x c - -v < /dev/null
Target: x86_64-unknown-none
…
#include <...> search starts here:
 /usr/local/include
 /…/lib/clang/16/include
 /usr/include                 ← 裸机 triple 也有 /usr/include
End of search list.
```

→ **唯一能挡住 glibc 的是 `-nostdlibinc` / `-nostdinc`，不是 `--target`。**
（注：这是 Debian clang 16 的实测行为；上游 clang 是否一致**未验证**。）

### 2.6 gcc 专用编译 flag 会被 clangd 报成"错误"（实测）

我在 CDB 里塞了 9 个 Linux 内核常用的 GCC 专用 flag，实测结果：

```
E[…] [drv_unknown_argument] Line 1: unknown argument: '-fno-tree-loop-distribute-patterns'
E[…] [drv_unknown_argument] Line 1: unknown argument: '-fconserve-stack'
E[…] [drv_unknown_argument] Line 1: unknown argument: '-mpreferred-stack-boundary=3'
E[…] [drv_unknown_argument] Line 1: unknown argument: '-fno-var-tracking-assignments'
E[…] [drv_unknown_argument] Line 1: unknown argument: '-fno-ipa-icf'
E[…] [drv_unknown_argument] Line 1: unknown argument: '-mno-direct-extern-access'
All checks completed, 6 errors
```

被 clang 接受的 3 个：`-mno-fp-ret-in-387`、`-mno-80387`、`-Wno-format-truncation`。

**⭐ 这些错误无法用 `Diagnostics.Suppress` 压掉（实测）**：

| 配置 | 结果 |
|---|---|
| `Diagnostics: Suppress: [drv_unknown_argument]` | **仍然 3 errors** ❌ |
| `Diagnostics: Suppress: ['*']` + 一个真正有 C 错误的文件 | 真正的 C 错误（`-Wint-conversion`）**被压掉了**，但 `drv_unknown_argument` **依然 3 errors** ❌ |
| `CompileFlags: Remove: [-fno-tree-loop-distribute-patterns, -fconserve-stack, -mpreferred-stack-boundary=3, -W*]` | **All checks completed, 0 errors** ✅ |

→ **结论：`drv_*` 是 driver 层错误，绕过 Diagnostics 过滤器；唯一办法是在 `CompileFlags.Remove` 里删掉这些 flag。**
PrincessIDE 应内置一张"GCC 专有 → clang 不识别"的 flag 黑名单自动生成 `.clangd`（或教模板工程自带 `.clangd`）。

**⭐ 补充：`.clangd` 的 `Remove` 支持"删 flag 连带删实参"（实测）**

```
Remove: [-isystem]   →  命令行里的 "-isystem /usr/lib/gcc/.../include" 整体消失 ✅
```
【有源】官方文档原文：<https://clangd.llvm.org/config>
> "If the value is a recognized clang flag (like `-I`) then it will be removed along with any arguments.
> Otherwise, if the value ends in `*` (like `-DFOO=*`) then any argument with the prefix will be removed.
> Otherwise any argument exactly matching the value is removed."

### 2.7 ⭐ glibc 头污染的"可见后果"（实测端到端）

污染不只是理论问题 —— 它直接污染**补全列表**。

在裸的内核 C 文件里输入前缀 `str` 请求补全：

| 配置 | 结果（实测） |
|---|---|
| 不加 `-nostdlibinc` | **45 个补全项**，其中 44 个是 glibc 的：`strcasecmp` `strcat` `strchr` `strcmp` `strcoll` `strcpy` `strcspn` `strdup` `strerror` `strftime` `strlen` `strncasecmp` … |
| 加 `-nostdlibinc` | **1 个补全项**（只有 `struct`） |

**机制已定位（实测）**：这不是编译命令的 include 路径造成的，而是 clangd 的 **标准库索引**：

| 配置 | 补全项数 |
|---|---|
| 不加 `-nostdlibinc`，`Index.StandardLibrary: false` | **1** |
| 加 `-nostdlibinc`，`Index.StandardLibrary: true`（默认） | **1** |

→ 两个开关任一关闭都能消除 glibc 补全。**推荐两个都做**：`-nostdlibinc` 保证解析正确，
`Index.StandardLibrary: false` 保证补全干净（内核工程没有"标准库"）。

### 2.8 ⭐ clangd 的"0 errors"不等于"能编译过"（实测对照实验）

同一个文件 `kernel/outb.c`：

```c
#include <kernel/types.h>
void serial_put(char c) { __asm__ volatile("outb %%al, $0x3f8" :: "a"(c)); }
void serial_write2(const char *s) { while (*s) serial_put(*s++); }
```

| 命令 | 结果（实测） |
|---|---|
| `clangd --check=kernel/outb.c`（用 §2.4 的 `.clangd`） | **All checks completed, 0 errors** ✅ |
| `gcc … -nostdinc -isystem … -c kernel/outb.c`（真实构建） | rc=0，1208 字节 `.o`（只有 warning） |
| `clang … -nostdlibinc --target=x86_64-unknown-none -c kernel/outb.c`（与 clangd 同一套 flag） | ❌ `error: invalid operand for instruction` |
| `clang … -fno-integrated-as -c kernel/outb.c` | rc=0，928 字节 ✅ |

**两个可落地的结论**：
1. **clangd 用 `-fsyntax-only`，跳过 codegen 与内联汇编汇编阶段**，所以它**看不到**集成汇编器报错 →
   `clangd --check` 全绿不代表构建能过。IDE 的"编译"按钮是唯一的真相来源，不要用诊断状态当作 build gate。
2. `outb %al, $0x3f8` 这种 GAS 方言被 **clang 的集成汇编器**拒绝、被 **GNU as** 接受。
   如果工程要用 clang 构建内核，`.clangd` 里加 `-fno-integrated-as` 可以对齐（也可写进 CDB）。

### 2.9 编译数据库的"发现与匹配"行为（实测）

**顺带记录：`clangd --check` 的退出码（实测，可用于 IDE 的健康检查/自检）**

| 场景 | 退出码 |
|---|---|
| 文件解析干净（`All checks completed, 0 errors`） | **0** |
| 有任何诊断（`All checks completed, N errors`，N>0） | **3** |
| 目标文件不存在 | **1** |

（`--help` 原文说明：`--check[=<string>]` — "Parse one file in isolation instead of acting as a language server.
Useful to investigate/reproduce crashes or configuration problems." —— **实测的 `clangd --help` 输出**。）
→ PrincessIDE 可以用它做"工程配置自检"：对一个代表性文件跑 `clangd --check`，rc=0 说明 include 路径与 flag 都对了。


| 场景 | 实测结果 |
|---|---|
| CDB 在工程根 | `Loaded compilation database from /…/proj/compile_commands.json` ✅ |
| 根目录**没有** CDB，但 `build/compile_commands.json` 有 | `Loaded compilation database from /…/proj/build/compile_commands.json` ✅ |
| 指定 `--compile-commands-dir=/tmp`（`/tmp` 里没有 CDB） | `Failed to find compilation database for …/kernel/kmain.c` —— **不再回退到祖先目录** ⚠️ |
| CDB 条目用 `"command": "<字符串>"` 而非 `"arguments": [...]` | 正常工作 ✅（clangd 自己按 shell 规则切分） |
| CDB 的 `"directory": "/root"`（错的） | 相对 `-Iinclude` 变成 `-I/root/include` → `pp_file_not_found: 'kernel/types.h' file not found` + 2 个连带错误 ❌ |

**【有源】** 官方对 `build/` 的说明（<https://clangd.llvm.org/installation>）：
> "clangd will look in the parent directories of the files you edit looking for it, and also in subdirectories named `build/`. For example, if editing `$SRC/gui/window.cpp`, we search in `$SRC/gui/`, `$SRC/gui/build/`, `$SRC/`, `$SRC/build/`, …"

**`--compile-commands-dir` 的坑**：帮助文本说 "If path is invalid, clangd will look in the current directory and parent paths"，
但实测「目录存在、里面没有 CDB」时是**直接失败**、不回溯。所以 IDE 传这个参数前必须先确认目录里确实有 `compile_commands.json`，
否则宁可**不传**，让 clangd 走祖先搜索。

### 2.10 索引、内存与性能（实测数据，供 2G 内存预算参考）

```
$ find .cache -type f          # 开着 --background-index 起 LSP 会话后
.cache/clangd/index/boot.S.89DE1647381C58E4.idx
.cache/clangd/index/panic.h.DF2744B382E3003E.idx
.cache/clangd/index/kmain.c.704A658B5C40E442.idx
.cache/clangd/index/types.h.26C6B637301C238E.idx
```

- 索引落在**工程根**的 `.cache/clangd/index/`，**`.S` 文件也会被索引**（实测）。
- `clangd --check` **不会**写索引（实测：跑完 `.cache` 不存在）→ 想预热索引必须起真正的 LSP 会话。
- 内存实测（`--background-index -j 4`，4 个小文件）：
  ```
  $/memoryUsage → "_total": 308497   (约 0.29 MiB，这是 clangd 自己统计的组件树，偏保守)
  ps -o rss,vsz → RSS 107988 KB (≈105 MiB)   VSZ 807212 KB (≈788 MiB)
  ```
  **VSZ 788 MiB 在 2G 可用内存的机器上是需要留意的数字**；RSS 105 MiB 是实际占用。
  建议：`-j` 不要给满 4（留核给构建/QEMU）；低内存场景加 `--pch-storage=disk`、`--malloc-trim`、
  用 `--background-index-priority=low`。
- 两个 clangd 实例同时开同一个 root（都开 `--background-index`）：**实测两者都正常服务**
  （各自都推出了诊断，`textDocument/definition` 正常）。→ IDE 允许多窗口打开同一工程是可行的，
  但索引目录会被共享写；【推测】可能出现冗余重建，建议 IDE 侧对同一 root 做单实例复用。
- 【有源】`CLANGD_TRACE` 环境变量可把 clangd 日志写到指定文件（<https://clangd.llvm.org/installation>），
  适合 IDE 做"导出 LSP 日志"功能。

---

## 3. clangd 对汇编的支持边界（全部实测，结论：**不要拿它当汇编的 IDE 后端**）

### 3.1 三种汇编后缀的实测结果

| 文件 | 后缀语义 | clangd 行为（实测） |
|---|---|---|
| `arch/x86_64/boot.S` | GAS + C 预处理器 | 能加载 CDB、能建 preamble、**能被索引**；但会推一条**假诊断** `expected_either: Expected identifier or '('`，且**所有语义功能全空** |
| `arch/x86_64/mini.s` | GAS，无预处理 | `E[…] [fe_expected_compiler_job] Line 1: unable to handle compilation, expected exactly one compiler job in ''` + `Failed to parse command line` ❌ |
| `arch/x86_64/isr.asm` | NASM 语法 | `E[…] [drv_unknown_argument] Line 1: unknown argument: '-f'` + `[fe_expected_compiler_job]` + `Failed to parse command line` ❌ |

### 3.2 `.S` 的详细取证

配合显式 CDB 条目（`gcc -ffreestanding … -c arch/x86_64/boot.S`）和修正后的 `.clangd`：

**（a）假诊断**：LSP 里实际收到的 `publishDiagnostics`：

```json
[{"severity":1,"code":"expected_either","message":"Expected identifier or '('"}]
```

而同一个文件用**真编译器**是好的：

```
$ clang -ffreestanding -c arch/x86_64/boot.S -o /tmp/boot.o
$ echo $?   # 0 ；/tmp/boot.o 936 字节
```

机制：clangd 用 `-x assembler-with-cpp` 建了 preamble，然后**用 C 语法解析器去解析汇编文本**，
第 1 行 `.set MB_MAGIC, 0x1BADB002` 在 C 里就是语法错误。
（这是 clangd 16.0.6 的行为，其他版本**未验证**。）

**（b）语义功能全空**（对 `_start:` 标签位置逐个请求，实测原始返回）：

```
textDocument/definition          -> []
textDocument/hover               -> null
textDocument/documentSymbol      -> []
textDocument/references          -> []
textDocument/semanticTokens/full -> {"data": [], "resultId": "1"}
textDocument/foldingRange        -> []
textDocument/documentHighlight   -> []
textDocument/rename              -> {"code": -32001, "message": "Cannot rename symbol: there is no symbol at the given location"}
```

→ **clangd 对汇编只提供"一个假错误"，零个真功能。**

**（c）可用的止血措施（实测有效）**：给汇编文件在 `.clangd` 里单独屏蔽诊断：

```yaml
---
If:
  PathMatch: .*\.(S|s|asm)
Diagnostics:
  Suppress: ['*']
```

实测：`boot.S` → `All checks completed, 0 errors`，LSP 侧不再推任何诊断 ✅。

### 3.3 NASM 的详细取证（这条路是死的）

**（a）clangd 无法解析 nasm 命令行**。显式给 CDB 条目：

```json
{"directory": "...", "file": ".../arch/x86_64/isr.asm",
 "arguments": ["nasm", "-f", "elf64", "-g", "-F", "dwarf",
               "arch/x86_64/isr.asm", "-o", "build/arch/x86_64/isr.asm.o"]}
```

clangd 日志（实测逐字）：

```
I[…] Compile command from CDB is: /usr/bin/nasm -f -g -F dwarf -resource-dir=… -- /…/arch/x86_64/isr.asm
I[…] Parsing command...
E[…] [drv_unknown_argument] Line 1: unknown argument: '-f'
E[…] [fe_expected_compiler_job] Line 1: unable to handle compilation, expected exactly one compiler job in ''
E[…] Failed to parse command line
```

注意 `-f elf64` 变成了孤零零的 `-f`：**clangd 的 Toolchain 启发式把 `elf64` 当成"输入文件名"删掉了**，
于是 `-f` 成了非法参数。这说明 clangd 是拿 **clang 的 driver 去解析一个非 clang 命令行**。

**（b）这些错误无法屏蔽**（实测）：即使 `.clangd` 里对 `.*\.asm` 设 `Diagnostics: Suppress: ['*']`，
LSP 侧**依然**推 2 条：

```
isr.asm -> [('drv_unknown_argument', "Unknown argument: '-f'"),
            ('fe_expected_compiler_job', 'Unable to handle compilation, expected exactly one')]
```

**（c）clang 自己也不认 NASM 语法**（实测，说明"用 clang 编译 .asm"也不可行）：

```
$ clang -c arch/x86_64/isr.asm -o /tmp/isr.o
arch/x86_64/isr.asm:1:15: error: unexpected token in argument list
; NASM-syntax interrupt entry stubs (x86_64, nasm -f elf64)
              ^
arch/x86_64/isr.asm:2:1: error: invalid instruction mnemonic 'bits'
BITS 64
```

—— clang 把 `.asm` 当 **GNU 汇编**处理（`as` 方言），于是 `BITS 64`、`;` 注释、`%macro` 全部炸。

**（d）gcc 则把 `.asm` 当链接输入**（实测）：

```
$ gcc -c arch/x86_64/isr.asm -o /tmp/isr2.o
gcc: warning: arch/x86_64/isr.asm: linker input file unused because linking not done
```

### 3.4 业界替代方案（对 PrincessIDE 的取舍建议）

**① 汇编改用 GAS `.S`（推荐）**
- 【实测】clang 能真正汇编 `.S`（`clang -c arch/x86_64/boot.S` → rc=0，936 字节 `.o`）；
  用 `-ffreestanding -mcmodel=kernel -mno-red-zone -m64` 等内核 flag 一样通过（实测）。
- 【实测】clangd 的 `.S` 假诊断可用 `Diagnostics.Suppress` 消掉（§3.2c）。
- 【实测】`clangd --check` 对 `.S` 的报错只影响 UX，不影响构建。
- 代价：放弃 NASM 的宏语法便利（`%macro` 要改写成 `.macro`）。

**② 保留 NASM，但把语言服务从 clangd 里摘出去**
- 【实测】clangd 对 `.asm` 只会产生 2 条无法屏蔽的假错误。
- 因此 IDE 的**文档路由**里，`.asm` 不应挂到 clangd 客户端上（见 §4.9 / §5）。
- NASM 的 `.asm` 语法高亮用简单的词法着色即可（NASM 指令表 + 宏/注释），不需要 LSP。
- 【实测】`nasm --version` 相关元数据：bookworm 提供 `nasm 2.16.01-1`（`apt-cache policy nasm`），
  本次**未安装**，所以"nasm 的 `-g -F dwarf` 调试信息"这一点**未实测**。
- 【有源】NASM 官方手册中 `-g` / `-F` 选项的说明页：<https://www.nasm.us/doc/nasm09.html>
  以及 `nasm -f elf64 -g -F dwarf` 生成 DWARF 调试信息的用法。
  **本次仅在 Makefile 里写了该命令，未执行验证。**

**③ 汇编侧的"IDE 体验"应该走 objdump/gdb，而不是语言服务（推荐给 PrincessIDE 的路线）**
- 目标是"图形化混合调试(C+汇编)"：汇编视图的正确数据源是 **`objdump -d`/`objdump -S`（源行交织）**、
  **DWARF 行号表**、**gdb/QEMU 的栈回溯**，这些**不依赖任何语言服务器**，且与"是否 NASM"无关。
- 这与本项目主链路 "panic/异常栈回溯符号化 → 图形化混合调试" 天然吻合，
  且完全避开 clangd 的汇编能力缺口。
- 【推测】若要 `.asm` 有"跳转到符号定义"，可以在 IDE 侧用一个轻量的 NASM 符号表解析器
  （`nasm -f elf64` 后读 `.symtab`，或用 `nm`），成本远低于等一个 NASM LSP。

**④ 关于"NASM 是否有可用的语言服务器"**：在本次 clangd 侧证据之外，**未做专门调研**，
标为未验证。可行的方向是社区项目（如面向 GAS 的 `asm-lsp`）—— **未验证，不作为结论**。

---

## 4. 把 clangd 当作被嵌入的 LSP 服务（协议层，全部实测）

实测方式：自己写了一个最小 stdio LSP 客户端（`/root/PrincessIDE/.researchA/lsp/lspcli.py`），
直接对 clangd 收发原始 JSON-RPC 帧。所有响应片段都是**真实抓包**。

### 4.1 帧格式（实测逐字节）

发送 `initialize` 时抓到的**原始 header 字节**：

```
b'Content-Length: 1381\r\n\r\n'
Content-Length 值 = 1381，body 的 UTF-8 实际字节数 = 1381   ← 相等，确认计量单位是"字节"而非"字符"
```

**【有源】** LSP 3.17 规范 Base Protocol 原文（<https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#headerPart>）：

| Header Field Name | Value Type | Description |
| :--- | :--- | :--- |
| `Content-Length` | number | The length of the content part **in bytes**. This header is **required**. |
| `Content-Type` | string | The mime type of the content part. Defaults to `application/vscode-jsonrpc; charset=utf-8` |

> "The header and content part are separated by a '\r\n'. … two '\r\n' sequences always immediately precede the content part."
> "The header part is encoded using the 'ascii' encoding. This includes the '\r\n' separating the header and content part."

**要点**：
- 3.17 **仍然支持** `Content-Type`（不是被删掉；被后续版本取代的只是 `utf8` 这个写法）。
- `Content-Length` 计的是 **body 的字节数**，中文/emoji 会 > 字符数 —— 实现时一定用 `len(utf8_bytes)`，别用 `strlen`。
- **Rust 实现提示（推测，非实测）**：Tauri 侧读 stdio 一定要按字节缓冲，先解析到 `\r\n\r\n`，再读满 `Content-Length` 字节；
  不要把流当行文本处理。

### 4.2 `initialize` 握手（实测）

请求（我实际发的 params，clangd 接受且后续功能全部正常）：

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{
  "processId": 12345,
  "clientInfo": {"name":"princesside","version":"0.0.1"},
  "rootUri": "file:///root/PrincessIDE/.researchA/proj",
  "rootPath": "/root/PrincessIDE/.researchA/proj",
  "workspaceFolders": [{"uri":"file:///root/PrincessIDE/.researchA/proj","name":"proj"}],
  "capabilities": {
    "textDocument": {
      "synchronization": {"didSave": true},
      "completion": {"completionItem": {"snippetSupport": true,
                                        "documentationFormat": ["markdown","plaintext"],
                                        "resolveSupport": {"properties":["documentation","detail"]}}},
      "hover": {"contentFormat": ["markdown","plaintext"]},
      "publishDiagnostics": {"relatedInformation": true, "versionSupport": true},
      "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
      "semanticTokens": {"requests": {"full": true}, "tokenTypes": [], "tokenModifiers": [], "formats": ["relative"]},
      "inlayHint": {}
    },
    "workspace": {"workspaceFolders": true, "symbol": {"symbolKind": {"valueSet": [1,2,3]}}},
    "window": {"workDoneProgress": true},
    "offsetEncoding": ["utf-8", "utf-16"]
  },
  "initializationOptions": {
    "compilationDatabasePath": "/root/PrincessIDE/.researchA/proj",
    "fallbackFlags": ["-ffreestanding","-nostdlibinc","--target=x86_64-unknown-none","-std=gnu11"],
    "clangdFileStatus": true
  },
  "trace": "off"
}}
```

响应（实测原文）：

```json
{"capabilities": { ... 30 个键 ... },
 "offsetEncoding": "utf-8",
 "serverInfo": {"name":"clangd",
                "version":"Debian clangd version 16.0.6 (15~deb12u1) linux+grpc x86_64-pc-linux-gnu"}}
```

之后必须发 `{"jsonrpc":"2.0","method":"initialized","params":{}}`。

**未 initialize 就发请求（实测）**：

```json
{"error":{"code":-32002,"message":"server not initialized"},"id":1,"jsonrpc":"2.0"}
```

→ IDE 的客户端必须把"initialize 完成"作为状态机的门禁，否则会收到 `-32002`（LSP 规范里的 `ServerNotInitialized`）。

### 4.3 clangd 16.0.6 声称的完整能力（实测 capability key 列表）

```
astProvider, callHierarchyProvider, clangdInlayHintsProvider, codeActionProvider,
compilationDatabase, completionProvider, declarationProvider, definitionProvider,
documentFormattingProvider, documentHighlightProvider, documentLinkProvider,
documentOnTypeFormattingProvider, documentRangeFormattingProvider, documentSymbolProvider,
executeCommandProvider, foldingRangeProvider, hoverProvider, implementationProvider,
inlayHintProvider, memoryUsageProvider, referencesProvider, renameProvider,
selectionRangeProvider, semanticTokensProvider, signatureHelpProvider,
standardTypeHierarchyProvider, textDocumentSync, typeDefinitionProvider,
typeHierarchyProvider, workspaceSymbolProvider
```

关键子结构（实测）：

```json
"textDocumentSync": {"change": 2, "openClose": true, "save": true}
```
→ `change: 2` = **Incremental**（IDE 必须发增量 didChange，或至少发全量 change 事件数组）。

```json
"completionProvider": {"resolveProvider": false,
                       "triggerCharacters": [".", "<", ">", ":", "\"", "/", "*"]}
"executeCommandProvider": {"commands": ["clangd.applyFix", "clangd.applyTweak"]}
"cwdDetection"/"compilationDatabase": {"automaticReload": true}
"documentOnTypeFormattingProvider": {"firstTriggerCharacter": "\n", "moreTriggerCharacter": []}
"documentLinkProvider": {"resolveProvider": false}
"inlayHintProvider": true                       ← LSP 3.17 标准
"clangdInlayHintsProvider": true                ← clangd 私有旧扩展
"offsetEncoding": "utf-8"                        ← 协商结果（见 §4.4）
```

**semanticTokens 图例（实测，必须用服务端下发的这一份）**：

```
tokenTypes (23):
["variable","variable","parameter","function","method","function","property","variable",
 "class","interface","enum","enumMember","type","type","unknown","namespace","typeParameter",
 "concept","type","macro","modifier","operator","comment"]
tokenModifiers (18):
["declaration","definition","deprecated","deduced","readonly","static","abstract","virtual",
 "dependentName","defaultLibrary","usedAsMutableReference","usedAsMutablePointer",
 "constructorOrDestructor","userDefined","functionScope","classScope","fileScope","globalScope"]
"range": false        ← 不支持 range 形式的语义 token 请求
```

⚠️ **注意 tokenTypes 有重复项**（`variable`×2、`function`×2、`type`×4）—— 这是 clangd 为兼容旧 LSP 枚举而故意填充的。
IDE **必须**按服务端返回的数组下标解释 token，不能硬编码自己的枚举表。

### 4.4 位置编码协商（实测，容易踩错的三次尝试）

clangd 文档明确了正确位置：**客户端 capability 的顶层 `offsetEncoding`**，服务端在 **InitializeResponse 顶层**回 `offsetEncoding`。
（<https://clangd.llvm.org/extensions> → "UTF-8 offsets"：**New client capability**: `offsetEncoding: string[]`；
**New InitializeResponse property**: `offsetEncoding: string`。）

实测结果矩阵：

| 客户端怎么声明 | clangd 响应里的 `offsetEncoding` |
|---|---|
| 什么都不声明 | **缺省（不存在）** → 按 LSP 默认必须当作 **utf-16** |
| `initializationOptions.offsetEncoding = ["utf-8","utf-16"]` | **缺省** ❌（放错地方了） |
| `capabilities.general.positionEncodings = ["utf-8","utf-16"]`（LSP 3.17 标准位置） | **缺省** ❌（clangd 16.0.6 不认这个） |
| `capabilities.offsetEncoding = ["utf-8","utf-16"]` | **`"utf-8"`** ✅ |
| CLI `--offset-encoding=utf-8` | **`"utf-8"`** ✅ |
| CLI `--offset-encoding=utf-32` | **`"utf-32"`** ✅ |

**【有源】** 该扩展已被上游标记为弃用：
> "**WARNING** This extension has been deprecated with `clangd-21` in favor of the `positionEncoding` introduced in LSP 3.17. It'll go away with `clangd-23`."
> —— <https://clangd.llvm.org/extensions>

**给 PrincessIDE 的落地结论**：
- 如果自带的 clangd 是 16.x：**必须在客户端 capabilities 顶层声明 `offsetEncoding`，并读响应顶层的值**；
  若响应里没有该字段，**按 utf-16 处理**。
- 若将来升级到 clangd ≥ 21：改用 LSP 3.17 的 `general.positionEncodings` + `capabilities.positionEncoding`。
  **实现时把"响应字段在顶层还是 capabilities 里"做成可适配的探测逻辑**，不要写死。

### 4.5 `initializationOptions`：三个 key 的实测有效性

**【有源】** 官方定义（<https://clangd.llvm.org/extensions> → "Compilation commands"）：
> **New initialization option**: `initializationOptions.compilationDatabasePath : string`
> - Specifies the directory containing the compilation database (e.g. `compile_commands.json`). This path will be used for all files, instead of searching their ancestor directories.
>
> **New initialization option**: `initializationOptions.fallbackFlags : string[]`
> - Controls the flags used when no specific compile command is found. The compile command will be approximately `clang $FILE $fallbackFlags` in this case.
>
> **New configuration setting**: `settings.compilationDatabaseChanges : {string: CompileCommand}`
> - Provides compile commands for files. This can also be provided on startup as `initializationOptions.compilationDatabaseChanges`.
> - Keys are file paths (Not URIs!)
> - Values are `{workingDirectory: string, compilationCommand: string[]}`

**【实测】三个都真的管用**：

| 实验 | 结果 |
|---|---|
| 工程里**没有** `compile_commands.json`，把命令塞进 `initializationOptions.compilationDatabaseChanges` | 诊断 **0 条**，`textDocument/definition` 正常返回 ✅ |
| 完全没有 CDB，只给 `fallbackFlags: ["-I<abs>/include","-ffreestanding","-nostdlibinc","-std=gnu11"]` | 诊断 **0 条** ✅ |
| `clangdFileStatus: true` | 收到 `textDocument/clangd.fileStatus` 通知 ✅ |
| `compilationDatabasePath` 指向工程根（根上本来就有 CDB） | 无报错，走 CDB ✅ |

⭐⭐ **`compilationDatabaseChanges` 是 PrincessIDE 最值得用的一招**：内核项目的构建参数往往在构建时才确定
（工具链路径、`-D` 配置、生成的 `version.h` 等），IDE **完全可以在构建完成后把真实命令直接经 LSP 喂给 clangd**，
不必先把 `compile_commands.json` 落盘或重写。**注意 key 是文件路径，不是 URI**（文档明确标注了这一点，是个历史 bug 遗留）。

### 4.6 功能实测矩阵（全部真实响应）

| LSP method | 实测结果（片段） |
|---|---|
| `textDocument/completion`（成员补全 `boot_pml4[0].`） | ✅ 返回 struct 位域成员，含 `textEdit`/`sortText`/`insertTextFormat:1` |
| `textDocument/completion`（标识符前缀 `pmm_`） | ✅ `insertText: "pmm_alloc_frame()"`，`insertTextFormat: 2`（Snippet） |
| `textDocument/completion`（`#include <kernel/`） | ✅ include-path 补全，`kind: 17`，`filterText: "panic.h>"` |
| `textDocument/definition` | ✅ `[{"uri":"file://…/kmain.c","range":{...}}]` |
| `textDocument/hover` | ✅ `{"contents":{"kind":"plaintext","value":"variable boot_pml4\n\nType: struct page_table_entry[512]\nValue = &boot_pml4[0]\n\nstatic struct page_table_entry boot_pml4[512]"},"range":{...}}` |
| `textDocument/references` | ✅ 3 处（含声明） |
| `textDocument/documentSymbol` | ⚠️ **只有客户端声明 `hierarchicalDocumentSymbolSupport: true` 时才返回 `DocumentSymbol`（含 `range`+`selectionRange`）**；否则返回扁平的 `SymbolInformation`（`location`+`containerName`） |
| `textDocument/prepareRename` | ✅ 返回 `Range`（不是 `{range, placeholder}`） |
| `textDocument/rename` | ✅ `{"changes":{"file://…/kmain.c":[{"newText":"pmm_setup","range":{...}}, …]}}` |
| `textDocument/references` + `container` capability | 【有源】`textDocument.references.container` 可要求返回 `containerName` |
| `textDocument/semanticTokens/full` | ✅ `{"data":[3,14,16,8,131072, 0,17,9,0,65539, …]}` |
| `textDocument/semanticTokens/full/delta` | 【实测】二进制里有该 method 名；本次客户端未声明 delta 支持，**未实测其响应** |
| `textDocument/inlayHint` | ✅ `[{"kind":2,"label":"s:","paddingLeft":false,"paddingRight":true,"position":{...}}, {"kind":2,"label":"fmt:", …}]` |
| `clangd/inlayHints`（旧扩展） | ✅ `[{"kind":"parameter","label":"s: ","range":{...}}]` |
| `textDocument/codeAction` | ✅ 返回 `[]`（该 range 无可用 action，属正常） |
| `textDocument/foldingRange` | ✅ 3 段 |
| `textDocument/selectionRange` | ✅ 嵌套 parent 链 |
| `textDocument/signatureHelp` | ✅ `{"activeParameter":0,"activeSignature":0,"signatures":[{"label":"pmm_init(unsigned long mem_upper_kb) -> void","parameters":[{"label":"unsigned long mem_upper_kb"}]}]}`；在**未解析到声明**的位置会返回 `{"signatures":[]}`（实测 `panic(` 处就是空数组） |
| `workspace/symbol` | ✅ 含 clangd 扩展字段 `"score": 0.55` |
| `textDocument/documentHighlight` | ✅ `kind: 1`（Text）/`3`（Read） |
| `textDocument/typeDefinition` | ✅ `[]`（本文件无类型定义，属正常） |
| `textDocument/implementation` | ✅ `[]` |
| `textDocument/prepareCallHierarchy` | ✅ 返回含 `data: "2BEA76967C820204"` 的 item |
| `callHierarchy/incomingCalls` | ✅ 返回 `kmain` |
| `callHierarchy/outgoingCalls` | ✅ `null`（无调用） |
| `textDocument/symbolInfo`（clangd 扩展） | ✅ 含 `containerName`、`declarationRange`、`usr`、`id` |
| `textDocument/switchSourceHeader`（clangd 扩展） | ✅ `"file://…/include/kernel/panic.h"`（C → 头文件） |
| `textDocument/ast`（clangd 扩展） | ✅ `{"arcana":"VarDecl 0x… </…/kmain.c:38:5, col:39> col:19 used f 'unsigned long' cinit","children":[…]}` |
| `$/memoryUsage`（clangd 扩展） | ✅ `{"_self":0,"_total":387611,"clangd_server":{…}}` |
| `textDocument/clangd.fileStatus`（通知） | ✅ `{"state":"parsing includes, running Update","uri":"…"}` → `{"state":"idle", …}` |
| `textDocument/publishDiagnostics`（通知） | ✅ 见 §4.7 |

**⭐ 方法名踩坑记录（实测 4 次 -32601 `method not found`）**：
我按直觉写过这些名字，全部被拒：

| 我写的（错） | clangd 里的真名 |
|---|---|
| `textDocument/callHierarchy/prepare` | `textDocument/prepareCallHierarchy` |
| `clangd/memoryUsage` | `$/memoryUsage` |
| `clangd/ast` | `textDocument/ast` |
| `clangd/compilationDatabase` | **不存在该方法**；`capabilities.compilationDatabase.automaticReload` 只是"我会自动重载"的信号 |

（真名来源：对 `clangd` 二进制 `strings` 过滤 `textDocument/`、`clangd/`、`$/`、`callHierarchy/`、`typeHierarchy/`；
以及 <https://clangd.llvm.org/extensions>。二进制里出现的 typeHierarchy 相关 method 还有
`textDocument/typeHierarchy`、`typeHierarchy/supertypes`、`typeHierarchy/subtypes`、`typeHierarchy/resolve`。）

**⚠️ 声明了 capability 却不实现，会被服务端反向调用**：我在首次实验里声明了
`workspace.configuration: true`，实测 clangd **没有**发 `workspace/configuration` 反向请求（因为它不要求）。
但通用规则是：**客户端能力声明必须与服务端可能发起的反向请求一一对应**，否则 clangd 在需要配置时会挂住。
PrincessIDE 应只声明自己真的能应答的能力。

### 4.7 诊断的推送与结构（实测）

```
publishDiagnostics: file:///…/kernel/kmain.c  version=1  count=2
    severity 1  code pp_file_not_found   "In included file: 'stdint.h' file not found"   at {line:0,character:9}
    severity 2  code -Wunused-parameter  "Unused parameter 'mbi'"                        at {line:34,character:46}
```

- 是 **push** 模型（`textDocument/publishDiagnostics`），带 `version` 字段（因为客户端声明了 `versionSupport`）。
- `code` 里混着三种东西：clangd 自己的码（`pp_file_not_found`）、clang 的告警码（`-Wunused-parameter`）、driver 码（`drv_unknown_argument`）。
- 【有源】可选的扩展字段：`Diagnostic.category`（需声明 `textDocument.publishDiagnostics.categorySupport`）、
  `Diagnostic.codeActions`（需声明 `...codeActionsInline`）—— <https://clangd.llvm.org/extensions>。
- 【有源】**按需诊断**：`textDocument/didChange` 可带 `wantDiagnostics: bool`
  （`true`=这个版本必须出诊断，`false`=这个版本别出）。对内核大文件编辑很有用 —— IDE 可以在
  "用户停止输入" 时才发 `wantDiagnostics: true`。<https://clangd.llvm.org/extensions> → "Force diagnostics generation"。
- 【实测】**`Diagnostics.Suppress` 对 driver 层错误无效**（§2.6）。

**诊断的过滤策略（推荐给 IDE）**：
```yaml
# 由 IDE 直接生成/管理的 .clangd 片段（放在用户工程 .clangd 之外、或由 IDE 用 --enable-config + 用户 config.yaml）
Diagnostics:
  Suppress:
    - drv_unknown_argument      # 注意：实测【无效】，必须改用 CompileFlags.Remove
```

### 4.8 生命周期（实测）

| 行为 | 实测结果 |
|---|---|
| `shutdown` 请求 → `exit` 通知 | clangd 进程 **退出码 0** ✅ |
| 直接发 `exit`（没先 `shutdown`） | clangd 进程 **退出码 1** ✅（符合 LSP 规范） |
| initialize 之前发其他请求 | `-32002 server not initialized` |
| 两个 clangd 实例同一 root、都开 `--background-index` | 两者均正常服务 ✅ |
| 通过 stdin 关闭（EOF） | clangd stderr 打 `E[…] Transport error: Input/output error` 后退出 |

**IDE 进程管理建议（基于实测 + 规范）**：
- 正常关闭顺序：`shutdown` →（等响应）→ `exit` →（等进程）→ 若超时 `SIGTERM` → 再超时 `SIGKILL`。
- 崩溃恢复：客户端必须处理"服务端在某次请求中途死掉"，把该请求以错误答复，并**重启服务端、重新 initialize、重发 didOpen**（clangd 无持久会话）。
- 【实测】重启后 `--background-index` 的 `.cache/clangd/index/` 可复用，无需重新全量索引。

### 4.9 可直接落地的启动参数与初始化 JSON

**推荐启动命令**（PrincessIDE 内置 clangd 时）：

```bash
clangd \
  --background-index \
  --background-index-priority=low \
  --clang-tidy=0 \
  -j 2 \
  --pch-storage=disk \
  --malloc-trim \
  --limit-results=100 \
  --compile-commands-dir="$PROJECT_ROOT"      # 仅当该目录确实含 compile_commands.json（见 §2.9）
```

可选的、**必须谨慎**的参数：
```bash
  --query-driver="$IDE_TOOLCHAINS/**/x86_64-elf-gcc,$IDE_TOOLCHAINS/**/clang"   # 仅指向 IDE 自管目录
  --offset-encoding=utf-16                                                    # 显式固定编码，避免协商歧义
  --enable-config                                                             # 才读 .clangd / config.yaml
  --log=error                                                                 # 日志走 stderr
```
（以上参数名与语义全部来自**实测的 `clangd --help` 输出**。）

**推荐 initializationOptions（PrincessIDE 直接可用）**：

```json
{
  "initializationOptions": {
    "clangdFileStatus": true,
    "fallbackFlags": [
      "-std=gnu11", "-ffreestanding", "-nostdlibinc",
      "--target=x86_64-unknown-none",
      "-mno-red-zone", "-mcmodel=kernel", "-m64",
      "-fno-stack-protector", "-fno-pic", "-fno-pie"
    ],
    "compilationDatabasePath": "/abs/path/to/project"
  }
}
```

**推荐的工程 `.clangd`**（经实测能把内核 C 工程压到 0 诊断）：

```yaml
CompileFlags:
  Add:
    - -nostdlibinc
    - --target=x86_64-unknown-none
    - -fno-integrated-as              # 仅当工程用 GAS 方言内联汇编、且要用 clang 构建时
  Remove:
    - -nostdinc                       # clang 的 -nostdinc 会连内置 freestanding 头一起删掉
    - -isystem                        # 连带删除其参数（Linux 内核式 -isystem $(CC) -print-file-name=include)
    - -W*                             # 去掉 GCC 专有告警
    - -fno-tree-loop-distribute-patterns
    - -fconserve-stack
    - -mpreferred-stack-boundary=*
    - -fno-var-tracking-assignments
    - -fno-ipa-icf
    - -mno-direct-extern-access

Index:
  Background: Build
  StandardLibrary: false             # 内核没有"标准库"，避免 glibc 符号进补全

Diagnostics:
  UnusedIncludes: None
  MissingIncludes: None
  ClangTidy:
    Add: []
---
If:
  PathMatch: .*\.(S|s|asm)
Diagnostics:
  Suppress: ['*']                    # 汇编：clangd 只会产生假错误（.asm 的错压不掉，见 §3.3）
```

**【实测】`.clangd` 分片的叠加行为**：根 `.clangd` 的 `Remove: [-isystem]` 会删掉 **CDB 里**的 `-isystem`，
但**不会**删掉 `cxx/.clangd` 内层分片自己 `Add` 上来的 `-isystem` —— 我据此把"删宿主机 system include"与
"给 C++ 补 libstdc++ include"两个互相矛盾的诉求分装在两层配置里，实测同时成立（C 文件 0 errors、C++ 文件 0 errors）。
【有源】官方对此的表述是 "Flags added by the same CompileFlags entry will not be removed."（<https://clangd.llvm.org/config>），
以及配置叠加顺序 "user config has the highest precedence, then inner project, then outer project"。

**`compile_flags.txt` 作为起步方案（有源）**：
> "If all files in a project use the same build flags, you can put those flags one-per-line in `compile_flags.txt` in your source root. Clangd will assume the compile command is `clang $FLAGS some_file.cc`. … However background-indexing will not work … This file will be ignored if `compile_commands.json` is present."
> —— <https://clangd.llvm.org/installation>

→ 适合"刚新建的内核模板工程、还没跑过构建"的场景，让 IDE 立刻有补全；一旦 CDB 生成就自动让位。

---

## 5. 多语言路线图：clangd / rust-analyzer / zls 如何共存

### 5.1 规范层的硬约束（有源）

**【有源】** LSP 3.17 规范原文（<https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#languageServerProtocol>）：

> "The protocol currently assumes that **one server serves one tool**. There is currently no support in the protocol to share one server between different tools. Such sharing would require additional protocol e.g. to lock a document to support concurrent editing."

→ 规范层结论：**一台语言服务器 = 一个工具**。一个工程要多种语言，就必须起多个服务端进程，
   由 IDE 自己做"文档 → 服务端"的路由。规范**没有**禁止两个服务端同时处理同一个文档，
   但也没有为"同一文档多服务端"提供任何协调机制（例如没有锁、没有版本仲裁），所以**不要**这么做。

### 5.2 实测：一个宿主同时驱动两个 stdio LSP 服务端

我实测跑通了：**clangd（C/C++）+ bash-language-server 5.6.0（shell）** 在同一个宿主进程里并行：

```
### 1) start every server as its own process (one client per (server, root) pair)
    clangd    pid=215659  serverInfo=clangd Debian clangd version 16.0.6 (15~deb12u1) linux+grpc x86_64-pc-linux-gnu
              encoding='utf-8'  #capabilities=30
    bash-ls   pid=215663  serverInfo=(none)
              encoding=None  #capabilities=11

### 2) route documents by extension to the right client
    kmain.c      -> clangd
    build.sh     -> bash-ls

### 3) diagnostics from both, concurrently
    [clangd] .clangd: 0 diagnostics []
    [clangd] kmain.c: 0 diagnostics []

### 4) feature requests to each server in the same session
    clangd  documentSymbol items: 9
    bash-ls documentSymbol items: 4
    bash-ls completion items: 1 ['target']

### 5) lifecycle: shutdown then exit, per server
    clangd    exit code = 0
    bash-ls   exit code = 0
```

（`bash-language-server` 5.6.0，来自 npm，装在工作区内：`npm install --cache <workspace>/.researchA/npm-cache bash-language-server`。
注意 `npm` 默认 cache 在 `/root/.npm`，沙箱下会 `EACCES`，**必须用 `--cache` 指到工作区** —— 这是本次实测的一个真实坑。）

**实测支撑的架构结论**：
- 每个服务端 = 一个**独立子进程** + 一套**独立请求 id 空间** + 一份**独立 capability**（30 vs 11，实测差异巨大）+ **独立生命周期**。
- `shutdown`/`exit` 必须**逐个**服务端执行；两者都实测正常退出码 0。
- **能力协商必须按服务端分别做**，IDE 的 UI 层要做"能力并集 + 来源标注"（例如"重命名"这个菜单项在 shell 文件里隐藏、
  在 C 文件里显示 clangd 的实现）。
- 【实测】bash-language-server **没有** `serverInfo`，也没有 `offsetEncoding` → **IDE 不能假设这些字段存在**，
  要按规范默认值兜底（`positionEncoding` 缺省 = utf-16）。
- 【实测】`shellcheck` 未安装时 bash-language-server 不产出任何诊断 → **诊断质量依赖外部工具链**，
  IDE 应把"外部 linter 是否可用"作为该服务端的能力状态上报（而不是显示"没有错误"）。

### 5.3 每种语言该挂哪个服务端（有源 / 未实测明确标注）

| 语言 | 服务端 | 状态 |
|---|---|---|
| C / C++ / Objective-C | **clangd** | 【实测】本报告主题 |
| 汇编 `.S` / `.s` / `.asm` | **不挂语言服务** | 【实测】clangd 零语义功能（§3） |
| Shell | bash-language-server | 【实测】可与之共存（§5.2）；仅作多语言机制验证用 |
| Rust | **rust-analyzer** | 【有源】<https://rust-analyzer.github.io/>、<https://github.com/rust-lang/rust-analyzer>；**本环境未安装、未实测** |
| Zig | **zls** | 【有源】<https://github.com/zigtools/zls>；**bookworm 未打包、未实测** |
| Cargo 生成 compile_commands | **无官方支持** | 【推测】需外部工具；未找到权威来源，不作为结论 |

> 【有源】clangd 官方文档只列出 Bear 和 Bazel extractor 两种 CDB 生成方式，未提及 cargo/zig：
> <https://clangd.llvm.org/installation>

### 5.4 `compile_commands.json` 的冲突与隔离（实测支撑）

**冲突的真实来源（实测）**：clangd 会从被编辑文件的**所有父目录**以及**每一级的 `build/` 子目录**里找 CDB
（实测：根目录没有 CDB 时，`build/compile_commands.json` 被自动加载；官方文档原文见 §2.9）。
所以多语言、多配置的工程里，**一个 `build/compile_commands.json` 会影响到整棵子树**。

**三种隔离手段（按推荐度排序）**：

1. **`CompileFlags.CompilationDatabase`（推荐）**【有源】<https://clangd.llvm.org/config>：
   > "Directory to search for compilation database (compile_commands.json etc). Valid values are:
   > A single path to a directory (absolute, or relative to the fragment) /
   > **`Ancestors`**: search all parent directories (the default) /
   > **`None`**: do not use a compilation database, just default flags."

   ⚠️ **纠正常见误解**：合法取值里**没有 `Directory` 这个关键字**（我在任务书里也见到过 "Ancestors/Directory" 这种写法，是错的）。
   要指定目录就直接写路径字符串。

   在子目录放 `.clangd` 精确指定该子树用哪个 CDB：
   ```yaml
   # cxx/.clangd
   CompileFlags:
     CompilationDatabase: ../build-cxx
   ---
   # arch/.clangd  —— 汇编目录干脆不用 CDB（配合上面的 Suppress）
   CompileFlags:
     CompilationDatabase: None
   ```

2. **`If.PathMatch` 按扩展名分片（推荐，已实测）**：一份 `.clangd` 里按路径分流（§4.9 的示例已验证有效）。

3. **IDE 侧不落盘、改用 `initializationOptions.compilationDatabaseChanges`（已实测有效）**：
   对混合工程最干净 —— 每个文件由 IDE 直接把它的真实编译命令喂进去，**根本不产生** CDB 文件冲突。
   代价：IDE 必须自己维护"文件 → 命令"的映射（这本来就是构建系统已经知道的信息）。

**多语言共存的进程/资源建议（基于实测数据）**：
- 每个 **(服务端, workspace root)** 一对客户端，**不要**每个文件起一个 clangd（实测 clangd 单实例 RSS ≈105 MiB）。
- 2G 内存机器上：clangd 给 `-j 2`、rust-analyzer/zls 类似收敛；总并发服务端数建议 ≤3。
- 全部服务端都用 **stdio**（实测 clangd 与 bash-language-server 都是 stdio），**不要**给每个服务端开 TCP 端口
  （内核开发场景下 IDE 常与 QEMU/gdb 争端口；且 stdio 天然随父进程退出）。
- 服务端重启策略：崩溃 → 指数退避重启，最多 N 次；重启后重放 `didOpen` 全量文档。
- 用户可见的进度：把 clangd 的 `textDocument/clangd.fileStatus`（实测存在）映射到状态栏，
  把 Rust/Zig 服务端各自的 progress（`$/progress`）映射到同一处。

---

## 6. 坑与反例（集中清单）

> 每条都标注了证据等级；「实测」条目在本环境中都有可复现的命令。

### 6.1 配置类

| # | 坑 | 证据 | 正确做法 |
|---|---|---|---|
| 1 | **`-nostdinc` 在 clang 下比 GCC 更狠**，会连 `<stdint.h>/<stddef.h>/<stdarg.h>` 一起干掉，clangd 直接 `pp_file_not_found` | 【实测】cc1 收到 `-nostdsysteminc -nobuiltininc`；搜索列表为空 | `.clangd` 里 `Remove: [-nostdinc]` + `Add: [-nostdlibinc]` |
| 2 | Makefile 只写 `-ffreestanding … -nostdinc` 会让 **gcc 真实构建也失败** | 【实测】`fatal error: stdint.h: No such file or directory` | 加 `-isystem $(CC) -print-file-name=include)` |
| 3 | **`--target=<bare-metal triple>` 不能阻止 `/usr/include` 进入搜索路径** | 【实测】`clang --target=x86_64-unknown-none -E -v` 仍列出 `/usr/include` | 只有 `-nostdlibinc`/`-nostdinc` 能挡 |
| 4 | **GCC 的 `-ffreestanding` 完全不改 include 路径** | 【实测】加/不加 `-ffreestanding` 的 `gcc -E -v` 输出逐字相同 | 别指望它挡 glibc |
| 5 | glibc 符号污染补全列表 | 【实测】无 `-nostdlibinc` → 45 项（`strdup`/`strerror`/`strftime`…）；有 → 1 项 | 同时关 `Index.StandardLibrary` |
| 6 | GCC 专有 flag 被报成 `drv_unknown_argument` 错误，**`Diagnostics.Suppress` 无效** | 【实测】`Suppress: ['*']` 仍 3 errors；对照实验证明 `Suppress` 对 C 诊断有效 | 只能用 `CompileFlags.Remove` |
| 7 | `Remove: [-isystem]` 会**连带删除它的实参** | 【实测】`-isystem /usr/lib/gcc/.../include` 整体消失 | 这正是想要的；但别写 `-I` 进 Remove 除非确定要连参数一起删 |
| 8 | CDB 的 `"directory"` 写错 → 相对 `-I` 全废 | 【实测】`directory:/root` → `'kernel/types.h' file not found` + 2 连带错误 | 保证 `directory` 是可执行该命令的真实工作目录 |
| 9 | `--compile-commands-dir` 指向的目录里没有 CDB 时**不回溯**，直接失败 | 【实测】`Failed to find compilation database for …` | 传之前先确认文件存在，否则不传 |
| 10 | CMake 的 `directory` 是 **build 目录**，拷到源码根后相对路径会错 | 【实测】+【有源】官方建议 `ln -s build/compile_commands.json $SRC/` | 用绝对路径，或 symlink 而非 copy |
| 11 | `.clangd` 里 `initializationOptions.offsetEncoding` / `capabilities.general.positionEncodings` 都不是 clangd 16 认的位置 | 【实测】三者对照矩阵 | 用 `capabilities.offsetEncoding`，并读响应顶层 |
| 12 | 只声明 `hierarchicalDocumentSymbolSupport` 才有层级 symbol，否则返回扁平结构 | 【实测】两种返回的 key 不同 | IDE 必须声明该 capability 才能拿到 `range`/`selectionRange` |
| 13 | 语义 token 的 `tokenTypes` **有重复项** | 【实测】23 项里 `variable`×2、`function`×2、`type`×4 | 严格按服务端下发的数组下标解释 |
| 14 | 声明 `workspace.configuration: true` 却不应答反向请求会挂住 clangd | 【推测/规范】本次实测 clangd 并未发起该请求 | 只声明自己能应答的能力 |

### 6.2 汇编类

| # | 坑 | 证据 |
|---|---|---|
| 15 | clangd 对 `.S` 推**假诊断** `expected_either: Expected identifier or '('`，而 `clang -c` 该文件是 rc=0 | 【实测】两端对照 |
| 16 | clangd 对 `.S` 的**所有语义功能返回空**（definition `[]`、hover `null`、semanticTokens `{"data":[]}`、rename `-32001`） | 【实测】8 个 method 逐个请求 |
| 17 | `.s`（无预处理）在 clangd 里直接 `fe_expected_compiler_job` + `Failed to parse command line` | 【实测】有显式 CDB 条目也一样 |
| 18 | NASM `.asm` → `drv_unknown_argument: unknown argument: '-f'`（clangd 把 `elf64` 当输入文件删掉了），**且无法用 `Suppress` 屏蔽** | 【实测】LSP 侧确实推了 2 条 |
| 19 | clang 把 `.asm` 当 GAS 汇编 → `invalid instruction mnemonic 'bits'` / `unexpected token in argument list` | 【实测】 |
| 20 | gcc 把 `.asm` 当**链接输入** | 【实测】`linker input file unused because linking not done` |

### 6.3 构建/工具类

| # | 坑 | 证据 |
|---|---|---|
| 21 | `-MJ` 在编译失败时写**空文件** | 【实测】0 字节 |
| 22 | `-MJ` 多输入 + `-o` 直接报错 | 【实测】`cannot specify -o when generating multiple output files` |
| 23 | 多次 `-MJ` 追加同一文件 → **非法 JSON** | 【实测】`json.decoder.JSONDecodeError: Extra data` |
| 24 | `-MJ` 是 clang 专有，gcc 没有 | 【实测】`unrecognized command-line option '-MJ'` |
| 25 | bear 的 Debian `Depends` 里有 `libspdlog1.10-fmt9`，但 bookworm main **没有这个包名**（Candidate: none） | 【实测】`apt-cache policy` |
| 26 | bear 只记录**实际执行**的编译命令 → 不 `make clean` 就只拿到增量条目 | 【实测】只编 1 个文件 → 只 1 条 |
| 27 | bear 装在工作区外或非默认路径时，必须显式传 `--library/--wrapper/--wrapper-dir/--bear-path` | 【实测】4 个参数都用上了 |
| 28 | CMake `-G Ninja` 在本环境直接失败（ninja 缺失且不能装） | 【实测】 |
| 29 | CDB 生成脚本里**空变量会吞掉下一个 flag**（`-isystem ${EMPTY} -Iinclude` → `-isystem -Iinclude`） | 【实测】clangd 日志原文 + `kernel.h not found` |
| 30 | clangd 的 `--query-driver` 是**执行任意程序**；glob 写宽 = 执行任意代码 | 【实测】假 driver 的 `calls.log` 收到 `-E -x c - -v -nostdinc` |
| 31 | 内核工具链用 `-nostdinc` 时，`--query-driver` 拿到的 include 列表是**空的**（因为 clangd 会附上 `-nostdinc`） | 【实测】`gcc -E -x c - -v -nostdinc` 输出空列表 |
| 32 | npm 默认 cache 在 `/root/.npm`，沙箱下 `EACCES` | 【实测】`npm error errno EACCES`；用 `--cache <workspace>/…` 解决 |
| 33 | Debian clangd 带 `linux+grpc`，解包后 618 MB 且需要 `LD_LIBRARY_PATH`（`DT_RUNPATH` 不传递） | 【实测】`ldd` 两次逐步排查 |
| 34 | **bear 静默漏条目**（这是最危险的失败模式）：交叉 SDK 的 glibc 与 preload 库不兼容时，"The intercepted invocation fails outright, so that command is **silently missing** from the database rather than reported as a warning"；WSL2 镜像网络会产出 "an empty or short `compile_commands.json` with no other error" | 【有源】<https://rizsotto.github.io/Bear/guides/troubleshooting.html>、<https://rizsotto.github.io/Bear/platforms/linux.html> |
| 35 | **bear 在容器外驱动容器内构建不生效**："Bear must run inside the container, as part of the build it observes; `bear -- docker exec ...` from the host does not work." | 【有源】同上 |
| 36 | **bear 的 preload 看不到静态链接的可执行文件**："It cannot see into a statically linked executable, because such a binary never goes through the dynamic linker in the first place." | 【有源】<https://rizsotto.github.io/Bear/understanding/how-it-works.html> |
| 37 | **bear 的 wrapper 模式只覆盖它启动时在 `PATH` 上认得的编译器**；自己探测编译器的 `./configure` 步骤也可能漏 | 【有源】同上 |
| 38 | **bear 4.x 两种模式不能互相兜底**："Neither mechanism is a fallback for the other. … forcing preload there is a startup error that names wrapper mode as the alternative, and the build does not run."（3.1.1 则可显式 `--force-preload`/`--force-wrapper`，实测） | 【有源】同上 + 【实测】3.1.1 `--help` |
| 39 | **bear 4.x 的配置文件名是 `bear.yml`（不是 `bear.config`）**，且顶层 `schema` 必填，"a file that omits it … is rejected rather than partially applied" | 【有源】<https://rizsotto.github.io/Bear/reference/configuration.html> |
| 40 | **`bear parse-sh` 的 dry-run 路径保真度更低**：递归 make 不总传 `-n`、依赖生成文件的目标不打印、子 shell/命令替换/here-doc 不支持；`*.c` 会以未展开的通配符原样进库（要用 `$(wildcard *.c)`） | 【有源】<https://rizsotto.github.io/Bear/guides/recipes/compile-commands-for-makefile.html> |
| 41 | **compiledb 的 `-n` 是它自己的 `--no-build`，必须写在 `make` 前面**（写成 `compiledb make -n` 会变成"把 `-n` 透传给 make"，语义完全不同） | 【有源】<https://pypi.org/project/compiledb/> |
| 42 | **CMake 导出的是 configure 期"打算执行"的命令**，遇到 `CMAKE_*_COMPILER_LAUNCHER`、toolchain file 里 bake 进去的 `ccache`/`distcc`、包装脚本、custom command 就会与实际执行不一致 | 【有源】<https://rizsotto.github.io/Bear/guides/recipes/cmake.html> |
| 43 | **CMake 的 `CMAKE_EXPORT_COMPILE_COMMANDS` 只对 Makefile / Ninja 生成器有效**，Visual Studio / Xcode 下**完全无效**；且与 `UNITY_BUILD` 组合不好 | 【有源】<https://cmake.org/cmake/help/latest/variable/CMAKE_EXPORT_COMPILE_COMMANDS.html> |
| 44 | **把解包前缀放在多 Agent/多进程共享目录里会被覆盖清掉**：本次 `.toolchain/prefix` 被其他 Agent 重建，clangd 消失 → `exec: …: not found`，`rc=127` | 【实测】 |

### 6.4 语义/认知类（最容易误判的）

| # | 坑 | 证据 |
|---|---|---|
| 34 | **clangd "0 errors" ≠ 能编译过**：clangd 用 `-fsyntax-only`，跳过 codegen 与内联汇编汇编 | 【实测】同一文件 clangd 0 errors，`clang -c` 报 `invalid operand for instruction` |
| 35 | 不要用 clangd 的诊断状态当作 build gate | 同上 |
| 36 | `gcc` 接受 `outb %al, $0x3f8`（GNU as），clang 集成汇编器拒绝；`-fno-integrated-as` 可对齐 | 【实测】三种组合对照 |
| 37 | **要 libstdc++ 头就必须把 glibc 头放回来**：`/usr/include/x86_64-linux-gnu/c++/12/bits/os_defines.h:39` 里就是 `#include <features.h>` | 【实测】实测报错 + 直接 `sed` 该文件取证 |
| 38 | 因此"内核 C++ 用 `<type_traits>/<new>`"与"零 glibc 污染"在宿主机头文件体系下**不可兼得** | 【实测/结论】；【推测】正解是自带极简 `<type_traits>`-like 头或 vendor libc++ 子集 |

**§6.4 第 37/38 条的实测细节**（很重要，单独展开）：

```
# (A) 只 -nostdlibinc，C++ 文件里 #include <new> / <type_traits> / <cstddef>
E[…] [pp_file_not_found] Line 2: 'new' file not found
E[…] [undeclared_var_use] Line 17: use of undeclared identifier 'std'
E[…] [ref_non_value]        Line 17: 'T' does not refer to a value
E[…] [no_member]            Line 17: no member named 'value' in the global namespace
E[…] [ovl_no_viable_function_in_call] Line 22: no matching function for call to 'operator new'
All checks completed, 5 errors

# (B) 把 g++ 的全部 include 目录（含 /usr/include）用 -isystem 补回去
All checks completed, 0 errors          ✅ 但 glibc 头也回来了

# (C) 取证：为什么必须补 /usr/include
$ grep -n "features.h" /usr/include/x86_64-linux-gnu/c++/12/bits/os_defines.h
39:#include <features.h>
$ g++ -E -v -x c++ /dev/null          # g++ 的 C++ include 目录
/usr/include/c++/12
/usr/include/x86_64-linux-gnu/c++/12
/usr/include/c++/12/backward
/usr/lib/gcc/x86_64-linux-gnu/12/include
/usr/local/include
/usr/include/x86_64-linux-gnu
/usr/include
```

顺便实测了 C++ 也能被 clangd 正确处理：clangd 识别出 `g++` 并自动把 argv[0] 改写成
`--driver-mode=g++`（日志原文），内核 C++（namespace/class/template/placement new）在
`-nostdlibinc + --target=x86_64-unknown-none -fno-exceptions -fno-rtti` 下 **0 errors**。

---

## 7. 结论与可直接落地的清单

### 7.1 对主题 A 五个问题的直接回答

1. **CDB 生成**：**make + bear 是本环境实测最可靠的一条**（`bear -- make`，必须 `make clean`，否则产出 `[]`）；
   其次是自己写 **wrapper shim**（JSONL 追加，可控、并发安全 —— 注意这是**自研方案，无官方背书**）；
   **CMake 用 `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON` + `-G "Unix Makefiles"` 实测通过**（不要默认 Ninja；该选项只支持 Makefile/Ninja 生成器）；
   `clang -MJ` 可用但坑多（空文件 / 非法 JSON / 多输入限制），只在 clang 工具链下考虑；
   `compiledb` 可以用（PyPI 0.10.7，注意写 `compiledb -n make`），但需校验条目数；
   **cargo / zig 都没有官方 CDB 支持**（`-Z build-plan` 已被 Cargo 1.93 删除；`zig build` 无相关选项），
   只能靠拦截或第三方库。详见 §1，以及 §7.4 的 5 条前提纠错。
2. **交叉工具链配置**：`--query-driver` 的语义是"**白名单允许执行的 driver**"，clangd 会跑
   `<driver> -E -x c - -v -nostdinc` 并解析 `Target:` 与 include 列表；只用它拿 **triple**，
   include 修正靠 `.clangd`。**避免 glibc 污染的唯二开关是 `-nostdlibinc`（clang）/ `-nostdinc -isystem $(gcc -print-file-name=include)`（gcc）**；
   `-ffreestanding`、`--target=<bare-metal>` **都不能**挡住 `/usr/include`。内核 flag 中
   `-ffreestanding/-mcmodel=kernel/-mno-red-zone/-mno-sse/-m64` 全部被 clangd 正确翻译；`-nostdlib` 被忽略（无害）。
3. **汇编边界**：**clangd 不能作为汇编的语言服务后端**。`.S` 会得到一个假诊断 + 全部语义功能为空；
   `.s` 和 NASM `.asm` 直接"Failed to parse command line"，`.asm` 的错误还无法屏蔽。
   正解：汇编写成 `.S`（clang 能真正汇编）+ `.clangd` 屏蔽诊断；或 IDE 侧把汇编体验交给
   `objdump -d/-S` + DWARF + gdb/QEMU 回溯（**这正好是本项目主链路已有的东西**）。
4. **嵌入 LSP**：帧是 `Content-Length: <UTF-8 字节数>\r\n\r\n` + JSON；握手 `initialize`（含
   `processId/rootUri/capabilities/initializationOptions`）+ `initialized`；
   编码协商必须用 `capabilities.offsetEncoding` 并在响应顶层读回；
   `compilationDatabasePath`/`fallbackFlags`/`clangdFileStatus`/`compilationDatabaseChanges` 四个
   initializationOptions **全部实测有效**，其中 `compilationDatabaseChanges` 最适合内核 IDE；
   关闭顺序 `shutdown`→`exit`（退出码 0），不先 shutdown 直接 exit 是退出码 1。
5. **多语言共存**：规范是"一服务端一工具"，必须多进程 + IDE 侧路由（**已实测 clangd + bash-language-server 并行**）；
   每 **(服务端, workspace)** 一对客户端；`compile_commands.json` 的冲突用
   `CompileFlags.CompilationDatabase` / `If.PathMatch` / 干脆改用 `compilationDatabaseChanges` 三条路解决；
   Rust/Zig 分别挂 rust-analyzer / zls（**有源，本环境未实测**）。

### 7.2 PrincessIDE 最小落地清单

**（1）自带 clangd 的获取（沙箱/无 root 可用）**
```bash
# 实测可用；生产环境建议换成上游 clangd zip（官方推荐做法）
apt-get download clangd-16 libclang-cpp16 libllvm16 libclang-common-16-dev \
                 libgrpc++1.51 libgrpc29 libprotobuf32 libc-ares2 libre2-9
for f in *.deb; do dpkg-deb -x "$f" "$PREFIX/"; done
```
⚠️ `$PREFIX` **必须是自己独占的目录**，不要用任何多 Agent/多进程共享的位置（本次实测被共享目录覆盖导致 `rc=127`，见 §0.2）。
**（2）启动包装器**：设 `LD_LIBRARY_PATH=$PREFIX/usr/lib/x86_64-linux-gnu:$PREFIX/usr/lib/llvm-16/lib`。
**（3）启动参数**：见 §4.9。
**（4）initializationOptions + capabilities**：见 §4.2 / §4.9（务必带 `capabilities.offsetEncoding`）。
**（5）工程 `.clangd` 模板**：见 §4.9（含 `Remove: [-nostdinc]`、`Add: [-nostdlibinc, --target=…]`、
`Index.StandardLibrary: false`、汇编分片 `Suppress: ['*']`）。
**（6）模板工程 Makefile**：用 `-nostdinc -isystem $(CC) -print-file-name=include)`，并加一个
   `compile_commands` target（bear / shim / `-MJ` 三选一）。
**（7）汇编**：不挂 clangd；用 objdump/gdb 管线；文本着色独立实现。
**（8）不要把 clangd 诊断当作构建状态**（§2.8）。

**（9）落地后的自检矩阵（本次最终实测状态，可直接照抄当验收标准）**

夹具 `/root/PrincessIDE/.researchA/proj/`，CDB 由 bear 3.1.1 生成 + 1 条 nasm 条目 + 1 条 g++ 条目，
根 `.clangd`（C 模板）+ `cxx/.clangd`（C++ 补 libstdc++ 目录）+ `.S` 分片 Suppress：

| 文件 | 期望 | 实测 |
|---|---|---|
| `kernel/kmain.c` | 0 errors | `All checks completed, 0 errors` ✅ |
| `cxx/klass.cpp` | 0 errors（需 libstdc++ isystem 目录） | `All checks completed, 0 errors` ✅ |
| `arch/x86_64/boot.S` | 0 errors（`Suppress: ['*']` 生效） | `All checks completed, 0 errors` ✅ |
| `arch/x86_64/isr.asm` | 必然失败，且**无法屏蔽** | `Failed to parse command line` + 2 条 LSP 诊断 ❌（符合预期，故 .asm 不挂 clangd） |

一条命令复现：

```bash
cd /root/PrincessIDE/.researchA/proj
for f in kernel/kmain.c cxx/klass.cpp arch/x86_64/boot.S arch/x86_64/isr.asm; do
  printf '%-34s' "$f"
  /root/PrincessIDE/.researchA/clangd-env.sh --check=$f --log=info 2>&1 \
    | grep -E 'All checks|Failed to parse' | tail -1
done
```

### 7.3 本报告未覆盖 / 未验证的部分（诚实清单）

- 上游 clangd standalone zip 的依赖与体积（GitHub API 被代理挡，**未验证**）。
- `--query-driver` 从哪个 clangd 版本引入（**未验证**）。
- 真正的交叉工具链（`x86_64-elf-gcc`）在本环境不存在，全部实验用宿主 `gcc` + `clang --target=x86_64-unknown-none` 代替。
  **`Target:` 行解析逻辑对交叉 gcc 是否等价，未验证。**
- clangd 16 之外版本的 `.S` 假诊断、`offsetEncoding` 协商行为、`fe_expected_compiler_job`（**均未验证**）。
- **bear 4.x 的任何行为**：本环境实测的是 Debian 3.1.1；4.x 的 Rust 重写、`bear.yml`、`intercept.mode`、
  `parse-sh`、`--overwrite` 等全部是【有源】而非实测。**`bear(1)` man page 正文与
  `reference/supported-compilers.html`（是否识别 rustc）未能抓取。**
- `compiledb` 的实际失败模式（**未实测**）；它对 `ccache`、`$(CC)` 覆盖、多配置重复命令的行为（**无官方来源**）。
- cargo / rust-analyzer / zig / zls 的实际行为（**未实测**，工具链在 bookworm 不可用）。
- `cargo build --message-format=json` 的 JSON message 具体字段（是否含完整 rustc argv）—— `external-tools.html#json-messages` **未抓取**。
- **`RUSTC_BOOTSTRAP` 的任何官方语义**（**未找到来源**，不得作为结论）。
- **rust-analyzer 对 C/C++ 文件的支持范围**（**未找到权威表述**；只能说"C 文件的编译信息只能由 clangd/`compile_commands.json` 提供，Cargo 不产生它"）。
- **Linux kernel 的 `make compile_commands.json` / `scripts/clang-tools/gen_compile_commands.py`**：
  kernel 官方文档 7.3.0-rc2 的 `kbuild.html` 与 `kbuild/llvm.html` **均无 `compile_commands` 字样**；
  仅有 LKML 补丁与下游源码树提及。**未找到官方来源。**
- **Zig 的 `zig cc` drop-in 语义 / `-target x86_64-freestanding` / `-fno-sanitize`**（**未找到官方来源**；`ziglang.org/blog/zig-cc/` 404）。
- `std.Build.Step.Compile` / `b.addCSourceFile` 的官方 API 文档正文（**未抓取**；官方 Build System 教程页 grep `addCSourceFile` 零匹配）。
- NASM `-g -F dwarf` 的调试信息质量，以及"是否存在可用的 NASM 语言服务器"（**未验证**）。
- 语义 token delta（`textDocument/semanticTokens/full/delta`）的响应结构（**未实测**）。
- 多服务端同时编辑**同一个**文档的行为（**未实测**；规范只说"未支持共享"，不建议尝试）。
- `cargo-compile-commands` crate：只能说"**未发现存在**"（docs.rs 404），不能说"确定不存在"。

### 7.4 ⭐ 对任务书里若干前提的纠错（避免把错误前提写进实现）

调研过程中发现任务书/常见说法里有 5 处事实性偏差，**如果照抄会变成编造**，在此单列：

| # | 常见错误说法 | 事实 | 来源 |
|---|---|---|---|
| 1 | Bear 是 "C++ rewrite" | **4.x 线是 Rust 重写**（crate：`bear-driver`、`intercept-supervisor`、`intercept-preload`、`bear-wrapper`、`crates/semantic`；CI 徽章是 `rust CI`；排障用 `RUST_LOG=debug`） | <https://rizsotto.github.io/Bear/understanding/how-it-works.html>、<https://raw.githubusercontent.com/rizsotto/Bear/master/README.md> |
| 2 | Bear 的配置文件叫 `bear.config` | **叫 `bear.yml`**，且顶层 `schema: "4.2"` 必填 | <https://rizsotto.github.io/Bear/reference/configuration.html> |
| 3 | `CompileFlags.CompilationDatabase` 的取值有 `Ancestors` / `Directory` | **没有 `Directory`**；合法取值是「一个目录路径」/`Ancestors`（默认）/`None` | <https://clangd.llvm.org/config> |
| 4 | 用 `cargo-compile-commands` / `cargo build --build-plan` 生成 CDB | **该 crate 未发现存在**（docs.rs 404）；**`-Z build-plan` 已在 Cargo 1.93 被彻底移除**（PR #16212） | <https://docs.rs/crate/cargo-compile-commands>、<https://doc.rust-lang.org/nightly/cargo/CHANGELOG.html> |
| 5 | `compiledb make -n` 让 compiledb 用 dry-run | **`-n` 是 compiledb 自己的 `--no-build`，必须写在 `make` 之前**：`compiledb -n make` | <https://pypi.org/project/compiledb/> |

（另：任务书问"bear 有没有 flag 跳过 preload 改用 wrapper" —— 在 bear **4.x** 里答案是"**没有 CLI flag**，
只有 `bear.yml` 的 `intercept.mode`"【有源】；而在 Debian **3.1.1** 里 `--force-preload` / `--force-wrapper`
**确实存在**且两者我都实测跑通了。写实现时按**你实际打包的版本**处理。）

---

## 附录 A：证据索引（关键命令一览）

```bash
# 环境
apt-cache madison clangd-14 clangd-15 clangd-16 ; apt-cache policy bear nasm rustc
clangd --version ; clangd --help            # 参数语义全部来自这里

# 安装与启动
apt-get download <pkgs> ; for f in *.deb; do dpkg-deb -x "$f" prefix/; done
LD_LIBRARY_PATH=prefix/usr/lib/x86_64-linux-gnu:prefix/usr/lib/llvm-16/lib prefix/usr/lib/llvm-16/bin/clangd --version

# CDB 生成
bear --output cc.json --make ...             # 或 --force-wrapper
PATH=shim:$PATH CDB_OUT=cdb.jsonl make ...   # 自写 shim
clang -MJ cc.json -c file.c                  # clang 原生
cmake -S . -B build -G "Unix Makefiles" -DCMAKE_EXPORT_COMPILE_COMMANDS=ON

# clangd 诊断
clangd --check=kernel/kmain.c --log=info     # cc1 参数、CSB 来源、全部诊断
clangd --check=file.S --log=info             # 复现 .S 假诊断

# include 语义取证
clang -ffreestanding -nostdinc   -fsyntax-only t.c
clang -ffreestanding -nostdlibinc -fsyntax-only t.c
clang -ffreestanding -nostdlibinc -E -v t.c        # include 搜索列表
clang -ffreestanding -H -fsyntax-only t.c          # 哪个头被真正加载
gcc    -ffreestanding -nostdinc -isystem "$(gcc -print-file-name=include)" -fsyntax-only t.c

# query-driver 取证
clangd --check=file.c "--query-driver=<dir>/*" --log=info
cat <dir>/calls.log                          # 收到: -E -x c - -v -nostdinc

# LSP 抓包（自写客户端）
python3 .researchA/lsp/run1.py               # initialize 帧 + capabilities
python3 .researchA/lsp/run3.py               # completion/definition/hover/references/rename/…
python3 .researchA/lsp/run6.py               # compilationDatabaseChanges / fallbackFlags
python3 .researchA/lsp/run8.py               # shutdown/exit 退出码、initialize 前请求
python3 .researchA/lsp/multilsp.py           # clangd + bash-language-server 并行
```

## 附录 B：外部来源清单

| 主题 | URL |
|---|---|
| clangd 安装 / 项目设置 / CDB 发现规则 / Bear 用法 / compile_flags.txt | <https://clangd.llvm.org/installation> |
| clangd 配置（CompileFlags/Index/Diagnostics/InlayHints/Hover/SemanticTokens/Documentation） | <https://clangd.llvm.org/config> |
| clangd 协议扩展（initializationOptions / offsetEncoding / fileStatus / ast / $/memoryUsage / inlayHints / inactiveRegions） | <https://clangd.llvm.org/extensions> |
| clangd 上游发版（官方推荐的获取方式） | <https://github.com/clangd/clangd/releases/latest> |
| compile_commands.json 格式规范 + `-MJ` + `compile_flags.txt` | <https://clang.llvm.org/docs/JSONCompilationDatabase.html> |
| LSP 3.17 规范（Base Protocol 帧格式、lifecycle、capabilities） | <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/> |
| Bear 发版信息（含 4.2.2 / 2026-09-05 与 4.2.0 breaking changes） | <https://github.com/rizsotto/Bear/releases.atom> |
| Bear 工作原理（preload / wrapper 两种机制与限制） | <https://rizsotto.github.io/Bear/understanding/how-it-works.html> |
| Bear 配置文件 `bear.yml`（schema / intercept.mode / duplicates / format / headers） | <https://rizsotto.github.io/Bear/reference/configuration.html> |
| Bear 4.x 命令行参考（确认已无 `--force-preload/wrapper`） | <https://rizsotto.github.io/Bear/reference/command-line.html> |
| Bear Makefile 食谱（`make clean` 必要性、`parse-sh` 限制） | <https://rizsotto.github.io/Bear/guides/recipes/compile-commands-for-makefile.html> |
| Bear CMake 食谱（CMake 导出的三种偏差、ninja/qbs/waf/bazel/clang 各自路径） | <https://rizsotto.github.io/Bear/guides/recipes/cmake.html> |
| Bear 排障（交叉 SDK 静默漏条目、LD_PRELOAD 未加载） | <https://rizsotto.github.io/Bear/guides/troubleshooting.html> |
| Bear Linux 平台说明（容器内运行、WSL2 镜像网络）、recipes 索引 | <https://rizsotto.github.io/Bear/platforms/linux.html>、<https://rizsotto.github.io/Bear/guides/recipes/index.html> |
| Bear 上游仓库（README：`bear --`；Rust CI 徽章） | <https://github.com/rizsotto/Bear> |
| compiledb（版本 0.10.7 / CLI / dry-run 自述） | <https://pypi.org/project/compiledb/> |
| compiledb 上游 | <https://github.com/nickdiego/compiledb> |
| CMake `CMAKE_EXPORT_COMPILE_COMMANDS`（3.5 加入；仅 Makefile+Ninja） | <https://cmake.org/cmake/help/latest/variable/CMAKE_EXPORT_COMPILE_COMMANDS.html> |
| GNU make `-n`/`-t`/`-q` 语义（哪些行仍会执行） | <https://www.gnu.org/software/make/manual/html_node/Instead-of-Execution.html> |
| GNU make `shell` 函数（在展开时执行，不受 `-n` 抑制） | <https://www.gnu.org/software/make/manual/html_node/Shell-Function.html> |
| Cargo `cargo build` 选项表（无 compile_commands） | <https://doc.rust-lang.org/cargo/commands/cargo-build.html> |
| Cargo changelog（1.93 移除 `-Z build-plan`，PR #16212） | <https://doc.rust-lang.org/nightly/cargo/CHANGELOG.html> |
| Cargo unstable 特性表（已无 build-plan；plumbing = `cargo metadata`） | <https://doc.rust-lang.org/nightly/cargo/reference/unstable.html> |
| rustc `--emit` 合法取值 / `-Z` | <https://doc.rust-lang.org/rustc/command-line-arguments.html> |
| Zig Build System（`zig build --help`，无 compile_commands 选项；`-Dtarget`） | <https://ziglang.org/learn/build-system/> |
| Zig `gen-compile-commands`（第三方） | <https://codeberg.org/smithcol11/gen-compile-commands> |
| Bazel compile_commands extractor | <https://github.com/hedronvision/bazel-compile-commands-extractor> |
| rust-analyzer | <https://rust-analyzer.github.io/> / <https://github.com/rust-lang/rust-analyzer> |
| zls | <https://github.com/zigtools/zls> |
| NASM 手册（`-g` / `-F` 等选项） | <https://www.nasm.us/doc/> |

---

*报告完成时间：本会话内。所有【实测】结论均可通过附录 A 的命令在本工作区复现。*
