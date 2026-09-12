# PrincessIDE — Token 消耗统计

> 统计时间：2026-09-12
> 数据来源：`/root/.dsh/sessions/--root-PrincessIDE--/*/session.jsonl.zstd`
> 权威记录：`type == "assistant/chunk"` 且 `data.chunk.type == "usage"`
> 归属方法：流式跟踪 `request/header.config.{provider,model}`，每条 usage 记到当时的模型
> 生成脚本：`scripts/token-stats.py`（可重跑）

---

## 1. 总览

| 指标 | 数值 |
|---|---|
| 会话数（本项目） | **32** |
| LLM 调用步数 | **3,006** |
| 新鲜 input tokens | **4,572,747** |
| output tokens | **2,521,358** |
| **input + output（全价计费）** | **7,094,105** |
| cache read tokens | 497,830,912 |
| **缓存命中率** | **99.1%** |
| reasoning tokens（含于 output） | 761,174 |

> **为什么 total 显示 5 亿而全价只有 709 万**：DSH 的 `totalTokens` 把 cache read 也计入。
> 本项目的缓存命中率高达 **99.1%**——因为每轮都重放几乎相同的系统提示与项目上下文，
> 这部分按缓存折扣计费，与新鲜 token 不是一个量级。

---

## 2. 按 provider / model

| provider/model | 步数 | input | output | 计费全价 | total（含 cache） |
|---|---|---|---|---|---|
| `deepseek-official/deepseek-v4-flash` | 2,481 | 2,462,256 | 2,207,204 | 4,669,460 | 350,969,108 |
| `xiaomi-token-plan-cn/mimo-v2.5-pro` | 344 | 1,342,837 | 145,326 | 1,488,163 | 125,383,715 |
| `deepseek-official/deepseek-v4-flash-vision-exp` | 28 | 648,053 | 28,862 | 676,915 | 18,367,923 |
| `deepseek-official/deepseek-v4-pro` | 153 | 119,601 | 139,966 | 259,567 | 10,204,271 |
| **合计** | **3,006** | **4,572,747** | **2,521,358** | **7,094,105** | **504,925,017** |

### 说明

- **DeepSeek Flash 占 82.6% 的步数**（2,481/3,006）——主会话 + 多数子 Agent
- **Mimo Pro 占 11.4%**（344 步）——headless 后台 Agent（P3-C 接线、P5/P7/A9 等）
- **vision-exp 仅 28 步**——用于解析你发的 IDE 截图（图像理解走视觉模型）
- **v4-pro 的 153 步全部在 09-11 00:09 之前**，即用户下达「不要使用 Pro」之前
  （当时的选型方案把调研 B/C 两路配了 pro；之后未再使用）

---

## 3. 主 Agent vs 子 Agent

| 类别 | 步数 | input | output | 计费全价 |
|---|---|---|---|---|
| 主 Agent（`session-d0c4164b`，本会话） | 526 | 2,060,016 | 482,966 | 2,542,982 |
| 子 Agent（31 个会话） | 2,480 | 2,512,731 | 2,038,392 | 4,551,123 |

**主/子几乎对半**（36% vs 64% 的新鲜 token）。主会话步数少但单步上下文大
（每轮都带完整项目上下文）；子 Agent 步数多但单会话上下文小。

---

## 4. 消耗最高的 10 个会话

| 会话 | 类型 | 步数 | input | output | total |
|---|---|---|---|---|---|
| `session-d0c4164b`（主会话） | main | 526 | 2,060,016 | 482,966 | 212,955,270 |
| `session-efdd6c9b` | agent | 212 | 157,332 | 169,679 | 41,981,795 |
| `session-e60db6ce` | agent | 256 | 126,422 | 105,885 | 34,336,371 |
| `session-b15604e2` | agent | 134 | 153,485 | 79,455 | 21,207,404 |
| `session-b9ee4442` | agent | 160 | 102,583 | 88,486 | 18,527,709 |
| `6a18e21e`（P2-A 核心引擎） | agent | 108 | 78,624 | 204,718 | 18,015,182 |
| `session-d3ab544a` | agent | 125 | 111,632 | 113,540 | 17,635,092 |
| `session-e215f02c` | agent | 112 | 194,007 | 33,498 | 17,238,065 |
| `2cd8b82f`（调研 D） | agent | 122 | 121,317 | 137,594 | 15,669,983 |
| `1c225fcd`（调研 A 父） | agent | 114 | 127,030 | 141,088 | 15,169,750 |

---

## 5. 统计方法（可复现）

```bash
cd ~/PrincessIDE
python3 scripts/token-stats.py                    # 打印统计表 + 写 JSON
python3 scripts/token-stats.py --workspace <name> # 换工作区
```

输出：控制台表格 + `docs/reports/token-stats.json`

**两个必须注意的坑**（脚本已处理）：

1. **`tool/result` 里会出现 "usage" 字符串**——那是工具输出内容（比如我 grep 出来的文本），
   **不是计费记录**。权威来源只有 `assistant/chunk(chunk.type=usage)`。
2. **模型归属要流式跟踪**——`model/selection` 只在切换时记录，
   所以必须同时解析每个 `request/header` 的 `config.model`，否则会话中途切模型会归属错乱。

---

## 6. 结论与建议

| 观察 | 含义 |
|---|---|
| 缓存命中率 99.1% | 实际全价成本远低于 total 显示的数字；要省钱应**压低每轮增量上下文**（少灌大段输出、少重读大文件） |
| 子 Agent 占 64% 新鲜 token | 派发粒度比主会话更影响总成本；能用 Mimo（配额充足）就别用 DeepSeek |
| 单会话最大消耗是主会话 | 主会话 526 步 × 每步大上下文；长会话要主动压缩自身开销 |
| 3,006 步 / 32 会话 | 平均每会话 94 步；子 Agent 普遍在 50–260 步之间 |
