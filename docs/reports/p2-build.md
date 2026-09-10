# P2-B1 — `crates/princess-build` 实现报告

> 阶段：**P2-B1**（`princess-build` 构建引擎）
> 状态：**完成**——全部验收项（P2-2 / P2-2b / P2-6 / P2-6b / CDB / `.clangd` / 单测）已用真实命令跑通。
> 报告人：P2-B1 实现 Agent（第二棒，接续被 OOM 打断的第一棒）
> 本报告所有结论均有真实命令输出支撑；命令与退出码逐条附在下方「验收证据」。

---

## 0. 接续说明（本棒做了什么）

第一棒被内核 OOM 杀掉，但留下了**实质成果**。本棒**没有从零重写**，先读懂已有代码再补齐：

| 文件 | 第一棒留下 | 本棒处理 |
|---|---|---|
| `src/artifacts.rs` | ✅ 完整（274 行） | 修 1 个测试；补 1 个回归测试 |
| `src/diagnostics.rs` | ⚠️ 有 3 个真实 bug | **修复**（见 §4.1） |
| `src/process.rs` | ✅ 完整（615 行） | 修 1 个 clippy lint |
| `src/toolchain.rs` | ⚠️ 编译不过 | **修复签名**；补项目级 `nasm` 需求判定 |
| `src/lib.rs` | ⚠️ 引用不存在的符号 | **修复导出** |
| `src/backend.rs` | ❌ **完全缺失** | **新写**（1434 行） |
| `src/clangd.rs` | ❌ **完全缺失** | **新写**（973 行） |
| 根 `Cargo.toml` | 无 `princess-build` 成员 | **加 1 行成员** |

第一棒的代码有 **12 个编译错误**，`cargo build` 从未通过（所以「已有产物」此前是未验证状态）。

---

## 1. 交付物清单

```
crates/princess-build/
├── Cargo.toml          （依赖仅 princess-core + serde/serde_json/sha2，D21 省内存）
└── src/
    ├── lib.rs          （75 行）  公开 API 汇总 + BUILD_EVENT_ORDER
    ├── backend.rs      （1434 行）MakeBackend / build_project / 事件编排 / 产物发现
    ├── clangd.rs       （973 行） D7 .clangd 生成+校验 / D8 CDB / bear 规范化 / wrapper shim
    ├── diagnostics.rs  （704 行） gcc/clang/ld/nasm → build.diagnostic
    ├── artifacts.rs    （279 行） 扩展名驱动的产物发现（不硬编码任何文件名）
    ├── process.rs      （616 行） 进程组 + 双管道流式 + 截止时间 + UTF-8 chunker
    └── toolchain.rs    （588 行） 跨编译器工具检测 + E_TOOLCHAIN_MISSING 修复建议

docs/reports/p2-build.md            （本文件）
.scratch/build/                     （验收脚本、事件流、原始输出）
根 Cargo.toml                        （仅加 "crates/princess-build" 一行）
```

### 公开 API

