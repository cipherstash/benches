#!/usr/bin/env python3
"""JSON scheme comparison: SteVec (EQL v3 `public.json`) vs the JSON tree
family (EQL v4 `public.eql_v4_json_tree`, stack-encrypt's Json index), with
the plaintext jsonb baseline alongside.

Reads the criterion result files and metadata sidecars:

    v3 SteVec:   results/query/v3/json_rows_<N>.json        ids JSON/json/<scenario>/<N>
    v4 tree:     results/query/v4/json_tree_rows_<N>.json   ids JSON_TREE/json_tree/<scenario>/<N>
    plaintext:   results/query/v3/plaintext_rows_<N>.json   ids PLAINTEXT/plaintext/json_*/<N>

and prints one table per tier (median latency, v4-vs-v3 delta, indexes the
planner used). Scenarios share a tail where they are twins (the v4 bench's
module docs say which); the rest are listed as v4-only.

Usage:
    python3 report_json_tree_compare.py [--output report/JSON_TREE_COMPARISON.md]
"""

import argparse
import json
import re
from pathlib import Path

ROOT = Path(__file__).parent
V3_DIR = ROOT / "results" / "query" / "v3"
V4_DIR = ROOT / "results" / "query" / "v4"

# v4 scenario tail -> v3 twin tail (same semantics, different SQL surface).
TWINS = {
    "contains/functional": "contains/functional",
    "field_eq/extractor": "field_eq/extractor",
    "field_gt/functional": "field_gt/functional",
    "field_order/functional": "field_order/functional",
}
# v4 scenario tail -> plaintext scenario.
PLAINTEXT = {
    "contains/functional": "json_contains",
    "contains/operator": "json_contains",
    "field_eq/extractor": "json_field_eq",
    "field_eq/unique": "json_field_eq",
}
V4_ORDER = [
    "contains/functional",
    "contains/operator",
    "field_eq/extractor",
    "field_eq/unique",
    "field_gt/functional",
    "field_between/functional",
    "field_order/functional",
    "extract/by_id",
]


def load_medians(path: Path) -> dict:
    """criterion `--message-format json` lines -> {id: median_ns}."""
    out = {}
    if not path.exists():
        return out
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        if rec.get("reason") == "benchmark-complete":
            out[rec["id"]] = rec["median"]["estimate"]
    return out


def load_metadata(path: Path) -> dict:
    if not path.exists():
        return {}
    data = json.loads(path.read_text())
    return {s["id"]: s for s in data.get("scenarios", [])}


def tiers() -> list:
    found = set()
    for f in V4_DIR.glob("json_tree_rows_*.json"):  # not json_tree_nested_rows_*
        m = re.match(r"json_tree_rows_(\d+)\.json$", f.name)
        if m:
            found.add(int(m.group(1)))
    return sorted(found)


def ms(ns):
    return "—" if ns is None else f"{ns / 1e6:.2f} ms"


def delta(v4, v3):
    if v4 is None or v3 is None:
        return "—"
    pct = (v4 - v3) / v3 * 100
    return f"{pct:+.0f}%"


