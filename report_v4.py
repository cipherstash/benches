#!/usr/bin/env python3
"""Summarise the EQL v4 / Go SDK bench results (results/v4/*.jsonl).

For each (sdk, variant, mode, tier, scenario, op) the latest run wins.
Prints one table per mode: median CPU per call for every variant side by
side, so the effect of each Go SDK change reads across a row.

    python3 report_v4.py [--op decrypt] [--metric cpu|wall] [--markdown]
"""
import argparse
import glob
import json
import os
import statistics
from collections import OrderedDict

ORDER = ["native", "shipped", "rebuilt-s", "noclose", "o3", "o3-noclose", "next", "next-fix", "next-yield"]


def label(r):
    return r["variant"] if r["sdk"] == "go" else "native"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--op", default="decrypt")
    ap.add_argument("--metric", default="cpu", choices=["cpu", "wall"])
    ap.add_argument("--markdown", action="store_true")
    ap.add_argument("--dir", default=os.path.join(os.path.dirname(__file__), "results", "v4"))
    a = ap.parse_args()

    latest = {}
    for path in sorted(glob.glob(os.path.join(a.dir, "*.jsonl"))):
        if os.path.basename(path).startswith("ingest"):
            continue
        for line in open(path):
            r = json.loads(line)
            if r["op"] != a.op:
                continue
            key = (r["mode"], r.get("tier", 0), r["scenario"], label(r), r.get("clients", 1) or 1)
            if key not in latest or r["at"] > latest[key]["at"]:
                latest[key] = r

    modes = OrderedDict()
    for (mode, tier, scen, var, clients), r in latest.items():
        name = scen if clients == 1 else f"{scen} ×{clients} clients"
        modes.setdefault((mode, tier), OrderedDict()).setdefault(name, {})[var] = r

    field = "cpu_ms" if a.metric == "cpu" else "wall_ms"
    for (mode, tier), rows in sorted(modes.items()):
        variants = sorted({v for cells in rows.values() for v in cells},
                          key=lambda v: (ORDER.index(v) if v in ORDER else len(ORDER), v))
        title = f"{mode}" + (f" — tier {tier:,}" if tier else "") + f" — {a.op} {a.metric} ms per call (median)"
        head = ["case", "values"] + variants
        table = []
        for scen, cells in rows.items():
            vals = next(iter(cells.values()))["values"]
            row = [scen, str(vals)]
            for v in variants:
                r = cells.get(v)
                row.append(f"{statistics.median(r[field]):.2f}" if r else "")
            table.append(row)
        if a.markdown:
            print(f"\n**{title}**\n")
            print("| " + " | ".join(head) + " |")
            print("|" + "|".join(["---"] + ["--:"] * (len(head) - 1)) + "|")
            for row in table:
                print("| " + " | ".join(row) + " |")
        else:
            print(f"\n{title}")
            w = [max(len(x) for x in col) for col in zip(head, *table)]
            print("  ".join(h.ljust(w[i]) if i == 0 else h.rjust(w[i]) for i, h in enumerate(head)))
            for row in table:
                print("  ".join(c.ljust(w[i]) if i == 0 else c.rjust(w[i]) for i, c in enumerate(row)))


if __name__ == "__main__":
    main()