```rust
// 后端与编排
pub struct MakeBackend;                       // impl princess_core::traits::BuildBackend
impl MakeBackend {
    pub fn new(toolchain: Toolchain) -> Self;
    pub fn for_project(project: &ResolvedProject) -> Self;
    pub fn with_runner(self, runner: Box<dyn CommandRunner>) -> Self;
    pub fn toolchain(&self) -> &Toolchain;
    pub fn build_env(&self, project: &ResolvedProject) -> BTreeMap<String, String>;
    pub fn require_tools(&self) -> Result<(), BuildOutcomeError>;
    pub fn require_tools_for(&self, project: &ResolvedProject) -> Result<(), BuildOutcomeError>;
    pub fn generate_compile_commands(&self, project, events) -> Result<Option<PathBuf>>;
    pub fn sync_dot_clangd(&self, project: &ResolvedProject) -> Result<PathBuf>;
    pub fn last_compile_commands(&self) -> Option<PathBuf>;
}
pub fn build_project(project, events, cancel) -> Result<BuildOutcome, BuildOutcomeError>;
pub struct BuildOutcome { finished, diagnostics, command, artifacts, compile_commands }
pub enum  BuildOutcomeError { ToolchainMissing { missing, suggestions }, Other(PrincessError) }
pub fn project_needs_nasm(project: &ResolvedProject) -> bool;
pub fn describe_toolchain(toolchain: &Toolchain) -> String;
pub fn shell_split(command: &str) -> Vec<String>;
pub fn source_for_tool(program: &str) -> Option<DiagnosticSource>;

// clangd 工程配置（D7 / D8 / D17 / D18）
pub const DEFAULT_TRIPLE: &str = "x86_64-unknown-none";
pub const NOSTD_INCLUDE_FLAG: &str = "-nostdlibinc";
pub const LANG_SERVICE_TOOL: &str = "clangd-16";
pub const GCC_ONLY_FLAG_BLACKLIST: [&str; 6];
pub struct DotClangd;   // for_kernel / new / write / load_and_validate
pub fn validate_dot_clangd(text: &str) -> Result<Vec<String>>;
pub fn lang_service_command() -> Vec<String>;
pub struct CompileCommand { directory, arguments, file, output }
pub struct CompileCommands;  // to_json / from_json / write
pub fn validate_compile_commands(text: &str) -> Result<Vec<String>>;
pub fn normalise_bear_output(raw: &str, build_cwd: &Path) -> Result<CompileCommands>;
pub fn compile_commands_usable(path: &Path) -> bool;
pub fn translation_unit(arguments: &[String], cwd: &Path) -> String;
pub struct WrapperShim;  // install / collect / env

// 诊断 / 进程 / 工具链
pub struct DiagnosticParser;  // feed / feed_text / feed_text_dedup / flush / family
pub fn parse_line(line: &str) -> Option<RawDiagnostic>;
pub fn parse_line_for(line: &str, family: ToolFamily) -> Option<RawDiagnostic>;
pub enum ToolFamily { Gcc, Clang, Ld, Nasm, Unknown }
pub struct SystemRunner;  pub trait CommandRunner;  pub struct FakeRunner;
pub fn kill_group(pgid: u32, signal: i32) -> Result<()>;
pub fn which(exe: &str) -> Option<PathBuf>;
pub fn detect_toolchain(overrides) -> Toolchain;
pub fn repair_suggestions(missing: &[&ToolStatus]) -> Vec<String>;
pub fn kernel_tool_specs() -> Vec<ToolSpec>;
```

---

## 2. 验收结果总表

**一键复现**：`bash .scratch/build/run-acceptance.sh`

| 项 | 命令 | 退出码 | 关键输出 |
|---|---|---|---|
| **P2-2** | `./target/debug/p2b1-e2e .scratch/build/p22-refkernel --dot-clangd` | 0 | `STATUS=ok` `ARTIFACT=.../refkernel.elf 24008` |
| **P2-2b** | `./target/debug/p2b1-e2e .scratch/build/p22b-template --dot-clangd` | 0 | `STATUS=ok` `ARTIFACT=.../kernel.elf 13568` |
| **P2-6** | `./target/debug/p2b1-e2e .scratch/build/p26-syntax` | 0 | `STATUS=failed` `EXIT_CODE=2`；`file=.../kernel.c line=101 col=40 source=gcc` |
| **P2-6b** | `PATH=.../nonasm-bin ./target/debug/p2b1-e2e .scratch/build/p26b-nonasm` | 0 | `ERROR_CODE=E_TOOLCHAIN_MISSING`；detail 含 `sudo apt-get install -y nasm` |
| **CDB** | 见 §3.5 | 0 | `arguments` 存在、无 `command`、`directory` 绝对 |
| **`.clangd`** | `clangd-16 --check=kernel.c` | 0 | 无 config error；`-triple x86_64-unknown-none`；glibc 路径消失；`-Wall -Wextra` 保留 |
| **单测** | `CARGO_BUILD_JOBS=1 cargo test -p princess-build` | 0 | **58 passed; 0 failed** |
| 事件顺序 | 见 §3.7 | 0 | `build.started` 首、`build.finished` 末且唯一 |

