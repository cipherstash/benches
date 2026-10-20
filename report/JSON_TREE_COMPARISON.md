# JSON: SteVec (EQL v3) vs json_tree (EQL v4) — first measurement

The JSON tree family (`public.eql_v4_json_tree`, sealed by stack-encrypt's `Json`
index, stack branch `feat/json-index-stack-encrypt` at e4023f261) against the
SteVec scheme it succeeds (`public.json`, EQL 3.0.0-alpha.3, cipherstash-client
0.38.1) and native jsonb, at 10,000 rows, on one machine (Apple M1 Max, 64 GB,
PostgreSQL 17.10, `shared_buffers` 4 GB). Query-only medians, criterion, 10
samples; no decrypt. Both EQL bundles are installed side by side in one
database (`BENCH_DB=jsonbench`), every table filled the same day, every
containment scenario checked against `jsonb @>` over the plaintext twin table
before timing. How it was run: `mise run report:json-tree-compare`, and the
`bench:v4:*` tasks in `mise.toml`.

Sealing is under an in-process fake key source, so the ingest numbers are not
compared here (the v3 ingest pays a ZeroKMS call per batch); the query numbers
are, since the cost under measurement is Postgres's.

## Summary

On the flat corpus the two schemes are in the same band and v4 wins on index
size; on anything nested, v4 as shipped is one to four orders of magnitude off
plaintext, and three mechanisms account for all of it. Each has a fix that was
tried here and restores the number:

| finding | as shipped | after the fix |
|---|--:|--:|
| `json_tree_subtree('$.orders')`, 244 entries, by id | 10.3 s | 141 ms — parent tags computed once per (node, ordinal) instead of per sv element |
| `@>` on a 218-entry document, LIMIT 10 / full set (1,976 rows) | 75 ms / 15.9 s | 1.3 ms / 161 ms — the projection stored, not recomputed in the recheck |
| `(value -> sel) > $1` under a prepared statement (generic plan) | 16.9 ms, seq scan | 0.12 ms, index — `ALTER FUNCTION eql_v4.json_tree_path_query_first COST 1000` |

## Results


Query-only medians (criterion, 10 samples), no decrypt. Delta is v4 relative to v3; negative is faster.

### 10,000 rows

| scenario | v3 SteVec | v4 json_tree | v4 vs v3 | plaintext jsonb | v4 rows | v4 index | v3 index |
|---|--:|--:|--:|--:|--:|---|---|
| contains/functional | 0.23 ms | 0.47 ms | +102% | 0.10 ms | 1 | query_gin_idx | jsonb_array_index |
| contains/operator *(v4-only)* | — | 0.47 ms | — | 0.10 ms | 1 | query_gin_idx | — |
| field_eq/extractor | 0.81 ms | 0.70 ms | -14% | 0.20 ms | 5 | query_gin_idx | seq scan |
| field_eq/unique *(v4-only)* | — | 0.37 ms | — | 0.20 ms | 1 | query_gin_idx | — |
| field_gt/functional | 0.81 ms | 16.90 ms | +1997% | — | 10 | field_order_idx | field_order_idx |
| field_between/functional *(v4-only)* | — | 0.20 ms | — | — | 10 | field_order_idx | — |
| field_order/functional | 0.25 ms | 0.92 ms | +266% | — | 10 | field_order_idx | field_order_idx |
| extract/by_id *(v4-only)* | — | 0.18 ms | — | — | 1 | pkey | — |

v3-only scenarios (no v4 surface): `field_eq/bare`, `field_eq/functional`

### Nested corpus, 10,000 rows (no SteVec twin; baseline is native jsonb over the same rows)

| scenario | v4 json_tree | plaintext jsonb | v4 / plain | rows | v4 index | plain index |
|---|--:|--:|--:|--:|---|---|
| contains/deep_object | 68.41 ms | 0.28 ms | 248.5× | 10 | query_gin_idx | gin_idx |
| contains/array_element | 12.68 ms | 0.11 ms | 117.2× | 1 | query_gin_idx | gin_idx |
| contains/nested_array | 82.15 ms | 0.35 ms | 232.7× | 10 | query_gin_idx | gin_idx |
| field_eq/nested | 12.86 ms | 0.11 ms | 122.2× | 1 | query_gin_idx | gin_idx |
| field_gt/nested | 0.10 ms | 0.15 ms | 0.7× | 10 | field_order_idx | seq scan |
| field_order/nested | 4.46 ms | 0.15 ms | 29.2× | 10 | field_order_idx | salary_idx |
| subtree/company | 292.47 ms | 0.10 ms | 2871.9× | 6 | pkey | pkey |
| subtree/user | 539.80 ms | 0.10 ms | 5189.3× | 12 | pkey | pkey |
| subtree/orders | 10248.60 ms | 0.13 ms | 80289.0× | 244 | pkey | pkey |
| subtree_at/items | 10223.95 ms | 0.14 ms | 72601.3× | 194 | pkey | pkey |
| children/orders | 7.05 ms | 0.13 ms | 54.7× | 10 | pkey | pkey |
| child_count/orders | 6.99 ms | 0.10 ms | 67.3× | 1 | pkey | pkey |
| path_query/wildcard | 4.20 ms | 0.11 ms | 37.5× | 10 | pkey | pkey |
| extract/nested | 0.79 ms | 0.10 ms | 8.0× | 1 | pkey | pkey |


Plaintext baseline for the flat corpus is `json_small_plaintext_10000`
(`value @> '{"age": n}'` and `value -> 'age' = $1`, jsonb_path_ops GIN); for
the nested corpus it is the plaintext twin table itself (same rows). The v4
`field_gt/functional` row is the measured number; see finding 3 for why it is
not the plan the EXPLAIN sidecar shows.

