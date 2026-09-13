# PrincessIDE

> **面向 x86_64 内核开发的集成开发环境。**

PrincessIDE 是一个专为 x86_64 内核开发者设计的桌面 IDE。它提供从构建、启动、调试到二进制分析的完整工作流，内置 QEMU 集成、GDB DAP 调试器、页表可视化、反汇编视图和 AI 辅助编码。

---

## 功能列表

| 功能 | 说明 |
|---|---|
| **构建系统** | 基于 `princess.toml` 的项目描述，自动调用 NASM + GCC/LD 或 Makefile，生成 Multiboot2 GRUB ISO |
| **运行 / QEMU** | 一键启动 QEMU 无头模式，实时捕获串口输出，自动检测 CPU 异常（`#UD`、`#PF`、三击故障） |
| **调试器** | 基于 GDB 16.3 内置 DAP（Debug Adapter Protocol），支持断点、单步、寄存器查看、栈回溯 |
| **二进制查看** | ELF 解析（段节/符号/入口点）、反汇编（iced-x86）、hex 视图 |
| **页表可视化** | 按 CR3 逐级解析页表、GDT/IDT 查看、QEMU monitor `info mem` / `info tlb` 解析 |
| **AI 辅助** | 可选的 AI 代码辅助，不阻塞主流程（AI 不可用时 IDE 功能完全不受影响） |
| **模板系统** | `x86_64-multiboot2` 项目模板，含 `princess.toml`、Makefile、`.clangd` 配置 |
| **语言服务** | 基于 clangd-16 的 C 语言服务（补全/跳转/诊断），针对内核项目自动配置 |

---

## 快速开始

### 1. 克隆仓库

```bash
git clone <repo-url> PrincessIDE
cd PrincessIDE
```

### 2. 安装工具链

```bash
bash scripts/bootstrap-toolchain.sh
```

此脚本会自动安装：
- Rust 稳定版工具链（cargo / rustc / rustfmt / clippy）
- QEMU、NASM、GCC、LLD、GDB、xorriso、mtools
- clangd-16（IDE 语言服务）、bear（编译数据库生成）
- GDB 16.3（DAP 调试后端，隔离于 `.toolchain/gdb16/`）

脚本是幂等的，重复运行不会重新下载。

### 3. 激活环境

```bash
source scripts/env.sh
```

### 4. 验证工具链

```bash
bash scripts/doctor.sh
```

所有必需工具应显示 `ok`。

### 5. 构建

```bash
cargo build --workspace
```

### 6. 运行冒烟测试

```bash
bash scripts/smoke-boot.sh
```

这会构建参考内核、在 QEMU 中启动、验证串口输出包含横幅和异常信息。

### 7. 运行完整测试套件

```bash
cargo test --workspace
```

### 8. 启动桌面应用（需要显示器环境）

```bash
bash scripts/run-ide.sh
```

脚本会自己 `source scripts/env.sh` 并做预检（依赖、Rust、GUI 库、显示器、1420 端口），
任一不满足都直接告诉你修复命令，而不是让 Tauri 在更深处报一个看不懂的错。

| 想要 | 命令 |
|---|---|
| 正常启动（Tauri 窗口 + 引擎） | `bash scripts/run-ide.sh` |
| 只预检、不启动 | `bash scripts/run-ide.sh --check` |
| 无显示器时的界面预览（只有界面，没有引擎） | `bash scripts/run-ide.sh --browser` |

等价的手工命令是 `source scripts/env.sh && pnpm -C apps/desktop tauri dev`。
注意**不要**用 `cd apps/desktop/src-tauri && cargo run` —— 那样不会起 vite 开发服务器，
前端加载的是 `dist/` 里的旧构建产物，也没有热更新。

---

## 依赖说明

### 系统依赖（Linux x86_64）

`bootstrap-toolchain.sh` 会自动处理以下依赖，但你需要确保系统有：

| 依赖 | 说明 |
|---|---|
| **Linux x86_64** | 目前仅支持 Linux（x86_64 架构） |
| **apt-get** | 用于下载 .deb 包（Debian/Ubuntu 系） |
| **curl** | 用于下载 rustup |
| **dpkg-deb** | 用于解压 .deb 包 |
| **python3** | 用于部分验收脚本 |
| **make** | 用于构建参考内核 |

### Rust 工具链

由 `bootstrap-toolchain.sh` 自动安装到 `.toolchain/` 目录，不会污染系统：

- **rustc** >= 1.75（stable）
- **cargo**、**rustfmt**、**clippy**

### 内核开发工具

同样由脚本自动安装到 `.toolchain/prefix/`：

| 工具 | 用途 |
|---|---|
| `qemu-system-x86_64` | x86_64 系统模拟器 |
| `nasm` | 汇编器（Multiboot2 引导代码） |
| `gcc` / `ld` | C 编译器 / 链接器 |
| `ld.lld` | LLVM 链接器（备选） |
| `grub-mkrescue` | 生成 GRUB 引导 ISO |
| `xorriso` / `mtools` | ISO 生成辅助工具 |
| `clangd-16` | C 语言服务（IDE 专用） |
| `bear` | 编译数据库生成 |
| `princess-gdb` | GDB 16.3（DAP 调试后端） |