**`run-acceptance.sh` 连跑两次均 `pass=11 fail=0`**（幂等，无偶发失败）。

---

## 3. 验收证据（原始输出）

### 3.1 P2-2 — `fixtures/refkernel/`

夹具原本**没有** `princess.toml`；为走真实 manifest 路径，在 `.scratch/build/p22-refkernel/`（副本）放了最小 manifest，其中 **`artifacts = []`**——这正是契约里「引擎必须自己发现产物、不得硬编码夹具文件名」的检验点。

```
$ ./target/debug/p2b1-e2e .scratch/build/p22-refkernel --dot-clangd
DOT_CLANGD_PATH=/root/PrincessIDE/.scratch/build/p22-refkernel/.clangd
DOT_CLANGD_PROBLEMS=0
STATUS=ok
EXIT_CODE=Some(0)
DURATION_MS=1111
ARTIFACT_COUNT=1
ARTIFACT=/root/PrincessIDE/.scratch/build/p22-refkernel/build/refkernel.elf	24008	9358b3f9...
COMPILE_COMMANDS=/root/PrincessIDE/.scratch/build/p22-refkernel/compile_commands.json
COMPILE_COMMANDS_PROBLEMS=0
```

产物确为真 ELF：

```
$ file .scratch/build/p22-refkernel/build/refkernel.elf
ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, with debug_info, not stripped
$ readelf -h ... | grep -E "Class|Machine|Entry"
  Class:  ELF64
  Machine: Advanced Micro Devices X86-64
  Entry point address: 0x100040
$ nm ... | grep -E "refkernel_fault_probe|kernel_main"
0000000000100b4e T kernel_main
0000000000100b39 T refkernel_fault_probe
```

### 3.2 P2-2b — `templates/x86_64-multiboot2/`（证明无硬编码）

复制到 `.scratch/build/p22b-template/`（**不改只读模板**），产出文件名与夹具**不同**（`kernel.elf` vs `refkernel.elf`）：

```
$ ./target/debug/p2b1-e2e .scratch/build/p22b-template --dot-clangd
STATUS=ok
EXIT_CODE=Some(0)
ARTIFACT_COUNT=1
ARTIFACT=/root/PrincessIDE/.scratch/build/p22b-template/build/kernel.elf	13568	4c8456e2...
COMPILE_COMMANDS=/root/PrincessIDE/.scratch/build/p22b-template/compile_commands.json
COMPILE_COMMANDS_PROBLEMS=0
```

> `crates/princess-build/src/` 全库扫描确认：`refkernel` / `kernel.elf` 在**生产代码**（`#[cfg(test)]` 之前的部分）中只出现在**注释与文档字符串**里（`backend.rs:31`、`artifacts.rs:5-6`、`diagnostics.rs:85/304/436`），**没有任何可执行逻辑依据夹具文件名做判断**。产物识别完全由扩展名驱动（`elf`/`iso`/`img`/`bin`）。

### 3.3 P2-6 — 负样本：语法错误

在副本 `.scratch/build/p26-syntax/kernel.c` 的**第 101 行**插入 `int p26_syntax_error_probe = (1 + 2`（缺右括号与分号）：

```
$ ./target/debug/p2b1-e2e .scratch/build/p26-syntax
STATUS=failed
EXIT_CODE=Some(2)
ARTIFACT_COUNT=0

build.diagnostic（共 6 条，已去重）:
  error   gcc  /root/.../p26-syntax/kernel.c 101 :: expected ‘)’ before ‘__asm__’
  error   gcc  /root/.../p26-syntax/kernel.c 107 :: expected ‘,’ or ‘;’ before ‘}’ token
  error   gcc  /root/.../p26-syntax/kernel.c 134 :: expected declaration or statement at end of input
  warning gcc  /root/.../p26-syntax/kernel.c 101 :: unused variable ‘p26_syntax_error_probe’ [-Wunused-variable]
  warning gcc  /root/.../p26-syntax/kernel.c 111 :: ‘kernel_main’ defined but not used [-Wunused-function]
  error   gcc  (无位置)                        :: make: *** [Makefile:40: build/kernel.o] Error 1
```

