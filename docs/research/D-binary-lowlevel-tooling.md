# 研究主题 D：二进制与底层可视化技术选型

> **范围**：ELF 段节/符号/调试信息、hex 查看与编辑、反汇编、页表遍历、GDT/IDT 描述符解析、
> QEMU monitor 数据解析 —— 六项可视化能力的 **Rust crate 选型 + 工程做法 + 落地参数**。
>
> **证据标注约定**（全文逐条标注）：
> - **[已实测]** 本环境真实执行命令/真实抓取输出，原始产物在 `.researchD/out/`；
> - **[有来源]** 有可引用来源（crates.io 元数据缓存、QEMU 源码、官方文档），并附 URL；
> - **[推测]** 经验推断，未在本环境验证；
> - **[未实测]** 明确声明"本轮没做"的部分集中列在 §7，**不冒充实测**。
>
> **证据目录**：`.researchD/out/`（`.researchD/` 已 gitignore）。
> 上一轮已抓好的 crate 元数据在 `.researchD/meta/*.json`，本报告直接复用，未重复抓取。

---

## 0. 实测环境（本报告的证据底座）

| 项 | 值 | 来源 |
|---|---|---|
| OS / 架构 | Debian 12 (bookworm) / x86_64 / 无显示器 | [已实测] |
| binutils | GNU readelf/objdump/nm/addr2line **2.40** | [已实测] `readelf --version` |
| GCC / G++ | 12.2.0 (Debian 12.2.0-14+deb12u1) | [已实测] |
| Rust | **rustc/cargo 1.98.1** (48a229cea 2026-09-01) | [已实测] |
| QEMU | **7.2.22**（Debian `1:7.2+dfsg-7+deb12u18+b3`） | [已实测] QMP `query-version` |
| GDB | GNU gdb 13.1-3 | [已实测] |
| NASM | 2.16.01 | [已实测] |
| 样本 | `fixtures/refkernel/build/refkernel.elf`（ELF64 EXEC，带完整 DWARF）、`/bin/ls`（ELF64 DYN PIE，带 `.gnu_debuglink`/`.gnu_debugaltlink`）、`.researchD/kern/ptdemo{4,5}.elf`（本报告新建的合成页表/GDT/IDT 探针内核） | [已实测] |

**新建的实测资产**（`.researchD/`，可复跑）：

| 文件 | 作用 |
|---|---|
| `kern/ptdemo.asm` | Multiboot1 内核：建 4 级/5 级页表（4 KiB + 2 MiB + 1 GiB 混合、故意留一个 not-present 空洞、标 NX/PWT/PCD/GLOBAL/US）→ 进长模式 → 装 GDT（含 64 位 TSS 描述符）+ IDT（6 个门，含 IST、DPL3 trap gate）→ halt |
| `rustprobe/` | 用 §1 选定 crate 写成的实测程序（`elf`/`dwarf`/`dis`/`cap`/`demangle`/`mmap` 六个子命令）。**依赖已全部下好解包**（`.toolchain/cargo/registry/src/.../object-0.40.0` 等），但**本轮没有编译运行**（见 §7）|
| `kern/build.sh` | `nasm -f elf32` + `ld -m elf_i386 -T linker.ld`，产出 `ptdemo4.elf`（LA48）/ `ptdemo5.elf`（LA57） |
| `probe2.py` / `probe2iso.py` / `probe3.py` / `probe4.py` | 起 QEMU（HMP unix socket + QMP unix socket），把 `info *` / `xp` / `x` 输出与 QMP JSON 原样落到 `out/mon-<tag>.txt` |
| `hexwin.py` / `hexwin2.py` | 8 GiB 稀疏磁盘镜像上的 mmap vs pread 访问策略实测 |
| `cratescan.py` | 用 tarball 核对选型 crate 的真实 `Cargo.toml` 与 API 入口 |

**修过的一个真实 bug（值得写进报告当教训）**：第一版 `ptdemo.asm` 的页表循环用
`mov [PT + ecx*4], eax` —— PAE/长模式下页表项是 **8 字节**，不是 4 字节。QEMU 直接给出
`v=0e e=0010 ... CR2=0000000000100273`（取指缺页，地址就是 `mov cr0` 的下一条），
`→ #DF → Triple fault`。改成 `ecx*8` 后立刻跑通。[已实测] `.researchD/out/` 与
`/tmp/d4.log` 有完整日志。
**这条教训对 IDE 同样成立：任何手写的页表遍历必须假设"读到的东西可能是垃圾"。**

---

## 1. Rust crate 选型（逐项表态）

### 1.0 选型总表

| 用途 | **选它** | 版本 | 许可证 | 一句话结论 |
|---|---|---|---|---|
| ELF/目标文件解析 | **`object`** | 0.40.0 | Apache-2.0 OR MIT | 唯一同时覆盖 ELF/PE/COFF/archive/wasm + 压缩段 + DWARF 生态的解析器 |
| DWARF 底层 | **`gimli`** | 0.34.0 | MIT OR Apache-2.0 | DWARF 解析的事实标准，object/addr2line 同生态 |
| 地址→源码行 | **`addr2line`** | 0.27.1 | Apache-2.0 OR MIT | `Context::new(&File)` 一行搞定，且用 `.debug_aranges` 加速 |
| x86 反汇编（主力） | **`iced-x86`** | 1.21.0 | MIT | 纯 Rust、零 C 依赖、带流控/寄存器读写元数据，桌面打包最省心 |
| 反汇编（多架构备选） | `capstone` | 0.14.0 | MIT | 只有确定要 ARM/RISC-V 时才引；它会拉 C 工具链进构建 |
| Rust 符号 demangle | **`rustc-demangle`** | 0.1.28 | MIT/Apache-2.0 | v0(`_R...`) 与 legacy(`_ZN...17h..E`) 一个库全吃 |
| C++ 符号 demangle | **`cpp_demangle`** | 0.5.1 | MIT OR Apache-2.0 | 纯 Rust Itanium 解，避免上万符号时 fork `c++filt` |
| 内存映射 | **`memmap2`**（仅只读符号文件） | 0.9.11 | MIT OR Apache-2.0 | 只映射 ELF；**GB 级磁盘镜像走 pread**（§2 有硬数据） |
| **不选** | `goblin` / `elf` / `zydis` / `yaxpeax-x86` | — | — | 理由见下 |

**整条链的 MSRV = Rust 1.88**（由 `gimli` 0.34.0 与 `addr2line` 0.27.1 决定）。
本环境 rustc 1.98.1 满足。[有来源] crates.io 元数据 `.researchD/meta/*.json`
（`object` MSRV 1.85 / `gimli` 1.88 / `addr2line` 1.88 / `iced-x86` 1.57 /
`capstone` 1.70 / `memmap2` 1.65）。

**许可证体检**：以上全部为 MIT 或 Apache-2.0（或两者双许可），**无 GPL/AGPL 污染**，
与 Tauri 桌面分发兼容。[有来源] `.researchD/meta/summary.txt`

---

### 1.1 ELF / 目标文件解析 → **`object` 0.40.0**

**结论：选 `object` 0.40.0。不要用 `goblin`，也不要用 `elf`。**

**理由**

1. **同生态一致性**（最重要）。`addr2line 0.27` 与 `gimli` 都直接吃 `object::File`。
   选 object 意味着「段/节/符号」与「DWARF」由**同一套 section/file_range 语义**解释；
   选 goblin 则要在 goblin 与 gimli 之间手写适配层，两套解析器对
   `SHT_NOBITS`（`.bss` 无文件内容）、压缩段、重叠段的处理不完全一致，
   在 IDE 里表现为"符号列表和源码行对不上"的偶发 bug。
2. **格式覆盖面**。object 支持 ELF/PE/COFF/Mach-O/archive/wasm/Goff。
   B 报告已实测 `objcopy --target=efi-app-x86_64` 产出 **PE32+ EFI application**
   —— 内核开发绕不开 UEFI，object 能直接解析，goblin 要换一套 API。
3. **压缩调试段**。`ObjectSection::uncompressed_data()` 直接解 SHF_COMPRESSED /
   `.zdebug_*`。内核 ELF 常带 `--compress-debug-sections=zlib`，不做这一步
   DWARF 会静默解析失败。
4. **可写回**。object 有 `write` feature，是 hex 编辑器"改一段再回写"
   （§2.7）的现成升级路径；goblin 基本只读。
5. `read` feature 走零拷贝 `&[u8]`，配合 `memmap2` 可避免把 200 MB 的 vmlinux
   复制一遍。[有来源] <https://docs.rs/object/0.40.0/object/>

**风险**

- **MSRV 1.85 + 0.x 语义化**：0.40.0 发布于 2026-08-01（距今约 1 个月），
  0.x 的次版本会 breaking。**必须 `=0.40.0` 精确锁定**，或依赖 `Cargo.lock` 并禁止自动升级。
  [有来源] crates.io 元数据。
- `object` 默认开启 `compression` 会拉入 flate2/ruzstd 等依赖，增大二进制体积；
  如果确定只读未压缩 ELF，可 `default-features = false, features = ["read","elf","std"]`，
  但一旦遇到压缩段就会失败 —— **建议保留 `compression`**，体积换确定性。[推测]
- 0.40 的 `Symbol`/`Section` API 与 0.32/0.36 有差异，网上大量示例代码用旧 API，
  照抄会编译不过。[推测]

**API 核对（已对着发布版源码逐条确认，不是凭记忆）** [已实测：读 registry 里解包出的
`object-0.40.0/src/`，未编译]：

| API | 位置 | 说明 |
|---|---|---|
| `object::File::parse(&[u8])` | `src/read/any.rs` | 一次解析，返回 `File<'_, &[u8]>` |
| `Object::section_by_name` / `symbol_by_name` | `src/read/traits.rs:200` | **trait 方法，没有 `pub`** |
| `ObjectSection::uncompressed_data()` | `src/read/traits.rs:424` | 返回 `Cow<'data,[u8]>`，透明处理 SHF_COMPRESSED |
| `ObjectSymbol::kind() -> SymbolKind` | `src/read/traits.rs:530` | 用于把符号分成 text/data/… |
| `ObjectSegment::file_range()` | `src/read/traits.rs` | **`(u64, u64)` 文件偏移 + 大小，hex 叠加层要靠它** |
| `BinaryFormat` / `Architecture` / `Endianness` | `src/read/mod.rs` | 头部信息 |
| Cargo 实测 feature 列表 | `Cargo.toml` | 与 crates.io 元数据一致（另有 `xcoff`）|

**为什么不选 `goblin` 0.10.7（MIT）**：功能上够用、编译更快、依赖更少，
但它是**独立生态**，DWARF 仍要用 gimli，于是引入"两个 ELF 解析器"的第一条问题；
且其 API 暴露的是更原始的 struct（`Elf`/`ProgramHeader` 字段直读），
IDE 需要的"统一 section/segment/symbol 抽象"要自己搭。
**定位：降级备选**（如果 object 的 MSRV 或依赖体积成为硬约束）。
[有来源] <https://docs.rs/goblin/0.10.7/goblin/>

**为什么不选 `elf` 0.8.0（MIT/Apache-2.0）**：它是**最窄**的一个 ——
只做 ELF 的零拷贝薄封装，没有 DWARF、没有符号便利 API、没有其他格式。
近 90 天下载 3.3 M vs object 107 M；单一维护者。
选它等于把 object 已经做好的抽象全部重写一遍。[有来源] `.researchD/meta/summary.txt`

---

### 1.2 DWARF 与源码行 → **`gimli` 0.34.0 + `addr2line` 0.27.1**

**结论：两个都上，分层使用 ——
「地址→文件:行:列 / 内联链 / 函数名」用 `addr2line` 高层 API；
「类型、结构体、变量、DIE 树」用 `gimli` 底层 API。**

**理由**

1. `addr2line::Context::new(&object::File)` 一步完成 DWARF 装载，
   `find_location(addr)` / `find_frames(addr)` 直接给出 IDE 要的结果。
2. **有加速索引可用**：`refkernel.elf` 里 `.debug_aranges` 存在
   （`readelf -SW` 实测：`.debug_aranges PROGBITS 0000000000000000 004590 0000f0`），
   内容是标准 CU 覆盖表：
   ```
   Length: 44 / Version: 2 / Offset into .debug_info: 0
       Address            Length
       0000000000100040 00000000000000dc
   ```
   [已实测] `readelf --debug-dump=aranges fixtures/refkernel/build/refkernel.elf`。
   addr2line 用它在 O(log n) 内定位 CU，再跑 line program；
   没有 aranges 时退化为逐 CU 扫描。
