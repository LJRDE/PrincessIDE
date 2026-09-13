# [M11·Plugins] 声明式插件：引擎侧 manifest 加载与校验（阶段 0，无 UI）

> 通用口径见 **`docs/spec/34-module-dispatch.md`**；模块定义见 `30-modules.md` 的 **M11**（D29.2 ⑧ 的裁决）。

## 1. 模块与职责

M11 = **插件管理**。D29.2 ⑧ 的裁决要点（照做，别改定位）：
1. **先声明式**：manifest 声明"面板 / 命令 / 模板 / 主题"，**本轮不加载任何原生代码**（原生插件＝任意代码执行，Rust 无稳定 ABI，必须 WASM 或进程 RPC 边界，留到最后一期）；
2. **必须有能力模型**：插件声明它需要什么能力，**越权必须被拒绝**（Tauri v2 capabilities 已有先例，照它的最小授权精神）；
3. **兼容性版本化**：manifest 必须带版本，兼容范围要显式声明（参考投影 `stateVersion` 与事件模型 `v` 的既有做法）。

**本轮范围（阶段 0）＝纯引擎侧**：manifest 的**格式、发现、解析、校验**与**能力模型的数据结构**，
外加**负样本**。**不加 IPC 命令、不碰前端、不加载代码**——UI 与契约接线留到 W3 之后另派。

## 2. 所有权

- **允许写**：`crates/princess-plugins/**`（**骨架已由主 Agent 建好并加入工作区，你只写这个目录**）、
  `fixtures/plugins/**`（新建：合法/畸形/越权的 manifest 样例）、`.scratch/mimo-m11/`
- **禁止新增任何依赖**（改 `[workspace.dependencies]` 就等于改根 `Cargo.toml`，会与并行那一路争抢；测试请照 `crates/princess-cli` 的既有做法用 std 自写 `temp_dir(tag)` helper，**不要**用 `tempfile`）
- **禁止触碰**：根 `Cargo.toml`/`Cargo.lock`（骨架已含，**不要改**）、其它 `crates/**`、`apps/**`、`scripts/**`、
  其它 `docs/**`（`docs/spec/30-modules.md`、`34-module-dispatch.md` 都不许改）、`fixtures/` 下别人的子目录
- **禁止 git 命令**

## 3. 要做的

1. **manifest 格式**（TOML，与 `languages/*.toml`、`princess.toml` 风格一致）：至少
   `id`、`version`、`apiVersion`（兼容范围）、`capabilities`（声明需要的能力）、
   `panels[]`（`id`/`title`/`testid`）、`commands[]`（**只声明**，不实现）、`templates[]`、`theme?`。
2. **发现与解析**：从一个目录（如 `plugins/`）发现全部 manifest；解析失败必须报**具体哪个文件哪一行**。
3. **校验（本轮的重点）**：
   - 必填字段缺失 / 类型错误 / 版本不兼容 / `id` 重复 → **明确错误**；
   - **能力模型**：manifest 声明了**未在允许集合内**的能力（例如 `fs.write`、`exec.native`、`net`、
     原生代码加载 `native`）→ **拒绝**并说明理由；允许集合本身要写在一个**显式、可审计**的位置（常量或配置文件）。
4. **纯数据优先**：本轮不接入任何执行路径；提供只读查询接口（按 id / 按能力过滤），并**给出被拒绝的清单与原因**。

## 4. 门（亲自跑，贴原始输出与退出码）

```bash
cd /root/PrincessIDE
CARGO_BUILD_JOBS=2 cargo test -p princess-plugins; echo "unit_exit=$?"
CARGO_BUILD_JOBS=2 cargo build -p princess-plugins; echo "build_exit=$?"
ls -1 fixtures/plugins/                                   # 合法/畸形/越权样例都在
node apps/desktop/scripts/check-contract.mjs | tail -2; echo "contract_exit=$?"   # 必须仍 ALIGNED（本轮不该变）
```

## 5. 负样本（必做，至少四条）

1. 缺 `id`/`version` → 明确错误（含文件名）；
2. `apiVersion` 不兼容 → 明确错误并说明支持范围；
3. **越权能力**（如 `native`、`fs.write`）→ 拒绝，且错误信息里**指出是哪个能力不被允许**；
4. 重复 `id` → 明确错误（不许静默取最后一个）。

**D28.3（照抄，违反即返工）**：**绝对禁止**改名 / 替换 / 移动工具链与环境的**真文件**构造负样本；
只许**临时 `PATH`（stub 放 `/tmp`）**、**环境变量**、**`/tmp` 或 `.scratch/` 隔离副本**。

## 6. 交付物

`.scratch/mimo-m11/REPORT.md`（中文、简洁）：manifest 字段表、**允许能力集合的定义位置与理由**、
改动摘要（文件+行号/LOC）、门命令的原始输出与退出码、四条负样本证据、
**诚实列出未验证项**（未接 UI、未加载任何插件代码、未做签名/来源校验等）、**逐步 STATUS**、末尾总 `STATUS:`。

## 不做

不加载原生代码；不加 IPC 命令；不碰前端；不动 Tauri capabilities；不做插件市场/下载。