**`file` 绝对、`line` 精确命中 101、`source=gcc`**（契约增补项），全部 6 条的 `source` 都是 `gcc`——没有把 GNU gcc 的诊断谎报成 `clang`。

### 3.4 P2-6b — 负样本：工具不可用（藏掉 `nasm`）

1. 造一个**真的需要 nasm** 的项目 `.scratch/build/p26b-nonasm/`（`boot.asm` + Makefile 里 `nasm -f elf64`）。
2. **基线**（nasm 可见）：`STATUS=ok` — 证明负样本不是假失败。
3. 用 symlink farm 造一条**不含 nasm** 的 PATH（`.scratch/build/nonasm-bin/`）：

```
$ PATH=/root/PrincessIDE/.scratch/build/nonasm-bin command -v nasm
nasm: NOT FOUND (as intended)
$ PATH=... ./target/debug/p2b1-e2e .scratch/build/p26b-nonasm --no-cdb
ERROR_CODE=E_TOOLCHAIN_MISSING
ERROR_MESSAGE=required toolchain tool(s) missing: nasm
ERROR_DETAIL=nasm is missing (assembles .s/.asm sources; ...) — nasm: not found on PATH; yasm: not found on PATH
             → fix: sudo apt-get install -y nasm    # or remove the NASM sources from the project
```

**修复建议是可执行的**，且已核实真实存在：

```
$ apt-cache policy nasm
nasm:
  Installed: (none)
  Candidate: 2.16.01-1
```

### 3.5 CDB — `compile_commands.json`（D8）

```
$ python3 -c "..."
CDB: arguments + absolute directory, no command field
entries: 2
  keys: ['arguments','directory','file','output']
  directory: /root/PrincessIDE/.scratch/build/p22b-template  (absolute: True)
  has command-field: False
```

条目用 `arguments` 数组而非 `command` 字符串，`directory` 为绝对路径——D8 逐条满足。生成走 **bear 3.1.1**，且**经 PATH 里的启动器** `/root/PrincessIDE/.toolchain/bin/bear` 调用（D18）；生成前先 `make clean`（D18 防 `[]` 覆盖）。

### 3.6 `.clangd`（D7 / D6 / D18）——**用真实 clangd-16 验证**

```
$ clangd-16 --version
Debian clangd version 16.0.6 (15~debian12u1)

$ cd .scratch/build/p22-refkernel && clangd-16 --check=kernel.c --compile-commands-dir=$PWD ; echo $?
0
```

对 cc1 参数做机器判定（两个工程都通过）：

| 检查 | p22-refkernel | p22b-template |
|---|---|---|
| clangd 接受 `.clangd`（无 `config error`） | OK | OK |
| 字典 schema 被接受（无 `should be a dictionary`） | OK | OK |
| `-triple x86_64-unknown-none` 生效 | OK | OK |
| `-nostdlibinc` 生效（`-nostdsysteminc`） | OK | OK |
| **glibc include 路径消失**（无 `-internal-externc-isystem`） | OK | OK |
| **`-Wall -Wextra` 保留** | OK | OK |
| `All checks completed, 0 errors` | OK | OK |

生成的 `.clangd`：

```yaml
CompileFlags:
  CompilationDatabase: .
  Add:
    - --target=x86_64-unknown-none
    - -nostdlibinc
  Remove:
    - "-fno-tree-loop-distribute-patterns"
    - "-fconserve-stack"
    - "-mpreferred-stack-boundary=*"
    - "-fno-var-tracking-assignments"
    - "-fno-ipa-icf"
    - "-mno-direct-extern-access"
```

**无 `-W*` 通配**（D7 明令禁止），D7 黑名单 6 条逐条在列。

### 3.7 事件顺序（契约 §2）

