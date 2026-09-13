# 派发任务书归档（入库副本）

这里是各模块的**派发任务书**（原先只在 `.scratch/main/`，而 `.scratch` 不入库，
所以 git bundle 里会丢失 —— 归档到此处，接手时自包含）。

| 文件 | 模块 | 波次 |
|---|---|---|
| `mimo-m1-task.md` | M1 工具链（doctor 补 QEMU 数据目录 + jdtls 版本断言 + 根因排查） | W1 |
| `mimo-m12-task.md` | M12 第二前端原生骨架 | W1 |
| `mimo-m10-task.md` | M10 bisect 编排器 | W2 |
| `mimo-m11-task.md` | M11 声明式插件（引擎侧） | W2 |
| `mimo-m5m6-task.md` | M5+M6 契约面与前端（P-D） | W3 串行首棒 |
| `mimo-m13-task.md` | M13 JDWP 调试后端 | W3 串行次棒 |
| `dispatch-instructions.md` | 派发口径（自然语言版） | — |

口径与硬规则见 `docs/spec/34-module-dispatch.md`；所有权数据见 `docs/spec/ownership.toml`。
