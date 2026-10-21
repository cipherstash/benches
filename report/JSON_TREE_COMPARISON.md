# JSON: SteVec (EQL v3) vs json_tree (EQL v4) — second measurement

The JSON tree family (`public.eql_v4_json_tree`, sealed by stack-encrypt's `Json`
index) against the SteVec scheme it succeeds (`public.json`, EQL 3.0.0-alpha.3,
cipherstash-client 0.38.1) and native jsonb, at 10,000 and 100,000 rows, on one
machine (Apple M1 Max, 64 GB, PostgreSQL 17.10, `shared_buffers` 4 GB).
Query-only medians, criterion, 10 samples; no decrypt. Both EQL bundles are
installed side by side in one database (`BENCH_DB=jsonbench`), every table
filled the same day, every containment scenario checked against `jsonb @>` over
the plaintext twin table before timing. How it was run: `mise run
report:json-tree-compare`, and the `bench:v4:*` tasks in `mise.toml`.

This is the second measurement. The first (commit 076ebfc of this branch) found
three mechanisms that put the nested corpus one to four orders of magnitude off
native jsonb; the fixes landed in cipherstash/stack#1325 (stacked on #1288,
branch `perf/json-tree-walk-and-projection`), and this run measures that
branch. The first run's numbers are quoted below where they explain a change.

Sealing is under an in-process fake key source, so the ingest numbers are not
compared here (the v3 ingest pays a ZeroKMS call per batch); the query numbers
are, since the cost under measurement is Postgres's.

## Summary

On the flat corpus v4 is in the same band as v3 and ahead on the operations
that matter most: equality at a path is 33% faster at 10k and 82% faster at
100k (v3 has no index for it), a range at a path is 87% faster at both tiers,
and the containment index is a tenth the size. v4 is behind on `ORDER BY` at a
path (0.96 ms against 0.25 ms, both index scans) and on bare containment of a
whole small document (0.49 against 0.23 ms), two of the four items left open in
cipherstash/stack#1324.

On the nested corpus, which v3 cannot index at all without the cross-element
false positives of #1231, every operation is now served by an index and sits
within one order of magnitude of native jsonb except the subtree walks, which
are a different operation (per-node hashing on the server) and sit at 3 to
13 ms per row, or 23 ms walked out in full:

| scenario, nested corpus, 10k rows | first run | this run | native jsonb |
|---|--:|--:|--:|
| `@>` deep object, LIMIT 10 | 68 ms | 3.2 ms | 0.36 ms |
| `@>` one array element, 1 row | 12.7 ms | 0.70 ms | 0.10 ms |
| equality at a nested path, 1 row | 12.9 ms | 0.50 ms | 0.10 ms |
| `json_tree_subtree('$.company')`, 6 entries | 292 ms | 2.8 ms | 0.10 ms |
| `json_tree_subtree('$.orders')`, 244 entries | 10.2 s | 9.5 ms (the sealed root) / 23 ms walked out | 0.11 ms |
| `json_tree_children('$.orders')` | 7.1 ms | 1.6 ms | 0.11 ms |
| containment GIN build | 44 s | 5 s | ~2 s |

What changed, in one line each (details under Findings): the document carries
its containment projection on the wire, so the GIN recheck reads a field
instead of running recursive plpgsql over every entry; the walk looks children
up in one map keyed by parent tag instead of rescanning the document per node;
the walk has a budget past which it returns the sealed root; the extractor
behind `->` and the projection reader are costed so the planner keeps their
indexes; and a fourth mechanism the re-measurement found, that a TOASTed column
makes a sequential scan look free, is closed by keeping the projection reader
out of the planner's inlining.

## Results

Query-only medians (criterion, 10 samples), no decrypt. Delta is v4 relative to
v3; negative is faster. `subtree/orders` and `subtree_at/items` return the
sealed root under the default budget (one row); the `_walked` twins call the
same functions with the budget off and return every entry.

### 10,000 rows

