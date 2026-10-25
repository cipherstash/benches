#!/usr/bin/env python3
"""json_tree as merged to `next` (cipherstash/stack ebdb5de0a, format `merged`)
against the gated walk (edabee625, `gates`) and #1363 as first measured
(683802bcc, `derivable`), all three timed interleaved on one machine.
Report: report/JSON_TREE_MERGED.md. Results under results/query/merged/.

    report_json_tree_merged.py render <rows> <time json> [<time json> ...]
        Markdown: sizes, ingest, then every timed scenario from the first
        time file in the three formats, with the M1 Max figures of the
        derivable-tokens measurement (results/query/derivable/) beside them,
        and the noise floor between the first two time files.
    report_json_tree_merged.py summary <rows> <time json>
        One JSON object per scenario: medians per format, here and on the M1.
"""

import json
import os
import statistics
import sys
from collections import defaultdict

OUT = os.environ.get("DT_OUT", "results/query/merged")
INGEST = "results/ingest/merged"
M1 = "results/query/derivable"
M1_INGEST = "results/ingest/derivable"
FORMATS = ("gates", "derivable", "merged")


def pct(a, b):
    if not a or b is None:
        return "n/a"
    return f"{(b - a) / a * 100:+.1f}%"


def mib(b):
    return f"{b / 1048576:,.1f} MiB"