def render(tier: int) -> list:
    v4 = load_medians(V4_DIR / f"json_tree_rows_{tier}.json")
    v3 = load_medians(V3_DIR / f"json_rows_{tier}.json")
    pt = load_medians(V3_DIR / f"plaintext_rows_{tier}.json")
    v4_meta = load_metadata(V4_DIR / f"json_tree_metadata_{tier}.json")
    v3_meta = load_metadata(V3_DIR / f"json_metadata_{tier}.json")

    lines = [
        f"### {tier:,} rows",
        "",
        "| scenario | v3 SteVec | v4 json_tree | v4 vs v3 | plaintext jsonb | v4 rows | v4 index | v3 index |",
        "|---|--:|--:|--:|--:|--:|---|---|",
    ]
    for tail in V4_ORDER:
        v4_id = f"JSON_TREE/json_tree/{tail}/{tier}"
        if v4_id not in v4:
            continue
        twin = TWINS.get(tail)
        v3_id = f"JSON/json/{twin}/{tier}" if twin else None
        pt_id = f"PLAINTEXT/plaintext/{PLAINTEXT[tail]}/{tier}" if tail in PLAINTEXT else None
        v3_ns = v3.get(v3_id) if v3_id else None
        pt_ns = pt.get(pt_id) if pt_id else None
        m4 = v4_meta.get(v4_id, {})
        m3 = v3_meta.get(v3_id, {}) if v3_id else {}
        idx4 = ", ".join(i.replace(f"json_tree_small_encrypted_v4_{tier}_", "") for i in m4.get("indexes_used", [])) or "seq scan"
        idx3 = ", ".join(i.replace(f"json_ste_vec_small_encrypted_v3_{tier}_", "") for i in m3.get("indexes_used", [])) or ("seq scan" if m3 else "—")
        label = tail if twin else f"{tail} *(v4-only)*"
        lines.append(
            f"| {label} | {ms(v3_ns)} | {ms(v4.get(v4_id))} | {delta(v4.get(v4_id), v3_ns)} | "
            f"{ms(pt_ns)} | {m4.get('rows_returned', '—')} | {idx4} | {idx3} |"
        )
    # v3 scenarios with no v4 twin.
    orphans = sorted(
        k.split("/", 2)[2].rsplit("/", 1)[0]
        for k in v3
        if k.endswith(f"/{tier}") and k.split("/", 2)[2].rsplit("/", 1)[0] not in TWINS.values()
    )
    if orphans:
        lines += ["", "v3-only scenarios (no v4 surface): " + ", ".join(f"`{o}`" for o in orphans)]
    lines.append("")
    return lines


NESTED_ORDER = [
    "contains/deep_object",
    "contains/array_element",
    "contains/nested_array",
    "field_eq/nested",
    "field_gt/nested",
    "field_order/nested",
    "subtree/company",
    "subtree/user",
    "subtree/orders",
    "subtree_at/items",
    "children/orders",
    "child_count/orders",
    "path_query/wildcard",
    "extract/nested",
]


def render_nested(tier: int) -> list:
    """The nested corpus: json_tree against the native jsonb twin (same rows)."""
    res = load_medians(V4_DIR / f"json_tree_nested_rows_{tier}.json")
    meta = load_metadata(V4_DIR / f"json_tree_nested_metadata_{tier}.json")
    if not res:
        return []
    lines = [
        f"### Nested corpus, {tier:,} rows (no SteVec twin; baseline is native jsonb over the same rows)",
        "",
        "| scenario | v4 json_tree | plaintext jsonb | v4 / plain | rows | v4 index | plain index |",
        "|---|--:|--:|--:|--:|---|---|",
    ]
    for tail in NESTED_ORDER:
        e_id = f"JSON_TREE_NESTED/json_tree/{tail}/{tier}"
        p_id = f"JSON_TREE_NESTED/plain/{tail}/{tier}"
        if e_id not in res:
            continue
        e, p = res.get(e_id), res.get(p_id)
        ratio = "—" if e is None or p is None else f"{e / p:.1f}×"
        me, mp = meta.get(e_id, {}), meta.get(p_id, {})
        idx_e = ", ".join(i.replace(f"json_tree_large_encrypted_v4_{tier}_", "") for i in me.get("indexes_used", [])) or "seq scan"
        idx_p = ", ".join(i.replace(f"json_tree_large_plain_v4_{tier}_", "") for i in mp.get("indexes_used", [])) or "seq scan"
        lines.append(f"| {tail} | {ms(e)} | {ms(p)} | {ratio} | {me.get('rows_returned', '—')} | {idx_e} | {idx_p} |")
    lines.append("")
    return lines


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", type=Path, help="also write the markdown here")
    args = ap.parse_args()
    out = ["## JSON: SteVec (v3) vs json_tree (v4) vs plaintext", "",
           "Query-only medians (criterion, 10 samples), no decrypt. Delta is v4 relative to v3; negative is faster.", ""]
    for tier in tiers():
        out += render(tier)
    for tier in sorted({int(m.group(1)) for f in V4_DIR.glob("json_tree_nested_rows_*.json")
                        if (m := re.match(r"json_tree_nested_rows_(\d+)\.json$", f.name))}):
        out += render_nested(tier)
    text = "\n".join(out)
    print(text)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text + "\n")
        print(f"\nwritten to {args.output}")


if __name__ == "__main__":
    main()