3. 与 §1.1 的同生态一致性（同一个 `object::File`）。
4. `loader` feature 存在（crates.io 元数据 features 列表里有 `loader`），
   用于查找 `.gnu_debuglink` 指向的分离调试文件 —— `/bin/ls` 同时带
   `.gnu_debuglink` **和** `.gnu_debugaltlink` [已实测 `readelf -SW /bin/ls`]，
   发行版二进制必然踩这个。[有来源] <https://docs.rs/addr2line/0.27.1/addr2line/>

**风险（这条最关键）**

- `Context::new` 是**饿汉式**：装载时解析全部 CU 的 line program 并建内存索引。
  对 200 MB–1 GB 的 vmlinux（`.debug_info` 可达 GB 级）会**吃几百 MB 内存 + 数秒到数十秒 CPU**。
  → **必须把它当"每文件一个、后台建、可淘汰"的重对象**，不能每次查询重建，
  也不能同时持有多个（§2.4 给了 LRU 参数）。[推测，依据是 addr2line 的 API 形态与
  DWARF line program 的固有结构]
- `.gnu_debugaltlink`（DWZ 交叉文件）**不一定被 `loader` feature 覆盖**
  （该 feature 主要处理 `.gnu_debuglink`）。`/bin/ls` 有这个节，
  说明 Debian 发行版二进制会用到；**本轮未编译验证**（§7）。[未实测]
- **优化等级会欺骗行号**：B 报告已实测 `-O2` 下 `add()` 被内联进 `compute()`，
  addr2line 把地址归到**被内联函数自身所在行而非调用点**，且 `-i` 内联链只回一层。
  → **IDE 必须"反汇编 + 行号"并排**，不能只显示一个行号当结论。
- 链接地址 ≠ 运行地址时（`objcopy -O binary` + bootloader 加载到别处、
  高半核、KASLR）必须先做地址换算；`ET_DYN`/PIE 内核要按运行时基址归一。
  B 报告 §5.2 已记录，本报告不重复。[已实测（B 报告）]

**实测 golden（本环境，可直接当回归基准）**

```console
$ addr2line -e fixtures/refkernel/build/refkernel.elf -f -C -i 0x100b3d
refkernel_fault_probe
/root/PrincessIDE/fixtures/refkernel/kernel.c:100
```
[已实测] `.researchD/out/refkernel-symbolize.txt`。
同一地址 `objdump -d` 给出 `100b3d: 0f 0b  ud2`，
与 `smoke-boot.sh` 抓到的 `FAULT_RIP=0x0000000000100b3d` 完全对上 ——
**这条链路（QEMU 故障 RIP → ELF 符号 → 源码行）已端到端闭环。**

---

### 1.3 反汇编 → **`iced-x86` 1.21.0（主力）**，capstone 仅作多架构备选

**结论：x86_64 内核 IDE 主用 `iced-x86` 1.21.0。`capstone` 0.14.0 只在
"确定要支持 ARM64/RISC-V 内核"时才引入。`zydis`、`yaxpeax-x86` 不选。**

**理由**

1. **纯 Rust、零 C 依赖**。这对 Tauri 桌面（Linux/Windows/macOS + CI）
   是决定性的：不需要 `cc`、不需要 CMake、不需要交叉编译 C 工具链、
   不会因宿主 gcc 版本产生差异。
2. **元数据齐全**：`InstructionInfoFactory::info(&Instruction) -> &InstructionInfo`
   （读写了哪些寄存器/内存操作数，`src/info/factory.rs:137`）、
   `Instruction::flow_control()`（分支/调用/返回/中断）、`Decoder` 的解码循环、
   以及 `block_enc` 模块（基本块切分）。
   IDE 要画"跳转箭头 / 数据流高亮 / 基本块"必须靠这些，
   capstone 的 detail 模式则需要遍历 CSV 表，用手写逻辑重建。
   **（本报告初稿把这条写成 `Instruction::instruction_info()` —— 对着 1.21.0 源码核对后更正：
   正确入口是 `iced_x86::InstructionInfoFactory`。）** [已实测：读
   `iced-x86-1.21.0/src/info/factory.rs`，未编译]
3. **多语法输出**：Intel / NASM / GAS / MASM 可切换（version 1.21 的 feature 列表里
   四个都在）。IDE 可以跟随用户偏好，无需自己写格式化器。
4. 新指令覆盖好（AVX-512 等），一个 crate 就能吃下内核里所有手写汇编。

**风险**

- **上游节奏**：1.21.0 发布于 **2024-01-20**，距今约 20 个月无新版本
  [有来源] `.researchD/meta/summary.txt`。功能已相当完整（成熟库的典型形态），
  但"未来新指令"依赖作者。→ 锁 `=1.21.0` 并接受该风险。
- **默认 feature 太重**：default 会带入 `encoder` / `code_asm` / `block_encoder` /
  `op_code_info`，对只读反汇编的 IDE 是纯浪费（crate 包 1.24 MB）。
  **必须**：
  ```toml
  iced-x86 = { version = "=1.21.0", default-features = false,
               features = ["std","decoder","instr_info","intel","nasm","gas","fast_fmt"] }
  ```
  （feature 名取自 crates.io 元数据里的 features 列表 [有来源]。）
- 只有 x86/x86_64 —— 但 PrincessIDE 的定位就是 x86_64 内核，这不是缺点。

**为什么 `capstone` 只做备选**：0.14.0（MIT）质量没问题（crates.io 近 90 天 946 K 下载），
但 **`capstone-sys` 的 `build.rs` 用 `cc::Build` 编译 capstone 的 C 源码**
（`capstone-sys-0.18.0/build.rs:117`），所以**必须有本地 C 编译器**；
`bindgen` 与 `libclang` **只在启用 `use_bindgen` feature 时才需要**
（`build.rs:38-46` 全在 `#[cfg(feature = "use_bindgen")]` 之下）。
→ **更正**：默认路径不需要 libclang（本报告初稿的担心是多余的），
但**仍然需要 gcc/clang**，交叉编译与 Windows 打包要额外准备 C 工具链。
对一个只做 x86_64 的 IDE，为了一个反汇编引入 C 工具链是净负担。
[已实测：读 `capstone-sys-0.18.0/build.rs`，未编译] [有来源]
<https://docs.rs/capstone/0.14.0/capstone/>

**为什么 `zydis` 4.1.1 不选**：MIT，但 crates.io **近 90 天下载仅 9.6 K**
（iced-x86 957 K、capstone 946 K），crate 包 881 KB 里捆绑 Zydis C 源码，
且 Rust 绑定最后一次发版 2024-03-09。生态过小 + C 构建 + 无收益。
[有来源] `.researchD/meta/summary.txt`

**为什么 `yaxpeax-x86` 2.2.0 不选（但可记为纯 Rust 备选）**：0BSD 许可、
纯 Rust，作为 iced-x86 的"万一"备选可以；但下载量 116 K、
API 更偏解码器原语，格式化与流控元数据不如 iced 完整。
[有来源] `.researchD/meta/summary.txt`

**实测对照（QEMU 自带反汇编器，可做交叉验证）**

QEMU monitor 的 `x /Ni <addr>` 内置一个 AT&T 语法反汇编器，实测可用：
```
$ x /4i 0x100b39
0x00100b39:  55                       pushq    %rbp
0x00100b3a:  48 89 e5                 movq     %rsp, %rbp
0x00100b3d:  0f 0b                    ud2
0x00100b3f:  bf 80 11 10 00           movl     $0x101180, %edi
```
[已实测] `.researchD/out/mon-refkernel2.txt`。
`objdump -d -M intel` 对同一段给出的字节与助记符一致
（`.researchD/out/refkernel-symbolize.txt`）。
**用途**：可以拿它当 IDE 反汇编引擎的"第三方裁判"——三路一致才敢相信。[已实测]

---

### 1.4 demangle → **`rustc-demangle` 0.1.28 + `cpp_demangle` 0.5.1**

**结论：两个都上，按前缀分派（`_R` / `_ZN..17h..E` → rustc-demangle；
`_Z..` 其余 → cpp_demangle；都失败则原样返回）。不要 fork `c++filt`/`nm -C`。**

**理由**

1. **两种 Rust mangling 都要支持**。本环境实测 rustc 1.98.1 默认产出 **v0**
   （`nm librdemo.rlib` 全是 `_RINvCsfJh2wXCkyFt_13princess_demo11generic_add...`），
   但大量既有 rlib/staticlib 仍是 legacy（`_ZN...17h<hash>E`）。
   `rustc-demangle` 一个库两种都吃（`demangle()` / `try_demangle()`）。
   [已实测] `.researchD/demangle/` 下的样本与 `nm` 输出。
2. `cpp_demangle` 是 gimli 组织出品、纯 Rust、支持 Itanium ABI，
   `ParseOptions` 能关掉返回类型等噪声（`no_return_type`）。
   实测本环境 `g++ -std=c++17 -O2` 产出的符号形态（含模板、虚函数、析构器多版本）：
   ```
   _ZNK4Base1fEi  →  Base::f(int) const
   _ZN7DerivedD0Ev → Derived::~Derived()      (D0/D1/D2 三个析构器变体!)
   _Z5greetRKNSt7__cxx1112basic_stringIcSt11char_traitsIcESaIcEEE
                  → greet(std::__cxx11::basic_string<char, ...> const&)
   ```
   [已实测] `nm .researchD/demangle/clib.o | c++filt`，见 `.researchD/out/demangle-golden.txt`。
3. **不能 shell out**：符号列表动辄上万条，一次 `c++filt` 进程 5–20 ms
   且需要维护长驻批处理管道，跨平台还要处理进程管理。
   纯 Rust 库在同一进程内做，可缓存、可并行。[推测，依据是进程创建开销的量级]

**风险**

- **`cpp_demangle` 不支持 MSVC 名字修饰**。未来若要解析 Windows 驱动/PE，
  需要另一套方案（object 有 `pe` feature 但 demangle 是另一件事）。[有来源]
  <https://docs.rs/cpp_demangle/0.5.1/cpp_demangle/>
- **legacy Rust 符号里的 `17h<hash>` 无法还原源码路径**；demangle 结果只能给
  crate/模块/泛型参数名。要源码路径必须靠 DWARF。
- **同一函数的多个符号变体**：C++ 的 `D0/D1/D2`（complete/base/deleting destructor）
  实测同时出现（`_ZN7DerivedD0Ev`/`D1Ev`/`D2Ev`，见上）。
  Rust 有 `.llvm.<hash>` / `.cold` 后缀（实测 `_Z3usev.cold` 也存在）。
  **UI 必须做"按 demangle 友好名聚合 + 展开变体"，否则符号列表会有 3 倍噪声。**
  [已实测]
- Rust 泛型实例化会让同一个泛型函数出现 N 个符号（实测 `generic_add` 被
  LRU 缓存 5 个不同实例化）。符号列表需要按"泛型根名"折叠。[已实测]

---

### 1.5 内存映射 → **`memmap2` 0.9.11（仅只读符号文件）**

**结论：ELF/符号文件用 `memmap2::Mmap::map`。GB 级磁盘镜像**不要**裸 mmap，
用 `pread` + 有界 LRU 窗口缓存。**

**理由 —— 本报告最硬的一组实测数字**（8 GiB 稀疏磁盘镜像，20000 次随机 4 KiB 访问）：

| 策略 | 耗时 | 每次 | **RSS 增量** |
|---|---|---|---|
| `mmap` 整文件（默认 advice） | 1.673 s | 83.6 µs | **+1 120 416 kB（≈1.12 GB）** |
| `mmap` + `MADV_RANDOM` | 0.121 s | 6.0 µs | +1 116 276 kB |
| `mmap` + `MADV_RANDOM` + 定期 `MADV_DONTNEED` | 0.239 s | 12.0 µs | **0 kB** |
| `pread` 1 字节/op | 0.043 s | 2.1 µs | **0 kB** |
| `pread` 4 KiB/op | 0.053 s | 2.7 µs | **0 kB** |
| **`pread` + LRU(256 个 4 KiB 窗口 = 1 MiB)** | **0.079 s** | 4.0 µs | **+104 kB** |

[已实测] `.researchD/hexwin2.py` → `.researchD/out/hexwin2.txt`。
补充：8 GiB 文件的 `mmap` 本身只要 **0.029 ms**、RSS 不变 —— 映射是 O(1)，
**问题出在访问**：内核对文件映射的随机缺页做 fault-around 预读 16 页 = 64 KiB，
20000 次随机访问把 1.28 GB 页缓存塞进进程 RSS（实测 1.12 GB，量级吻合）。
[已实测] `.researchD/out/hexwin.txt`。

