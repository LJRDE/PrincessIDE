#!/usr/bin/env python3
"""PrincessIDE 边界门 —— 把"约定"变成"断言"。

背景（用户 2026-09-13 的批评，已写进 D33）：
    在这之前，六条边界里**只有契约那条是机器强制的**（`check-contract.mjs`），其余靠文档 + 主 Agent 事后复核。
    后果是真实发生过的：M11 的子 Agent 把 `crates/princess-plugins` 从根 workspace 成员里删掉（该模块就此
    **掉出 CI 门**，正是 BUG-009 那种"没有门的模块"）并加了任务书禁止的依赖；P-F1 的子 Agent 用
    `mv .toolchain/... .hidden` 造负样本（D28.3 明令禁止）。**这些操作没有任何门报警。**

本检查器把三类边界变成非零退出：

    1. workspace 成员   —— `crates/*` 必须在根成员里；`apps/*` 要么是根成员，要么**登记为独立 workspace
                           且必须被某个门覆盖**（"掉出门外"与"有意独立"必须可区分）。
    2. 工具链禁令       —— 变动集里不得出现 `.toolchain/**` 的改名/删除/`.hidden` 残留（D28.3）。
    3. 模块所有权       —— `--agent M11` 时，变动集必须全部落在该模块的 `allow` glob 内；
                           `shared` 路径需主 Agent 显式授权（即出现在 allow 里）；`governance` 路径
                           **任何 Agent 都不许改**（否则门会被被治理者拆掉）。

用法：
    python3 scripts/check-boundaries.py                        # 1+2（全局有效，任何时候都能跑）
    python3 scripts/check-boundaries.py --agent M11             # 1+2+3（**仅单所有者时段**，见下）
    python3 scripts/check-boundaries.py --record-baseline M11   # 派发前记基线（写 .scratch/dispatch/M11.baseline）
    python3 scripts/check-boundaries.py --agent M11 --baseline .scratch/dispatch/M11.baseline
    python3 scripts/check-boundaries.py --files a/b.rs c/d.rs   # 用显式文件集（自测/负样本用，不查 git）
    python3 scripts/check-boundaries.py --print-ownership M11   # 生成器：打印可贴进任务书的所有权段
    python3 scripts/check-boundaries.py --root /tmp/fake-tree   # 换个仓库根（自测用）

⚠️ 所有权检查（第 3 类）的前提：**变动集可归属于该 Agent**。
    多路并行或主 Agent 同时在改治理文件时，整棵树的 `git status` 混着好几个人的改动，
    此时所有权检查必然误报。做法是**派发前记一次基线**（`--record-baseline`），
    复核时用 `--baseline` 把"派发前就已经脏的文件"扣除；不记基线时请只跑 1+2。

退出码：0 = 全部通过；1 = 有违规（逐条打印规则名与文件）。
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
import tomllib

DEFAULT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


# --------------------------------------------------------------------------- helpers
def glob_to_regex(pattern: str) -> re.Pattern[str]:
    """把 glob 编译成**区分路径层级**的正则。

    不能用 `fnmatch`：它的 `*` 会连 `/` 一起吃掉，于是 `crates/*` 会错误地匹配
    `crates/a/src/b.rs`（而我们要的是"只允许该 crate 根目录这一层"）。
    `**` 才表示跨层级。"""
    out = []
    i = 0
    while i < len(pattern):
        ch = pattern[i]
        if ch == "*":
            if pattern.startswith("**", i):
                out.append(".*")
                i += 2
                continue
            out.append("[^/]*")
        elif ch == "?":
            out.append("[^/]")
        else:
            out.append(re.escape(ch))
        i += 1
    return re.compile("^" + "".join(out) + "$")


def matches_any(path: str, patterns: list[str]) -> bool:
    return any(glob_to_regex(p).match(path) for p in patterns)


def git(root: str, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", root, *args], capture_output=True, text=True, check=False
    ).stdout


def changed_files(root: str) -> list[str]:
    """变动集 = 已跟踪的改动 + 未跟踪文件（含 rename 的**新旧两侧**）。

    用 `git status --porcelain` 而不是 `git diff`：未跟踪的新文件（子 Agent 最常产出的东西）
    在 `git diff` 里根本不出现，只看 diff 会漏掉一半。
    """
    lines = git(root, "status", "--porcelain").splitlines()
    paths: list[str] = []
    for line in lines:
        entry = line[3:] if len(line) > 3 else ""
        if " -> " in entry:                      # rename: "old -> new"
            old, new = entry.split(" -> ", 1)
            paths += [old.strip().strip('"'), new.strip().strip('"')]
        elif entry:
            paths.append(entry.strip().strip('"'))
    return sorted(set(paths))


def load_spec(root: str) -> dict:
    with open(os.path.join(root, "docs/spec/ownership.toml"), "rb") as fh:
        return tomllib.load(fh)


# --------------------------------------------------------------------------- checks
def check_workspace_members(root: str, spec: dict) -> list[str]:
    """`crates/*` 必须在根成员里；`apps/*` 要么是根成员，要么登记为**有门覆盖**的独立 workspace。"""
    problems: list[str] = []
    root_manifest = os.path.join(root, "Cargo.toml")
    if not os.path.isfile(root_manifest):
        return problems
    with open(root_manifest, "rb") as fh:
        members = tomllib.load(fh).get("workspace", {}).get("members", [])
    members = [m.rstrip("/") for m in members]
    independent = [p.rstrip("/") for p in spec.get("independent_workspaces", {}).get("paths", [])]
    coverage = spec.get("independent_workspaces", {}).get("coverage", {})

    candidates: list[str] = []
    for parent in ("crates", "apps"):
        base = os.path.join(root, parent)
        if not os.path.isdir(base):
            continue
        for name in sorted(os.listdir(base)):
            manifest = os.path.join(base, name, "Cargo.toml")
            if os.path.isfile(manifest):
                candidates.append(f"{parent}/{name}")

    for rel in candidates:
        manifest = os.path.join(root, rel, "Cargo.toml")
        with open(manifest, "rb") as fh:
            doc = tomllib.load(fh)
        is_workspace_root = "workspace" in doc
        is_member = rel in members
        if is_workspace_root:
            if rel not in independent:
                problems.append(
                    f"[workspace-members] {rel} 是独立 workspace 但未在 docs/spec/ownership.toml 的 "
                    f"independent_workspaces 登记 —— 无法区分「有意独立」与「掉出门外」"
                )
            elif rel not in coverage:
                problems.append(
                    f"[workspace-members] {rel} 登记为独立 workspace，但没有任何门覆盖它"
                    f"（等于「没有门的模块」，参见 BUG-009）"
                )
        elif not is_member and rel not in independent:
            problems.append(
                f"[workspace-members] {rel} 既不是根 workspace 成员、也未登记为独立 workspace\n"
                f"                    → 它**掉出了 `cargo test --workspace`**，没人会发现它的测试变红"
            )
    return problems


def check_toolchain(root: str, files: list[str], agent: str | None) -> list[str]:
    """`.toolchain/**` 不得被 Agent 改动；改名/删除/`.hidden` 残留一律违规（D28.3）。"""
    problems: list[str] = []
    affected = [f for f in files if f.startswith(".toolchain/")]
    if agent and affected:
        problems += [
            f"[toolchain] Agent {agent} 改动了工具链：{p}（D28.3 禁止；负样本只许用临时 PATH / 环境变量 / /tmp 副本）"
            for p in affected
        ]
    if not agent:
        for p in affected:
            if re.search(r"\.(hidden|orig|bak)\d*$|~$", p):
                problems.append(f"[toolchain] 工具链里出现改名残留：{p}（D28.3：这正是上一轮污染工具链的手法）")

    # 文件系统扫描：残留的 *.hidden 即使还没提交也要抓
    tc = os.path.join(root, ".toolchain")
    for dirpath, _dirnames, filenames in os.walk(tc):
        if "/cargo/registry/" in dirpath or "/cargo/git/" in dirpath:
            continue
        for name in filenames:
            if re.search(r"\.(hidden|orig|bak)\d*$|~$", name):
                rel = os.path.relpath(os.path.join(dirpath, name), root)
                problems.append(f"[toolchain] 发现残留文件 {rel} —— 疑似「改名后忘了还原」（D28.3 事故特征）")
    return problems


def check_ownership(files: list[str], spec: dict, agent: str) -> list[str]:
    """Agent 的变动集必须全部落在它模块的 allow 里；governance 永远不许碰。"""
    problems: list[str] = []
    modules = spec.get("modules", {})
    if agent not in modules:
        return [f"[ownership] 未知模块 id {agent!r}（docs/spec/ownership.toml 的 modules 段里没有它）"]
    allowed = modules[agent].get("allow", [])
    governance = spec.get("governance", {}).get("paths", [])
    shared = spec.get("shared", {}).get("paths", [])

    for path in files:
        if matches_any(path, governance):
            problems.append(
                f"[governance] {path} 是治理文件（所有权数据/门/决策日志/派发手册），**只允许主 Agent 修改**"
            )
            continue
        if matches_any(path, allowed):
            continue                      # 显式授权优先（例如 M5M6 被授权改契约镜像）
        if matches_any(path, shared):
            problems.append(
                f"[ownership] {path} 是共享资源（跨模块）—— Agent {agent} 未获授权；需主 Agent 在 "
                f"ownership.toml 的该模块 allow 里显式列出"
            )
            continue
        problems.append(f"[ownership] {path} 不在模块 {agent} 的允许范围内（见 docs/spec/ownership.toml）")
    return problems


def ownership_snippet(spec: dict, module: str) -> str:
    """生成器：打印可直接贴进任务书的所有权段（口径不再手写，消除漂移）。"""
    mod = spec.get("modules", {}).get(module)
    if mod is None:
        raise SystemExit(f"未知模块 id {module!r}")
    governance = spec.get("governance", {}).get("paths", [])
    lines = [
        f"## 所有权（由 scripts/check-boundaries.py --print-ownership {module} 生成，勿手改）",
        "",
        f"模块：**{module} · {mod.get('title', '')}**　门：`{'` / `'.join(mod.get('gate', []))}`",
        "",
        "**允许写**：",
        *[f"- `{p}`" for p in mod.get("allow", [])],
        "",
        "**禁止（会被 `scripts/check-boundaries.py` 判为违规）**：",
        "- 任何不在上表的路径",
        "- 治理文件（只允许主 Agent 改）：" + "、".join(f"`{p}`" for p in governance),
        "- `.toolchain/**` 的任何改动、改名、`.hidden` 残留（D28.3）",
        "",
    ]
    return "\n".join(lines)


# --------------------------------------------------------------------------- main
def main() -> int:
    ap = argparse.ArgumentParser(description="PrincessIDE 边界门")
    ap.add_argument("--root", default=DEFAULT_ROOT)
    ap.add_argument("--agent", default=os.environ.get("PRINCESSIDE_AGENT"))
    ap.add_argument("--files", nargs="*", help="显式变动集（默认取 git status）")
    ap.add_argument("--print-ownership", metavar="MODULE")
    ap.add_argument("--baseline", metavar="FILE", help="扣除该文件中列出的路径（派发前就已脏的文件）")
    ap.add_argument("--record-baseline", metavar="MODULE", help="把当前变动集写入 .scratch/dispatch/<MODULE>.baseline")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    root = os.path.abspath(args.root)
    spec = load_spec(root)

    if args.print_ownership:
        print(ownership_snippet(spec, args.print_ownership))
        return 0

    if args.record_baseline:
        base_dir = os.path.join(root, ".scratch/dispatch")
        os.makedirs(base_dir, exist_ok=True)
        path = os.path.join(base_dir, f"{args.record_baseline}.baseline")
        current = changed_files(root) if args.files is None else args.files
        with open(path, "w") as fh:
            fh.write("\n".join(sorted(f for f in current if f.strip())) + "\n")
        print(f"[boundaries] 基线已记录：{path}（{len(current)} 个路径）")
        print("[boundaries] 复核时用：--agent %s --baseline %s" % (args.record_baseline, os.path.relpath(path, root)))
        return 0

    files = [f for f in (args.files if args.files is not None else changed_files(root)) if f.strip()]
    if args.baseline:
        with open(args.baseline) as fh:
            before = {line.strip() for line in fh if line.strip()}
        kept = [f for f in files if f not in before]
        if not args.quiet:
            print(f"[boundaries] 基线扣除 {len(files) - len(kept)} 个「派发前就已脏」的路径")
        files = kept

    problems = []
    problems += check_workspace_members(root, spec)
    problems += check_toolchain(root, files, args.agent)
    if args.agent:
        problems += check_ownership(files, spec, args.agent)

    if not args.quiet:
        scope = f"（agent={args.agent}）" if args.agent else ""
        print(f"[boundaries] 仓库 {root}{scope}")
        print(f"[boundaries] 变动集 {len(files)} 个路径")
        for p in files[:20]:
            print(f"    - {p}")
        if len(files) > 20:
            print(f"    … 还有 {len(files) - 20} 个")

    if problems:
        print(f"\n[boundaries] ❌ {len(problems)} 条违规：", file=sys.stderr)
        for p in problems:
            print(f"  • {p}", file=sys.stderr)
        return 1
    print("\n[boundaries] ✅ 成员关系、工具链禁令、模块所有权 均通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
