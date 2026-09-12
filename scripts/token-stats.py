#!/usr/bin/env python3
"""统计 DSH 会话的 token 消耗，按 provider/model 归属。

数据来源：/root/.dsh/sessions/<workspace>/<session>/session.jsonl.zstd

关键设计：
  * 权威 usage 记录 = type=="assistant/chunk" 且 data.chunk.type=="usage"
    （tool/result 里偶然出现的 "usage" 字符串是工具输出噪声，必须排除）
  * 模型归属 = 流式跟踪最近一条 model/selection 的 provider+model，
    把其后的每条 usage 记到该模型上（会话中途切换模型也能正确归属）
  * 主会话与子 Agent 会话分开统计（子 Agent id 形如 <uuid>，主会话形如 session-<uuid>）

用法: python3 scripts/token-stats.py [--workspace <name>] [--json <out.json>]
"""
import argparse
import json
import os
import subprocess
import sys
from collections import defaultdict

DEFAULT_ROOT = "/root/.dsh/sessions"
DEFAULT_WORKSPACE = "--root-PrincessIDE--"


def scan(path):
    """流式扫描一个会话文件，返回按模型聚合的数据。"""
    per_model = defaultdict(
        lambda: {"steps": 0, "input": 0, "output": 0, "cache_read": 0, "reasoning": 0, "total": 0}
    )
    cur = ("unknown", "unknown")
    p = subprocess.Popen(
        ["zstd", "-dc", path], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL
    )
    for raw in p.stdout:
        if (b"model/selection" not in raw and b"request/header" not in raw
                and b'"usage"' not in raw):
            continue
        try:
            d = json.loads(raw)
        except Exception:
            continue
        data = d.get("data") or {}
        t = d.get("type")
        if t == "model/selection":
            prov = data.get("provider")
            mod = data.get("model")
            if prov and mod:
                cur = (prov, mod)
        elif t == "request/header":
            cfg = (data.get("header") or {}).get("config") or {}
            prov = cfg.get("provider")
            mod = cfg.get("model")
            if prov and mod:
                cur = (prov, mod)
        elif t == "assistant/chunk":
            ch = data.get("chunk") or {}
            if ch.get("type") != "usage":
                continue
            u = ch.get("usage") or {}
            b = per_model[cur]
            b["steps"] += 1
            b["input"] += u.get("inputTokens", 0) or 0
            b["output"] += u.get("outputTokens", 0) or 0
            b["cache_read"] += u.get("cacheReadTokens", 0) or 0
            b["reasoning"] += u.get("reasoningTokens", 0) or 0
            b["total"] += u.get("totalTokens", 0) or 0
    p.stdout.close()
    p.wait()
    return per_model


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default=DEFAULT_ROOT)
    ap.add_argument("--workspace", default=DEFAULT_WORKSPACE)
    ap.add_argument("--json", default="/root/PrincessIDE/docs/reports/token-stats.json")
    ap.add_argument("--top", type=int, default=20)
    args = ap.parse_args()

    sessions = []
    by_model = defaultdict(
        lambda: {"steps": 0, "input": 0, "output": 0, "cache_read": 0, "reasoning": 0, "total": 0}
    )
    by_kind = defaultdict(lambda: defaultdict(int))

    for ws in sorted(os.listdir(args.root)):
        if args.workspace and ws != args.workspace:
            continue
        wsdir = os.path.join(args.root, ws)
        if not os.path.isdir(wsdir):
            continue
        for entry in sorted(os.listdir(wsdir)):
            sdir = os.path.join(wsdir, entry)
            f = os.path.join(sdir, "session.jsonl.zstd")
            if not os.path.isfile(f):
                continue
            pm = scan(f)
            tot = {"steps": 0, "input": 0, "output": 0, "cache_read": 0, "reasoning": 0, "total": 0}
            for (prov, mod), b in pm.items():
                for k in tot:
                    tot[k] += b[k]
                m = by_model[f"{prov}/{mod}"]
                for k in b:
                    m[k] += b[k]
            if tot["steps"] == 0:
                continue
            kind = "main" if entry == "session-d0c4164b-f1f6-4906-af82-b45f7344ca13" else "agent"
            sessions.append({"workspace": ws, "session": entry, "kind": kind, **tot})
            for k in tot:
                by_kind[kind][k] += tot[k]

    sessions.sort(key=lambda r: -r["total"])

    W = 82
    print("=" * W)
    print("PrincessIDE — Token 消耗统计（按 provider/model 归属）")
    print("=" * W)
    print()

    print("【按模型】")
    print(f"  {'provider/model':46s} {'步数':>6s} {'input':>12s} {'output':>11s} {'cache_read':>14s} {'total':>15s}")
    allm = {"steps": 0, "input": 0, "output": 0, "cache_read": 0, "reasoning": 0, "total": 0}
    for m, b in sorted(by_model.items(), key=lambda x: -x[1]["total"]):
        print(f"  {m[:46]:46s} {b['steps']:>6d} {b['input']:>12,} {b['output']:>11,} "
              f"{b['cache_read']:>14,} {b['total']:>15,}")
        for k in allm:
            allm[k] += b[k]
    print(f"  {'─' * 46} {'─' * 6} {'─' * 12} {'─' * 11} {'─' * 14} {'─' * 15}")
    print(f"  {'合计':46s} {allm['steps']:>6d} {allm['input']:>12,} {allm['output']:>11,} "
          f"{allm['cache_read']:>14,} {allm['total']:>15,}")
    print()

    print("【主 Agent vs 子 Agent】")
    for kind, label in (("main", "主 Agent（本会话）"), ("agent", "子 Agent（本项目管理）")):
        b = by_kind.get(kind)
        if not b:
            continue
        print(f"  {label:20s} 步数={b['steps']:>5d}  input={b['input']:>11,}  "
              f"output={b['output']:>10,}  total={b['total']:>14,}")
    print()

    fresh = allm["input"] + allm["output"]
    print("【计费视角】")
    print(f"  新鲜 input tokens   : {allm['input']:>14,}")
    print(f"  output tokens       : {allm['output']:>14,}")
    print(f"  input+output（全价）: {fresh:>14,}")
    print(f"  cache read tokens   : {allm['cache_read']:>14,}  （通常按折扣计费）")
    print(f"  reasoning tokens    : {allm['reasoning']:>14,}  （已含在 output 内）")
    if allm["input"]:
        print(f"  缓存命中率          : {allm['cache_read'] / (allm['cache_read'] + allm['input']) * 100:>13.1f}%")
    print()

    print(f"【会话明细】前 {args.top} 名（共 {len(sessions)} 个有记录的会话）")
    print(f"  {'会话':40s} {'类型':5s} {'步数':>6s} {'input':>11s} {'output':>10s} {'total':>14s}")
    for r in sessions[: args.top]:
        print(f"  {r['session'][:40]:40s} {r['kind']:5s} {r['steps']:>6d} "
              f"{r['input']:>11,} {r['output']:>10,} {r['total']:>14,}")

    out = {
        "schema": "princesside.tokenstats/v2",
        "note": "usage 记录取自 assistant/chunk(chunk.type=usage)；模型归属按流式 model/selection 跟踪",
        "by_model": {m: b for m, b in by_model.items()},
        "by_kind": {k: dict(v) for k, v in by_kind.items()},
        "totals": allm,
        "sessions": sessions,
    }
    os.makedirs(os.path.dirname(args.json), exist_ok=True)
    with open(args.json, "w") as fh:
        json.dump(out, fh, ensure_ascii=False, indent=2)
    print()
    print(f"JSON → {args.json}")


if __name__ == "__main__":
    main()
