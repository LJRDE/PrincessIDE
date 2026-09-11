# A9 交付报告：gdb ≥14 提升为一线工具链

> 日期：2026-09-11
> 状态：**能力已验证，报告由主 Agent 基于独立实测证据撰写**（A9 Agent 被 Mimo 配额中断前完成脚本修改，未写报告）

## 1. 完成内容

| 文件 | 改动 |
|---|---|
| `scripts/bootstrap-toolchain.sh` | 新增 gdb-16.3 trixie rootfs 安装（deb 解包 + 依赖闭包）、`princess-gdb` 启动器 |
| `scripts/env.sh` | 新增 `PRINCESSIDE_DEBUG_GDB=princess-gdb`、路径说明 |
| `scripts/doctor.sh` | 新增 `gdb-16.3 (DAP)` 检查（版本 ≥14 断言）+ `gdb` 13.1 检查（保留不变） |

## 2. 独立验证（主 Agent 亲测）

```
$ source scripts/env.sh && princess-gdb --version
GNU gdb (Debian 16.3-1) 16.3

$ <DAP initialize frame> | princess-gdb -q -i=dap
Content-Length: 1406
{"request_seq":1,"type":"response","command":"initialize","success":true,"body":{...}}

$ source scripts/env.sh && scripts/doctor.sh
doctor: all required tools present (30 resolved).   exit=0
```

- `gdb 13.1` 仍保留为默认 `gdb`（零回归）
- `princess-gdb` 走 trixie rootfs 的 ld-linux-x86-64.so.2 启动器（避免 glibc 污染）
- P0 链未被破坏：`scripts/smoke-boot.sh` exit 0

## 3. 关键设计决策

**glibc 隔离**：trixie 的 gdb 16.3 需要 trixie 的 libc6（≥2.38），但宿主是 bookworm（2.36）。将 rootfs 的 libc.so.6 放进 `LD_LIBRARY_PATH` 会污染宿主所有进程（主 Agent 亲自踩过这个坑）。解决方案：`princess-gdb` 启动器使用 rootfs 自己的 `ld-linux-x86-64.so.2 --library-path ...`，不触碰宿主 LD_LIBRARY_PATH。

## 4. 已知限制

- gdb 16.3 在 Debian trixie 上可用，trixie 镜像在部分网络不可达（已将默认镜像改为 USTC）
- KVM 不可用 → QEMU TCG only，调试启动较慢
