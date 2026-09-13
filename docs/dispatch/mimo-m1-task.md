# [M1·Toolchain] 工具链模块：补两个缺口 + 查清一个环境根因

> 通用口径（命名、六段模板、硬规则、冲突矩阵）见 **`docs/spec/34-module-dispatch.md`**，动手前先读它。
> 你只负责 **M1**（`docs/spec/30-modules.md` 的 M1 行：探测 / 安装 / 版本断言）。

## 1. 模块与职责

M1 = **工具链与环境**：clangd-16 / bear / gdb≥14 / QEMU / nasm / jdtls 的**探测、版本断言与修复提示**。
它是本项目**最大的支持痛点**：BUG-004/005/006 全出在这里，本轮又因此暴露两个缺口。

## 2. 所有权

- **允许写**：`scripts/doctor.sh`、`scripts/bootstrap-toolchain.sh`（**仅限** QEMU 数据段）、`scripts/env.sh`（**仅限** `PRINCESSIDE_QEMU_DATA` 那几行）、`.scratch/mimo-m1/`
- **禁止触碰**：`crates/**`、`apps/**`、`docs/**`、`templates/**`、`fixtures/**`、其它脚本
- **禁止 git 命令**

## 3. 要做的三件事

### (a) doctor 增加 **QEMU 数据目录**检查（这是用户实机报障的根因缺口）
背景（已实测）：`env.sh` 无条件导出 `PRINCESSIDE_QEMU_DATA="$PRINCESSIDE_PREFIX/usr/share/qemu"`，而**用户机器上该目录不存在** →
`scripts/smoke-boot.sh`、`templates/*/run.sh`、P4 验收全部跑不了；`doctor.sh` 却只查 `qemu-system-x86_64 --version`，**完全没提这件事**。
这跟 BUG-005（GUI 库漏检）是同一类缺口。要求：

- 检查顺序：`$PRINCESSIDE_PREFIX/usr/share/qemu` → 系统 `/usr/share/qemu`（若存在则也算可用，并**明确打印用的是哪个**）。
- 都不存在 → `MISSING`/`WARN` + **可直接复制的修复命令**（指向 `bash scripts/bootstrap-toolchain.sh`），并按既有 required 机制决定是否置非零退出（**你来判断并说明理由**：没有 QEMU 数据目录时，`smoke-boot`/模板/P4 必然失败，倾向非零）。
- `-L` 只在目录真实存在时才应传给 QEMU —— 这条已在 `bdbc112` 由 Rust 侧修好（`Toolchain::discover` 丢弃非目录 hint），**doctor 只需如实报告**，不要去改 Rust。

### (b) doctor 对 **jdtls 的版本断言**（现在只验"binary present"）
现状：`jdtls (LSP) ok binary present`，另有 `java ≥ 17` 检查。但 D6/D18 的教训是**版本化断言**（对比 `clangd-16 ver ok major=16 (>=16)`）。
- 先确认 jdtls 是否有稳定的版本查询方式（`jdtls --version`？启动器脚本里读 manifest？`.toolchain/jdtls/` 里的版本文件或目录名？）。
- **有** → 加版本断言（并写明最低要求与依据）。
- **没有**（长驻进程、无 `--version`）→ 就**把这件事写成注释与报告里的明确理由**，并给出替代断言（例如断言 jar/目录的版本标识、或至少断言 `java` 版本 + jdtls 启动器可执行）。**不要伪造一个版本号。**

### (c) 查清"用户机器上 qemu 数据目录为何缺失"的**根因**
读 `bootstrap-toolchain.sh`（QEMU 段，约 380-400 行）回答：它**在什么条件下**会把数据文件放进 `$PREFIX/usr/share/qemu`？用户跑了 bootstrap 却仍然没有该目录，最可能的原因是什么（缺包？跳过分支？解包路径不同？）？
- 若根因在 bootstrap 且可修 → 修（**只动 QEMU 数据段**）并在报告里给出前后对比。
- 若不可修（例如取决于发行版包）→ 在报告里给出**用户可执行的补救步骤**（具体命令）。

## 4. 门（亲自跑，贴原始输出与退出码）

```bash
cd /root/PrincessIDE
bash scripts/doctor.sh; echo "exit=$?"                      # 期望 0，且含 QEMU 数据目录与 jdtls 两行
bash -n scripts/doctor.sh; echo "syntax_exit=$?"
bash -n scripts/bootstrap-toolchain.sh; echo "boot_syntax_exit=$?"   # 若改过
# 负样本（合规手法：/tmp 隔离副本，**不碰真文件**）
rm -rf /tmp/pp-m1 && mkdir -p /tmp/pp-m1 && cp -r scripts /tmp/pp-m1/
bash /tmp/pp-m1/scripts/doctor.sh; echo "isolated_exit=$?"   # 期望非零，且逐项列出缺失 + 修复命令
```

## 5. 负样本（必做）

至少：**隔离副本里 doctor 必须非零退出并列出缺失项与 apt/bootstrap 修复命令**（上面最后一条）。
**D28.3（照抄，违反即返工）**：**绝对禁止**改名 / 替换 / 移动工具链与环境的**真文件**来构造负样本（上一轮有 Agent 用 `mv .toolchain/bin/jdtls → .hidden` 的手法，虽侥幸无残留但属违规）；只许**临时 `PATH`（stub 放 `/tmp`）**、**环境变量**、**`/tmp` 或 `.scratch/` 隔离副本**。

## 6. 交付物

`.scratch/mimo-m1/REPORT.md`（中文、简洁）：改动摘要（文件+行号）、**(c) 的根因结论**、每条门命令的**原始输出与退出码**、负样本证据、**诚实列出未验证项**（例如未在"真的没有 QEMU 数据目录"的机器上跑过）、**逐步 STATUS**、末尾总 `STATUS:`。

## 不做

不改 Rust / 前端 / 契约；不动 `.toolchain` 里的真文件；不新增脚本文件（除必要）；不改 `templates/`。