---

## 项目结构

```
PrincessIDE/
├── apps/
│   └── desktop/          # Tauri v2 桌面外壳
│       └── src-tauri/    # Rust 侧（独立 cargo workspace）
├── crates/
│   ├── princess-core/    # 核心事件模型与数据结构
│   ├── princess-cli/     # CLI 入口（构建/运行/事件流）
│   ├── princess-build/   # 构建引擎（Make/clangd 配置生成）
│   ├── princess-run/     # QEMU 运行引擎（串口捕获、异常检测）
│   ├── princess-symbol/  # 符号化引擎（ELF/DWARF/反汇编）
│   ├── princess-bin/     # 二进制查看器引擎
│   ├── princess-debug/   # 调试引擎（GDB DAP 适配）
│   └── princess-ai/      # AI 辅助引擎
├── fixtures/
│   ├── refkernel/        # 参考内核（C + NASM，Multiboot2）
│   ├── paging-kernel/    # 分页内核（调试验收用）
│   └── qemu-monitor/     # QEMU monitor 录制样本
├── templates/
│   └── x86_64-multiboot2/ # 项目模板
├── scripts/
│   ├── bootstrap-toolchain.sh  # 工具链安装
│   ├── env.sh                  # 环境激活
│   ├── doctor.sh               # 工具链检测
│   ├── smoke-boot.sh           # 冒烟启动测试
│   ├── symbolicate.sh          # 符号化测试
│   ├── smoke-ci.sh             # CI 冒烟脚本
│   ├── package-deb.sh          # deb 打包
│   └── package-appimage.sh     # AppImage 打包
└── docs/
    ├── spec/             # 规格文档
    └── reports/          # 阶段验收报告
```

---

## 开发者指南

### 运行测试

```bash
# 激活环境
source scripts/env.sh

# 运行全部 workspace 测试
cargo test --workspace

# 运行单个 crate 测试
cargo test -p princess-core
cargo test -p princess-build
cargo test -p princess-run
cargo test -p princess-symbol
cargo test -p princess-debug

# 运行冒烟启动测试（需要 QEMU）
bash scripts/smoke-boot.sh

# 运行符号化测试
bash scripts/symbolicate.sh
```

### Workspace 结构

项目采用双 workspace 设计：

- **根 workspace**（`Cargo.toml`）：8 个引擎 crate，共享 `workspace.dependencies`
- **Tauri workspace**（`apps/desktop/src-tauri/Cargo.toml`）：独立 workspace，避免与引擎的依赖树冲突

### Tauri 开发

```bash
cd apps/desktop

# 安装前端依赖（需要 node + pnpm）
pnpm install

# 构建前端
pnpm build

# 运行桌面应用
cd src-tauri
cargo run
```

### 构建配置

在 `princess.toml` 中配置项目：

```toml
[build]
# 可选：并行构建任务数（不设置则交给 make/cargo 自行决定）
# jobs = 2

[run]
timeout_ms = 15000
memory = "256M"
```

### 内核项目 `.clangd` 配置

IDE 会自动生成 `.clangd` 配置，内核开发者也可以手动配置：

```yaml
CompileFlags:
  Add:
    - --target=x86_64-unknown-none
    - -nostdlibinc
  Remove:
    - -fno-tree-loop-distribute-patterns
    - -fconserve-stack
    - -mpreferred-stack-boundary=*
    - -fno-var-tracking-assignments
    - -fno-ipa-icf
    - -mno-direct-extern-access
```

**注意**：不要用 `-nostdinc`（会删除 clang 自带的 freestanding 头），也不要用 `-W*` 通配删除（会丢失 `-Wall -Wextra` 诊断）。

---

## 已知限制

| 限制 | 说明 |
|---|---|
| **仅 Linux** | 目前只支持 Linux x86_64，macOS/Windows 暂不支持 |
| **KVM 可能不可用** | 容器/云环境中 KVM 可能不可用，QEMU 将使用 TCG 软件模拟（启动较慢） |
| **无 swap 需谨慎** | 内存有限的机器建议启用 swap（`fallocate -l 4G /swapfile && mkswap /swapfile && swapon /swapfile`） |
| **无显示器时无法验证 GUI** | 桌面应用的视觉效果需在有显示器的环境中验证 |
| **汇编语言服务受限** | NASM 汇编仅支持语法高亮和调试期反汇编视图，不支持跳转定义/补全（clangd 不支持 NASM） |
| **仅 C 语言服务** | v1 语言服务仅支持 C（clangd-16），Rust/C++/Zig 暂不支持 |
| **GDB 16.3 需要 trixie 镜像** | DAP 调试后端需要 Debian trixie 镜像下载 GDB 16.3，网络不佳时可能安装失败 |

---

## 许可证

MIT OR Apache-2.0