### Sizes, 10,000 rows

| | v3 SteVec | v4 json_tree | plaintext |
|---|--:|--:|--:|
| flat document, chars | 1,890 | 1,893 | — |
| flat heap | 16 MB | 14 MB | — |
| flat containment GIN | 11 MB (`eql_v3.jsonb_array`) | 1.0 MB (`eql_v4.json_tree_query`) | — |
| flat containment GIN build | ~1 s | ~1 s | — |
| nested document, chars (218 entries avg) | — | 96,596 | 4,034 |
| nested heap + TOAST | — | 820 MB | 64 MB |
| nested containment GIN | — | 40 MB, **44 s** to build | 39 MB, ~2 s |

The nested document is 24× its plaintext: ~440 chars per entry (six 32-hex
tags, a base64 AES-GCM ciphertext with a 12-byte nonce and 16-byte tag per
node) plus the sealed-root copy of the whole document. The design doc's
estimate was ~230 chars per leaf.

## Findings

### 1. The token-chain walk recomputes every parent tag per sv element

`eql_v4.json_tree_subtree` (and `_subtree_at`, `_children`, `_child_count`) finds
a node's children by computing, for each ordinal under the fan-out, the parent
tag a child at that ordinal would carry, and matching it against `p` across
`sv`. The SQL puts `json_tree_parent_tag(iv, w.t, o)` inside the LATERAL that
scans `sv`, so it is evaluated once per (node, ordinal, element): for the
sampled row (fan-out 10, 274 entries, 244 nodes walked) that is ~670k
sha256 + `json_tree_xor16` calls, the latter a plpgsql byte loop. 10.3 s.

Computing the fan-out's tags once per node and joining them against a
per-document `p → entry` map (a CTE over `sv`, no other change) returns the
identical 244 entries in 141 ms. That is still ~1 ms per node — the xor loop
and the recursive CTE's per-level re-scan remain — but it is the difference
between unusable and usable. `children/orders` (7 ms) and `child_count` (7 ms)
have the same shape and the same fix.

### 2. Containment pays the projection on every recheck, and the GIN build pays it per row

`eql_v4.json_tree_contains` inlines to `json_tree_query(value)::jsonb @> needle`,
which is what makes the GIN expression index engage — and it is also what the
executor evaluates for the lossy GIN recheck, and again for the Filter (the OR
with the `alt` branch is not provably equal to the recheck condition, so both
run). `json_tree_query` is a recursive plpgsql projection over `sv` — about
3 ms for a 218-entry document, ~12 ms per row while building the index or a
generated column. On the flat corpus (5 entries) this is invisible; on the
nested one it is the whole cost: 75 ms to return 10 rows, 15.9 s for the
1,976-row result set, 44 s to build the index, versus 0.3 ms / 161 ms / 2 s
with the projection materialised.

A `STORED` generated column holding `json_tree_query(value)::jsonb` (7 KB per
row here, 68 MB), indexed directly, gives the plaintext-shaped recheck. The
client already derives the projection shape for the needle; having it emit the
stored projection alongside the document (or EQL defining the column that way)
removes plpgsql from the read path entirely. Also worth doing: the `alt`
branch of `json_tree_contains` always plans a second GIN probe (`BitmapOr`)
even when the needle carries no `alt`; cheap, but visible in every plan.

### 3. The range operator loses its index under a prepared statement

`(value -> sel) > $1::eql_v4.query_json_tree_leaf` inlines to
`decode(json_tree_path_query_first(value, sel) ->> 'op', 'hex') > decode(...)`.
With a literal (what `EXPLAIN` in the sidecar ran) the planner picks the
functional btree. After five executions of a prepared statement — which is
what sqlx, and every driver, does — PostgreSQL switches to a generic plan,
estimates the predicate at 1/3 of the table, and takes a seq scan with the
inlined expression as a Filter, because an inlined SQL function's cost is
its arithmetic, not the `jsonb_array_elements` scan it hides. 16.9 ms, the
one scenario where v4 measures far behind v3 (0.81 ms, which keeps its index
in both plan modes: `eql_v3.ore_cllw` is not inlined that way).

`ALTER FUNCTION eql_v4.json_tree_path_query_first(jsonb, text) COST 1000`
makes the generic plan take the index: 0.12 ms, five times faster than v3.
`field_between` (two bounds) already measured 0.20 ms because its estimate
was selective enough. `field_order` (0.92 ms vs v3 0.25 ms) is a real index
scan both ways; the gap is not yet explained (96 buffers for 10 rows) and is
worth a look alongside the `->` cost.

### 4. The rest

- Flat containment: v4 0.47 ms vs v3 0.23 ms vs plaintext 0.10 ms. Both are a
  GIN bitmap scan plus a recheck; v4's recheck runs `json_tree_query` on the
  five entries (finding 2 at small scale), v3's `jsonb_array` is a plain SQL
  function. Equality at a path through containment is on par (0.70 vs 0.81
  ms; the unique-key form 0.37 ms). v4's containment index is 11× smaller.
- `->` extraction is 0.18 ms on a flat document and 0.79 ms on a nested one:
  `json_tree_path_query_first` scans `sv` with `jsonb_array_elements` on every
  call. An index on `sv` elements by `s` would make it a lookup.
- The two v3 scenarios with no v4 surface are `field_eq/bare` and
  `field_eq/functional` (entry `=`): equality at a path is containment under
  v4, by design.

## Not measured yet

Decryption (the fake key source's data keys do not leave the ingest process);
ingest throughput against ZeroKMS; the 100k and 1M tiers (the nested corpus
is 820 MB at 10k, and the index build is 44 s per 10k rows until finding 2 is
fixed); any corpus with wide arrays of scalars.
