#!/usr/bin/env python3
"""Three-way JSON comparison: native jsonb vs SteVec (EQL v3) vs json_tree
(EQL v4), over the same documents. Report:
report/JSON_TREE_VS_STEVEC_VS_PLAINTEXT.md. Inputs and outputs live under
results/query/compare/.

Subcommands (each talks to the database named by DATABASE_URL / PG* env, as
mise sets them with BENCH_DB=jsonbench):

    gin-rebuild   drop and rebuild the containment GIN of every table once,
                  timed (results/query/compare/gin_rebuild.json)
    storage       per-table column, heap, TOAST and index sizes
                  (results/query/compare/storage.json)
    indexes       every index on every table (results/query/compare/indexes.json)
    explain       EXPLAIN (ANALYZE, BUFFERS) of every scenario at a tier, with
                  the bench's own bound parameters, after five warm executions
                  of the prepared statement (results/query/compare/explain_<N>.json)
    render DIR    write the report fragments (results, noise floor, storage,
                  correctness, appendix) as markdown into DIR

The timing runs themselves are `mise run bench:compare:json`.
"""

import json
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).parent
OUT = ROOT / "results" / "query" / "compare"
TIERS = [10000, 100000]

# (system, corpus) -> (table template, column)
TABLES = {
    ("plaintext", "flat"): ("json_tree_small_plain_v4_{n}", "plain"),
    ("SteVec", "flat"): ("json_ste_vec_small_encrypted_v3_{n}", "value"),
    ("json_tree", "flat"): ("json_tree_small_encrypted_v4_{n}", "value"),
    ("plaintext", "nested"): ("json_tree_large_plain_v4_{n}", "plain"),
    ("SteVec", "nested"): ("json_ste_vec_large_encrypted_v3_{n}", "value"),
    ("json_tree", "nested"): ("json_tree_large_encrypted_v4_{n}", "value"),
}
SYSTEMS = ["plaintext", "SteVec", "json_tree"]


