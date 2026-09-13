# [M10·Bisect] 编排器：`git bisect run` 接"构建 + QEMU + 断言"

> 通用口径见 **`docs/spec/34-module-dispatch.md`**；模块定义见 `30-modules.md` 的 **M10**（原"版本控制模块"改形态）。

## 1. 模块与职责

M10 = **编排器**，不是 VCS 实现。做法（D29.2 ⑥）：
- `git` 只走 **CLI 薄封装**（像现在封装 QEMU/clangd/gdb 那样），**永不触碰 packfile / 索引格式**；
- 真正的价值在编排：把引擎已有的"**构建 + 启动 QEMU + 断言**"接到 `git bisect run`，
  回答内核开发者的真问题：**"哪个提交让内核启动失败了"**。

**本轮不加 IPC 命令、不碰前端**（契约接线排在 W3 串行队列，由主 Agent 另派）。本轮的验收全部在引擎侧 + 脚本层。

## 2. 所有权

- **允许写**：`crates/princess-bisect/**`（**骨架已由主 Agent 建好并加入工作区，你只写这个目录**）、
  `scripts/bisect-demo.sh`（新建，可选：端到端演示）、`.scratch/mimo-m10/`
- **禁止新增任何依赖**（改 `[workspace.dependencies]` 就等于改根 `Cargo.toml`，会与并行那一路争抢；测试请照 `crates/princess-cli` 的既有做法用 std 自写 `temp_dir(tag)` helper，**不要**用 `tempfile`）
- **禁止触碰**：根 `Cargo.toml`/`Cargo.lock`（骨架已含，**不要改**）、其它 `crates/**`、`apps/**`、`docs/**`、`fixtures/**`、其它 `scripts/**`
- **⚠️ git 命令的特别规定（本模块唯一例外，必须严格遵守）**：
  - **允许**在**你自己创建的 `/tmp` 临时克隆**里执行任意 git 命令（bisect 需要我们生成提交历史）；
  - **绝对禁止**在 `/root/PrincessIDE` 里执行任何 git 命令（`bisect` 会移动 HEAD、切走工作树 → 会毁掉正在开发的仓库）。
  - 演练必须在 `/tmp`：`git clone /root/PrincessIDE /tmp/pp-bisect-demo`（**clone 后立刻在那个副本里验证 `git status` 是干净的**）。

## 3. 要做的

1. **git CLI 薄封装**（`princess-bisect`）：只封装需要的子命令（如 `rev-parse`/`log`/`bisect start|good|bad|run|reset`/`status`），
   **每个调用都带明确的 cwd**，并把 stderr 原样保留到错误里（不许吞）。
2. **判据注入**：`bisect run` 需要一个"判定脚本/命令"——设计一个**可注入**的接口（例如 `Predicate` trait 或一条命令模板），
   本轮用"跑一次构建 + 启动 QEMU + 断言横幅/退出归因"作为默认判据；**复用既有引擎能力**（`princess-build` / `princess-run`），不要重写。
3. **零副作用保证**：bisect 必须在**副本**里跑（或至少不移动调用方仓库的 HEAD）；
   失败/中断时必须能把副本恢复到干净状态（`bisect reset` 的封装 + 超时保护）。
4. **结果结构化**：输出"第一个坏提交"的 `sha` / `subject` / 判定命令的真实输出片段。

## 4. 门（亲自跑，贴原始输出与退出码）

```bash
cd /root/PrincessIDE
CARGO_BUILD_JOBS=2 cargo test -p princess-bisect; echo "unit_exit=$?"
CARGO_BUILD_JOBS=2 cargo build -p princess-bisect; echo "build_exit=$?"
# 端到端（在 /tmp 副本里，制造 5 个提交、第 3 个引入失败，bisect 必须精确指到它）
bash scripts/bisect-demo.sh; echo "demo_exit=$?"     # 或你报告里贴出的等价命令
```

## 5. 负样本（必做，三条）

1. **非 git 目录**（例如 `/tmp/pp-not-a-repo`）→ 明确错误（不许 panic、不许"假装成功"）。
2. **没有提交历史**（`git init` 后空仓库）→ 明确错误。
3. **判据命令本身非零/超时** → 必须如实归因（区分"guest 启动失败"与"判据坏了"，别把工具故障算成 bad commit）。

**D28.3（照抄，违反即返工）**：**绝对禁止**改名 / 替换 / 移动工具链与环境的**真文件**构造负样本；
只许**临时 `PATH`（stub 放 `/tmp`）**、**环境变量**、**`/tmp` 或 `.scratch/` 隔离副本**。

## 6. 交付物

`.scratch/mimo-m10/REPORT.md`（中文、简洁）：改动摘要（文件+行号/LOC）、**端到端 bisect 的原始输出**（必须能看到"第一个坏提交 = 第 3 个"）、
门命令的原始输出与退出码、三条负样本证据、**诚实列出未验证项**（未在真内核仓库上跑过、SMP/多分支等）、**逐步 STATUS**、末尾总 `STATUS:`。

## 不做

不实现 VCS 任何底层格式；不加 IPC 命令；不碰前端；不在 `/root/PrincessIDE` 里跑 git。