**这是反直觉但可复现的结论：在这个负载下 `pread` 比 mmap 更快（2.1 µs vs 83.6 µs）
且 RSS 有上界。** mmap 的"快"只在顺序访问 + 热页反复读时成立。

**风险**

- **`Mmap::map` 是 `unsafe`，且文件截断会发 SIGBUS**（不可捕获，直接杀进程）。
  IDE 打开"QEMU 正在写的镜像"或用户外部 `truncate` 镜像 → 崩溃。
  **必须**：访问前校验文件大小；或用 `pread` 路线（截断只导致短读，可优雅处理）。
  [有来源] <https://docs.rs/memmap2/0.9.11/memmap2/struct.Mmap.html>
- `memmap2` 在 Windows 上对已映射文件的删除/截断语义更严格，
  Tauri 目标平台包含 Windows → 用 `pread` 完全绕开。[推测]
- `memmap2` 0.9.11 无任何 feature（元数据 features 列表为空），依赖只有 libc，
  非常轻；`Advice`/`advise()` 在 0.9 存在（**本轮未编译验证，见 §7**）。[有来源]

---

### 1.6 建议的 `Cargo.toml` 片段（可直接抄）

```toml
[dependencies]
# --- ELF / 目标文件 ---
object = { version = "=0.40.0", default-features = false,
           features = ["read", "elf", "archive", "compression", "std"] }
# --- DWARF / 源码行 ---
gimli     = { version = "=0.34.0", default-features = false, features = ["read", "std"] }
addr2line = { version = "=0.27.1", default-features = false, features = ["std", "loader"] }
# --- demangle ---
rustc-demangle = "=0.1.28"
cpp_demangle   = { version = "=0.5.1", default-features = false, features = ["std"] }
# --- 内存映射（只读符号文件；磁盘镜像用 pread） ---
memmap2 = "=0.9.11"
# --- 反汇编 ---
iced-x86 = { version = "=1.21.0", default-features = false,
             features = ["std", "decoder", "instr_info", "intel", "nasm", "gas", "fast_fmt"] }
# 只有确定要支持非 x86 架构时才加（会引入 C 工具链构建依赖）：
# capstone = { version = "=0.14.0", default-features = false,
#              features = ["std", "arch_x86", "full"] }
```

**为什么全部用 `=` 精确锁**：这条链里 4 个是 0.x（`object`/`gimli`/`capstone`/`cpp_demangle`），
0.x 的 `^` 语义允许次版本 breaking。对本项目这种"要把解析结果当事实来渲染"
的场景，宁可手动升级也不要静默 breaking。[推测]

### 1.7 选型 crate 的 API 核对表（**发布版源码级**，未编译运行）

上一轮只抓了 crates.io 元数据（版本/许可/MSRV/feature），本报告补了一层：
把各版本的 `.crate` 包解开，**在发布版源码里逐条核对 API 入口是否真的存在、叫什么名字**。
下面每一条都是"文件:行号"级别的确认，**但注意：这是读源码，不是编译运行**（§7 有说明）。

| crate | 已核对的 API | 位置 | 结果 |
|---|---|---|---|
| `object` 0.40.0 | `File::parse` / `Object::section_by_name` / `Object::symbol_by_name` / `ObjectSection::uncompressed_data` / `ObjectSymbol::kind` / `ObjectSegment::file_range` / `BinaryFormat` | `src/read/any.rs`、`src/read/traits.rs:200,424,530` | 全部存在。**注意 `symbol_by_name`/`uncompressed_data`/`kind` 是 trait 方法，签名里没有 `pub`** —— 网上很多示例写 `pub fn` 是错的 |
| `gimli` 0.34.0 | `Dwarf` / `Dwarf::load` / `EndianSlice` / `DebugLine` / **`DebugAranges`** / `DebugFrame` / `Section` trait | `src/read/*` | 全部存在，含 `.debug_aranges` 加速结构（§1.2 的加速结论有据） |
| `addr2line` 0.27.1 | `Context::new` / `find_location` / `find_frames` / `Location` / `Frame` / `Frame::function().demangle()`、feature `loader` | `src/lib.rs`、`src/frame.rs` | 全部存在；`loader` feature 确实在 feature 列表里 |
| `iced-x86` 1.21.0 | `Decoder` / `Decoder::with_ip` / `decode_out` / `Instruction` / `NasmFormatter` / `IntelFormatter` / `GasFormatter` / `flow_control` / **`InstructionInfoFactory::info`** / `block_enc` 模块 | `src/decoder.rs`、`src/formatter/*`、`src/info/factory.rs:137`、`src/block_enc.rs` | 全部存在。**`instruction_info` 不是 `Instruction` 的方法**，入口是 `InstructionInfoFactory`（本报告已据此更正 §1.3） |
| `capstone` 0.14.0 | `disasm_all` / `disasm_count` / `Capstone` / `InsnDetail` / `ArchMode` / `ArchSyntax` | `src/lib.rs`、`src/arch/mod.rs:110`（`define_subset_enum!([ArchMode = Mode] ...)` 宏生成）、`src/arch/x86.rs` | 存在，但 **`ArchMode` 是 feature 门控的宏生成类型** —— 不开 `arch_x86` 就没有 `capstone::arch::x86::ArchMode` |
| `capstone-sys` 0.18.0 | `build.rs` 用 `cc::Build`（:117）；`bindgen` 全在 `#[cfg(feature="use_bindgen")]` 内（:38,:46,:167） | `capstone-sys-0.18.0/build.rs` | **默认需要 C 编译器，不需要 libclang** |
| `memmap2` 0.9.11 | `unsafe fn map` / `map_mut` / `Mmap` / `MmapMut` / `advise` / **`advise_range`** / `flush` / `Advice::Random` / `Advice::DontNeed` | `src/lib.rs`、`src/advice.rs` | 全部存在 —— §2.3 给的 `advise(Advice::Random)` / `Advice::DontNeed` 写法成立。crate `Cargo.toml` 的 `[features]` 为空（0 个 feature） |
| `rustc-demangle` 0.1.28 | `demangle` / `try_demangle` / `Demangle` / `v0` 模块 | `src/lib.rs`、`src/v0.rs` | 存在；v0 与 legacy 两条路径都在 |

**这张表的价值**：它把"我记得是这个 API"变成了"这个版本里确实叫这个名字"，
可以省掉 P5 实现阶段的一次试错；**但它不能替代编译**（签名细节、feature 组合、
trait 对象/lifetime 约束仍可能踩坑）。所以 §7 第 1 条仍然成立。

---

## 2. hex 视图工程做法（GB 级镜像不 OOM）

### 2.1 三层架构（结论）

```
[磁盘镜像 / 裸设备 / QEMU 镜像]      ← 只读打开，记录 size
        │
        │  (A) 有界窗口缓存：pread + LRU(256 × 4 KiB)   ← 实测 RSS +104 kB
        ▼
[后端 offset index]                 ← 稀疏区段 / 已知区域 的区间表
        │
        │  (B) 行区间 RPC：read_lines(row_start, row_count) → Vec<HexRow>
        ▼
[前端虚拟滚动]                       ← 只渲染可见行 ± overscan，绝不全量 DOM
```

### 2.2 绝对不要做的三件事

1. **不要 `fs::read` 整个镜像**。8 GiB 镜像会立刻 OOM。
2. **不要裸 `mmap` 整个镜像再随机访问**。实测 RSS 涨 1.12 GB（§1.5），
   在 4–8 GB 内存的笔记本上会触发页回收颠簸，表现为"滚动 hex 时整机卡死"。
   注意：这些是**干净的 file-backed 页，理论上可回收**，所以不是硬 OOM，
   但回收压力会打到整个系统。**结论仍然是别这么干。**[已实测 + 推测]
3. **不要把总行数交给浏览器 DOM 高度**。8 GiB / 16 B 每行 = **5.37 亿行**；
   即使 24 px 行高也是 1.29e10 px，远超浏览器元素高度上限。
   用 `transform: translateY()` 自绘滚动条，或做"页 + 页内虚拟滚动"。[推测]

### 2.3 具体的窗口参数（可直接落地）

| 参数 | 建议值 | 理由 |
|---|---|---|
| 窗口大小 | **4 KiB** | 与磁盘页/内存页对齐；一次 `pread` 覆盖 256 行（16 B/行）；实测 2.7 µs/次 |
| 窗口数上限（LRU） | **256**（= 1 MiB） | 实测 RSS 增量仅 104 kB，命中率对"顺序滚动 + 少量回看"足够 |
| 预取 | 滚动方向 **+2 窗口** | 顺序滚动时把 readahead 变成显式预取，避免滚动卡顿 |
| 请求去抖 | **40–60 ms** 合并 | 一次滚轮/拖动会产生大量行区间请求 |
| 一次 RPC 返回 | **≤ 64 KiB**（4096 行） | 单个 IPC 消息别超过 Tauri 的舒适区 |
| 超界 | `offset >= size` → 短读/空行 | 实测 `xp` 对非 RAM 地址返回 `Cannot access memory` 而不是崩，**IDE 也要同款宽容** |

**为什么不用 mmap 至少也要保留 `MADV_RANDOM` 这条路**：如果确实要 mmap
（例如需要对镜像做 `memcmp`/搜索），**必须**：
```rust
// memmap2 0.9 提供的 advice（API 名称 [有来源] docs.rs，本轮未编译验证）
mmap.advise(memmap2::Advice::Random)?;          // 关掉 fault-around：实测 83.6 µs → 6.0 µs
// 每 N 次访问后：
mmap.advise(memmap2::Advice::DontNeed)?;        // 归还页：实测 RSS 增量归 0
```
**但即便如此也不比 `pread` 快**（12.0 µs vs 2.1 µs），所以首选 pread。[已实测]

### 2.4 ELF 感知的叠加层（IDE 的核心增值）

hex 视图要能把 **section / segment / 符号** 画成色带，需要三张区间表，
全部来自 `object`（§1.1）而**不需要**读内容：

| 叠加物 | 需要的字段 | 注意 |
|---|---|---|
| 段（Program Header） | `(p_offset, p_filesz, p_memsz, flags)` | `p_memsz > p_filesz` 的尾巴是 **BSS 语义（无文件内容）**，色带要用虚线区分 |
| 节（Section Header） | `(sh_offset, sh_size, sh_type, flags)` | `SHT_NOBITS`（`.bss`/`.stack`）**没有文件内容** —— 实测 `refkernel.elf` 的 `.bss`/`.stack` 都是 NOBITS，`sh_offset` 相同（0x3008）只是占位 [已实测 `readelf -SW`] |
| 符号 | `(st_value, st_size, st_info)` | `st_value` 是 **VMA**，要显示在文件偏移轴上必须经 `segment.virtual_range → file_range` 换算 |

**实测 `refkernel.elf` 的换算依据**（`.researchD/out/refkernel-hdr-seg.txt`）：
```
LOAD  0x001000 0x0000000000100000 0x0000000000100000 0x001368 0x001368 R E 0x1000
LOAD  0x003000 0x0000000000102000 0x0000000000102000 0x000008 0x016010 RW  0x1000
```
→ 第一个 LOAD：**vaddr - 0x100000 + 0x1000 = file offset**（vaddr==paddr，偏移 0xFFFFF 对齐差）。
**这个关系每个段都不一样，必须逐段算，不能全局用一个 base。**[已实测]

### 2.5 前端渲染裁剪（具体做法）

- 固定行高（如 22 px）、固定 16 B/行 → 行号 = `offset >> 4`，列 = `offset & 0xF`。
- 可视行数 = `viewportHeight / rowHeight`，渲染 `[firstRow - overscan, lastRow + overscan]`，
  `overscan ≈ 20`。
- 每行是一个 `<div>`，内部 **不** 为每个字节建 DOM 节点，改用
  `<span>` + `textContent` 一次性写入整行字符串（ASCII 列另用一个 span）。
- 选择/高亮用「区间 → CSS class」映射，不要为每个字节加事件监听（用行级事件 + 坐标换算）。
- 编辑模式：只对"已加载窗口"允许就地改字节，改动进 dirty map，
  保存时按窗口粒度 `pwrite`；**不要**为了编辑把整文件 mmap 成 `MmapMut`（同 §1.5 的 SIGBUS 风险）。

### 2.6 后端接口草案（可直接落地）