```
total events: 24
build.started: 1 | log.append: 21 | artifact.changed: 1 | build.finished: 1
first: build.started | last: build.finished | count(build.finished): 1
ORDER OK；seq 单调递增
```

### 3.8 单测

```
$ CARGO_BUILD_JOBS=1 cargo test -p princess-build
test result: ok. 58 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0 passed; 0 failed  (doc-tests)

$ cargo build -p princess-build        → 0 warnings
$ cargo clippy -p princess-build       → princess-build 0 warnings（余下 9 条属冻结的 princess-core）
```

---

## 4. 本棒发现并修复的真实 bug

这些**全部**是被测试或真实验收暴露出来的，不是预防性重构：

### 4.1 `split_location` 让 `file:line`（无列号）完全解析失败 ⚠️ 最严重
```rust
let colon = location[..end].rfind(':')?;   // 第二圈找不到冒号就 return None
```
`boot.s:12` 这类**最常见**的形状（nasm 与部分 gcc 输出）永远返回 `None` → **P2-6 会直接失败**。改成 `let ... else { break }`。

### 4.2 `parse_colon_form` 取错 marker 位置
原逻辑在多个 marker 命中时保留**最后遍历到**的而非**最靠左**的；`: fatal error:` 又包含 `: error:` 子串。改为 `min_by_key(at)` 且 longest-first。

### 4.3 `.clangd` 用了 clangd 不接受的**列表** schema ⚠️ 隐蔽
第一版写成 `CompileFlags:` 后直接跟 `- --target=...`。clangd-16 **报 `CompileFlags should be a dictionary` 然后整个文件作废**，静默退回 host triple `x86_64-pc-linux-gnu` 并加载 glibc `/usr/include`——**这正是 D7 要防的 glibc 污染**，而文本级校验却给不出任何告警（假通过）。
- 生成器改为 clangd 真正的字典 schema（`Add:` / `Remove:`）。
- **校验器补了结构检查**：列表形式直接判 ERROR（附回归测试 `the_list_form_is_rejected_...`）。

### 4.4 wrapper shim 写出的不是合法 JSON
用 `printf` 拼 `'%s',` 得到的是**单引号**（shell 引用），`serde_json` 静默跳过每一行 → `compile_commands.json` 永远为空。改为对每个参数做 JSON 转义，并补 `translation_unit()`（此前错误地把 `-o` 的**输出**路径当作 TU）。

### 4.5 `[-Wflag]` 被当作 clang 指纹 ⚠️ 数据失真
GNU gcc 的 `-Wall/-Wextra` 告警同样打印 `[-Wunused-variable]`，于是**每一条 gcc 告警都被标成 `clang`**。契约把 `gcc` 作为增补项正是因为这种失真不可接受。改为**由实际运行的工具族决定**，文本指纹只在族未知时兜底。

### 4.6 同一诊断被重复推送 5 次
gcc 一个语法错误会打印「错误行 + 源码回显 + 脱字符 + 下一行」5 行；原实现每遇到续行就把整个诊断**重新 emit 一次**（消息越来越长）。改为续行只**折叠进同一条**，不再产生新事件；新增 `flush()` 保证折叠后的完整消息仍能拿到。

### 4.7 《契约 §2》被违反：`build.finished` 之后还有事件
CDB 生成原本放在 `execute()` 之后，它自己的 `log.append` 就**尾随在终结事件之后**。改为在 `execute()` 内部、`build.finished` **之前**完成。

### 4.8 每隔一次构建 `artifacts: []` ⚠️ 偶发
`make clean`（D18，CDB 步骤）在**产物扫描之后**执行，导致「无需重建」的那一次扫描看到的全是旧 mtime → 报空，随后文件又被删掉重建。改为 **CDB 生成先于产物发现**。修复后连跑 5 次稳定，回归测试 `a_rebuild_of_an_up_to_date_project_still_reports_its_artifacts`。

### 4.9 bear 的 `--output` 放在了 `--` 之后
`bear -- make all --output x.json` 会把 `--output` 当成 make 的**目标**，报 `No rule to make target 'compile_commands.json'`。bear 自己的 flag 必须在 `--` **之前**。