def q(values, p):
    v = sorted(values)
    if not v:
        return float("nan")
    k = (len(v) - 1) * p
    lo, hi = int(k), min(int(k) + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (k - lo)


def ms(us):
    if us is None:
        return "n/a"
    return f"{us / 1000:.3f}" if us < 10000 else f"{us / 1000:.2f}"


def stats(m):
    c = m["measured"]["calls_us"]
    p = m["measured"]["pass_us_per_doc"]
    return {
        "med": statistics.median(c) if c else None,
        "p95": q(c, 0.95) if c else None,
        "pass": statistics.median(p) if p else None,
        "rows": m["measured"]["rows_per_doc"] if p else m["measured"]["rows_rep"],
        "bytes": m["measured"]["bytes_per_doc"] if p else m["measured"]["bytes_rep"],
    }


def m1_time(rows, name):
    """The M1 Max figures for a scenario: the focused run where it has the
    scenario, else run 1, as the derivable-tokens report quotes them."""
    files = [f"{M1}/time_{rows}_focus.json", f"{M1}/time_{rows}_r1.json"]
    for f in files:
        if not os.path.exists(f):
            continue
        t = json.load(open(f))
        per = t["scenarios"].get(name)
        if per:
            return {k: stats(v) for k, v in per.items()}
    return {}


def render_sizes(rows):
    path = f"{OUT}/storage_{rows}.json"
    if not os.path.exists(path):
        return
    data = json.load(open(path))
    m1 = json.load(open(f"{M1}/storage_{rows}.json")) if os.path.exists(f"{M1}/storage_{rows}.json") else {}
    print(f"\n#### Stored size, {int(rows):,} rows\n")
    print("| table | measure | gates | derivable | merged | merged vs gates | M1: gates → derivable |")
    print("|---|---|--:|--:|--:|--:|--:|")
    for t, per in data.items():
        if not all(f in per for f in FORMATS):
            continue
        g, d, m = per["gates"], per["derivable"], per["merged"]
        o = m1.get(t, {})
        name = t.replace("_encrypted_v4", "")

        def m1cell(key, fmt):
            if "gates" in o and "derivable" in o:
                return f"{fmt(o['gates'][key])} → {fmt(o['derivable'][key])}"
            return "–"

        print(f"| {name} | entries per document | {g['entries']:,.1f} | {d['entries']:,.1f} | {m['entries']:,.1f} | | {m1cell('entries', lambda v: f'{v:,.1f}')} |")
        print(f"| {name} | `pg_column_size` per document | {g['pg_column_size']:,.0f} B | {d['pg_column_size']:,.0f} B | {m['pg_column_size']:,.0f} B | {pct(g['pg_column_size'], m['pg_column_size'])} | {m1cell('pg_column_size', lambda v: f'{v:,.0f} B')} |")
        print(f"| {name} | characters per document | {g['chars']:,.0f} | {d['chars']:,.0f} | {m['chars']:,.0f} | {pct(g['chars'], m['chars'])} | {m1cell('chars', lambda v: f'{v:,.0f}')} |")
        print(f"| {name} | heap incl. TOAST | {mib(g['table_bytes'])} | {mib(d['table_bytes'])} | {mib(m['table_bytes'])} | {pct(g['table_bytes'], m['table_bytes'])} | {m1cell('table_bytes', mib)} |")
        if g["gin_bytes"] or m["gin_bytes"]:
            print(f"| {name} | containment GIN | {mib(g['gin_bytes'])} | {mib(d['gin_bytes'])} | {mib(m['gin_bytes'])} | {pct(g['gin_bytes'], m['gin_bytes'])} | {m1cell('gin_bytes', mib)} |")
        print(f"| {name} | total | {mib(g['total_bytes'])} | {mib(d['total_bytes'])} | {mib(m['total_bytes'])} | {pct(g['total_bytes'], m['total_bytes'])} | {m1cell('total_bytes', mib)} |")


def read_ingest(path):
    seal = defaultdict(list)
    if not os.path.exists(path):
        return seal
    for line in open(path):
        line = line.strip()
        if not line:
            continue
        r = json.loads(line)
        if "seal_seconds" not in r:
            continue
        corpus = r["table"].replace("json_tree_", "").split("_encrypted")[0]
        seal[(corpus, r["inserted"], r["format"])].append(r)
    return seal


def render_ingest():
    seal = read_ingest(f"{INGEST}/ingest.jsonl")
    if not seal:
        return
    m1 = read_ingest(f"{M1_INGEST}/ingest.jsonl")
    print("\n#### Ingest: seal and INSERT, seconds (median of the runs; min–max)\n")
    print("| corpus | rows | runs | step | gates | derivable | merged | merged vs gates | M1: gates → derivable |")
    print("|---|--:|--:|---|--:|--:|--:|--:|--:|")
    for (corpus, n) in sorted({(c, n) for c, n, _ in seal}):
        rs = {f: seal.get((corpus, n, f), []) for f in FORMATS}
        if not all(rs.values()):
            continue
        for key, label in [("seal_seconds", "seal"), ("insert_seconds", "INSERT")]:
            def cell(lst):
                v = [r[key] for r in lst]
                return statistics.median(v), f"{statistics.median(v):.2f} ({min(v):.2f}–{max(v):.2f})"
            cells = {f: cell(rs[f]) for f in FORMATS}
            o = {f: m1.get((corpus, n, f), []) for f in ("gates", "derivable")}
            m1c = (f"{statistics.median(r[key] for r in o['gates']):.2f} → {statistics.median(r[key] for r in o['derivable']):.2f}"
                   if all(o.values()) else "–")
            runs = " / ".join(str(len(rs[f])) for f in FORMATS)
            print(f"| {corpus} | {n:,} | {runs} | {label} | {cells['gates'][1]} | {cells['derivable'][1]} | {cells['merged'][1]} | "
                  f"{pct(cells['gates'][0], cells['merged'][0])} | {m1c} |")
    gin = f"{INGEST}/gin_build.jsonl"
    if os.path.exists(gin):
        b = defaultdict(list)
        for line in open(gin):
            if line.strip():
                r = json.loads(line)
                b[(r["index"], r["format"])].append(r["seconds"])
        print("\n| containment GIN build (REINDEX, median of 3) | gates | derivable | merged | merged vs gates |")
        print("|---|--:|--:|--:|--:|")
        for idx in sorted({i for i, _ in b}):
            v = {f: statistics.median(b[(idx, f)]) for f in FORMATS if b[(idx, f)]}
            if len(v) == 3:
                print(f"| {idx} | {v['gates']:.2f} s | {v['derivable']:.2f} s | {v['merged']:.2f} s | {pct(v['gates'], v['merged'])} |")


TITLES = {
    "floor": "Floors (no json_tree read)",
    "read": "Reads with no wildcard",
    "wildcard": "Wildcard reads",
    "enumerate": "Enumerating children: `$.orders` on the nested corpus (5 to 10 elements), and the wide corpus (1,000 rows: 300 members, 300 elements)",
    "predicate": "Containment and `->` extraction",
}


def render_time(rows, files):
    runs = [json.load(open(f)) for f in files]
    t = runs[0]
    groups = defaultdict(list)
    for name, per in t["scenarios"].items():
        any_fmt = next(iter(per.values()))
        groups[any_fmt["scenario"]["group"]].append(name)
    for g in ["floor", "read", "wildcard", "enumerate", "predicate"]:
        if g not in groups:
            continue
        print(f"\n#### {TITLES[g]}\n")
        print("Per call is the client wall time of one prepared call, median (p95 below it in the raw files). "
              "In-server is per document over the pass set.\n")
        print("| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | "
              "in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |")
        print("|---|--:|--:|--:|--:|--:|--:|")
        for name in groups[g]:
            per = t["scenarios"][name]
            s = {f: stats(per[f]) for f in FORMATS if f in per}
            o = m1_time(rows, name)

            def trio(key):
                return " / ".join(ms(s[f][key]) if f in s else "–" for f in FORMATS)

            def ch(a, b, key):
                if a in s and b in s:
                    return pct(s[a][key], s[b][key])
                return "n/a"

            m1c = " / ".join(ms(o[f]["med"]) if f in o else "–" for f in ("gates", "derivable")) if o else "–"
            print(f"| {name} | {trio('med')} | {ch('gates', 'merged', 'med')} | {ch('derivable', 'merged', 'med')} | "
                  f"{trio('pass')} | {ch('gates', 'merged', 'pass')} | {m1c} |")
    if len(runs) > 1:
        print("\n#### Noise floor: the same format, run 1 against run 2\n")
        print("| metric | median over scenarios | largest | scenario |")
        print("|---|--:|--:|---|")
        for key, label in [("med", "per call, median"), ("p95", "per call, p95"), ("pass", "in-server per doc")]:
            diffs = []
            for name, per in runs[0]["scenarios"].items():
                for f in FORMATS:
                    if f in per and name in runs[1]["scenarios"] and f in runs[1]["scenarios"][name]:
                        a, b = stats(per[f])[key], stats(runs[1]["scenarios"][name][f])[key]
                        if a and b:
                            diffs.append((abs(b - a) / a * 100, f"{name} ({f})"))
            if diffs:
                diffs.sort()
                print(f"| {label} | {statistics.median(d for d, _ in diffs):.1f}% | {diffs[-1][0]:.1f}% | {diffs[-1][1]} |")


def summary(rows, file):
    t = json.load(open(file))
    out = {}
    for name, per in t["scenarios"].items():
        s = {f: stats(per[f]) for f in FORMATS if f in per}
        o = m1_time(rows, name)
        sc = next(iter(per.values()))["scenario"]
        out[name] = {
            "group": sc["group"],
            "note": sc["note"],
            "m4": {f: {"med_ms": (v["med"] or 0) / 1000, "p95_ms": (v["p95"] or 0) / 1000,
                       "pass_ms": (v["pass"] / 1000) if v["pass"] else None,
                       "rows": v["rows"], "bytes": v["bytes"],
                       "tokens": per[f]["scenario"]["tokens"], "gates": per[f]["scenario"]["gates"],
                       "param_bytes": per[f]["scenario"]["param_bytes"]} for f, v in s.items()},
            "m1": {f: {"med_ms": (v["med"] or 0) / 1000,
                       "pass_ms": (v["pass"] / 1000) if v["pass"] else None} for f, v in o.items()},
        }
    print(json.dumps(out, indent=1))


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "render"
    if cmd == "render":
        rows = sys.argv[2]
        render_sizes(rows)
        render_ingest()
        if len(sys.argv) > 3:
            render_time(rows, sys.argv[3:])
    elif cmd == "summary":
        summary(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