```rust
/// 只读、有界、随机访问的镜像句柄。
/// 只依赖 std::fs::File + os::unix::fs::FileExt::read_exact_at（pread）
struct ImageReader {
    file: File,
    len: u64,
    cache: lru::LruCache<u64 /*window index, 4 KiB 对齐*/, Box<[u8; 4096]>>,
}
impl ImageReader {
    fn read_window(&mut self, idx: u64) -> Result<&[u8; 4096]> { /* pread, LRU 256 */ }
    fn read_range(&mut self, off: u64, len: usize) -> Vec<u8> { /* 跨窗口拼接, 越界短读 */ }
}
```
**关键点**：`read_range` 越界返回短读而不是错误 —— 镜像可能正在被 QEMU 增长/截断，
"读到少了"是正常状态，UI 显示 `??` 即可。[推测]

---

## 3. 页表可视化：从 guest 内存按 CR3 逐级解析 x86_64

### 3.1 结论

**自己走页表，不要解析 QEMU 的 `info mem` / `info tlb` 文本**（格式脆、信息不全、
LA57 下有坑，见 §5）。数据源用 QEMU 的物理内存读（`xp` 或 gdbstub）。

### 3.2 CR3 与层级

| 模式 | 层级 | CR3 指向 | 每级索引位 | 索引中提取的物理地址位 |
|---|---|---|---|---|
| 4 级（LA48） | PML4 → PDPT → PD → PT | PML4 | 47:39, 38:30, 29:21, 20:12 | 51:12 |
| 5 级（LA57, `CR4.LA57=1`） | **PML5** → PML4 → PDPT → PD → PT | PML5 | 56:48, 47:39, 38:30, 29:21, 20:12 | 51:12（PML5 表物理地址同样 51:12） |

**CR3 的两个坑**：

1. **低 12 位不是数据**：`CR4.PCIDE=1` 时 `CR3[11:0]` 是 PCID（进程上下文 ID）。
   **必须 `cr3 & 0x000F_FFFF_FFFF_F000`**（即 `& !0xFFF`）再当表地址。
   本环境实测的 CR3 都是 PCID=0（`CR3=0000000000001000`、
   `CR3=0000000000104000`），**不能因此就假设 PCID 永远是 0**。[已实测 + 推测]
2. **`CR0.PG=1` 之前不要走表**。B 报告已实测：分页关闭时 `info mem` 打印 `PG disabled`。
   IDE 必须先检查 CR0.PG（以及长模式下 `EFER.LMA`），否则会把垃圾当页表。
   [已实测（B 报告 §4.2）+ 本源码 §5.2 实测]
3. **LMA=0 时是 32 位 PAE / 非 PAE 页表**（结构完全不同，每级 4 字节、1024 项）。
   IDE 若只支持 x86_64 长模式，应把"LMA=0"显式报成"当前不是 64 位模式"，而不是硬走 4 级。

### 3.3 必须解析的位域（4 KiB 叶 PTE / PML4E / PDPTE / PDE 通用）

| 位 | 名称 | 必须解析的原因 | 实测证据 |
|---|---|---|---|
| 0 | **P** present | 遍历的终止/继续条件；0 时该级指向的整棵子树无效 | 空洞页 `0x1FE000` 在 `info tlb` 中不出现 [已实测] |
| 1 | **R/W** | 写权限；页表视图要显示"只读"（内核 `.rodata`） | `PT[i] \|= 0x2` → `info tlb` 末位 `W` [已实测] |
| 2 | **U/S** | 用户可访问；**注意语义是"每一级都必须为 1"才有效** | 见 §3.8，实测 `info mem` 与 `info tlb` 因此不一致 [已实测] |
| 3 | **PWT** | 写穿；页表视图的"内存类型"标注 | PD[2] 置 0x8 → `X-P--CT-W` 的 `T` [已实测] |
| 4 | **PCD** | 禁缓存；MMIO 映射的关键标注 | PD[2] 置 0x10 → 同一个 `C` [已实测] |
| 5 | **A** accessed | **由 CPU 写回**，展示"这页被访问过" | `xp /4gx 0x1000` = `0x2023`，写入时是 `0x2003` —— **A 位是硬件加的** [已实测] |
| 6 | **D** dirty | **由 CPU 写回**（仅叶项有意义） | 栈所在 2 MiB 页 `0x2000e7`：0x40 位已置 [已实测] |
| 7 | **PS** page size | **1 GiB(PDPTE) / 2 MiB(PDE) 的开关**；也是 `info tlb` 里那个 `P` 字母的真实含义 | PDPT[1] `0x40000183` → `-GP-----W` [已实测] |
| 8 | **G** global | 有 G 位的页在 CR3 切换时不被刷 TLB | PDPT[1] 置 0x100 → `-GP-----W` 的 `G` [已实测] |
| 9:11 | AVL | **软件可用**：Linux 用它存 `_PAGE_SOFT_DIRTY`/`_PAGE_DEVMAP` 等；**IDE 不该把它们当架构位显示** | [有来源] Intel SDM Vol.3 §4.5 |
| 12:51 | **物理地址** | 表地址或页地址。**叶项要按页大小取位**：4 KiB → 51:12；2 MiB → 51:21；1 GiB → 51:30 | `PD[1]=0x2000e7` → paddr 0x200000 [已实测] |
| 52:58 | 忽略/保留 | 必须为 0，否则 **#PF 且 error code 的 RSVD 位置 1** | [有来源] Intel SDM |
| 59:62 | **Protection Key** | `CR4.PKE=1` 时有效（PKRU 索引）；页表视图可显示 | [有来源] Intel SDM |
| 62 | （同 PK 区）| 注意 **NX 是 63** | — |
| 63 | **XD/NX** | 需要 `EFER.NXE=1` 才生效 | PT[1] 高 dword 置 0x80000000 → `X-------W` [已实测] |

**非叶项的特殊性**：PML4E/PML5E **必须** `PS=0`；PDPTE 允许 `PS=1`（1 GiB）；
PDE 允许 `PS=1`（2 MiB）；PTE **必须** `PS=0`。`D` 位在非叶项无意义（部分 CPU 会保留，
不能当 dirty 显示）。[有来源] Intel SDM Vol.3 §4.5。[推测：部分实现会保留该位]

### 3.4 遍历伪代码（含大页与防护）

```rust
const PTE_ADDR: u64 = 0x000F_FFFF_FFFF_F000; // 位 51:12，顺带清掉 PCID

fn walk(mem: &mut GuestMem, cr3: u64, va: u64, la57: bool)
    -> Result<Vec<Level>, WalkErr>
{
    if !cr0_pg { return Err(WalkErr::PagingDisabled); }   // B 报告实测的坑
    let mut levels = Vec::new();
    let mut idx = Vec::new();
    if la57 { idx.push(((va >> 48) & 0x1FF) as usize); }  // PML5: 56:48
    idx.push(((va >> 39) & 0x1FF) as usize);              // PML4: 47:39
    idx.push(((va >> 30) & 0x1FF) as usize);              // PDPT: 38:30
    idx.push(((va >> 21) & 0x1FF) as usize);              // PD  : 29:21
    idx.push(((va >> 12) & 0x1FF) as usize);              // PT  : 20:12

    let mut table = cr3 & PTE_ADDR;                        // ← 必须清掉 PCID 低 12 位
    for (depth, i) in idx.iter().enumerate() {
        // 1. 非规范地址先拒绝（LA48: bit63..47 全等；LA57: bit63..57 全等）
        if !is_canonical(va, la57) { return Err(WalkErr::NonCanonical); }
        // 2. 表的物理地址必须落在 RAM 内（否则 xp 会返回 "Cannot access memory"）
        if !mem.is_ram(table) { return Err(WalkErr::TableNotInRam(table)); }
        let e = mem.read_u64(table + (*i as u64) * 8)?;    // 8 字节！不是 4
        let ps = e & (1 << 7) != 0;
        levels.push(Level { depth, index: *i, raw: e, ..decode(e) });
        if e & 1 == 0 { return Err(WalkErr::NotPresent { depth, index: *i }); }
        if depth >= 2 && ps {                               // PDPTE 1GiB / PDE 2MiB
            let pa = e & if depth == 2 { 0x000F_FFFF_C000_0000 }  // 51:30
                           else        { 0x000F_FFFF_FFE0_0000 }; // 51:21
            return Ok(levels_leafful(levels, pa, size(depth)));
        }
        table = e & PTE_ADDR;
        // 3. 环检测：同一物理表被再次引用 => 直接报环（自映射页表/last-level 复用）
        if seen.contains(&table) { return Err(WalkErr::Cycle(table)); }
        seen.insert(table);
    }
    let pte = levels.last().unwrap();
    Ok(leafful(levels, pte.raw & PTE_ADDR, 4096))
}
```

**必须有的三种防护**（否则 UI 会挂死或显示垃圾）：

| 防护 | 原因 | 本环境证据 |
|---|---|---|
| **表地址必须在 guest RAM 内** | `xp` 对非 RAM 返回 `Cannot access memory`；页表项可以指向 MMIO 或干脆是垃圾 | 实测 `xp /1gx 0xfff00000 → Cannot access memory`、`0x30000000 → Cannot access memory`（guest 只有 256 M）[已实测] |
| **环检测** | 内核常用"页表自映射"技巧（把 PML4 的一个槽指向 PML4 自己），递归会死循环 | [有来源] OSDev / 自映射惯例 |
| **规范地址检查** | 非规范 VA 上 CPU 直接 #GP，不走页表；UI 若允许输入任意地址必须先拦 | [有来源] Intel SDM |

### 3.5 2 MiB / 1 GiB 大页

**实测：QEMU 自己的 `info tlb` 就是把三种粒度混合打印的**，可以直接当对照基准：

```
$ info tlb
0000000000000000: 0000000000000000 --------W     ← 4 KiB 页（PTE，PS=0）
0000000000001000: 0000000000001000 X-------W     ← 4 KiB 页 + NX
...
00000000001ff000: 00000000001ff000 -------UW     ← 4 KiB 用户页
0000000000200000: 0000000000200000 --PDA--UW     ← 2 MiB 页（PDE，PS=1，D+A+U）
0000000000400000: 0000000000400000 X-P--CT-W     ← 2 MiB 页 + NX + PWT + PCD
0000000040000000: 0000000040000000 -GP-----W     ← 1 GiB 页（PDPTE，PS=1，G）
```
[已实测] `.researchD/out/mon-4lvl.txt`（合成探针内核 ptdemo4.elf），
每一条都对应我在 `ptdemo.asm` 里手写的位。

**处理要点**：

1. 一旦在某级看到 `PS=1`，**立即终止遍历**，剩下各级索引位属于页内偏移：
   - 1 GiB（PDPTE，深度 2）：VA 位 29:0 是页内偏移，物理地址取 `e[51:30]`
   - 2 MiB（PDE，深度 3）：VA 位 20:0 是页内偏移，物理地址取 `e[51:21]`
2. **UI 必须把"大页覆盖的 512 个 4 KiB 页"折叠成一行**，否则 1 GiB 身份映射会画出
   26 万个节点。实测 `refkernel` 就是这样：`info mem` 只有 **1 行**
   `0000000000000000-0000000040000000 0000000040000000 -rw`，
   而 `info tlb` 有 **512 行** —— 同一个映射，两种粒度。[已实测] §5.2
3. **大页的 PTE 值不是"基址 + 索引×4 KiB"**：2 MiB 页的地址位是 51:21，
   所以 `0x2000e7 & 0x000F_FFFF_FFE0_0000 = 0x200000`。[已实测]
4. 混用粒度要正确**排序 + 合并**：按 VA 排序，相邻且物理连续、属性相同的区间合并成一条，
   这与 QEMU `mem_info_la48` 的 `mem_print` 逻辑一致。[有来源] 见 §5.2 的源码引用

### 3.6 5 级页表（LA57）

**结论：数据结构上支持 LA57；判定用 `CR4[12]`；每级索引整体上移 9 位；别指望 QEMU 的 `info mem`。**

- 触发条件：`CR4.LA57 = 1`（本环境实测 `CR4=0000000000001020` → 0x1000 位已置）
  且 `EFER.LMA=1`。此时 CR3 指向 **PML5**，多一级索引 `VA[56:48]`，
  规范地址有效位变成 56 位。[已实测]
- **PML5 表本身也在 51:12 的物理地址范围内**，所以页表项格式不变，只是多一层。
- 实测 5 级页表可正常工作：`ptdemo5.elf`（PML5@0x1000 → PML4@0x2000 → …）
  串口打印 `[ptdemo] long mode + paging + GDT/TSS/IDT ready`，
  `CR4=0x1020`、`EFER=0xd00`。[已实测]