### 4.10 其它
- `Path::ends_with("/sh")` 语义误用（`ends_with` 按**整段组件**匹配，`"/sh"` 解析成 root+`sh`，对绝对路径永远为 false）。
- `nasm` 被无条件视为「非必需」，导致藏掉 nasm 时**不会**产生 `E_TOOLCHAIN_MISSING`。改为**由工程内容决定**（`project_needs_nasm`：manifest 里 pin 了 nasm，或树里存在 `.asm`/`.nasm`）——不是硬编码夹具名。
- `ToolSpec::new` 签名不匹配（12 个编译错误之一）。

---

## 5. 有歧义 / 可能被质疑的设计决定

1. **`nasm` 的条件性必需**。D7/D8 没说 nasm 是否必需。我判定：纯 C 内核不需要它，所以不放进无条件必需集；但**工程内容**（`.asm`/`.nasm` 文件或 `[toolchain] as = "nasm"`）证明需要时，缺失即 `E_TOOLCHAIN_MISSING`。`.s` **不**触发（GNU `as` 与 nasm 争这个扩展名，夹具与模板都用 GNU `as`）。**若主 Agent 认为 nasm 应无条件必需，改 `ToolRole::required` 一行即可。**
2. **`build.finished.artifacts` 的 generation 过滤**：声明式产物**总是**上报（manifest 是用户的明确声明），扩展名发现式产物才要求「构建开始后被写过」。这是契约 §4「为空则发现」的落地方式。代价是：一个被删掉但 mtime 很旧的声明产物若仍存在也会上报——这符合「manifest 是权威」。
3. **CDB 失败不致构建失败**（只发 `log.append` 提示）。依据 D17「LSP 不在关键路径」。**若主 Agent 要求 CDB 失败即构建失败，需要改一处。**
4. **CDB 重跑会 `make clean` 并重建一次**（D18 要求），因此一次 `build` 实际调用 make 两次（第二次在 bear 下）。构建耗时约翻倍（实测 refkernel 0.3s→1.1s）。这是 D18 的直接后果，我没有绕开。
5. **wrapper shim 用「第二次 make」而非拦截第一次**：shim 需要 `CC` 环境变量，而 `make` 的变量优先级使 `make CC=...` 才可靠；为不改变用户 Makefile 语义，选择 clean 后重建一次。
6. **`build_env` 强制注入 `CARGO_BUILD_JOBS=1` 与 `MAKEFLAGS=-j1`**：这是 D21 内存纪律在**引擎侧**的落实，但会覆盖用户显式的 `-j`。考虑到本机无 swap，我选择安全优先。**若用户需要并行构建，需要放开这里。**
7. **`.clangd` 里我加了 `CompilationDatabase: .`**（D7 未要求）。理由是让 clangd 在任意 cwd 下都能找到 `compile_commands.json`。它被 clangd-16 接受，无副作用。
8. **`events` 的 `artifact.changed` payload 只填 `path` + `kind`**：core 的 `ArtifactChangedPayload` 只有这两个字段（`size`/`sha256` 在 `build.finished.artifacts[]` 里）。我按 core 冻结类型实现，未擅自加字段。
9. **验收用的 `princess.toml`**：`fixtures/refkernel/` **没有** manifest。为检验「`artifacts = []` → 发现」这条契约，我在**副本**里放了一个最小 manifest（未改只读夹具）。模板自带 manifest，直接用。
10. **验收 harness 放在 `.scratch/build/e2e/`**（`princess-cli` 的 `build` 子命令归 P2-A，我不能越界改它，且它尚未接 `princess-build`）。harness 是一个独立小 crate，直接调用本 crate 的公开 API 并输出真实事件流。
11. **`princess-core` 的 9 条 clippy 提示未动**（冻结，只读）。

---

## 6. 给下游 Agent 的接口提示

