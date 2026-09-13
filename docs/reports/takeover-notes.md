# 接手须知（单人维护版）

> 面向：把仓库带回自己机器、**自己改**的场景。
> 生成时间：2026-09-13，对应 `master` 上的 **75 个提交**（门全绿的那批）。
> 载体：`PrincessIDE.bundle`（`git clone` 即可，历史完整）。**bundle 只含提交，不含任何未提交的在制品。**

---

## 一、现在到底能用了吗：三阶段

| 阶段 | 预估 | 说明 |
|---|---|---|
| **A. 第一次跑起来** | **半天～1 天** | 功能骨架齐、门全绿、前端 124 测试 + 外壳 44 测试撑着；**但界面从未在任何真机启动过**（开发机无显示器）→ 首次联调会有依赖/权限类问题（见 §四） |
| **B. 能做内核开发** | **再 ~1 天** | Open→Build→Run→串口/故障符号化这条链**引擎侧已无头验过**（`smoke-boot`、P4 36/36、`symbolicate` → `kernel.c:100`）；GUI 里走一遍、修几个小问题 |
| **C. 用起来顺手** | **自己写几天** | P5 可视化前端（hex/反汇编/页表）与 P7 AI 面板**后端已完成、契约 0 命令、前端 0 行**；调试面板也还只有最小子集 |

**与"能用"无关（别被拖住）**：M10 bisect 编排器 / M11 插件层 / M12 原生前端 / M13 JDWP —— 全是可选或长期项。

---

## 二、第一天怎么走（逐步命令）

```bash
git clone PrincessIDE.bundle PrincessIDE && cd PrincessIDE

# 1) 系统依赖（GUI 那节见 INSTALL.md §方式一）
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev \
  libjavascriptcoregtk-4.1-dev libsoup-3.0-dev librsvg2-dev \
  pkg-config build-essential file wget curl

# 2) 工具链（幂等；QEMU/clangd-16/bear/gdb 16.3/nasm…都装进 .toolchain/）
bash scripts/bootstrap-toolchain.sh
source scripts/env.sh && bash scripts/doctor.sh     # 期望 exit 0

# 3) 前端依赖 + 快速门
pnpm install
bash scripts/ci-gate.sh --fast                      # 期望 pass=4 fail=0（+ 边界门）

# 4) 启动（这是第一次真机运行，重点观察这一步）
pnpm -C apps/desktop tauri dev
```

成功标志：窗口打开 → 顶栏有 **Open Project / 🔨 Build / ▶ Run** → 选 `templates/x86_64-multiboot2/` 或自建工程 → Build 后事件流出现 `build.started → build.finished` → Run 后串口区出现内核输出。

---

## 三、门在哪（改完必须跑）

| 门 | 命令 | 期望 |
|---|---|---|
| 全量门（7 步） | `bash scripts/ci-gate.sh` | `pass=7 fail=0` |
| 快速门（跳重型构建） | `bash scripts/ci-gate.sh --fast` | `pass=4 fail=0` |
| 契约三方一致 | `node apps/desktop/scripts/check-contract.mjs` | `result: ALIGNED` |
| 引擎 | `cargo test --workspace` | 全绿（当前 ~600+ 测试） |
| 外壳 | `cd apps/desktop/src-tauri && cargo test` | 全绿（含真跑 `doctor.sh` 的 e2e） |
| 边界门（D33） | `python3 scripts/check-boundaries.py` | exit 0 |
| 内核链路 | `bash scripts/smoke-boot.sh` / `bash scripts/p4-acceptance.sh` / `bash scripts/symbolicate.sh` | 见 `docs/spec/20-acceptance.md` |

**边界门**（`docs/spec/ownership.toml` + `scripts/check-boundaries.py`）会检查：workspace 成员关系、
`.toolchain/**` 不得改名（D28.3）、以及（给 Agent 用时）模块所有权。**改动治理文件只允许你自己来。**

---

## 四、已知坑清单（每条都真实发生过）

1. **QEMU 数据目录**：`env.sh` 无条件导出 `PRINCESSIDE_QEMU_DATA=.toolchain/prefix/usr/share/qemu`，而 **`doctor.sh` 目前不检查它**。
   该目录缺失时：`smoke-boot.sh` / `templates/*/run.sh` / P4 验收全部失败（它们会 fail loudly）。
   引擎侧已修（`Toolchain::discover` 会丢弃非目录的 hint，不再给 QEMU 传 `-L <不存在>`，见 `bdbc112`）。
   **修复入口**：`scripts/bootstrap-toolchain.sh` 的 QEMU 段（约 380–400 行）；`doctor` 的检查段见
   `docs/dispatch/mimo-m1-task.md` 第一项。