- **QEMU 侧的差异（重要）**：
  - `info tlb` 在 LA57 下**工作正常**，输出与 LA48 完全同构（实测 514 行，
    同样能看到 `X-------W`/`--PDA--UW`/`-GP-----W`）。[已实测]
  - `info mem` 在 LA57 下**返回空**（QMP `human-monitor-command` 的 `return` 是空字符串）。
    这是 **QEMU 上游 bug**：`mem_info_la57()` 里 PDPT/PD 两层的 present 判断写反了
    （写成 `if (pdpe & PG_PRESENT_MASK) { prot = 0; ... continue; }`，
    而正确形式是取反后 `continue`）。对照同文件的 `mem_info_la48()` 一眼可见。
    [已实测 + 有来源]
    <https://raw.githubusercontent.com/qemu/qemu/v7.2.22/target/i386/monitor.c>（本环境抓取的副本
    `/tmp/i386mon7222.c`，md5 `71dd6646215e176c668734e6b2e76810`）
  → **这条直接决定了 §5 的结论：IDE 不能把 `info mem` 当页表数据源。**

### 3.7 实测：真实内核的页表长什么样

**A) `refkernel.elf`（真实 fixture，`CR3=0x0000000000104000`）**

符号表给出了页表位置（`readelf -sW`）：`pml4@0x104000`、`pdpt@0x105000`、`pd@0x106000`、
`idt@0x107000`、`idtr@0x108000`。用 `xp` 读物理内存得到：

```
$ xp /8gx 0x104000                 # PML4
0000000000104000: 0x0000000000105023 0x0000000000000000     ← [0] → PDPT@0x105000, P|RW|A
$ xp /8gx 0x105000                 # PDPT
0000000000105000: 0x0000000000106023 0x0000000000000000     ← [0] → PD@0x106000
$ xp /8gx 0x106000                 # PD  （2 MiB 大页，PS=1）
0000000000106000: 0x00000000000000e3 0x0000000000200083     ← [0]=0x0 页(D+A) [1]=0x200000 页
0000000000106010: 0x0000000000400083 0x0000000000600083     ← [2] [3] ...
0000000000106020: 0x0000000000800083 0x0000000000a00083
0000000000106030: 0x0000000000c00083 0x0000000000e00083
```
[已实测] `.researchD/out/mon-refkernel2.txt`

**逐项解码（可作为 IDE 的单元测试基准）**：

| 原始值 | 解码 |
|---|---|
| `0x105023` | P=1, R/W=1, **A=1**（硬件加）→ PDPT@0x105000 |
| `0x106023` | 同上 → PD@0x106000 |
| `0x0000e3` | P=1, R/W=1, **A=1, D=1**, **PS=1** → 2 MiB 页 @0x000000（内核代码+数据，被写过） |
| `0x200083` | P=1, R/W=1, PS=1 → 2 MiB 页 @0x200000（未访问未写） |

**为什么 PD[0] 有 D 位而 PD[1..] 没有**：内核只用了低 2 MiB（256 M RAM 中其余未触）。
**这正好证明 A/D 位是 CPU 在运行时写回的 —— IDE 每次刷新页表视图都会看到它们变化，
不能缓存成"静态属性"。**[已实测]

**`info mem` 对应的输出（同一时刻）**：
```
0000000000000000-0000000040000000 0000000040000000 -rw
```
即 1 GiB 身份映射被合并成一行（因为连续 512 个 2 MiB 页属性一致）。
**这与 `info tlb` 的 512 行是同一份数据的不同粒度呈现。**[已实测]

**B) 合成探针 `ptdemo4.elf`（4 级 + 三种粒度 + 空洞 + NX/PWT/PCD/GLOBAL/US）**

```
$ info mem
0000000000000000-00000000001fe000 00000000001fe000 -rw    ← PT 覆盖区，到空洞前
00000000001ff000-0000000000600000 0000000000401000 -rw    ← 空洞后的 4KiB + 两个 2MiB 页
0000000040000000-0000000080000000 0000000040000000 -rw    ← 1 GiB 页

$ xp /4gx 0x1000    (PML4)
0000000000001000: 0x0000000000002023 0x0000000000000000
$ xp /4gx 0x2000    (PDPT)
0000000000002000: 0x0000000000003023 0x0000000040000183     ← [1] 是 1 GiB 页 (P|RW|G|PS)
$ xp /8gx 0x3000    (PD)
0000000000003000: 0x0000000000004023 0x00000000002000e7     ← [1] 2MiB U+A+D+PS
0000000000003010: 0x800000000040009b 0x0000000000000000     ← [2] 2MiB PWT+PCD+PS+NX
$ xp /3gx 0x4008    (PT[1], PT[2] …)
0000000000004008: 0x8000000000001003 0x0000000000002003     ← PT[1] 带 NX（bit63=1）
0000000000004018: 0x0000000000003003
```
[已实测] `.researchD/out/mon-4lvl.txt`。
**故意留的 not-present 空洞在 `0x1FE000`**，`info tlb` 里它整行消失、
`info mem` 把它切成两段 —— 这就是 IDE 必须正确处理的"映射空洞"。

### 3.8 一个容易被误读的坑：**`info mem` 显示的是"有效权限"，`info tlb` 显示的是"叶子权限"**

`ptdemo4.elf` 里我把 `PT[256..511]`（0x100000–0x1FFFFF，含内核代码）和
`PD[1]` 的 2 MiB 页都置了 `U/S=1`，但 `info mem` 仍显示 `-rw` 而不是 `urw`，
`info tlb` 却显示 `U`。

**原因（对照 QEMU 源码）**：`mem_info_la48()` 对有效权限做**逐级 AND**：
```c
prot  = pte  & (PG_USER_MASK | PG_RW_MASK | PG_PRESENT_MASK);
prot &= pml4e & pdpe & pde;     /* ← 逐级 AND，这正是 x86 的语义 */
```
而 `print_pte()`（`info tlb` 用）只打印**该级表项自己的位**。

→ **x86 的有效 U/S 是"路径上每一级都必须为 1"**。我的 PML4E/PDPTE/PDE 都没置 US，
所以那些"用户页"实际上用户态根本访问不了。
**IDE 的页表视图必须同时给出"叶子属性"和"有效属性"两列，否则会误导用户。**
[已实测 + 有来源] 源码：
<https://raw.githubusercontent.com/qemu/qemu/v7.2.22/target/i386/monitor.c>

---

## 4. GDT / IDT 可视化

### 4.1 结论：**QEMU 7.2 没有 `info gdt` / `info idt` / `info ldt`，必须自己读内存解析**

实测（HMP）：
```
$ info gdt
unknown command: 'info gdt'
$ info idt
unknown command: 'info idt'
```
并且 QEMU 的 `help info` 完整列表里**确实没有**这三条
（有 `mem` / `tlb` / `registers` / `lapic` / `mtree` / `pic` / `irq` 等）。[已实测]
`.researchD/out/mon-4lvl.txt`

### 4.2 怎么拿到 GDTR / IDTR

**唯一可靠来源是 `info registers` 输出的这两行**（实测原文）：
```
GDT=     0000000000100300 00000037
IDT=     0000000000008000 00000fff
```
格式：`GDT=<base(16 位十六进制)><空格><limit>`、`IDT=<base><空格><limit>`。
[已实测] `.researchD/out/mon-4lvl.txt`、`.researchD/out/mon-refkernel.txt`。

**注意**：
- 这两行在 `info registers -a`（多 CPU）里每个 CPU 各一份 —— **GDT/IDT 是 per-CPU 的**
  （SMP 内核通常共享，但 `ltr`/`lgdt` 是每核执行）。IDE 必须让用户选 CPU。[推测]
- **limit 是字节数减 1**：`0x37 = 55` → 56 字节 = **7 个 8 字节项**；
  `0xfff = 4095` → 4096 字节 = **256 个 16 字节门**。IDE 要按 limit 算项数，
  不能硬编码 256/8192。[已实测]

### 4.3 GDT 描述符位域（长模式）

**实测原始 GDT 内存（`refkernel`，`GDT=0x100c10`）**：
```
$ xp /4gx 0x100c10
0000000000100c10: 0x0000000000000000 0x00af9a000000ffff
0000000000100c20: 0x00cf93000000ffff 0x00cf9a000000ffff
```
对照符号表 `GDT_NULL=0 / GDT_CODE64=8 / GDT_DATA64=0x10 / GDT_CODE32=0x18`，
**完全对上**。[已实测] `.researchD/out/mon-refkernel2.txt`

| 描述符 | 原始值 | 关键位 | 含义 |
|---|---|---|---|
| 0 (null) | `0x0` | — | null 描述符（**必须为 0**，`lgdt` 不检查，但 CPU 会检查加载的选择子） |
| 1 (0x08) | `0x00af9a000000ffff` | flags nibble `0xa` = `1010` → **G=1, D/B=0, L=1**, AVL=0；access `0x9a` = P=1,DPL=0,S=1,type=1010 | **64 位内核代码段** |
| 2 (0x10) | `0x00cf93000000ffff` | flags `0xc` = `1100` → G=1, **D/B=1, L=0**；access `0x92` = P=1,DPL=0,S=1,type=0010 | 内核数据段 |
| 3 (0x18) | `0x00cf9a000000ffff` | flags `0xc`、access `0x9a` | **32 位**代码段（兼容模式用） |

**位域表（8 字节描述符，字节序小端）**：

| 位 | 名称 | 解析要点 |
|---|---|---|
| 15:0 | Limit 15:0 | 与 51:48 拼成 20 位 limit |
| 31:16 | Base 15:0 | 与 39:32、63:56 拼成 32 位 base |
| 39:32 | Base 23:16 | — |
| **40:47** | **Access byte** | bit40 **A**(accessed，CPU 写回)、bit41 **R/W**、bit42 **DC/EW/C**、bit43 **E**（0=数据段，1=代码段）、bit44 **S**（0=系统段，1=代码/数据段）、**bit45:46 DPL**、**bit47 P** |
| **48:51** | **Limit 19:16** | — |
| **52** | **AVL** | 软件可用 |
| **53** | **L** | **长模式代码段标志**；L=1 ⇒ D/B 必须为 0 |
| **54** | **D/B** | 代码段：默认操作数宽度（1=32 位）；数据段：1=32 位栈指针 |
| **55** | **G** | 粒度：1 ⇒ limit 单位是 4 KiB |
| 56:63 | Base 31:24 | — |

[有来源] Intel SDM Vol.3 §3.4.5 / §5.2.1；上表中的 `L`/`D-B`/`G` 解释已用实测的
4 个描述符逐条验证。

### 4.4 长模式下系统段描述符与 TSS 的特殊性（**最容易做错的地方**）

1. **64 位 TSS 描述符占 16 字节（两个连续槽）**。
   低 8 字节：limit 15:0 + base 15:0 + base 23:16 + access + flags/limit19:16 + base 31:24；
   高 8 字节：**base 63:32**。所以它占用 GDT 的两个"选择子槽"，
   **选择子是低 8 字节槽的偏移**（本例 `0x18`），高 8 字节那个偏移（`0x20`）不可再分配。
   实测 `lgdt` limit `0x37`（7 项）里第 4、5 项就是同一个 TSS 描述符的两半。[已实测]
2. **type 只有 9(available 64-bit TSS) 与 11(busy) 合法**。
   实测：`info registers` 打印 `TR =0018 0000000000005000 00000067 00008900 DPL=0 TSS64-avl`。
3. **`ltr` 会把 GDT 里的 busy 位置 1**（真实 x86 行为）。**实测证据（原始内存）**：
   ```
   $ xp /8gx 0x100300        # ptdemo4 的 GDT
   0000000000100318: 0x00008b0050000067    ← access byte = 0x8b（busy），
   ```
   而我写入 GDT 的常量是 `0x0000890050000067`（access `0x89` = available）。
   **`0x89 → 0x8b` 是 `ltr` 写回的。**
   → **IDE 若允许"编辑 GDT"，必须知道某些位是 CPU 的，不是用户的。**
   [已实测] `.researchD/out/mon-4lvl.txt`、`.researchD/out/mon-5lvl1.txt`
4. **LDT 描述符也是 16 字节**（长模式下 LDT 可有 64 位基址），type 2/3。
   实测 `info registers` 打印 `LDT=0000 0000000000000000 0000ffff 00008200 DPL=0 LDT`
   —— limit `0xffff` 是"未加载 LDT"的复位值，IDE 要能识别这种"空 LDT"而不是画出 65536 项。
   [已实测]