def psql(sql, *, tuples=True):
    args = ["psql", "-X", "-q", "-v", "ON_ERROR_STOP=1"]
    if tuples:
        args += ["-At"]
    r = subprocess.run(args, input=sql, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(f"psql failed: {r.stderr}\n--- sql ---\n{sql[:2000]}")
    return r.stdout


def gin_name(table):
    rows = psql(
        f"SELECT indexname FROM pg_indexes WHERE tablename = '{table}' "
        "AND indexdef ILIKE '%USING gin%'"
    ).split()
    assert len(rows) == 1, (table, rows)
    return rows[0]


# ---------------------------------------------------------------- gin-rebuild


def gin_rebuild():
    out = []
    for n in TIERS:
        for (system, corpus), (tmpl, _) in TABLES.items():
            table = tmpl.format(n=n)
            if system == "plaintext" and not psql(
                f"SELECT 1 FROM pg_indexes WHERE tablename = '{table}' AND indexdef ILIKE '%USING gin%'"
            ).strip():
                # A twin whose bench has not run at this tier yet: the GIN the
                # bench and sql/indexes/*_up.sql create.
                name = f"{table}_gin_idx"
                indexdef = f"CREATE INDEX {name} ON public.{table} USING gin (plain jsonb_path_ops)"
            else:
                name = gin_name(table)
                indexdef = psql(f"SELECT indexdef FROM pg_indexes WHERE indexname = '{name}'").strip()
                psql(f"DROP INDEX {name}")
            t0 = time.monotonic()
            psql(indexdef)
            secs = time.monotonic() - t0
            size = int(psql(f"SELECT pg_relation_size('{name}')").strip())
            print(f"{table}: {name} rebuilt in {secs:.1f}s, {size / 2**20:.1f} MiB", flush=True)
            out.append({"system": system, "corpus": corpus, "rows": n, "table": table,
                        "index": name, "indexdef": indexdef, "build_seconds": secs, "bytes": size})
    OUT.mkdir(parents=True, exist_ok=True)
    (OUT / "gin_rebuild.json").write_text(json.dumps(out, indent=2) + "\n")


# ---------------------------------------------------------------- storage


def storage():
    out = []
    for n in TIERS:
        for (system, corpus), (tmpl, col) in TABLES.items():
            table = tmpl.format(n=n)
            gin = gin_name(table)
            row = psql(f"""
                SELECT count(*), avg(pg_column_size({col}))::numeric(12,1),
                       avg(length({col}::text))::numeric(12,1),
                       pg_relation_size('{table}'),
                       coalesce(pg_total_relation_size(NULLIF(c.reltoastrelid, 0)), 0),
                       pg_table_size('{table}'),
                       pg_relation_size('{gin}'),
                       pg_indexes_size('{table}'),
                       pg_total_relation_size('{table}')
                FROM {table}, pg_class c WHERE c.oid = '{table}'::regclass
                GROUP BY c.reltoastrelid
            """).strip().split("|")
            rec = dict(zip(["rows_counted", "avg_column_bytes", "avg_text_chars", "main_bytes",
                            "toast_bytes", "table_bytes", "gin_bytes", "indexes_bytes", "total_bytes"],
                           [float(x) for x in row]))
            rec.update({"system": system, "corpus": corpus, "rows": n, "table": table, "gin": gin})
            print(json.dumps(rec), flush=True)
            out.append(rec)
    (OUT / "storage.json").write_text(json.dumps(out, indent=2) + "\n")


def indexes():
    out = {}
    for n in TIERS:
        for (system, corpus), (tmpl, _) in TABLES.items():
            table = tmpl.format(n=n)
            rows = psql(
                f"SELECT indexname, indexdef, pg_relation_size(indexname::regclass) FROM pg_indexes "
                f"WHERE tablename = '{table}' ORDER BY indexname"
            ).strip().splitlines()
            out[table] = [dict(zip(["name", "def", "bytes"], r.split("|"))) for r in rows]
    (OUT / "indexes.json").write_text(json.dumps(out, indent=2) + "\n")


# ---------------------------------------------------------------- explain

# The metadata sidecars of the three benches, per corpus.
SIDECARS = {
    "flat": [("SteVec", "stevec_small_metadata_{n}.json"), ("json_tree+plain", "json_tree_metadata_{n}.json")],
    "nested": [("SteVec", "stevec_large_metadata_{n}.json"), ("json_tree+plain", "json_tree_nested_metadata_{n}.json")],
}


def literal(v):
    """A bound parameter as a SQL literal and the type the bench binds it as."""
    if isinstance(v, bool):
        raise ValueError("no boolean parameters")
    if isinstance(v, int):
        return str(v), "int4"
    if isinstance(v, str):
        return f"$p${v}$p$", "text"
    if isinstance(v, list):
        return "ARRAY[" + ",".join(f"$p${x}$p$" for x in v) + "]::text[]", "text[]"
    if isinstance(v, dict):
        return f"$p${json.dumps(v)}$p$", "jsonb"
    raise ValueError(f"unsupported parameter {v!r}")


def slow_ids(n):
    ids = set()
    for c in ["small", "large"]:
        p = OUT / f"stevec_{c}_slow_{n}.json"
        if p.exists():
            ids |= {s["id"] for s in json.loads(p.read_text())}
    return ids


def explain(n):
    n = int(n)
    slow = slow_ids(n)
    out = []
    for corpus, files in SIDECARS.items():
        for _, f in files:
            meta = json.loads((OUT / f.format(n=n)).read_text())
            for s in meta["scenarios"]:
                lits = [literal(p) for p in s["parameters"]]
                types = ",".join(t for _, t in lits)
                vals = ",".join(v for v, _ in lits)
                prep = f"PREPARE s{'(' + types + ')' if lits else ''} AS {s['query']};"
                exe = f"EXECUTE s{'(' + vals + ')' if lits else ''}"
                warm = 0 if s["id"] in slow else 5
                sql = "\n".join(
                    [prep, "\\o /dev/null"] + [exe + ";"] * warm + ["\\o",
                     f"EXPLAIN (ANALYZE, BUFFERS) {exe};", "DEALLOCATE s;"])
                t0 = time.monotonic()
                plan = psql(sql)
                print(f"{s['id']}: {time.monotonic() - t0:.1f}s", flush=True)
                out.append({"id": s["id"], "corpus": corpus, "query": s["query"],
                            "parameter_types": [t for _, t in lits], "warm_executions": warm,
                            "parameters": s["parameters"], "plan": plan.rstrip("\n")})
    (OUT / f"explain_{n}.json").write_text(json.dumps(out, indent=2) + "\n")


# ---------------------------------------------------------------- render


def medians(path):
    out = {}
    if not path.exists():
        return out
    for line in path.read_text().splitlines():
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        if rec.get("reason") == "benchmark-complete":
            out[rec["id"]] = rec["median"]["estimate"] / 1e6  # ms
    return out


def slow_medians(path):
    if not path.exists():
        return {}
    return {s["id"]: s["median_ms"] for s in json.loads(path.read_text())}


def load_meta(path):
    if not path.exists():
        return {}
    return {s["id"]: s for s in json.loads(path.read_text())["scenarios"]}


# Row order and what each system runs per scenario. The tail is the scenario
# id's tail in each bench; None means the system cannot express it (footnote).
FLAT_ROWS = [
    ("contains/functional", "whole document, function spelling"),
    ("contains/operator", "whole document, `@>`"),
    ("field_eq/extractor", "`$.first_name` = value (containment)"),
    ("field_eq/unique", "`$.email` = value (containment)"),
    ("field_eq/eq_term", "`$.email` = value (SteVec `eq_term` btree)"),
    ("field_gt/functional", "`$.age` > value"),
    ("field_between/functional", "`$.age` between lo and hi"),
    ("field_order/functional", "ORDER BY `$.age` LIMIT 10"),
    ("extract/by_id", "`$.age` of one row"),
    ("path_query/field", "path query `$.email`, one row"),
]
NESTED_ROWS = [
    ("contains/deep_object", "`@>` 4 objects deep"),
    ("contains/deep_object_fn", "same, function spelling"),
    ("contains/array_element", "`@>` one address (city + state)"),
    ("contains/array_element_fn", "same, function spelling"),
    ("contains/nested_array", "`@>` order status + item product"),
    ("contains/nested_array_fn", "same, function spelling"),
    ("field_eq/nested", "`$.user.email` = value (containment)"),
    ("field_gt/nested", "`$.company.salary` > value"),
    ("field_between/nested", "`$.company.salary` between lo and hi"),
    ("field_order/nested", "ORDER BY `$.company.salary` LIMIT 10"),
    ("extract/nested", "`$.company.salary` of one row"),
    ("path_query/wildcard", "`$.orders[*].status`, one row"),
    ("subtree/company", "subtree `$.company` (6 leaves)"),
    ("subtree/user", "subtree `$.user` (12 leaves)"),
    ("subtree/orders", "subtree `$.orders`, default budget"),
    ("subtree/orders_walked", "subtree `$.orders`, budget 0"),
    ("subtree_at/items", "`$.orders[*].items`, default budget"),
    ("subtree_at/items_walked", "`$.orders[*].items`, budget 0"),
    ("children/orders", "children of `$.orders`"),
    ("child_count/orders", "child count of `$.orders`"),
]

# Plaintext scenarios absent by design (one spelling, or the json_tree-only
# variant), and SteVec scenarios with no SteVec query (footnotes in report).
PLAIN_BLANK = {"contains/functional": "a", "field_eq/eq_term": "b",
               "contains/deep_object_fn": "a", "contains/array_element_fn": "a",
               "contains/nested_array_fn": "a"}
STEVEC_BLANK = {"subtree/orders_walked": "c", "subtree_at/items_walked": "c"}
V4_BLANK = {"field_eq/eq_term": "d"}


def ids_for(corpus, tail, n, tag=""):
    if corpus == "flat":
        return (f"JSON_TREE/plain/{tail}/{n}", f"JSON_COMPARE_V3/small/{tail}/{n}",
                f"JSON_TREE/json_tree/{tail}/{n}")
    return (f"JSON_TREE_NESTED/plain/{tail}/{n}", f"JSON_COMPARE_V3/large/{tail}/{n}",
            f"JSON_TREE_NESTED/json_tree/{tail}/{n}")


def load_tier(corpus, n, tag=""):
    c = "small" if corpus == "flat" else "large"
    v4 = "json_tree" if corpus == "flat" else "json_tree_nested"
    med = {}
    med.update(medians(OUT / f"stevec_{c}_rows_{n}{tag}.json"))
    med.update(medians(OUT / f"{v4}_rows_{n}{tag}.json"))
    slow = slow_medians(OUT / f"stevec_{c}_slow_{n}{tag}.json")
    meta = {}
    meta.update(load_meta(OUT / f"stevec_{c}_metadata_{n}{tag}.json"))
    meta.update(load_meta(OUT / f"{v4}_metadata_{n}{tag}.json"))
    return med, slow, meta


def fmt_ms(v):
    if v is None:
        return None
    if v >= 100:
        return f"{v:,.0f}"
    if v >= 10:
        return f"{v:.1f}"
    return f"{v:.2f}"


def idx_label(meta, sid, table):
    m = meta.get(sid)
    if not m:
        return ""
    names = [i.replace(table + "_", "") for i in m["indexes_used"]]
    return ", ".join(names) or "seq scan"


def render_results(corpus, n):
    rows = FLAT_ROWS if corpus == "flat" else NESTED_ROWS
    med, slow, meta = load_tier(corpus, n)
    tables = [TABLES[(s, corpus)][0].format(n=n) for s in SYSTEMS]
    lines = [
        "| scenario | plaintext | SteVec | json_tree | SteVec / plain | json_tree / plain | json_tree / SteVec | rows (p / S / j) | index (p / S / j) |",
        "|---|--:|--:|--:|--:|--:|--:|--:|---|",
    ]
    for tail, desc in rows:
        p_id, s_id, j_id = ids_for(corpus, tail, n)
        vals, cells = [], []
        for sys_, sid, blanks in [("plaintext", p_id, PLAIN_BLANK), ("SteVec", s_id, STEVEC_BLANK),
                                  ("json_tree", j_id, V4_BLANK)]:
            if tail in blanks:
                vals.append(None)
                cells.append(f"—[^{blanks[tail]}]")
                continue
            if sid in med:
                vals.append(med[sid])
                cells.append(fmt_ms(med[sid]))
            elif sid in slow:
                vals.append(slow[sid])
                cells.append(fmt_ms(slow[sid]) + "[^s]")
            else:
                vals.append(None)
                cells.append("n/m")

        def ratio(a, b):
            if a is None or b is None:
                return ""
            r = a / b
            if r < 0.01:
                return "<0.01×"
            if r < 1:
                return f"{r:.2f}×"
            if r < 10:
                return f"{r:.1f}×"
            return f"{r:,.0f}×"

        rr = "/".join(str(meta[i]["rows_returned"]) if i in meta else "–" for i in (p_id, s_id, j_id))
        ix = " / ".join(idx_label(meta, i, t) if i in meta else "–" for i, t in zip((p_id, s_id, j_id), tables))
        lines.append(f"| {tail} — {desc} | {' | '.join(cells)} | {ratio(vals[1], vals[0])} | "
                     f"{ratio(vals[2], vals[0])} | {ratio(vals[2], vals[1])} | {rr} | {ix} |")
    return "\n".join(lines)


def render_noise():
    a = {}
    b = {}
    for tag, d in [("_run1", a), ("", b)]:
        med, _, _ = load_tier("flat", 10000, tag)
        d.update(med)
    pairs = []
    for k in sorted(set(a) & set(b)):
        pairs.append((k, a[k], b[k], (b[k] - a[k]) / a[k] * 100))
    lines = ["| scenario | run 1 | run 2 | change |", "|---|--:|--:|--:|"]
    for k, x, y, d in pairs:
        name = k.replace("JSON_COMPARE_V3/small/", "SteVec ").replace("JSON_TREE/json_tree/", "json_tree ") \
            .replace("JSON_TREE/plain/", "plaintext ").replace("/10000", "")
        lines.append(f"| {name} | {fmt_ms(x)} | {fmt_ms(y)} | {d:+.1f}% |")
    abs_d = [abs(p[3]) for p in pairs]
    summary = {
        "scenarios": len(pairs),
        "median_abs_pct": statistics.median(abs_d) if abs_d else None,
        "p90_abs_pct": sorted(abs_d)[int(0.9 * (len(abs_d) - 1))] if abs_d else None,
        "max_abs_pct": max(abs_d) if abs_d else None,
        "over_10pct": sum(1 for x in abs_d if x > 10),
    }
    return "\n".join(lines), summary


def mib(b):
    return f"{b / 2**20:,.1f}"


def render_storage():
    st = json.loads((OUT / "storage.json").read_text())
    gin = {(g["table"]): g for g in json.loads((OUT / "gin_rebuild.json").read_text())}
    lines = [
        "| corpus | rows | system | avg column, bytes (`pg_column_size`) | avg column, chars (`::text`) | heap incl. TOAST, MiB (`pg_table_size`) | of which TOAST, MiB | containment GIN, MiB | GIN build, s | other indexes, MiB | total, MiB |",
        "|---|--:|---|--:|--:|--:|--:|--:|--:|--:|--:|",
    ]
    for corpus in ["flat", "nested"]:
        for n in TIERS:
            for system in SYSTEMS:
                r = next(x for x in st if x["system"] == system and x["corpus"] == corpus and x["rows"] == n)
                g = gin[r["table"]]
                other = r["indexes_bytes"] - r["gin_bytes"]
                lines.append(
                    f"| {corpus} | {n:,} | {system} | {r['avg_column_bytes']:,.0f} | {r['avg_text_chars']:,.0f} | "
                    f"{mib(r['table_bytes'])} | {mib(r['toast_bytes'])} | {mib(r['gin_bytes'])} | "
                    f"{g['build_seconds']:.1f} | {mib(other)} | {mib(r['total_bytes'])} |")
    return "\n".join(lines)


def render_correctness():
    lines = []
    for n in TIERS:
        p = OUT / f"stevec_correctness_{n}.json"
        if not p.exists():
            continue
        d = json.loads(p.read_text())
        lines += [f"**{n:,} rows** (`check_json_v3_twin`, {d['needles_per_predicate']} needles per predicate)", "",
                  "| check | needles | agree | disagree | extra rows | missing rows | control rows |",
                  "|---|--:|--:|--:|--:|--:|--:|"]
        for k, v in d["checks"].items():
            lines.append(f"| {k} | {v['needles']} | {v['agree']} | {v['disagree']} | "
                         f"{v.get('extra_rows', '')} | {v.get('missing_rows', '')} | {v.get('control_rows', '')} |")
        lines.append("")
    return "\n".join(lines)


LONG_LITERAL = re.compile(r"'((?:[^']|'')*)'")


def shorten_literals(plan):
    """Plans of custom-planned executions inline the bound operands, some of
    them kilobytes of hex. Keep the first 100 characters of any literal over
    200 and say how long it was; the full plans are in explain_<N>.json."""
    def cut(m):
        body = m.group(1)
        if len(body) <= 200:
            return m.group(0)
        return "'" + body[:100] + f"…[{len(body):,} chars]'"
    return LONG_LITERAL.sub(cut, plan)


BLANK_REASONS = {
    "a": "no query of its own: native jsonb has one containment spelling, `@>`, measured in the operator row.",
    "b": "not built: the native equivalent would be a btree on `plain ->> 'email'`; the containment rows are the native comparison.",
    "c": "no query of its own: SteVec has no walk and no budget; `->` on the container returns the subtree sealed whole, which is the default-budget row.",
    "d": "not expressible: json_tree has no equality operator on an entry; equality at a path is containment (`field_eq/unique`).",
}


def blank_key(system, tail):
    return {"plaintext": PLAIN_BLANK, "SteVec": STEVEC_BLANK, "json_tree": V4_BLANK}[system].get(tail)


def render_appendix(n):
    ex = json.loads((OUT / f"explain_{n}.json").read_text())
    by_tail = {}
    for e in ex:
        sid = e["id"]
        if sid.startswith("JSON_COMPARE_V3/"):
            _, c, rest = sid.split("/", 2)
            system = "SteVec"
            corpus = "flat" if c == "small" else "nested"
        else:
            g, sysname, rest = sid.split("/", 2)
            system = "plaintext" if sysname == "plain" else "json_tree"
            corpus = "flat" if g == "JSON_TREE" else "nested"
        tail = rest.rsplit("/", 1)[0]
        by_tail.setdefault((corpus, tail), {})[system] = e
    out = []
    for corpus, rows in [("flat", FLAT_ROWS), ("nested", NESTED_ROWS)]:
        out.append(f"### {corpus.capitalize()} corpus, {n:,} rows\n")
        for tail, desc in rows:
            got = by_tail.get((corpus, tail), {})
            out.append(f"#### {corpus}: {tail} — {desc}\n")
            for system in SYSTEMS:
                e = got.get(system)
                if not e:
                    out.append(f"*{system}:* {BLANK_REASONS.get(blank_key(system, tail), 'not measured.')}\n")
                    continue
                kinds = ", ".join(f"${i + 1} {t}" for i, t in enumerate(e["parameter_types"])) or "no parameters"
                warm = (f"after {e['warm_executions']} warm executions of the prepared statement"
                        if e["warm_executions"] else "single execution (slow scenario)")
                out.append(f"*{system}* — parameters: {kinds}; EXPLAIN {warm}.\n")
                out.append("```sql\n" + e["query"] + "\n```\n")
                out.append("```\n" + shorten_literals(e["plan"]) + "\n```\n")
    return "\n".join(out)


def render_param_samples(n):
    """One line per parameter kind, with a truncated example, for the method section."""
    ex = json.loads((OUT / f"explain_{n}.json").read_text())
    seen = {}
    for e in ex:
        for t, p in zip(e["parameter_types"], e["parameters"]):
            key = (e["id"].split("/")[0], t)
            if key not in seen:
                s = json.dumps(p)
                seen[key] = s[:160] + ("…" if len(s) > 160 else "")
    return seen


def render(dirname):
    d = Path(dirname)
    d.mkdir(parents=True, exist_ok=True)
    for corpus in ["flat", "nested"]:
        for n in TIERS:
            (d / f"results_{corpus}_{n}.md").write_text(render_results(corpus, n) + "\n")
    noise, summary = render_noise()
    (d / "noise.md").write_text(noise + "\n")
    (d / "noise.json").write_text(json.dumps(summary, indent=2) + "\n")
    (d / "storage.md").write_text(render_storage() + "\n")
    (d / "correctness.md").write_text(render_correctness() + "\n")
    (d / "appendix.md").write_text(render_appendix(100000) + "\n")
    idx = json.loads((OUT / "indexes.json").read_text())
    lines = []
    for t, rows in idx.items():
        lines.append(f"`{t}`:\n\n```")
        lines += [f"{r['def']}   -- {int(r['bytes']) / 2**20:,.1f} MiB" for r in rows]
        lines.append("```\n")
    (d / "indexes.md").write_text("\n".join(lines) + "\n")
    print(json.dumps(summary, indent=2))


def main():
    cmd = sys.argv[1] if len(sys.argv) > 1 else "help"
    if cmd == "gin-rebuild":
        gin_rebuild()
    elif cmd == "storage":
        storage()
    elif cmd == "indexes":
        indexes()
    elif cmd == "explain":
        explain(sys.argv[2] if len(sys.argv) > 2 else "100000")
    elif cmd == "render":
        render(sys.argv[2])
    else:
        print(__doc__)


if __name__ == "__main__":
    main()
