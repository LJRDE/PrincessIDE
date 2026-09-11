# P5 交付报告：`princess-bin`（二进制与底层可视化后端）

> 日期：2026-09-11
> 状态：**代码完成、109/109 测试通过**（Mimo Agent 被配额中断前完成代码，未写报告；主 Agent 基于测试结果与代码审查撰写）

## 1. 验收结果

| 项 | 证据 |
|---|---|
| `cargo test -p princess-bin` | **109 passed; 0 failed**（首次运行即全绿） |
| D20 选型合规 | 代码使用 `object` 0.40.0、`gimli` 0.34.0、`addr2line` 0.27.1、`iced-x86` 1.21.0 |
| P5-1 ELF 解析 | 测试覆盖：段节、符号、入口点、golden 对比 vs `readelf`/`objdump` |
| P5-2 反汇编 | 测试覆盖：`iced-x86` 输出与 `objdump -d` 逐指令比对 |
| P5-3 hex 读取 | 测试覆盖：边界、跨页、`pread`+LRU 缓存命中率 |
| P5-4 monitor 解析 | 测试覆盖：`info-registers/tlb/mem/cpus` 录制样本解析、`P`=PS 非 present、QMP JSON |
| P5-5 页表走表 | 测试覆盖：CR3→PML4→PDPT→PD→PT 四级走表、2MB/1GB 大页、`#PF` 解码、`info-tlb` 1021 行 |
| GDT/IDT | 测试覆盖：从 `info-registers` 的 `GDT=`/`IDT=` 取址后解码描述符 |

## 2. 关键设计（已实现）

- **`pread` + LRU 策略**：hex 读取不使用 mmap（D20 实测：8GB 镜像 mmap 随机访问 RSS +1.12GB；`pread`+LRU(256×4KB) 只 +104KB 且更快）
- **从 CR3 走页表**：不依赖 `info mem`/`info tlb`（D20 实测：LA57 下 info mem 返回空是 QEMU 上游 bug）
- **GDT/IDT 自解码**：QEMU 7.2 无 `info gdt`/`info idt`（D20 实测）
- **`info-tlb` 第 3 列 `P` = PS（大页）**：D20 用 6 个构造页表项 6/6 验证
- **页表 A/D 位是运行时回写的**：不缓存为静态快照

## 3. 未验证项（需在用户机器上跑）

| 项 | 原因 |
|---|---|
| P5-1/2 golden 对比 | 需要 `readelf`/`objdump` 实际运行 |
| P5-3 大文件压力测试 | 需要真实大镜像 |
| P5-4 录制样本解析 e2e | 测试已覆盖，但未跑过完整的端到端流程 |

## 4. 未产出

- `docs/reports/p5-bin.md`：本次由主 Agent 代写（原 Agent 被 Mimo 配额中断）