5. **64 位 TSS 的 limit 必须 ≥ 0x67**（104 字节结构），且 G 位应为 0。
   实测 `TR ... 00000067` → limit 0x67 = 103 → 104 字节。✓ [已实测]
6. **长模式下 CS/DS 等的 base/limit 被忽略**（除 FS/GS），但 `info registers` 仍会打印
   它们。**不要把这些值当有效信息展示给用户**（实测打印 `0000000000000000 ffffffff`，
   是复位默认值，与 GDT 里的值无关）。[已实测 + 有来源] Intel SDM
7. **call gate（type 12）在 64 位下仍是 16 字节**，且 IST 字段有效。
8. **32 位兼容段（L=0, D=1）**在长模式下仍可用（实测 refkernel 的 `0x18` 是 CODE32），
   所以 GDT 视图要按 `L`/`D-B` 组合推断"这是 64 位还是 32 位段"。[已实测]

### 4.5 64 位 TSS 结构（实测布局）

**实测原始内容（`ptdemo4.elf`，TSS@0x5000，我写入了 RSP0=0x300000、IST1=0x7000、IOMAP=104）**：
```
$ xp /16gx 0x5000
0000000000005000: 0x0030000000000000 0x0000000000000000    ← +0x00 reserved4 | +0x04 RSP0=0x300000
0000000000005010: 0x0000000000000000 0x0000000000000000    ← +0x0C RSP1 | +0x14 RSP2
0000000000005020: 0x0000700000000000 0x0000000000000000    ← +0x1C reserved | +0x24 IST1=0x7000
...
0000000000005060: 0x0068000000000000 0x0000000000000000    ← +0x64 reserved2 | +0x66 IOMAP=0x68
```
[已实测] `.researchD/out/mon-4lvl.txt`

| 偏移 | 字段 | 大小 | 说明 |
|---|---|---|---|
| 0x00 | reserved | 4 | 必须 0 |
| **0x04** | **RSP0** | 8 | 从 ring3 进 ring0 时切到的栈（**IDE 最该显示的字段**） |
| 0x0C | RSP1 | 8 | 进 ring1 用（Linux 不用） |
| 0x14 | RSP2 | 8 | 进 ring2 用 |
| 0x1C | reserved | 8 | — |
| **0x24** | **IST1** | 8 | 中断栈表 1（Linux 用它做 double-fault/NMI 栈） |
| 0x2C..0x54 | IST2..IST7 | 8×6 | — |
| 0x5C | reserved | 10 | — |
| **0x66** | **I/O Map Base** | 2 | 相对 TSS 基址的偏移；**≥ limit ⇒ 无 I/O 位图**（本例 0x68 = 104 = limit+1 ⇒ 无位图） |

[有来源] Intel SDM Vol.3 §7.7 / §8.7。

### 4.6 IDT 门结构（64 位，16 字节）

**实测原始（`refkernel`，`IDT=0x107000`）**：
```
$ xp /4gx 0x107000
0000000000107000: 0x00108e000008011c 0x0000000000000000
$ xp /2gx 0x107000
0000000000107000: 0x00108e000008011c 0x0000000000000000
$ xp /4gx 0x1070e0
00000000001070e0: 0x00108e000008017b 0x0000000000000000    ← 向量 0x0E (#PF)
$ xp /2gx 0x107800
0000000000107800: 0x00108e0000080152 0x0000000000000000    ← 向量 0x80
```
**解码 `0x00108e000008011c`**（字节序小端 → 字节序列
`1c 01 08 00 00 8e 10 00`）：

| 偏移 | 字段 | 本例值 | 含义 |
|---|---|---|---|
| +0 | offset 15:0 | `0x011c` | 处理函数地址低 16 位 |
| +2 | selector 15:0 | `0x0008` | 内核代码段选择子 |
| +4 | **IST(3) + 保留(5)** | `0x00` | IST=0 ⇒ 不切栈 |
| +5 | **type_attr** | `0x8e` = `1000_1110` | **P=1, DPL=00, 0, gate type 1110 = 中断门** |
| +6 | offset 31:16 | `0x0010` | |
| +8 | offset 63:32 | `0x00000000` | ⇒ 处理函数 = `0x000000000010011c` = `isr_stub_0` |
| +12 | reserved | 0 | 必须 0 |

[已实测]；与 `readelf -sW` 的 `isr_stub_17@0x10018b` 等符号交叉一致。
[有来源] Intel SDM Vol.3 §6.14。

**实测 `ptdemo4.elf` 的完整门表（我手工设置的 6 个门）**：
```
0000000000008000: 0x00108e00000802c7 0x0000000000000000   ← 向量 0x00 #DE  中断门 DPL0
0000000000008030: 0x0010ef00000802c7 0x0000000000000000   ← 向量 0x03 #BP  TRAP门  DPL3 (0xef)
00000000000080d0: 0x00108e00000802c7 ...                   ← 向量 0x0D #GP  中断门 DPL0
00000000000080e0: 0x00108e00000802c7 ...                   ← 向量 0x0E #PF  中断门 DPL0, IST=1
0000000000008200: 0x00108e00000802c7 ...                   ← 向量 0x20 定时器
0000000000008800: 0x0010ee00000802c7 ...                   ← 向量 0x80 syscall 中断门 DPL3 (0xee)
```
注意 `0x8e`（DPL0 中断门）vs `0xef`（DPL3 trap 门）vs `0xee`（DPL3 中断门）
的差别全在 `type_attr` 字节：**bit7=P，bit6:5=DPL，bit3:0=type（0xE 中断门 / 0xF trap 门）**。

**IDT 视图必须处理的边界情况**：

| 情况 | 表现 | UI 处理 |
|---|---|---|
| 门 not present | 整个 16 字节为 0 | 显示"未安装"，**不要**当成 handler=0 的有效门 |
| offset 63:32 非 0 | 处理器在高地址（高半核常见） | 必须拼 64 位，不能只取低 32 位 |
| IST ≠ 0 | 该门会用 IST 栈 | 显示 IST 索引，并联动 TSS 视图显示 IST 栈地址（§4.5） |
| 未对齐读 | 实测 `xp /1gx 0x107e0e` = `0x8e00000801520000`（跨门读） | 解析器必须按 `base + i*16` 对齐读，**不能**顺序流式读 |
| DPL=3 的门 | 用户态可 `int n` 触发 | 安全审查要标红（内核攻击面） |
| 选择子 ≠ 0x08 | 某些内核用 call gate / 不同 CS | 要回 GDT 查该选择子并显示其 DPL/L 位 |

---

## 5. QEMU monitor 接口：格式、解析难度、QMP 是否更稳

> 本节与已完成的 `docs/research/B-qemu-panic-symbolization.md` §4 互补。
> B 报告给出了 QMP 握手与 `info registers`/`info mem`/`info tlb` 的样本；
> 本节把 **格式逐字段拆开**、补齐 **`xp`/`x`/不存在的命令**、并**更正 B 报告里
> 关于 `info tlb` 标志位的一处推断**。

### 5.1 三条独立通道（本报告全部实测）

```bash
qemu-system-x86_64 ... \
  -serial file:serial.txt \
  -monitor unix:/tmp/mon.sock,server=on,wait=off \   # HMP：人类可读
  -qmp     unix:/tmp/qmp.sock,server=on,wait=off     # QMP：JSON
```

**实测连接注意**：QEMU 退出时会 unlink socket 文件，所以"看到 socket 不存在"
可能只是 QEMU 已经退出（本报告第一次探测就踩了这个：探针内核 triple fault →
QEMU 带 `-no-reboot` 退出码 0 → 连接失败，看起来像 socket 配置错）。[已实测]
**另外**：直接从 HMP socket 读大输出（如 512 行 `info tlb`）容易读不全
（readline 回显 + 分片），而 **QMP `human-monitor-command` 一次返回完整字符串**。
→ **程序化取数一律走 QMP。**[已实测]

### 5.2 `info mem` / `info tlb` 的确切格式与解析难度

**`info mem`** —— 格式（源码 `mem_print()`）：
```
<start>-<end> <size> <u|->r<w|->
```
实测：
```
0000000000000000-00000000001fe000 00000000001fe000 -rw
00000000001ff000-0000000000600000 0000000000401000 -rw
0000000040000000-0000000080000000 0000000040000000 -rw
```
- 已经**按有效权限合并**成区间（同一映射 512 个 2 MiB 页 → 1 行）。
- 只输出 `u`/`r`/`w` 三个权限，**没有 NX、没有 PWT/PCD、没有 A/D、没有 present**。
- `PG disabled` 当分页关闭。
**解析难度：低。** 正则 `^([0-9a-f]+)-([0-9a-f]+) ([0-9a-f]+) ([urw-]{3})$` 即可。
**但信息量太少 + LA57 下返回空（§3.6），不适合做页表数据源。**[已实测]

**`info tlb`** —— 格式（源码 `print_pte()`）：
```
<vaddr>: <paddr> <9 chars>
```
**9 个字符的确切含义（QEMU 源码逐字对应）**：

| 位置 | 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 |
|---|---|---|---|---|---|---|---|---|---|
| 字母 | `X` | `G` | **`P`** | `D` | `A` | `C` | `T` | `U` | `W` |
| 含义 | **NX** | **GLOBAL** | **PS（大页!）** | DIRTY | ACCESSED | PCD | PWT | USER | RW |
| 来源 | `PG_NX_MASK` | `PG_GLOBAL_MASK` | `PG_PSE_MASK` | `PG_DIRTY_MASK` | `PG_ACCESSED_MASK` | `PG_PCD_MASK` | `PG_PWT_MASK` | `PG_USER_MASK` | `PG_RW_MASK` |

> **⚠️ 更正 B 报告 §4.2 的推断**：B 报告写"标志位字母语义 P/A/W = present/accessed/writable
> 是**根据两行差异推断**"。本报告用**故意构造的页表**逐位验证后确认：
> **第 3 个字母 `P` 是 `PS`（大页标志），不是 `present`**。
> 条目只有在 present 时才会被打印（源码里 `if (pte & PG_PRESENT_MASK)` 先过滤），
> 所以输出里根本没有 present 字母。
> [有来源] <https://raw.githubusercontent.com/qemu/qemu/v7.2.22/target/i386/monitor.c>
> `print_pte()`；本环境抓下的副本 `/tmp/i386mon7222.c`。

**逐位实测验证（6/6 全部命中）**：

| 我构造的条目 | 原始值 | 预期位 | `info tlb` 实测输出 | 命中 |
|---|---|---|---|---|
| PT[n] 4 KiB RW | `0x…0003` | 仅 RW（P 是过滤条件） | `--------W` | ✅ |
| PT[1] 4 KiB RW + NX | `0x8000_0000_0000_1003` | +NX | `X-------W` | ✅ |
| PT[256] @0x100000 RW+US | `0x100027` = `0x100000｜P(1)｜RW(2)｜US(4)｜A(0x20)` | +U +A | `----A--UW` | ✅ |
| PD[1] 2 MiB RW+US+PS+A+D | `0x2000e7` | +PS +D +A | `--PDA--UW` | ✅ |
| PD[2] 2 MiB RW+PS+PWT+PCD+NX | `0x8000_0000_0040_009b` | +PS +PWT +PCD | `X-P--CT-W` | ✅ |
| PDPTE[1] 1 GiB RW+G+PS | `0x40000183` | +PS +G | `-GP-----W` | ✅ |

（`PD[2]` 的原始值与 `XP /8gx 0x3000` 的实测输出一致；`PT[256]` 的 A 位是 CPU 访问代码页后
回写的，与 `PD[1]` 的 D 位同理。）

**解析难度：低（格式极稳定），但有两个隐藏成本**：

1. **输出量爆炸**：QEMU 按**最细粒度**逐条打印。实测 `refkernel` 只映射 1 GiB，
   `info tlb` 就是 **512 行**；如果在 4 GiB 地址空间上映射更多区域，
   行数是 `映射字节数 / 4 KiB`（或 `/ 2 MiB`，取决于该级的粒度）。
   源码里 `for l1..for l2..for l3..for l4` 是全表遍历。
   [已实测] + [有来源] 源码。上游已有补丁讨论"`info tlb` 输出对小目标可能极大"
   —— [有来源] <https://patchew.org/QEMU/20260619125602.17077-1-AlanoSong@163.com/>
