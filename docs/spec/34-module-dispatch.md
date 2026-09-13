# M13+ 模块化派发手册（按模块派 Agent 的固定口径）

> 依据：`docs/spec/30-modules.md`（模块表与边界规则）、`docs/reports/module-agents.md`（台账）、
> `docs/spec/00-decisions.md`（D14 验收纪律 / D15 单目录所有权 / D22 内存纪律 / **D28.1 可见性** / **D28.3 负样本禁令** / D31）。
> 用途：**一个模块 = 一张任务书 = 一个可见的 Agent**，让"哪个模块谁在负责"在 GUI 里一眼可辨。

---

## 一、命名与可见性约定

- **Agent 名（`subagent` 的 `description`）= 模块 id + 切片名**，例如 `[M12·NativeUI] 原生骨架`。
- **任务书首行同样带 `[Mxx·Name]`**：GUI 里每行显示的是**会话标题**（由首条 prompt 自动生成），带前缀你就能对上模块。
- **派发位置**：**用户的新会话**用 `subagent(provider="xiaomi-token-plan-cn", model="mimo-v2.5-pro", run_in_background=true)`
  —— 这是唯一"**可见 + 不阻塞主 Agent + 可直选 Mimo**"的组合（D27.2：模型选择只对新会话生效；D28.1：headless 不可见）。
  回退顺序：`workflow`（可见但前台阻塞）→ headless（不可见）。

## 二、任务书固定六段（缺一段即返工）

1. **模块与职责**：引用 `30-modules.md` 里该模块那一行（模块 id、职责、所有权、门）。
2. **所有权白名单 / 黑名单**：写清"允许写哪些路径""禁止触碰哪些"，并声明**跨模块改动需主 Agent 授权**（D15）。
3. **门**：该模块**现在就能跑、退出码即结论**的命令（从模块表那一行抄）。
4. **负样本必做**：至少一条"坏输入/坏环境必须被明确拒绝"的证据，**判据是退出码或明确错误，不是"看着像成功"**。
5. **D28.3 禁令（原文照抄）**：**绝对禁止**改名 / 替换 / 移动工具链与环境的**真文件**来构造负样本；只许
   **临时 `PATH`（stub 放 `/tmp`）**、**环境变量**、**`/tmp` 或 `.scratch/` 里的隔离副本**三种手法。
6. **交付物**：`.scratch/mimo-<切片>/REPORT.md`（中文、简洁）：改动摘要（文件+行号）、**每条门命令的原始输出与退出码**、
   负样本证据、**诚实列出未验证项**、**逐步 STATUS**（每步 done/blocked，便于中途接手）、末尾总 `STATUS:`。

## 三、硬规则（每条都有血泪）

| 规则 | 出处 | 为什么 |
|---|---|---|
| 单目录所有权；并行两路**文件不得交叉** | D15 | 并发改同一文件必然互相覆盖 |
| **改契约必须三处原子迁移**：`docs/spec/10-contracts.md` §3 + `apps/desktop/src/contract/ipc.ts` + `apps/desktop/src-tauri/src/contract.rs`（+ 必要时 `commands.rs` 路由） | P-E0b 的教训 | `check-contract.mjs` 三方比对，缺一处即红 |
| **新增 crate 会改根 `Cargo.toml`/`Cargo.lock`** → 同一时间只允许**一路**做这件事 | 本文档 | 两路同时加成员必冲突 |
| **`princess-core` 是冻结 API**：扩展必须 ①改契约/注释 ②引用决策编号 ③加验收 ④不改既有语义 | D23/D31 | 8 个 crate 依赖它 |
| 并发上限 **2 路重型构建**，`CARGO_BUILD_JOBS=2` | D22 | 4G swap 兜底但不提速 |
| Agent **禁 git**；提交由主 Agent 做 | D15 | 避免并发踩索引锁 |
| 每路结束主 Agent **亲跑门**，不采信自述 | D14 | 本轮已两次靠这抓到真缺陷 |

## 四、冲突矩阵（派发前必须查这张表）

| 共享资源 | 谁在用 | 结论 |
|---|---|---|
| `docs/spec/10-contracts.md` §3 + `contract/ipc.ts` + `src-tauri/contract.rs` | M5+M6(P-D)、M10(接线)、M11(UI)、M13(JDWP) | **串行队列**，一次只允许一路 |
| `crates/princess-core/**`（冻结枚举） | M13(JDWP 需 `DebugBackendKind::Jdwp`)、M10/M11（若需新配置字段） | 同上，串行 |
| 根 `Cargo.toml` / `Cargo.lock` | 任何**新建 crate** 的模块 | 由主 Agent **预先建好骨架**，避免两路争抢 |
| `apps/desktop/src/**`（前端） | M5+M6(视图)、M11(加载器) | 串行（或明确划分子目录） |
| `apps/desktop/src-tauri/**`（外壳） | M13(JDWP 路由)、M5+M6(新命令路由) | 串行 |
| `scripts/doctor.sh` / `env.sh` | M1 | 独占；**别人不许碰** |
| `apps/native/**`（独立 workspace） | M12 | 独占；自带 `Cargo.lock`，不碰根工作区 |
| `fixtures/**` | 各模块自己的夹具子目录（如 `fixtures/javaproj/`） | 只在自己的子目录里加 |

## 五、波次计划（见 `docs/reports/module-agents.md` 的实时状态）

- **W1（并行 2 路）**：`[M12·NativeUI]`（最重、独立 workspace）+ `[M1·Toolchain]`（最轻、独占 scripts）——两者零交叉。
- **W2（并行 2 路）**：`[M10·Bisect]` + `[M11·Plugins]`——**前提是不加 IPC 命令**（只做引擎侧 + 独立验收），
  且两个新 crate 骨架由主 Agent 预先提交，两路各自只写自己的目录。
- **W3（串行队列）**：`[M5+M6·契约面]` → `[M13·JDWP]` → `[M10·bisect 契约接线]`，逐个落地（每个都必须等到前一个**提交**后再发）。

## 六、每路的"门"必须是模块表里的那条（示例）

| 模块 | 门 |
|---|---|
| M1 | `bash scripts/doctor.sh`（exit 0）+ 缺失负样本（非零） |
| M5+M6 | `cargo test -p princess-symbol -p princess-bin -p princess-ai` + `check-contract.mjs` ALIGNED + 前端 vitest |
| M10 | 新增：bisect 端到端脚本（真仓库上二分到已知提交）+ "无 git/无提交"负样本 |
| M11 | 新增：manifest 校验 + 越权/畸形 manifest 必须明确拒绝 |
| M12 | 视图模型同构断言 + 离屏渲染哈希（lavapipe，无显示器可跑）+ 实测数字 |
| M13 | `cargo test -p princess-debug` + JDWP 真会话验收（独立于 `p4-acceptance.sh`） |
