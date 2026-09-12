# PrincessIDE 安装指南

> 本文档介绍如何安装和运行 PrincessIDE。  
> 更多项目信息请参阅 [README.md](README.md)。

---

## 方式一：从源码构建（推荐）

### 前提条件

- **操作系统**：Linux x86_64（Debian 12+、Ubuntu 22.04+ 或兼容发行版）
- **磁盘空间**：约 3 GB（工具链 + 构建产物）
- **内存**：建议 4 GB+，无 swap 的低内存机器请先创建 swap：

```bash
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile
# 持久化（可选）：
echo '/swapfile none swap sw 0 0' | sudo tee -a /etc/fstab
```

### 步骤

#### 1. 安装系统依赖

```bash
# Debian / Ubuntu
sudo apt-get update
sudo apt-get install -y \
    curl \
    make \
    gcc \
    binutils \
    python3 \
    grub-pc-bin \
    grub-common \
    xorriso \
    mtools \
    dpkg
```

> **注意**：`bootstrap-toolchain.sh` 会自动下载大部分开发工具（QEMU、NASM、clangd 等）到工作区 `.toolchain/` 目录，不需要系统安装。上面的包只是构建 ISO 所需的基础依赖。

#### GUI / 桌面依赖（Tauri 外壳必需）

Tauri v2 的 Linux 外壳依赖 WebKitGTK 4.1 和 GTK3，缺少这些库时 `pnpm tauri dev` 会在**编译期**报错（找不到 `webkit2gtk-4.1` 等 pkg-config 模块）。

```bash
sudo apt-get update -y && sudo apt-get install -y \
    libwebkit2gtk-4.1-dev \
    libgtk-3-dev \
    libjavascriptcoregtk-4.1-dev \
    libsoup-3.0-dev \
    librsvg2-dev \
    libayatana-appindicator3-dev \
    libxdo-dev \
    libssl-dev \
    pkg-config \
    build-essential \
    file \
    wget \
    curl
```

验证 WebKitGTK 已安装：

```bash
pkg-config --modversion webkit2gtk-4.1
# 预期输出：2.50.6（或更高）
```

> **提示**：`scripts/doctor.sh` 也会检查这些 GUI 开发库。如果验证命令失败，请先运行 `bash scripts/doctor.sh` 查看具体缺少哪些模块。

#### 2. 克隆仓库

```bash
git clone <repo-url> PrincessIDE
cd PrincessIDE
```

#### 3. 安装工作区工具链

```bash
bash scripts/bootstrap-toolchain.sh
```

首次运行约需 10-30 分钟（取决于网络速度），会安装：

| 组件 | 安装位置 |
|---|---|
| Rust 工具链 | `.toolchain/rustup/` + `.toolchain/cargo/` |
| QEMU、NASM、GCC 等 | `.toolchain/prefix/` |
| clangd-16、bear | `.toolchain/prefix/` |
| GDB 16.3（DAP） | `.toolchain/gdb16/rootfs/` |
| 启动器脚本 | `.toolchain/bin/` |

脚本是**幂等**的，重复运行会跳过已完成的步骤。

#### 4. 激活环境

```bash
source scripts/env.sh
```

这会将工具链路径添加到 `PATH` 和 `LD_LIBRARY_PATH`。每次打开新终端时需要重新执行。

> **提示**：可以将 `source /path/to/PrincessIDE/scripts/env.sh` 添加到 `~/.bashrc` 中。

#### 5. 验证工具链

```bash
bash scripts/doctor.sh
```

预期输出：所有必需工具显示 `ok`，最后输出 `doctor: all required tools present`。

如果有工具显示 `MISSING`，重新运行 `bootstrap-toolchain.sh`。

#### 6. 构建引擎

```bash
cargo build --workspace
```

#### 7. 运行冒烟测试

```bash
bash scripts/smoke-boot.sh
```

预期输出：`[smoke-boot] PASS`。

#### 8. 运行完整测试

```bash
cargo test --workspace
```

预期输出：所有测试通过（500+ 个测试）。

#### 9. 构建桌面应用（可选，需要显示器环境）

```bash
# 安装 Node.js 和 pnpm（如果尚未安装）
curl -fsSL https://get.pnpm.io/install.sh | sh -
source ~/.bashrc

# 安装前端依赖
cd apps/desktop
pnpm install
pnpm build

# 构建并运行 Tauri 应用
cd src-tauri
cargo run
```

---

## 方式二：从 deb 包安装

### 安装

```bash
sudo dpkg -i princesside_0.1.0_amd64.deb
sudo apt-get install -f   # 修复依赖（如有需要）
```

### 运行

```bash
princesside
```

> **注意**：deb 包仅包含 Tauri 桌面应用二进制。内核开发工具链（QEMU、NASM 等）仍需通过 `bootstrap-toolchain.sh` 安装到工作区。

### 卸载

```bash
sudo dpkg -r princesside
```

---

## 方式三：从 AppImage 运行

```bash
chmod +x PrincessIDE-0.1.0-x86_64.AppImage
./PrincessIDE-0.1.0-x86_64.AppImage
```

AppImage 是自包含的，无需安装。但内核开发工具链仍需单独安装。