2. **无显示器**：`pnpm tauri dev` 需要 X11/Wayland；开发机没有，所以界面**从未启动过**。
3. **capabilities**：新增 Tauri 插件权限必须同时改 `apps/desktop/src-tauri/capabilities/default.json`
   （dialog 那次就是漏了会运行期被拒）。
4. **事件桥**：前端**只在 `isTauri()` 时**订阅 `princess:event`（`apps/desktop/src/main.ts` → `state/liveStream.ts`）。
   在浏览器里预览时只会显示夹具重放 —— **别误判成"事件流坏了"**。
5. **契约三处原子迁移**：改命令必须同时改 `docs/spec/10-contracts.md` §3 + `apps/desktop/src/contract/ipc.ts` +
   `apps/desktop/src-tauri/src/contract.rs`（+ `commands.rs` 路由），否则契约门红。
6. **`.toolchain/**` 不许改名/替换**（D28.3）：负样本只许用**临时 `PATH`（stub 放 `/tmp`）**、**环境变量**、
   **`/tmp` 或 `.scratch/` 隔离副本**。曾有 Agent 用 `mv … .hidden` 且死在半路，把工具链搞坏。
7. **独立 workspace 必须登记且要有门覆盖**：`apps/desktop/src-tauri` ✅ 已在门里；**`apps/native` 尚未接入 `ci-gate`**
   （已在 `ownership.toml` 里显式登记为已知缺口，不是静默漏掉）。
8. **`princess-core` 是冻结 API**：扩展它要走"四件套"（改契约/注释 + 引用决策号 + 加验收 + 不改既有语义）。

---

## 五、代码地图（想改哪里看哪里）

| 想改 | 看 |
|---|---|
| 引擎行为 | `crates/princess-{core,build,run,symbol,bin,debug,ai,cli,view,lang}/` |
| 命令/事件契约 | `docs/spec/10-contracts.md` + `apps/desktop/src/contract/` |
| 界面 | `apps/desktop/src/`（`views/registry.ts` 是**视图注册表**，新面板自注册即被挂载）+ `apps/desktop/src-tauri/src/` |
| 决策依据 | `docs/spec/00-decisions.md`（**D1–D33**，动手前必读） |
| 模块与所有权 | `docs/spec/30-modules.md`（散文）+ `docs/spec/ownership.toml`（数据） |
| 待修清单 | `docs/reports/fixlist.json`（10 项，已全部处理）＋ `docs/reports/module-agents.md`（台账） |
| 要派 Agent 时 | `docs/dispatch/`（六份任务书 + 派发口径） |

---

## 六、单人维护的最小流程

1. 改前读 `docs/spec/00-decisions.md` 里与该模块相关的 D 条目（尤其 D14 验收纪律 / D28.3 负样本禁令 / D33 边界即门）。
2. 改完立刻 `bash scripts/ci-gate.sh --fast`；收尾跑全量 `bash scripts/ci-gate.sh`。
3. 每个新功能配一个**负样本**（坏输入/坏环境必须被明确拒绝），判据是**退出码或明确错误**。
4. 若将来还想派 Agent：任务书的所有权段用
   `python3 scripts/check-boundaries.py --print-ownership <模块>` **生成**，派发前 `--record-baseline <模块>` 记基线，
   复核时 `--agent <模块> --baseline …`。**一次只派一路**（"能并行"≠"该并行"）。

---

## 七、诚实标注：尚未验证的东西

- **界面从未启动过**（无显示器）：按钮、面板、CodeMirror、LSP 交互、dialog 原生选择器**全部只有 DOM/单测覆盖**。
- **jdtls 在编辑器里的补全/跳转/诊断**未验；`doctor` 对 jdtls 只断言"binary present"，**不验版本**（D6/D18 的教训）。
- **Java 夹具端到端**（javac 编译 + JVM 运行 + `build.diagnostic`）由子 Agent 报告 + 单元测试覆盖，**我未亲跑**。
- **P4 调试面板**只做了最小子集（断点/寄存器/栈回溯），内存与反汇编视图未做。
- **打包**：`scripts/package-portable.sh` 未端到端跑过（<15MiB 预算门写进脚本了）。