| scenario | v3 SteVec | v4 json_tree | v4 vs v3 | plaintext jsonb | v4 rows | v4 index | v3 index |
|---|--:|--:|--:|--:|--:|---|---|
| contains/functional | 0.23 ms | 0.49 ms | +114% | 0.10 ms | 1 | query_gin_idx | jsonb_array_index |
| contains/operator *(v4-only)* | — | 0.50 ms | — | 0.10 ms | 1 | query_gin_idx | — |
| field_eq/extractor | 0.81 ms | 0.54 ms | -33% | 0.20 ms | 6 | query_gin_idx | seq scan |
| field_eq/unique *(v4-only)* | — | 0.42 ms | — | 0.20 ms | 1 | query_gin_idx | — |
| field_gt/functional | 0.81 ms | 0.11 ms | -87% | — | 10 | field_order_idx | field_order_idx |
| field_between/functional *(v4-only)* | — | 0.21 ms | — | — | 10 | field_order_idx | — |
| field_order/functional | 0.25 ms | 0.96 ms | +285% | — | 10 | field_order_idx | field_order_idx |
| extract/by_id *(v4-only)* | — | 0.20 ms | — | — | 1 | pkey | — |

v3-only scenarios (no v4 surface): `field_eq/bare`, `field_eq/functional`

### 100,000 rows

| scenario | v3 SteVec | v4 json_tree | v4 vs v3 | plaintext jsonb | v4 rows | v4 index | v3 index |
|---|--:|--:|--:|--:|--:|---|---|
| contains/functional | 0.25 ms | 0.48 ms | +88% | 0.20 ms | 1 | query_gin_idx | jsonb_array_index |
| contains/operator *(v4-only)* | — | 0.47 ms | — | 0.20 ms | 1 | query_gin_idx | — |
| field_eq/extractor | 6.06 ms | 1.07 ms | -82% | 0.22 ms | 10 | query_gin_idx | seq scan |
| field_eq/unique *(v4-only)* | — | 0.25 ms | — | 0.22 ms | 1 | query_gin_idx | — |
| field_gt/functional | 0.86 ms | 0.11 ms | -87% | — | 10 | field_order_idx | field_order_idx |
| field_between/functional *(v4-only)* | — | 0.21 ms | — | — | 10 | field_order_idx | — |
| field_order/functional | 0.24 ms | 0.97 ms | +296% | — | 10 | field_order_idx | field_order_idx |
| extract/by_id *(v4-only)* | — | 0.20 ms | — | — | 1 | pkey | — |

v3-only scenarios (no v4 surface): `field_eq/bare`, `field_eq/functional`

### Nested corpus, 10,000 rows (no SteVec twin; baseline is native jsonb over the same rows)

| scenario | v4 json_tree | plaintext jsonb | v4 / plain | rows | v4 index | plain index |
|---|--:|--:|--:|--:|---|---|
| contains/deep_object | 3.23 ms | 0.36 ms | 8.9× | 10 | query_gin_idx | gin_idx |
| contains/array_element | 0.70 ms | 0.10 ms | 6.8× | 1 | query_gin_idx | gin_idx |
| contains/nested_array | 3.94 ms | 0.36 ms | 11.0× | 10 | query_gin_idx | gin_idx |
| field_eq/nested | 0.50 ms | 0.10 ms | 5.0× | 1 | query_gin_idx | gin_idx |
| field_gt/nested | 0.11 ms | 0.14 ms | 0.8× | 10 | field_order_idx | seq scan |
| field_order/nested | 4.81 ms | 0.13 ms | 35.8× | 10 | field_order_idx | salary_idx |
| subtree/company | 2.84 ms | 0.10 ms | 28.9× | 6 | pkey | pkey |
| subtree/user | 3.90 ms | 0.10 ms | 39.1× | 12 | pkey | pkey |
| subtree/orders | 9.48 ms | 0.11 ms | 89.6× | 1 | pkey | pkey |
| subtree_at/items | 12.95 ms | 0.11 ms | 121.0× | 1 | pkey | pkey |
| subtree/orders_walked | 22.97 ms | 0.11 ms | 216.4× | 122 | pkey | pkey |
| subtree_at/items_walked | 23.12 ms | 0.11 ms | 215.7× | 87 | pkey | pkey |
| children/orders | 1.63 ms | 0.11 ms | 14.9× | 7 | pkey | pkey |
| child_count/orders | 1.62 ms | 0.10 ms | 16.7× | 1 | pkey | pkey |
| path_query/wildcard | 2.24 ms | 0.10 ms | 21.9× | 7 | pkey | pkey |
| extract/nested | 0.59 ms | 0.10 ms | 6.1× | 1 | pkey | pkey |