2. **`vaddr: paddr` 的 paddr 是"该级解析出的物理基址"**，对 4 KiB 页是页地址、
   对 2 MiB 页是 2 MiB 对齐地址 —— **必须结合粒度解读**，不能一律加页内偏移。[已实测]

**判定粒度的方法**：`info tlb` 本身**不告诉你**每条是 4 KiB 还是 2 MiB。
只能靠相邻 vaddr 的步长反推（步长 0x1000 → 4 KiB；0x200000 → 2 MiB；
0x40000000 → 1 GiB）。**这是它比"自己走页表"差的根本原因。**[已实测 + 推测]

### 5.3 `info registers` 格式

实测（截取，`refkernel` + `ptdemo4`）：
```
CPU#0
RAX=000000008000000a RBX=00000000001002c7 ... R15=0000000000000000
RIP=000000000010029d RFL=00000046 [---Z-P-] CPL=0 II=0 A20=1 SMM=0 HLT=1
ES =0010 0000000000000000 ffffffff 00cf9300 DPL=0 DS   [-WA]
CS =0008 0000000000000000 ffffffff 00af9a00 DPL=0 CS64 [-R-]
SS =0010 ... DS =0010 ... FS =0010 ... GS =0010 ...
LDT=0000 0000000000000000 0000ffff 00008200 DPL=0 LDT
TR =0018 0000000000005000 00000067 00008900 DPL=0 TSS64-avl
GDT=     0000000000100300 00000037
IDT=     0000000000008000 00000fff
CR0=80000011 CR2=0000000000000000 CR3=0000000000001000 CR4=00000020
EFER=0000000000000d00
```
**字段说明与解析要点**：

| 行 | 格式 | 要点 |
|---|---|---|
| `RAX..R15` | `NAME=16 位十六进制` | 定宽，好切 |
| `RIP/RSP/RBP` | 同上 | — |
| `RFL` | `EFL [标志串]` | 标志串是**位置敏感**的固定 8 字符框 |
| `CS =SS DPL=0 CS64 [-R-]` | `sel base limit access-byte DPL=<n> <类型> <权限串>` | **类型串最有价值**：实测有 `CS64`（长模式 64 位代码段）、`DS`、`LDT`、`TSS64-avl`、`TSS64-busy`、`CS32`（兼容模式 32 位段） |
| `GDT=` / `IDT=` | `base(16 hex) limit` | **GDT/IDT 地址的唯一来源**（§4.2） |
| `CR0/CR2/CR3/CR4` | `NAME=16 位十六进制` | **CR3 是页表遍历的入口**；CR2 是最近一次 #PF 的故障地址；CR4 的 bit5=PAE、bit12=LA57 |
| `EFER` | 16 位十六进制 | bit8=LME、bit10=LMA、bit11=NXE |

**解析难度：低。** 每行独立正则即可，不需要跨行状态机（除了 `CS =` 行的
"权限串"可选）。**这是我在所有 monitor 输出里最推荐保留的解析对象**
—— 因为 GDT/IDT/CR3/CR4/EFER 只能从这里拿。[已实测]

### 5.4 `xp`（物理内存读）与 `x`（虚拟内存读）

**实测格式**：
```
$ xp /8gx 0x104000
0000000000104000: 0x0000000000105023 0x0000000000000000
0000000000104010: 0x0000000000000000 0x0000000000000000
```
格式：`<16 位十六进制地址>: <值1> <值2> ...`，每行 2 个 `gx`（8 字节）。[已实测]

| 事实 | 实测结果 |
|---|---|
| 长度上限 | `xp /1024gx`（1024 个 qword = 8 KiB）**未被截断**（512 行）—— 没有硬上限 |
| 单位 | `gx`=8 字节、`wx`=4、`xb`=1；`/Ni` 触发**反汇编**（AT&T 语法） |
| 越界/非 RAM | `xp /1gx 0xfff00000` → `00000000fff00000: Cannot access memory`（**文本错误，不是异常**）；`0x30000000` 同样 |
| RAM 内未写区 | `xp /1gx 0x2000000` → `0x0000000000000000`（正常返回 0） |
| 未对齐 | `xp /1gx 0x107e0e` → `0x8e00000801520000`（**允许**，跨门读） |

[已实测] `.researchD/out/mon-xplim.txt`、`.researchD/out/mon-refkernel2.txt`

**给 IDE 的结论**：
- **`xp` 是可行的页表数据源**：一次读 8 KiB 就够一棵 PD（512×8 B）。
- **必须区分两种失败**：① "Cannot access memory"（地址不在 guest 物理空间）；
  ② 返回 0（在 RAM 但没写过）。**页表遍历器必须处理①，否则 UI 会显示一棵由 0 组成的假树。**
- **吞吐不理想**：文本形式（16 进制 + 冒号 + 空格）比二进制膨胀约 5×，
  且每次要等一个 monitor RTT。走完整棵 4 级树（4 次读 × 512 项）
  在 1 GiB 映射上会产生 512 行 × 4 级 ≈ 2000 行的文本 —— **必须有分页/按需展开 UI**。

### 5.5 QMP 是否更稳？—— **结论：是，但要知道它"没有什么"**

| 命令 | QEMU 7.2.22 实测结果 |
|---|---|
| `query-version` | ✅ `{"qemu":{"major":7,"minor":2,"micro":22},"package":"Debian 1:7.2+dfsg-7+deb12u18+b3"}` |
| `query-status` | ✅ `{"status":"running","singlestep":false,"running":true}` |
| `query-cpus-fast` | ✅ 数组，每项含 `thread-id`/`cpu-index`/`target` —— **多 CPU 页表视图要靠它枚举 CPU** |
| `query-memory-size-summary` | ✅ `{"base-memory":268435456,"plugged-memory":0}` —— 用来判断"这个物理地址在不在 RAM"（配合 `info mtree`） |
| **`query-registers`** | ❌ `{"error":{"class":"CommandNotFound","desc":"The command query-registers has not been found"}}` —— **7.2 没有这条**（B 报告标为"未验证"，本报告实测确认不可用） |
| `human-monitor-command` | ✅ 万能兜底：把任意 HMP 文本塞进 `return`。**这是取 `info registers`/`info mem`/`info tlb`/`xp` 的正确姿势** |

[已实测] `.researchD/out/mon-*.txt` 的 QMP 段。

**QMP 相比 HMP socket 的三个实际优势（本报告亲历）**：
1. **一次调用拿到完整输出**，不用处理 readline 回显/ANSI/分片（本报告第一版
   从 HMP socket 读 512 行 `info tlb` 就漏读了，改用 QMP 才拿到完整数据）。[已实测]
2. **错误是结构化的**（`{"error":{"class":...}}`），不用正则匹配 `unknown command:`。
3. **有真正的结构化命令**（`query-cpus-fast` 等），不必解析文本。

**但 QMP 不能替代 HMP**：`info mem`/`info tlb`/`xp` 都**没有**结构化等价物，
只能 `human-monitor-command`。

**给 IDE 的最终建议（可直接落地）**：

```
优先：QMP query-*                          → 结构化，稳
其次：QMP human-monitor-command + 精确正则  → info registers / xp / info tlb
不用：HMP socket（除人工调试）
不用：info mem（信息量太少 + LA57 下返回空）
不用：info gdt / info idt / info ldt（不存在）
```

### 5.6 gdbstub：物理内存的"二进制批量读"通道 [有来源，未实测]

QEMU 的 gdbstub 支持 `maintenance packet qqemu.PhyMemMode:1` 把后续内存读切到**物理地址空间**。
本环境实测 QEMU 二进制里**确实包含字符串 `PhyMemMode`**（`grep -a` 命中 2 处）
[已实测]，但**本轮没有实际连 gdb 验证该 packet 的行为**（§7）。

价值（如果验证通过）：gdb remote 的 `m<addr>,<len>` 返回**二进制/hex 字节流**，
没有 `xp` 的 5× 文本膨胀，且一次可以读几 KB，对"递归走完整棵页表树"是数量级改善。
**建议 P5 实现前先花 30 分钟验证这一条。**[推测]

### 5.7 其他实测的 monitor 行为（可直接用）

| 命令 | 实测结果 | 用途 |
|---|---|---|
| `info mtree -f` | 打印 FlatView：`0000000000100000-000000000fffffff (prio 0, ram): pc.ram @0000000000100000`、`00000000fd000000-00000000fdffffff (prio 1, ram): vga.vram` | **判断物理地址是否在 RAM**，给页表遍历器做前置校验；也给 MMIO 视图做数据源 |
| `info registers -a` | 每个 CPU 一段，`CPU#0` / `CPU#1` 分开 | 多核视图 |
| `info cpus` | `* CPU #0: thread_id=299245`（`*` 标记当前 CPU） | 要知道 `info mem/tlb` 是**针对当前 CPU** 的 |
| `x /Ni <addr>` | 内置 AT&T 反汇编器 | **反汇编引擎的第三方对照**（§1.3） |
| `info lapic [id]` / `info pic` / `info irq` | 存在 | 中断控制器可视化（本报告未深入） |
| `help info` | 完整子命令列表 | **验证命令存在性的唯一可靠方式** |

[已实测] `.researchD/out/mon-4lvl.txt`、`mon-xplim.txt`

---

## 6. 不建议的做法（明确表态）

| # | 不建议 | 为什么（证据） | 替代 |
|---|---|---|---|
| 1 | **shell out 到 `readelf`/`objdump`/`nm`/`addr2line` 做在线解析** | ① 每文件一次进程 + 一次全文遍历，IDE 每次刷新都重跑；② 输出是**给人看的**，跨 binutils 版本会变（GNU 2.40 的 `readelf -l` 表头与 2.3x 不同）；③ 打包分发要把 binutils 带进 Tauri bundle；④ 无法增量/流式 | `object` + `gimli` + `addr2line`（§1）。**工具留给 golden 测试和人工调试** |
| 2 | **自己写 ELF/DWARF 解析器** | DWARF 有 5 种版本 × 70+ 种 DIE form × line program 状态机 × `.debug_aranges`/`.debug_str_offsets`/`.debug_addr` 各种间接表；一个 section header 的 `sh_link`/`sh_info` 语义就够写一周 | `object` + `gimli` |
| 3 | **`fs::read` 整个磁盘镜像 / 裸 `mmap` 整个 GB 镜像随机访问** | 实测 8 GiB 随机访问 RSS 涨 **1.12 GB**（fault-around 每次 64 KiB） | `pread` + LRU(256×4 KiB)，实测 RSS +104 kB（§2） |
| 4 | **用 `memmap2` 映射 QEMU 正在写的镜像** | 文件被截断 → **SIGBUS，直接杀进程**，Rust 无法 catch | `pread` 路线；或先校验 size + 用副本 |
| 5 | **把 `info mem` 当页表数据源** | ① 只有 `u/r/w` 三个权限位；② LA57 下**返回空**（QEMU 上游 bug，§3.6）；③ 不区分粒度 | 自己从 CR3 走页表（§3） |
| 6 | **解析 `info tlb` 当页表数据源** | ① 粒度靠步长反推；② 输出量按 2 MiB/4 KiB 线性膨胀（refkernel 1 GiB → 512 行）；③ 只有叶子权限没有有效权限（§3.8） | 同上；`info tlb` 只做**交叉校验** |
| 7 | **调 `info gdt` / `info idt` / `info ldt`** | 实测 `unknown command`，QEMU 7.2 根本没有（`help info` 全表确认） | 从 `info registers` 的 `GDT=`/`IDT=` 取 base/limit，用 `xp` 读原始字节自己解码（§4） |
| 8 | **用 `query-registers`** | 实测 7.2.22 返回 `CommandNotFound` | `human-monitor-command` + `info registers` |
| 9 | **假设 CR3 低 12 位是 0** | `CR4.PCIDE=1` 时低 12 位是 PCID | `cr3 & 0x000F_FFFF_FFFF_F000` |
| 10 | **在 `CR0.PG=0` 时走页表** | 分页关闭时 CR3 无意义（实测 `info mem` 会打印 `PG disabled`） | 先查 CR0.PG + EFER.LMA，不是长模式就明确报错 |
| 11 | **假设 `info tlb` 的 `P` 字母表示 present** | 源码里 `PG_PSE_MASK` → `'P'`，**是 PS（大页）**；present 是过滤条件不打印（§5.2） | 按 X/G/P(PS)/D/A/C/T/U/W 九位解析 |
| 12 | **用 `capstone` 做 x86_64 主力反汇编** | 会引入 C 工具链（`capstone-sys` 的 `build.rs` 用 `cc::Build` 编译 C 源码），多平台打包/交叉编译成本高 | `iced-x86`（纯 Rust）。capstone 留给多架构需求 |
| 13 | **用 `zydis` 绑定** | crates.io 近 90 天下载 **9.6 K**（对比 iced-x86 957 K），crate 包 881 KB 捆 C 源码 | 同上 |
| 14 | **每次查询都 `addr2line::Context::new`** | 饿汉式装载全部 CU 的 line program，200 MB–1 GB vmlinux 会吃几百 MB + 数十秒 | 每文件一个 `Context`，后台建 + LRU 淘汰；地址查询走缓存 |
| 15 | **把 `addr2line` 的单行号当"崩溃行"显示** | `-O2` 下地址归属内联函数体而非调用点，`-i` 内联链也不完整（B 报告已实测） | 反汇编 + 行号并排；显示内联链但标注"可能不完整" |
| 16 | **用 `nm`/`readelf -s` 的 `st_value` 直接当运行地址** | `ET_DYN`/PIE、高半核、KASLR、`objcopy -O binary` 后加载到非链接地址 —— 都需要换算 | 按"链接地址 vs 加载基址"归一（B 报告 §5.2） |
| 17 | **把符号列表原样渲染** | 实测：C++ 析构器 `D0/D1/D2` 三个变体、Rust 泛型 N 个实例化、`.cold` 后缀、`vtable for`/`typeinfo for` 前缀 | 按 demangle 友好名聚合 + 可展开变体；RTTI 符号单独归类 |
| 18 | **用 `-nographic` 抓串口做数据源** | B 报告已实测：BIOS/iPXE 噪声 + monitor 多路复用进同一流；`-serial stdio -monitor stdio` 直接报错 | `-display none -serial file:` + `-monitor unix:` + `-qmp unix:` 三路分离 |
| 19 | **把总行数交给浏览器 DOM 高度** | 8 GiB / 16 B 每行 = 5.37 亿行，行高像素远超浏览器元素高度上限 | `transform: translateY` + 自绘滚动条，或分页 |
| 20 | **把 `info mem`/`info tlb` 的输出缓存成"静态页表快照"** | 实测 A/D 位是 **CPU 运行时写回**的（PML4E 我写 0x2003，读到 0x2023；TSS 描述符 busy 位被 `ltr` 改写） | 每次刷新重读；把 A/D 标为"运行时状态"而非"配置" |
| 21 | **把 `-cpu max` 当成"跟开发者机器一致"** | `-cpu max` 打开所有 TCG 支持的 CPUID（含 LA57、PCID、SMAP…），内核行为会与 `-cpu qemu64` 显著不同 | 明确固定 `-cpu` 字符串，并把它显示在 UI 上 |
| 22 | **假设 `xp` 读任何地址都会成功** | 实测非 RAM 地址返回文本 `Cannot access memory` | 解析器显式分支处理，页表遍历器把该情况当"表不在 RAM"错误 |