1. **给 B2（`princess-run`）/ P2-C**：`build_project(&ResolvedProject, &mut dyn EventSink, &CancelToken) -> Result<BuildOutcome, BuildOutcomeError>` 就是入口；`BuildOutcome.artifacts` 可直接喂 `artifacts::boot_candidates()` 选启动介质（ISO > Image > ELF，D2 的 GRUB-ISO 路径优先）。**`E_TOOLCHAIN_MISSING` 走 `BuildOutcomeError::to_error()`**，`detail` 里是可粘贴的修复命令。
2. **`build.finished.artifacts[]` 可能为空**（契约允许）。取 ELF 时先 `artifacts::elf_artifacts()`；若为空请回退到 `[run] kernel` / `[debug] symbols`，不要假定一定有产物。
3. **CDB 路径**在 `BuildOutcome.compile_commands`（`Option<PathBuf>`）。clangd 的命令行请用 `clangd::lang_service_command()`（已写死 `clangd-16`，D18）——**不要**回退到不带版本号的 `clangd`（那是 14）。
4. **诊断的 `source` 是诚实值**：B3 做符号化 / UI 做过滤时**不要**再按消息文本猜 `gcc`/`clang`，直接用 `payload.source`；`ToolFamily::for_program()` 是唯一判定入口。
5. **复用 `SystemRunner` + `kill_group()` 做进程组收尾**（`process.rs` 已实现 `setsid` + 组杀 + 截止时间 + UTF-8 chunker）。QEMU 的孤儿进程纪律（派发计划 §一之二）可直接复用这套，不要再写一遍。

---

## 7. 复现命令（逐条可跑）

```bash
cd /root/PrincessIDE
source scripts/env.sh
export CARGO_BUILD_JOBS=1          # D21

# 单测
cargo test -p princess-build

# 全量验收（11 项）
bash .scratch/build/run-acceptance.sh

# 单独复现某一项
cd .scratch/build/e2e && cargo build && cd /root/PrincessIDE
./target/debug/p2b1-e2e .scratch/build/p22-refkernel --dot-clangd   # P2-2
./target/debug/p2b1-e2e .scratch/build/p22b-template --dot-clangd   # P2-2b
./target/debug/p2b1-e2e .scratch/build/p26-syntax                   # P2-6
PATH=/root/PrincessIDE/.scratch/build/nonasm-bin \
  ./target/debug/p2b1-e2e .scratch/build/p26b-nonasm --no-cdb       # P2-6b

# .clangd 用真实 clangd-16 验证
cd .scratch/build/p22-refkernel
clangd-16 --check=kernel.c --compile-commands-dir="$PWD"; echo "exit=$?"
```

---

## 8. 纪律自查

| 纪律 | 状态 |
|---|---|
| D21：构建一律 `CARGO_BUILD_JOBS=1`，启动前 `free -h` | ✅ 每次构建前记录可用内存（1.2G~2.3G，均 ≥ 1.2G 阈值） |
| D21：重活串行、不并发编译 | ✅ 未与其他 Agent 并发重活；遇 `Blocking waiting for file lock` 即等待 |
| D19：网络镜像 | ✅ 未改 `.cargo/config.toml`；构建均在秒级完成 |
| D18：bear 经 PATH 启动器调用 | ✅ 实测命令显示 `/root/PrincessIDE/.toolchain/bin/bear` |
| D18：生成 CDB 前 `make clean` | ✅ `run_clean_step()` |
| D7：无 `-W*` 通配 | ✅ 校验器主动拒绝，且有回归测试 |
| 只写授权目录 | ✅ 仅 `crates/princess-build/`、`docs/reports/p2-build.md`、`.scratch/build/`；根 `Cargo.toml` 只加 1 行成员 |
| 未执行改动 git 的命令 | ✅ |
| 未改冻结的 `princess-core` | ✅ |
| 一切结论有真实输出支撑 | ✅ 本报告每条均附命令与退出码 |

**未安装任何 apt 包**（`E_TOOLCHAIN_MISSING` 的负样本用 symlink farm 模拟，未改系统状态）。
