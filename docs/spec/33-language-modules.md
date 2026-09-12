# PrincessIDE 语言模块层（M13）需求与工作单 v0.1

> 依据：`docs/spec/00-decisions.md` **D31**；需求由用户于 2026-09-12 拍板。
> 关联：`30-modules.md`（模块表）、`10-contracts.md`（M0 契约）、`31-native-frontend.md`（M12）。
> 状态：**需求已锁定，待新会话派发**。

## 一、优化后的目标（用户原话"添加对 JAVA 的开发支持为新模块"的升档表述）

> **把"语言支持"做成可插拔的一层（M13），Java 是它的第一个实例。**

理由（与既有资产咬合）：

1. **一次性扩张 vs 可复用能力**：硬编码 Java，下一门语言（Kotlin/Rust/C++/Zig —— D4 已留"接口与路线图"）就是又一次全套集成；做成缝，每门语言 = **一个 manifest + 一组适配器 + 一条门**。
2. **它给 M11（声明式插件 + 能力模型）第一个真客户**，否则插件架构一直是纸面设计。
3. **保住产品身份**：内核路径仍是主路径（D1/D4 冻结），语言是**可选模块**，不是产品分叉。

**可验收的强化版目标**：*"一门语言 = 一个 manifest + 一组适配器 + 一条门"*，并用 Java 端到端证明这条缝成立。

## 二、已锁定的需求（用户拍板）

| 项 | 结论 |
|---|---|
| 定位 | **语言模块化（M13），Java 为第一个实例** |
| 第一版能力 | **编辑 + LSP（jdtls）**、**构建（javac 直编）**、**运行（JVM）** |
| 调试 | **本轮不做**（JDWP 是第二个调试协议，单列 P-F2） |
| 构建工具 | 第一版只用 **`javac` 直编**（零下载；Maven/Gradle 另议，D19 网络差） |
| Java 工具链 | 用系统 JDK（实测 **openjdk/javac 17.0.20.1**），doctor 断言 **≥17**；**`jdtls` 装进 `.toolchain/` 并纳入 doctor 断言**（照 D6/A1 对 clangd-16 的做法，否则必重演 BUG-004/005/006） |
| Java 权威夹具 | 新增 `fixtures/javaproj/`，与内核夹具（`fixtures/refkernel/`）并列；**它自己的固定输出就是断言基础**（D3 的同款纪律） |

## 三、必须点明的技术事实（决定工作量，不影响定位）

1. **Java 项目不在 QEMU 里跑** → 现有引擎的大半（GRUB/Multiboot2、串口、`#PF` 符号化）对它**不适用**。Java 模块要的是：**第二个构建后端（javac）＋ 第一个非 clangd 语言服务器（jdtls）＋（后续）第二个调试后端（JDWP）**。
2. **本项目验证文化是夹具驱动**（D3：`refkernel` 是唯一权威夹具、常量不可改）。所以 Java 模块**必须自带权威夹具与验收**，否则必然腐烂（BUG-009 就是"没有门"的产物）。
3. **jdtls 不在本机**（实测无 `jdtls`/`mvn`/`gradle`）→ 这是**M1 工具链模块的工作**，不是"顺手装个包"：要进 `.toolchain/`、要 doctor 断言、要有缺失负样本。
4. `princess.toml` 已有 `language` 字段（D4）→ Java 模块大概率只是给它加一个合法值 + 对应后端；**若动 §4 配置契约，按 P-E0b 的同款纪律做"spec + TS + Rust 三方原子迁移"**。

## 四、分期与门

### P-F0 语言模块层本体（**先证明缝能容纳现状**，不引入新语言）
- **写**：`crates/princess-lang/`（或等价命名）——语言模块 manifest 格式 + 适配器接口（`lsp` / `build` / `run` / `debug`(留空) ）+ 发现与注册 + 加载校验。
- **关键验收**：**把现有的 C 路径改造成"语言模块的一份 manifest"**，行为不变、所有既有门（`cargo test --workspace`、`build` 验收、`p4-acceptance`、`smoke-boot`）继续绿。**缝不能容纳现状就等于纸上设计。**
- **门**：`cargo test` + 既有 `scripts/ci-gate.sh` 全绿 + **负样本**（manifest 缺字段/引用了不存在的适配器 → 必须明确报错，不许静默降级）。

### P-F1 Java 模块第一版（用户选定的三项能力）
1. **工具链**：`jdtls` 装入 `.toolchain/`，`env.sh` 导出（如 `PRINCESSIDE_LANG_SERVICE_JDTLS`），`doctor.sh` 断言存在与版本；缺 jdtls 时 `E_TOOLCHAIN_MISSING` 且带修复建议。
2. **夹具**：`fixtures/javaproj/`（最小 Java 工程 + 固定 stdout）+ 一条断言脚本（照 `templates/verify-template.sh` 的风格）。
3. **构建适配器**：`javac` 直编（产出 class/jar），含编译错误 → `build.diagnostic` 事件（复用既有诊断面板与事件契约）。
4. **运行适配器**：`java -jar` / classpath 启动，stdout/stderr 走既有日志流。
5. **LSP 接入**：jdtls 经既有 `princess:lsp:*` 通道接入（D17：**不重写协议栈**），前端只做编辑器交互。
- **门**：`cargo test` + `ci-gate.sh` 全绿；Java 夹具的构建/运行断言（脚本 exit 0）；**三条负样本**：①编译错误必须出现在诊断里 ②jdtls 缺失必须报 `E_TOOLCHAIN_MISSING` 且带修复命令 ③运行非零退出必须如实归因。

### P-F2（后置）JDWP 调试后端
第二个调试协议，与 GDB DAP 并列；需要独立的验收（不能复用 `p4-acceptance.sh` 的断言）。

## 五、边界规则（新增，与 30-modules §二 并列）

9. **语言是模块，不是分叉**：内核路径（C+asm）保持主路径与参考实现；任何语言模块不得改变内核路径的行为与断言常量（D3/D26）。
10. **语言模块必须自带门**：manifest + 适配器 + **自己的权威夹具与负样本**；没有门的语言模块不许合入。
11. **语言模块不得自带协议栈**：LSP/调试一律走既有通道（D17/D11 同款原则）。

## 六、用户验收清单（自动化覆盖不到）

- [ ] 打开 `fixtures/javaproj/`，编辑器出现 jdtls 诊断/补全/跳转
- [ ] 触发构建 → 事件流出现真实 `build.started → build.finished`
- [ ] 运行 → 日志流出现夹具的固定 stdout
- [ ] 故意写错一行 Java → 诊断面板显示错误（而不是静默失败）
