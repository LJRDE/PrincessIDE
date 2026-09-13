# [M12·NativeUI] 第二前端：原生骨架 + 自研渲染核心（只读切片）

> 通用口径见 **`docs/spec/34-module-dispatch.md`**；本模块的需求与工作单见 **`docs/spec/31-native-frontend.md`** 与 **D30**。

## 1. 模块与职责

M12 = 与 Web+JS **并列可切换**的原生自绘前端（**不是"加速某些视图"**）。
用户已拍板：**自研渲染核心 + 字形图集**；窗口/输入用 `winit`；文本整形用现成库；**X11 + Wayland 都要**；
第一版视图＝**事件流（虚拟化滚动）/ 只读代码（高亮+行号）/ hex·反汇编**；**不含编辑、不含 IME、不含 LSP**（那是 P-E2 与后续）。

**底层选型（⚠️ 用户尚未裁决 wgpu vs ash）**：本轮**按 wgpu（Vulkan 后端）实现**，但必须把渲染核心写成
**只依赖一层薄 GPU 抽象**（创建 surface / 上传图集 / 提交绘制），以便后续下沉重写为 `ash` 调用。**不得**把 wgpu 类型泄漏进视图层。
在报告里明确写出"下沉到 ash 需要替换哪些函数（清单）"。

## 2. 所有权

- **允许写**：`apps/native/**`（**新建，独立 cargo workspace**，自带 `Cargo.lock`）、`.scratch/mimo-m12/`
- **禁止触碰**：根 `Cargo.toml`/`Cargo.lock`（**不要**把 `apps/native` 加进根工作区）、`crates/**`、`apps/desktop/**`、`scripts/**`、`docs/**`、`fixtures/**`
- **禁止 git 命令**

## 3. 只读依赖（已就绪，直接用）

- `docs/spec/32-view-models.md` + **`fixtures/view/*.json`**（黄金视图模型：`eventlog`/`source`/`hex`/`disassembly`）
  —— **本轮直接吃这些夹具**，不必等 IPC 接线（`view:` 契约域由 P-E0b 后续迁移）。
- `crates/princess-view`（只读：类型定义参考；**不要**直接依赖其内部，**只依赖 JSON 契约**）。
- **D20 硬要求**：反汇编**每行自带源码行**（`{addr, bytes, mnemonic, sourceFile, sourceLine}`）——并排关系已在模型里，**不要**在渲染层自己去查。

## 4. 门（亲自跑，贴原始输出与退出码）

```bash
cd /root/PrincessIDE/apps/native
CARGO_BUILD_JOBS=2 cargo build; echo "build_exit=$?"
CARGO_BUILD_JOBS=2 cargo test; echo "test_exit=$?"
# 离屏渲染（无显示器可跑，走 lavapipe 软件 Vulkan）：渲染 → 图像哈希与黄金值比对
CARGO_BUILD_JOBS=2 cargo test offscreen_render_hash; echo "offscreen_exit=$?"
# 同构断言：同一份 fixtures/view/*.json 解析出的内部模型必须与夹具逐字一致（序列化回 JSON 再比）
CARGO_BUILD_JOBS=2 cargo test view_model_matches_golden; echo "golden_exit=$?"
```

**必须产出实测数字**（写进报告）：冷启动时延、按键→上屏延迟（可无头测量输入事件到帧提交的时间）、
10 万行事件流的滚动帧时、LOC。**本机无硬件 GPU**（`lspci` = QEMU 标准 VGA，Vulkan 只有 lavapipe 软件光栅）
→ **不要**宣称"变快了"，只报可复现的数字，并标注是软件光栅下的数字（D31.2）。

## 5. 负样本（必做）

- 坏视图模型输入（空 `rows`、`totalRows` 与 `rows` 不一致、`addr` 不连续）→ **明确报错或安全降级**，不许 panic、不许编造数据。
- **无 GPU/无 Vulkan 环境** → 必须给出明确错误与提示（而不是崩溃或黑屏）。

**D28.3（照抄，违反即返工）**：**绝对禁止**改名 / 替换 / 移动工具链与环境的**真文件**来构造负样本；
只许**临时 `PATH`（stub 放 `/tmp`）**、**环境变量**、**`/tmp` 或 `.scratch/` 隔离副本**。

## 6. 交付物

`.scratch/mimo-m12/REPORT.md`（中文、简洁）：改动摘要（文件+LOC）、**实测数字表**、每条门命令的
**原始输出与退出码**、负样本证据、**"下沉 ash 需要替换的函数清单"**、
**诚实列出未验证项**（X11/Wayland 真机窗口、输入法、目视外观 —— 本机无显示器，只能用户验）、**逐步 STATUS**、末尾总 `STATUS:`。

## 不做

编辑/IME/LSP/插件面板/AI 面板；不改 Web 前端；不动根工作区；不做"替换某几个视图"的加速器方案（D30 已否决该定位）。