### Nested corpus, 100,000 rows (no SteVec twin; baseline is native jsonb over the same rows)

| scenario | v4 json_tree | plaintext jsonb | v4 / plain | rows | v4 index | plain index |
|---|--:|--:|--:|--:|---|---|
| contains/deep_object | 7.41 ms | 1.91 ms | 3.9× | 10 | query_gin_idx | gin_idx |
| contains/array_element | 2.04 ms | 0.12 ms | 17.1× | 1 | query_gin_idx | gin_idx |
| contains/nested_array | 9.54 ms | 1.51 ms | 6.3× | 10 | query_gin_idx | gin_idx |
| field_eq/nested | 0.58 ms | 0.10 ms | 5.8× | 1 | query_gin_idx | gin_idx |
| field_gt/nested | 0.11 ms | 0.17 ms | 0.6× | 10 | field_order_idx | seq scan |
| field_order/nested | 5.23 ms | 0.14 ms | 36.8× | 10 | field_order_idx | salary_idx |
| subtree/company | 2.94 ms | 0.10 ms | 30.1× | 6 | pkey | pkey |
| subtree/user | 3.99 ms | 0.10 ms | 40.4× | 12 | pkey | pkey |
| subtree/orders | 8.77 ms | 0.11 ms | 81.8× | 1 | pkey | pkey |
| subtree_at/items | 10.92 ms | 0.11 ms | 100.8× | 1 | pkey | pkey |
| subtree/orders_walked | 24.57 ms | 0.11 ms | 231.0× | 130 | pkey | pkey |
| subtree_at/items_walked | 24.83 ms | 0.11 ms | 229.5× | 105 | pkey | pkey |
| children/orders | 1.64 ms | 0.11 ms | 15.0× | 5 | pkey | pkey |
| child_count/orders | 1.66 ms | 0.10 ms | 17.0× | 1 | pkey | pkey |
| path_query/wildcard | 1.80 ms | 0.10 ms | 17.5× | 5 | pkey | pkey |
| extract/nested | 0.63 ms | 0.10 ms | 6.4× | 1 | pkey | pkey |

Plaintext baseline for the flat corpus is `json_small_plaintext_<N>` (`value @>
'{"age": n}'` and `value -> 'age' = $1`, jsonb_path_ops GIN); for the nested
corpus it is the plaintext twin table itself (same rows). `field_gt/functional`
is now the index at both tiers under the prepared statement's generic plan
(finding 3 of the first run).

### Sizes

| | v3 SteVec | v4 json_tree | plaintext |
|---|--:|--:|--:|
| flat document, chars | 1,893 | 2,343 (443 of it the projection `q`) | — |
| flat heap + TOAST, 10k / 100k | 21 MB / 209 MB | 27 MB / 270 MB | — |
| flat containment GIN, 10k / 100k | 11 MB / 92 MB | 1.0 MB / 7.1 MB | — |
| nested document, chars (215 entries avg) | — | 114,309 (19,100 of it `q`) | 3,977 |
| nested heap + TOAST, 10k / 100k | — | 810 MB / 8.1 GB | 24 MB / 240 MB |
| nested containment GIN, 10k / 100k | — | 40 MB, 5 s / 358 MB, 58 s | 39 MB, ~2 s / 355 MB |

The projection adds about 18% to the document in characters and nothing
measurable on disk (the hex pairs compress against `sv`). The nested document
is 29× its plaintext in characters and 34× on disk: ~440 characters per entry
(six 32-hex tags, a base64 AES-GCM ciphertext with a 12-byte nonce and 16-byte
tag per node), the sealed-root copy of the whole document, and now the
projection. The levers are hex-to-base64url on the sixteen-byte fields and the
opaque-subtree policy in the design document; filed in cipherstash/stack#1324.

## Findings

### 1. The walk recomputed every parent tag per sv element (fixed)