---

## 依赖列表

### 系统包（Debian/Ubuntu）

| 包名 | 用途 | 是否必需 |
|---|---|---|
| `curl` | 下载 rustup | 是 |
| `make` | 构建参考内核 | 是 |
| `gcc` | C 编译器 | 是 |
| `binutils` | 链接器、objdump 等 | 是 |
| `python3` | 验收脚本 | 是 |
| `grub-pc-bin` | 生成 GRUB 引导 ISO | 是 |
| `grub-common` | grub-mkrescue | 是 |
| `xorriso` | ISO 生成 | 是 |
| `mtools` | FAT 文件系统工具 | 是 |
| `dpkg` | 解压 .deb 包 | 是 |

### Rust 工具链（自动安装）

- **rustc** >= 1.75
- **cargo**（包管理器）
- **rustfmt**（格式化）
- **clippy**（静态分析）

### 内核开发工具（自动安装到 `.toolchain/`）

| 工具 | 版本 | 用途 |
|---|---|---|
| `qemu-system-x86_64` | 7.2+ | x86_64 系统模拟 |
| `nasm` | 2.14+ | 汇编器 |
| `clang` / `clangd` | 14.0 | 基础编译/语言服务 |
| `clang-16` / `clangd-16` | 16.0 | IDE 语言服务（推荐） |
| `ld.lld` | 14.0+ | LLVM 链接器 |
| `gdb` | 13.1 | 基础调试 |
| `princess-gdb` | 16.3 | DAP 调试后端 |
| `bear` | 3.1+ | 编译数据库生成 |
| `xorriso` | 1.5+ | ISO 生成 |
| `mtools` | 4.0+ | FAT 工具 |

---

## 故障排除

### `bootstrap-toolchain.sh` 卡在下载

**症状**：脚本长时间停在 "deb: downloading ..." 或 "rust: installing ..."。

**原因**：网络速度慢（特别是国际带宽不佳的环境）。

**解决**：
- Rust 工具链使用清华镜像（默认已配置）。
- deb 包使用系统 apt 源，确保 `/etc/apt/sources.list` 配置了可用镜像。
- 如果持续失败，设置环境变量使用备用镜像：
  ```bash
  PRINCESSIDE_RUSTUP_DIST_SERVER=https://mirrors.ustc.edu.cn/rustup \
  bash scripts/bootstrap-toolchain.sh
  ```

### `cargo build` 很慢或卡住

**症状**：`cargo build` 长时间停在 "Downloading ..." 阶段。

**原因**：crates.io 下载慢。

**解决**：项目已配置 USTC 镜像（`.cargo/config.toml`），如果仍慢，可尝试 rsproxy 备用源：
```bash
# 编辑 .cargo/config.toml，将 replace-with 改为 "rsproxy"
```

### `smoke-boot.sh` 超时

**症状**：`[smoke-boot] FAIL: banner not found in serial output`。

**可能原因**：
1. KVM 不可用，QEMU 使用 TCG 软件模拟，启动很慢。
   - **解决**：增加超时：`bash scripts/smoke-boot.sh -t 60`
2. QEMU 数据目录缺失（BIOS 文件）。
   - **解决**：重新运行 `bootstrap-toolchain.sh`。
3. ISO 构建失败。
   - **解决**：检查 `fixtures/refkernel/build/build.log`。

### `doctor.sh` 报告 MISSING

**症状**：某个工具显示 `MISSING`。

**解决**：重新运行 `bootstrap-toolchain.sh`，它会跳过已安装的工具并补装缺失的。

### OOM（内存不足）

**症状**：构建过程中进程被杀，`dmesg | grep -i oom-kill` 显示 OOM 事件。

**解决**：
```bash
# 创建 4GB swap
sudo fallocate -l 4G /swapfile
sudo chmod 600 /swapfile
sudo mkswap /swapfile
sudo swapon /swapfile

# 降低 swappiness（优先回收 page cache）
echo 'vm.swappiness=20' | sudo tee /etc/sysctl.d/99-princesside-swap.conf
sudo sysctl -p /etc/sysctl.d/99-princesside-swap.conf
```

### `princess-gdb` 不可用

**症状**：`princess-gdb: command not found` 或 DAP 测试失败。

**原因**：GDB 16.3 需要从 Debian trixie 镜像下载，网络问题可能导致安装失败。

**解决**：
```bash
# 使用 USTC 镜像重试
PRINCESSIDE_GDB16_MIRROR=https://mirrors.ustc.edu.cn/debian \
bash scripts/bootstrap-toolchain.sh
```

> **注意**：GDB 16.3 仅用于 DAP 调试功能。如果不使用调试器，可以忽略此问题。

### 磁盘空间不足

**症状**：安装过程中报错 "No space left on device"。

**解决**：`.toolchain/` 目录约 2-3 GB。确保至少有 5 GB 可用空间：
```bash
du -sh .toolchain/
df -h .
```

---

## 获取帮助

- 查看 `bash scripts/doctor.sh` 诊断工具链状态
- 查看 `docs/spec/` 了解设计决策
- 查看 `docs/reports/` 了解各阶段验收报告