---

## 7. 诚实声明：本轮**未实测**的部分

> **一句话总结**：§2–§6（hex 工程、页表、GDT/IDT、QEMU monitor）**全部是本环境真跑出来的**；
> §1 的 crate **版本/许可/MSRV/feature/依赖形态**来自 crates.io 官方元数据，
> **API 入口**来自发布版源码逐条核对，但**没有一个 crate 被编译运行过**。
> 这条界线很重要：选型方向和风险判断是可靠的，**具体调用代码仍需 P5 编译时验证**。

### 7.1 已完成到什么程度

| 层次 | 状态 |
|---|---|
| crate 版本 / 许可证 / MSRV / feature 列表 | ✅ 上一轮 crates.io API 元数据（`.researchD/meta/*.json`） |
| crate 发布版源码里的 **API 入口名与位置** | ✅ 本轮解包 `.crate` 逐条核对（§1.7），**纠正了本报告初稿两处 API 名错误** |
| crate 的 **依赖形态**（是否需要 C 工具链等） | ✅ 读 `build.rs` / `Cargo.toml` 确认（capstone-sys 用 `cc::Build`） |
| crate 的**实际编译与运行** | ❌ **未做**。`.researchD/rustprobe/` 已写好（6 个子命令）、依赖已下载解包，但因本机 3 个并发 cargo 构建 + 关键路径优先，本轮没有跑完 |

### 7.2 逐条未实测清单

| # | 未实测项 | 影响 | 建议验证方式 |
|---|---|---|---|
| 1 | **所有 Rust crate 的实际编译与调用** | §1.7 是源码核对，**签名/lifetime/feature 组合未验证** | `.researchD/rustprobe/` 已就绪：`cargo run --release -- elf <path>` / `dwarf <path> <addr>` / `dis <path> <vaddr> <len>` / `cap <...>` / `demangle <sym>` / `mmap <path> <iters>`，一条命令可一次性验证 6 个 crate |
| 2 | `capstone` 能否在本机构建 | 已确认默认不需要 libclang（§1.3 更正），但**没实际编译过 capstone 的 C 源码** | `cargo check -p dprobe`（rustprobe 里已含 capstone 依赖） |
| 3 | `addr2line` 的 `loader` feature 是否处理 `.gnu_debugaltlink` | `/bin/ls` 同时有 debuglink 和 debugaltlink，Debian 二进制必然踩 | 用 `/bin/ls` + `/usr/lib/debug` 分离文件实测 |
| 4 | `memmap2::Mmap::advise` 的精确签名 | API 名已核对存在（§1.7），签名未验证 | 看 `registry/src/.../memmap2-0.9.11/src/advice.rs`，或 `cargo doc --open` |
| 5 | **gdbstub `qqemu.PhyMemMode`** | §5.6 的"二进制批量物理读"是页表遍历的最优通道，但只验证了 QEMU 二进制含该字符串 | `qemu … -s` + `gdb -ex 'target remote :1234' -ex 'maintenance packet qqemu.PhyMemMode:1' -ex 'x/4gx 0x104000'` |
| 6 | **LA57 下 `info mem` 的空输出在更新版 QEMU 是否已修** | 只在本机 7.2.22 实测 + 读了 v7.2.0/v7.2.22 源码（两者该函数一致）；v8/v9/v10 源码抓取因网络失败 | 抓 `v10.0.0/target/i386/monitor.c` 看 `mem_info_la57()` 的 present 判断 |
| 7 | **多 CPU 的 `info mem`/`info tlb`** | 本报告都是单核（`-smp 1`）实测；多核下"当前 CPU"的语义未验证 | `-smp 4` + `info cpus` 切换 CPU 后对比 |
| 8 | `xp` 的理论长度上限 | 实测 1024 qword 未截断，但没找到上限 | 试 `xp /65536gx` |
| 9 | 真实 GB 级**非稀疏**镜像的 mmap 行为 | §1.5/§2 用的是 8 GiB **稀疏**文件（块分配仅 8 KiB）。fault-around 预读机制与稀疏无关，但页缓存压力会不同 | 用真实 qcow2/raw 镜像重跑 `hexwin2.py` |
| 10 | `cpp_demangle` / `goblin` / `elf` / `zydis` / `yaxpeax-x86` 的源码核对 | 这几个是"不选"或"备选"，本轮未解包（下载被中断） | `.researchD/cratescan.py` 可复跑 |
| 11 | 真实内核（自研）的 GDT/IDT 解析正确性 | 用的是 fixture 的 refkernel + 自建探针内核，**没有测过第三方/真实 Linux 内核** | 拿一个真实 vmlinux 跑一遍 §4 的解码器 |


**另外两条本环境的构建环境实测（对项目有用，顺手记下）**：

1. **crates.io 稀疏索引在本机走 HTTP/2 会挂死，强制 HTTP/1.1 立刻恢复**：
   ```
   curl -sL https://index.crates.io/ob/je/object            → 60 s 超时，0 字节
   curl -sL --http1.1 https://index.crates.io/ob/je/object  → 6.6 s，125 950 字节 ✅
   ```
   [已实测]。项目已配置 USTC 镜像（`.cargo/config.toml`）绕过该问题，
   但**如果镜像临时不可用而要回落官方源，记得开 HTTP/1.1**。
2. **不要为 probe 单独设 `CARGO_HOME`**：会让 cargo 从零重拉整个索引，
   在本机网络下代价很大。`source scripts/env.sh` 已把 `CARGO_HOME`
   指向共享的 `.toolchain/cargo`（索引已就绪）。[已实测]

---

## 8. 附录：可复跑的实测命令

```bash
cd /root/PrincessIDE && source scripts/env.sh

# ---- ELF / DWARF / 符号（golden 参考）----
F=fixtures/refkernel/build/refkernel.elf
readelf -hW  $F ; readelf -lW $F ; readelf -SW $F ; readelf -sW $F
readelf --debug-dump=aranges $F
addr2line -e $F -f -C -i 0x100b3d
objdump -d -l --start-address=0x100a9c --stop-address=0x100ad0 $F

# ---- 合成页表/GDT/IDT 探针内核 ----
bash .researchD/kern/build.sh          # -> ptdemo4.elf (LA48) / ptdemo5.elf (LA57)
python3 .researchD/probe2.py .researchD/kern/ptdemo4.elf qemu64 4lvl
python3 .researchD/probe2smp1.py .researchD/kern/ptdemo5.elf 'max,+la57' 5lvl1
# 真实内核（ELF64 需经 GRUB ISO 启动）
python3 .researchD/probe2iso.py fixtures/refkernel/build/refkernel.iso qemu64 refkernel
# 原始输出：.researchD/out/mon-*.txt

# ---- hex 视图访问策略 ----
python3 .researchD/hexwin.py
python3 .researchD/hexwin2.py
# 原始输出：.researchD/out/hexwin{,2}.txt

# ---- 手工看 QEMU 状态 ----
QEMU=.toolchain/prefix/usr/bin/qemu-system-x86_64
$QEMU -kernel .researchD/kern/ptdemo4.elf -m 256M -cpu qemu64 -display none \
      -serial file:/tmp/s.txt -monitor unix:/tmp/m.sock,server=on,wait=off \
      -qmp unix:/tmp/q.sock,server=on,wait=off -no-reboot -L "$PRINCESSIDE_QEMU_DATA"
# 另开一个 shell:
#   python3 -c 'import socket;s=socket.socket(1);s.connect("/tmp/m.sock");s.sendall(b"info tlb\n");...'
```

---

## 引用

- crates.io 元数据（本仓库缓存）：`.researchD/meta/summary.txt`、`.researchD/meta/*.json`
  - object <https://crates.io/crates/object> / <https://docs.rs/object/0.40.0/object/>
  - gimli <https://crates.io/crates/gimli> / <https://docs.rs/gimli/0.34.0/gimli/>
  - addr2line <https://docs.rs/addr2line/0.27.1/addr2line/>
  - iced-x86 <https://crates.io/crates/iced-x86> / <https://docs.rs/iced-x86/1.21.0/iced_x86/>
  - capstone <https://docs.rs/capstone/0.14.0/capstone/>
  - memmap2 <https://docs.rs/memmap2/0.9.11/memmap2/struct.Mmap.html>
  - cpp_demangle <https://docs.rs/cpp_demangle/0.5.1/cpp_demangle/>
  - goblin <https://docs.rs/goblin/0.10.7/goblin/>
- QEMU monitor 源码（`print_pte` 九字母表、`mem_info_la48` 的逐级 AND、
  `mem_info_la57` 的 present 判断反转、`hmp_info_tlb` 的分派）：
  <https://raw.githubusercontent.com/qemu/qemu/v7.2.22/target/i386/monitor.c>
  （本环境抓取副本 `/tmp/i386mon7222.c`，md5 `71dd6646215e176c668734e6b2e76810`）
- QEMU monitor 文档 <https://www.qemu.org/docs/master/system/monitor.html>
- QMP 参考 <https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html>
- QEMU `info tlb` 输出量问题（上游补丁讨论）
  <https://patchew.org/QEMU/20260619125602.17077-1-AlanoSong@163.com/>
- Intel SDM Vol.3（页表位域 §4.5 / 段描述符 §3.4.5 / TSS §7.7 / IDT 门 §6.14）
  <https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html>
- DWARF 5 规范 <https://dwarfstd.org/doc/DWARF5.pdf>
- 本主题的相关报告：`docs/research/B-qemu-panic-symbolization.md`（QEMU 编排 / panic 符号化 /
  QMP 握手实测。本报告更正了其中 §4.2 对 `info tlb` 标志位 `P` 的推断）