`eql_v4.json_tree_subtree` found a node's children by computing, for each
ordinal under the fan-out, the parent tag a child at that ordinal would carry,
and matching it against `p` across `sv`, with the hash inside the LATERAL that
scanned `sv`: once per (node, ordinal, entry), about 670,000 SHA-256
evaluations for the sampled row's 244-entry subtree, 10.2 s. The walk now
builds one jsonb object of the chained entries keyed by `p` and looks each
computed tag up in it (`eql_v4_internal.json_tree_by_parent`), so a level costs
its fan-out in hashes plus one lookup each: 23 ms for the same 244 entries,
2.8 ms for six. `children` and `child_count` share the map: 1.6 ms from 7.

Past that the cost is the design's: fan-out hashes per node visited at about
15 µs each, plus the domain cast per returned entry. The fixed part (reading
the header, building the map, the `->` accesses on a 110 KB compressed datum)
is about 2 ms per row.

### 2. The walk has a budget and returns the sealed root past it (new)

With the root sealed, the root's ciphertext is the whole document at about 1.3×
the plaintext, where the walk returns about 440 characters per entry and pays
the hashes above. Past a dozen or so entries the whole document is cheaper on
both sides. `json_tree_subtree` and `json_tree_subtree_at` take a `budget`
(default 64) and, once the walk passes it, return the root entry alone with
`d = 0`; the client opens it and takes the path (`JsonSubtree::at(path)`).
`subtree/orders` is 9.5 ms this way against 23 ms walked out; `subtree_at/items`
13 ms against 23 ms. The fallback still walks up to the budget before it knows,
which is where its cost goes; a budget of 0 turns it off, a token that names no
node still returns nothing, and an unsealed root is always walked out.

### 3. Containment paid the projection on every recheck (fixed)

`eql_v4.json_tree_contains` inlined to `json_tree_query(value) @> needle`, and
`json_tree_query` was a recursive plpgsql projection over `sv` that ran once per
candidate row in the lossy GIN recheck and again in the Filter: about 3 ms per
220-entry document, which was the whole cost of containment on the nested corpus
(68 ms to return ten rows, 44 s to build the index). The client already derives
that shape for the needle, so the document now writes it to the wire as `q`
beside `sv`; the domain CHECK validates it, and `json_tree_query` reads it.
Containment is 3.2 ms for ten deep-object rows (0.36 ms native), 0.5 to 0.7 ms
for one row, and the index builds in 5 s per 10k rows.

### 4. A TOASTed column makes the sequential scan look free (new, fixed)

The first re-run with `json_tree_query` as an inlinable SQL function returned
every nested containment scenario as a sequential scan: 1.5 s for a one-row
match at 10k, 17 s at 100k, with the index present and usable. Inlined to
`value -> 'q'` the filter is costed as one operator per row, and a column of
110 KB documents is TOASTed out of the heap, so the main relation is a few
hundred pages and the planner costs a scan of it at 414 against a bitmap scan
at 247 plus a recheck it also thinks is free. It is not: every row is
decompressed to read `q`. The flat corpus, whose rows sit in the heap, took the
index all along. `json_tree_query` is now plpgsql (which the planner cannot
inline) at `COST 1000`; the index expression still matches the call, and the
planner takes the index at both tiers. The per-row call is a few microseconds.

### 5. The range operator lost its index under a prepared statement (fixed)

`(value -> sel) > $1` extracts through `json_tree_path_query_first`, a SQL
function with a FROM clause (so not inlined) at the default cost. After five
executions of a prepared statement the generic plan estimated the predicate at
a third of the table and took a sequential scan with the extraction as a
Filter: 16.9 ms in the first run. At `COST 1000` the generic plan keeps the
functional btree: 0.11 ms, at both tiers.

### 6. What is left

Filed as cipherstash/stack#1324: `->` scans `sv` on every extraction (0.6 ms on
a nested document, 0.2 on a flat one, where native is 0.1); `ORDER BY
ord_term(value -> sel) LIMIT 10` is 0.96 ms against v3's 0.25 ms on an index
scan both ways, unexplained; `json_tree_contains` always plans a second GIN
probe for `alt`; and the document size above. Not measured: decryption (the
fake key source's data keys do not leave the ingest process); ingest throughput
against ZeroKMS; the 1M tier (the nested corpus is 8 GB at 100k); any corpus
with wide arrays of scalars.
