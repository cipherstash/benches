#!/usr/bin/env python3
"""The derivable-tokens comparison: cipherstash/stack#1363 (format `derivable`)
against its base (format `gates`). Report: report/JSON_TREE_DERIVABLE_TOKENS.md.

    report_derivable_tokens.py sizes <rows> [table...]
        Measure the live json_tree tables of both databases (DT_GATES_DB,
        DT_DERIVABLE_DB; default jsonbench_base / jsonbench_new) and write
        results/query/derivable/storage_<rows>.json.
    report_derivable_tokens.py render <rows> <time json> [<time json> ...]
        Print the Markdown tables: sizes, ingest, then every timed scenario
        from the first time file, with the noise floor between the first two
        when two are given.
"""

import json
import os
import statistics
import subprocess
import sys
from collections import defaultdict

OUT = "results/query/derivable"
INGEST = "results/ingest/derivable"
FORMATS = ("gates", "derivable")


def dbs():
    return {
        "gates": os.environ.get("DT_GATES_DB", "jsonbench_base"),
        "derivable": os.environ.get("DT_DERIVABLE_DB", "jsonbench_new"),
    }


def psql(db, sql):
    out = subprocess.run(
        ["psql", "-h", "localhost", "-p", "5400", "-U", "postgres", "-d", db, "-tAF", "|", "-c", sql],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    return [line.split("|") for line in out.splitlines() if line]


def sizes(rows, tables):
    if not tables:
        tables = [f"json_tree_small_encrypted_v4_{rows}", f"json_tree_large_encrypted_v4_{rows}"]
    result = {}
    for fmt, db in dbs().items():
        for t in tables:
            exists = psql(db, f"SELECT to_regclass('{t}') IS NOT NULL")[0][0] == "t"
            if not exists:
                continue
            r = psql(db, f"""
                SELECT count(*),
                       avg(pg_column_size(value)),
                       avg(length(value::text)),
                       avg(length((value::jsonb -> 'sv')::text)),
                       avg(jsonb_array_length(value::jsonb -> 'sv')),
                       avg((SELECT count(*) FROM jsonb_array_elements(value::jsonb -> 'sv') e WHERE e ? 'x')),
                       pg_table_size('{t}'),
                       COALESCE(pg_total_relation_size((SELECT reltoastrelid FROM pg_class WHERE oid = '{t}'::regclass)), 0),
                       pg_indexes_size('{t}'),
                       COALESCE((SELECT sum(pg_relation_size(indexrelid)) FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
                                 WHERE indrelid = '{t}'::regclass AND c.relname LIKE '%query_gin%'), 0),
                       pg_total_relation_size('{t}')
                FROM {t}""")[0]
            keys = ["rows", "pg_column_size", "chars", "sv_chars", "entries", "entries_with_x",
                    "table_bytes", "toast_bytes", "indexes_bytes", "gin_bytes", "total_bytes"]
            result.setdefault(t, {})[fmt] = {k: float(v) for k, v in zip(keys, r)}
            print(fmt, t, result[t][fmt], file=sys.stderr)
    os.makedirs(OUT, exist_ok=True)
    path = f"{OUT}/storage_{rows}.json"
    existing = json.load(open(path)) if os.path.exists(path) else {}
    existing.update(result)
    json.dump(existing, open(path, "w"), indent=2)
    print(f"written to {path}", file=sys.stderr)


def pct(a, b):
    if a == 0:
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
    return f"{us / 1000:.3f}" if us < 10000 else f"{us / 1000:.2f}"


def render_sizes(rows):
    path = f"{OUT}/storage_{rows}.json"
    if not os.path.exists(path):
        return
    data = json.load(open(path))
    print(f"\n#### Stored size, {int(rows):,} rows\n")
    print("| table | measure | gates | derivable | change |")
    print("|---|---|--:|--:|--:|")
    for t, per in data.items():
        if not all(f in per for f in FORMATS):
            continue
        g, d = per["gates"], per["derivable"]
        name = t.replace("_encrypted_v4", "")
        print(f"| {name} | entries per document | {g['entries']:,.1f} | {d['entries']:,.1f} | |")
        print(f"| {name} | characters per document | {g['chars']:,.0f} | {d['chars']:,.0f} | {pct(g['chars'], d['chars'])} |")
        print(f"| {name} | `pg_column_size` per document | {g['pg_column_size']:,.0f} B | {d['pg_column_size']:,.0f} B | {pct(g['pg_column_size'], d['pg_column_size'])} |")
        print(f"| {name} | per entry: characters / bytes on disk | {g['chars'] / g['entries']:,.1f} / {g['pg_column_size'] / g['entries']:,.1f} | "
              f"{d['chars'] / d['entries']:,.1f} / {d['pg_column_size'] / d['entries']:,.1f} | "
              f"+{(d['chars'] - g['chars']) / d['entries']:,.1f} chars / +{(d['pg_column_size'] - g['pg_column_size']) / d['entries']:,.1f} B |")
        print(f"| {name} | heap incl. TOAST | {mib(g['table_bytes'])} | {mib(d['table_bytes'])} | {pct(g['table_bytes'], d['table_bytes'])} |")
        if g["gin_bytes"] or d["gin_bytes"]:
            print(f"| {name} | containment GIN | {mib(g['gin_bytes'])} | {mib(d['gin_bytes'])} | {pct(g['gin_bytes'], d['gin_bytes'])} |")
        print(f"| {name} | all indexes | {mib(g['indexes_bytes'])} | {mib(d['indexes_bytes'])} | {pct(g['indexes_bytes'], d['indexes_bytes'])} |")
        print(f"| {name} | total | {mib(g['total_bytes'])} | {mib(d['total_bytes'])} | {pct(g['total_bytes'], d['total_bytes'])} |")


def render_ingest():
    path = f"{INGEST}/ingest.jsonl"
    if not os.path.exists(path):
        return
    seal = defaultdict(list)
    for line in open(path):
        line = line.strip()
        if not line:
            continue
        r = json.loads(line)
        if "seal_seconds" not in r:
            continue
        corpus = r["table"].replace("json_tree_", "").split("_encrypted")[0]
        key = (corpus, r["inserted"], r["format"])
        seal[key].append(r)
    print("\n#### Ingest: seal and INSERT, seconds per batch of rows (median of the runs; min–max)\n")
    print("| corpus | rows | runs | gates seal | derivable seal | change | gates INSERT | derivable INSERT | change |")
    print("|---|--:|--:|--:|--:|--:|--:|--:|--:|")
    for (corpus, n) in sorted({(c, n) for c, n, _ in seal}):
        g, d = seal.get((corpus, n, "gates"), []), seal.get((corpus, n, "derivable"), [])
        if not g or not d:
            continue
        def cell(rs, k):
            v = [r[k] for r in rs]
            return statistics.median(v), f"{statistics.median(v):.2f} ({min(v):.2f}–{max(v):.2f})"
        gs, gsc = cell(g, "seal_seconds")
        ds, dsc = cell(d, "seal_seconds")
        gi, gic = cell(g, "insert_seconds")
        di, dic = cell(d, "insert_seconds")
        print(f"| {corpus} | {n:,} | {len(g)} / {len(d)} | {gsc} | {dsc} | {pct(gs, ds)} | {gic} | {dic} | {pct(gi, di)} |")
    gin = f"{INGEST}/gin_build.jsonl"
    if os.path.exists(gin):
        b = defaultdict(list)
        for line in open(gin):
            if line.strip():
                r = json.loads(line)
                b[(r["index"], r["format"])].append(r["seconds"])
        print("\n| containment GIN build (REINDEX) | runs | gates | derivable | change |")
        print("|---|--:|--:|--:|--:|")
        for idx in sorted({i for i, _ in b}):
            g, d = b[(idx, "gates")], b[(idx, "derivable")]
            print(f"| {idx} | {len(g)} / {len(d)} | {statistics.median(g):.2f} s | {statistics.median(d):.2f} s | {pct(statistics.median(g), statistics.median(d))} |")


def stats(m):
    c = m["measured"]["calls_us"]
    p = m["measured"]["pass_us_per_doc"]
    return {
        "med": statistics.median(c) if c else float("nan"),
        "p95": q(c, 0.95),
        "pass": statistics.median(p) if p else None,
        "pass_p95": q(p, 0.95) if p else None,
        "rows": m["measured"]["rows_per_doc"] if p else m["measured"]["rows_rep"],
        "bytes": m["measured"]["bytes_per_doc"] if p else m["measured"]["bytes_rep"],
        "n": len(c),
    }


def render_time(files):
    runs = [json.load(open(f)) for f in files]
    t = runs[0]
    groups = defaultdict(list)
    for name, per in t["scenarios"].items():
        any_fmt = next(iter(per.values()))
        groups[any_fmt["scenario"]["group"]].append(name)
    titles = {
        "floor": "Floors (no json_tree function)",
        "read": "Reads with no wildcard",
        "wildcard": "Wildcard reads",
        "enumerate": "Enumerating children: `$.orders` on the nested corpus (5 to 10 elements), and the wide corpus (1,000 rows: 300 members, 300 elements)",
        "predicate": "Containment and `->` extraction",
    }
    for g in ["floor", "read", "wildcard", "enumerate", "predicate"]:
        if g not in groups:
            continue
        print(f"\n#### {titles[g]}\n")
        print("| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |")
        print("|---|---|--:|--:|--:|--:|--:|--:|--:|")
        for name in groups[g]:
            per = t["scenarios"][name]
            s = {f: stats(per[f]) for f in FORMATS if f in per}
            sc = {f: per[f]["scenario"] for f in FORMATS if f in per}

            def sent(f):
                if f not in sc:
                    return "—"
                x = sc[f]
                if x["tokens"] == 0 and x["gates"] == 0:
                    return f"{x['param_bytes']:,} B" if x["param_bytes"] else "–"
                part = f"{x['tokens']} tok"
                if x["gates"]:
                    part += f" + {x['gates']} gates"
                return f"{part}, {x['param_bytes']:,} B"

            def col(key, fmt_fn):
                return " → ".join(fmt_fn(s[f][key]) if f in s and s[f][key] is not None else "n/a" for f in FORMATS)

            change = pct(s["gates"]["med"], s["derivable"]["med"]) if len(s) == 2 else "n/a"
            pchange = (pct(s["gates"]["pass"], s["derivable"]["pass"])
                       if len(s) == 2 and s["gates"]["pass"] and s["derivable"]["pass"] else "n/a")
            print(f"| {name} | {sent('gates')} → {sent('derivable')} | {col('rows', lambda v: f'{v:,.1f}')} | "
                  f"{col('bytes', lambda v: f'{v:,.0f}')} | {col('med', ms)} | {col('p95', ms)} | {change} | "
                  f"{col('pass', ms)} | {pchange} |")
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


def render_explain(file, names):
    t = json.load(open(file))
    for name in names:
        per = t["scenarios"].get(name)
        if not per:
            continue
        print(f"\n`{name}`, representative row:\n")
        for f in FORMATS:
            if f not in per:
                continue
            lines = per[f]["measured"]["explain"]
            print(f"{f}:\n\n```\n" + "\n".join(lines) + "\n```\n")


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "render"
    if cmd == "sizes":
        sizes(sys.argv[2], sys.argv[3:])
    elif cmd == "render":
        rows = sys.argv[2]
        render_sizes(rows)
        render_ingest()
        if len(sys.argv) > 3:
            render_time(sys.argv[3:])
    elif cmd == "explain":
        render_explain(sys.argv[2], sys.argv[3:])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
