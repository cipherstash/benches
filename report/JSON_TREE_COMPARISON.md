# JSON: SteVec (EQL v3) vs json_tree (EQL v4) — fifth measurement

Three-way comparison over identical documents (native jsonb, SteVec, json_tree; stack 6adf37fcc): [JSON_TREE_VS_STEVEC_VS_PLAINTEXT.md](JSON_TREE_VS_STEVEC_VS_PLAINTEXT.md).

The JSON tree family (`public.eql_v4_json_tree`, sealed by stack-encrypt's `Json`
index) against the SteVec scheme it succeeds (`public.json`, EQL 3.0.0-alpha.3,
cipherstash-client 0.38.1) and native jsonb, at 10,000 and 100,000 rows, on one
machine (Apple M1 Max, 64 GB, PostgreSQL 17.10, `shared_buffers` 4 GB).
Query-only medians, criterion, 10 samples; no decrypt. Both EQL bundles are
installed side by side in one database (`BENCH_DB=jsonbench`), the tables of
each scheme filled the same day, every containment scenario checked against `jsonb @>` over
the plaintext twin table before timing. How it was run: `mise run
report:json-tree-compare`, and the `bench:v4:*` tasks in `mise.toml`.

This is the fourth measurement. The first (commit 076ebfc of this branch) found
three mechanisms that put the nested corpus one to four orders of magnitude off
native jsonb; the fixes landed in cipherstash/stack#1325 (stacked on #1288,
branch `perf/json-tree-walk-and-projection`), and the second (commit 57abcf9)
measured that branch. The third measures branch `feat/json-tree-ordinal-chain`
(commit dd13d800a), which shrinks the stored entry to four fields and chains the
walk by ordinal; its before/after comparison against the second measurement is
the section after next. The fourth (stack ba62e3fc5, same branch) measures
storage only: it makes the leaf ordering term opt-in per path, and the bench
now orders only `$.age` (flat) and `$.company.salary` (nested); see the
fourth measurement's section. The fifth (2026-10-25) measures
cipherstash/stack#1363 (derivable tokens and the self tag, commit 683802bcc)
against its base (edabee625); it has a report of its own,
[JSON_TREE_DERIVABLE_TOKENS.md](JSON_TREE_DERIVABLE_TOKENS.md), and the next
section summarises it. The Summary and Findings further down are the second
measurement's and quote its numbers. The Results tables are the fifth
measurement's json_tree figures.

## Fifth measurement (2026-10-25): derivable tokens

Full report, with methodology, every table and the noise floor:
[JSON_TREE_DERIVABLE_TOKENS.md](JSON_TREE_DERIVABLE_TOKENS.md).

cipherstash/stack#1363 (commit 683802bcc) stores an eight-byte self tag `x` on
every entry and drops the ordinal from an object member's token. So the client
derives the token of any path with no wildcard, a read sends one token
(`json_tree_subtree(doc, tokens text[], budget)`), and a wildcard is expanded
on the client to N + 1 tokens a level. It was measured against its base
(edabee625, the gated walk) on two new databases on this cluster
(`jsonbench_base` and `jsonbench_new`, the same plaintext twins), timed
interleaved by a new harness (`bench:dt:*`), at 10,000 rows in full and
100,000 rows for sizes and the headline reads. Per-call medians at 10,000
rows:

| | gates | derivable | change |
|---|--:|--:|--:|
| `$.user.contact.emergency_contact.name` (4 steps) | 2.48 ms | 1.93 ms | −22% |
| `$.company.name` (2 steps) | 1.97 ms | 1.98 ms | +0% |
| `$.company` (whole object) | 2.00 ms | 2.81 ms | +41% |
| `$.orders`, walked out (176 entries) | 18.1 ms | 25.9 ms | +43% |
| `$.orders`, default budget (sealed root) | 6.36 ms | 9.62 ms | +51% |
| `$.orders[*].total`, N = 64 | 5.51 ms | 2.54 ms | −54% |
| `$.orders[*].items[*].price`, N = 64 (4,161 tokens) | 17.4 ms | 25.8 ms | +48% |
| `json_tree_children`, 300-element array | 25.1 ms | 12.8 ms | −49% |
| `json_tree_child_count`, `$.orders` | 1.75 ms | 2.79 ms | +59% |
| full read of a 300-element array | 31.1 ms | 55.4 ms | +78% |
| nested `pg_column_size` per document | 31,714 B | 35,256 B | +11.2% |

Containment and `->` moved by 0 to +10%, in step with the larger document.
Both formats passed every correctness check at both tiers (gates 692 and
672, derivable 717 and 697, the difference being the read of `$` the base
refuses).

**Results tables.** The json_tree columns of the Results tables below were
re-run on the derivable format (`BENCH_DB=jsonbench_new mise run
bench:v4:query:json_tree{,_nested} <N>`). `subtree/*` and `subtree_at/items`
there are now token-list reads (`subtree_at/items` keeps its name; it is
`$.orders[*].items` at the default N = 64). The v3 SteVec and flat plaintext
columns were not re-run and are the second measurement's.

## Fourth measurement (2026-10-22): ordering term opt-in

Storage only. No query was re-timed for this measurement; the Results tables
below are still the third measurement's.

### What changed

Stack commit ba62e3fc5 (branch `feat/json-tree-ordinal-chain`) makes the leaf
ordering term `op` opt-in per path. `JsonOptions` gained `ordered`, a set of
`JsonPath`s, and no path is ordered by default. A leaf carries `op` only when
an ordered path covers it. An ordering query (`Lt`, `Lte`, `Gt`, `Gte`,
`Between`) at a path the index does not order is refused with
`TermError::JsonPathNotOrdered` before any SQL runs. Until this commit every
number and string leaf carried `op`. In the third measurement it was the
largest field of a nested document: 24,832 of 85,157 characters, on 156 of its
213 entries.

The bench now orders one path per corpus, the one its scenarios order and
range-compare on:

- flat corpus: `$.age` (`field_gt`, `field_between`, `field_order`);
- nested corpus: `$.company.salary` (`field_gt/nested`,
  `field_between/nested`, `field_order/nested`).

Nothing else is ordered. Every flat document now carries one `op` (5 entries,
was 4 with `op`), and every nested document one (213 entries, was 156). The
projection `q` and the containment index are unchanged: `q` is still 443
characters per flat document and 18,961 per nested one.

Bench crate changes: `stack-encrypt` and `eql-bindings` pinned to ba62e3fc5.
A `Corpus` enum in `json-tree-bench/src/lib.rs` holds each corpus's ordered
paths, its `JsonOptions`, its query plan
(`Plan::value().encrypt_as(Json::new(options))`) and its sealing layout
(`eql_bindings::v4::json_tree::JsonTree::new(options)`). The ingest seals one
document at a time with `keyset.run(layout.encryption(), ..)`, because the
layout has no `Vec` form. The benches and `check_json_tree_v4` query each
corpus through its own plan, and they derive subtree keys under the nested
corpus's options.

### How it was run

Same machine, cluster and database (`BENCH_DB=jsonbench`). The EQL v4 bundle
was not reinstalled, because the SQL did not change: `ord_term` was already
NULL for an entry without `op`. The four `json_tree_*_encrypted_v4_{10000,100000}`
tables were truncated and re-ingested with `mise run
prepare:json_tree_{small,large}_v4 <N>`. Truncating keeps each table's indexes
and leaves the plaintext twins alone. The ingest reseals the document each
twin already holds, so the documents are the same as in the third
measurement. Ingest time, including the GIN build: flat 10k 17 s, nested 10k
50 s, flat 100k 35 s, nested 100k 7 min 9 s. The prepare task built the GIN
after the load. The bench-built `field_order_idx` btree was maintained during
the load, so it was then `REINDEX`ed to match a fresh build. Each table was
then `VACUUM (ANALYZE)`d.

Sizes come from the query in `report_json_three_way.py storage`:
`avg(pg_column_size(value))`, `avg(length(value::text))`, `pg_table_size`
(heap with TOAST), the TOAST relation's total size, the containment GIN's
`pg_relation_size` and `pg_indexes_size`. "Before" is the third measurement's
row in `results/query/compare/storage.json`. Before truncating, the live
tables were measured again and matched those rows exactly.

### Sizes

| | before (`op` on every number and string leaf) | after (`op` on the ordered path only) | change |
|---|--:|--:|--:|
| flat `pg_column_size`, 10k / 100k | 2,250 B / 2,250 B | 1,596 B / 1,596 B | −29% |
| flat `::text` chars, 10k / 100k | 2,132 / 2,132 | 1,474 / 1,474 | −31% |
| flat heap incl. TOAST, 10k | 27.1 MiB (26.5 MiB TOAST) | 17.0 MiB (0.0 MiB TOAST) | −37% |
| flat heap incl. TOAST, 100k | 269.8 MiB (264.8 MiB TOAST) | 170.1 MiB (0.0 MiB TOAST) | −37% |
| flat containment GIN, 10k / 100k | 0.98 MiB / 7.09 MiB | 1.03 MiB / 7.93 MiB | +5% / +12% |
| flat other indexes (pkey + `field_order_idx`), 10k / 100k | 0.33 MiB / 2.95 MiB | 0.33 MiB / 2.95 MiB | 0 |
| flat total, 10k / 100k | 28.4 MiB / 279.9 MiB | 18.4 MiB / 181.0 MiB | −35% |
| nested `pg_column_size`, 10k / 100k | 62,143 B / 62,092 B | 40,578 B / 40,549 B | −35% |
| nested `::text` chars, 10k / 100k | 86,821 / 86,743 | 60,045 / 59,998 | −31% |
| nested heap incl. TOAST, 10k | 620.9 MiB | 407.5 MiB | −34% |
| nested heap incl. TOAST, 100k | 6,201.2 MiB | 4,069.1 MiB | −34% |
| nested containment GIN, 10k / 100k | 39.9 MiB / 358.2 MiB | 39.9 MiB / 358.2 MiB | 0 |
| nested other indexes, 10k / 100k | 1.16 MiB / 10.03 MiB | 1.16 MiB / 10.03 MiB | 0 |
| nested total, 10k / 100k | 662.0 MiB / 6,569.4 MiB | 448.6 MiB / 4,437.3 MiB | −32% |

A nested document is now 40.5 KB on disk. In the three-way report SteVec is
108.5 KB and plaintext 2.2 KB, so json_tree is now 0.37× SteVec, down from
0.57×, and 19× plaintext, down from 29×. A flat document is 1.6 KB against
SteVec's 1.9 KB.

Two side effects:

- **The flat document no longer goes to TOAST.** At 1,596 bytes it is under
  PostgreSQL's ~2 KB TOAST threshold, so it is stored in the main heap.
  Before, it was a TOAST pointer in a 0.5 MiB main fork. Now the main fork is
  17 MiB at 10k rows and 170 MiB at 100k. Finding 4 below rests on the column
  being TOASTed: a sequential scan reads only the small main fork until it
  detoasts. The flat timings, the sequential-scan ones above all, may move
  and need re-measuring.
- **The flat GIN grew 5% at 10k and 12% at 100k with the same projection.**
  A GIN posting list stores row addresses as deltas, and those deltas are
  wider when rows are spread over 34 times as many heap pages. A `REINDEX` reproduces
  the size. The nested documents are still TOASTed and their GIN is
  byte-for-byte the same size.

Raw: `results/query/compare/storage.json`. The four new rows carry
`"measurement": "fourth: ..."` and `"ordered"`. The earlier json_tree rows are
labelled `"third: ..."`. `report_json_three_way.py render` takes the first row
per system, corpus and tier, so the three-way report still renders the third
measurement's sizes, which match its timings. Running
`report_json_three_way.py storage` again overwrites the file with the live
tables, which are now the fourth measurement's.

### Correctness

`mise run check:v4:json_tree <N>` ran after the re-ingest. Its flat checks
query through the `$.age`-ordered plan and its nested checks through the
`$.company.salary`-ordered plan.

| tier | result |
|---|---|
| 10,000 | `json_tree v4 correctness, 10000 rows: 462 passed, 0 failed` |
| 100,000 | `json_tree v4 correctness, 100000 rows: 462 passed, 0 failed` |

The checks and counts are the same as in the third measurement, ordering
checks included: `field_gt`, `field_between` and `field_order` on `$.age`, and
`field_gt` and `field_order` on `$.company.salary`.

## Third measurement (2026-10-22): ordinal-chained walk

### What changed

The stack branch `feat/json-tree-ordinal-chain` (commit dd13d800a) changes the
stored wire and the walk:

- A stored entry in `sv` carries four fields: `s` (path selector), `c`
  (ciphertext), `p` (parent tag, below the chain heads) and `op` (OPE term, on
  number and string leaves). `vs`, `o`, `w`, `n` and `g` are gone; the
  projection `q` is unchanged and is now the only place value selectors live.
- A child's token is derived on the server as `H16("t" ‖ t(parent) ‖ LE32(o))`.
  Ordinals under a node are dense from 0, so the walk probes 0, 1, 2, … and
  stops at the first miss: a subtree of N nodes costs about 2N hashes instead of
  the document's widest fan-out per visited node.
- `JsonSubtreeKey` has no `selector()`; `gates()` returns one gate per step
  below the derivable token. `json_tree_subtree_at(doc, token, gates text[],
  budget)`, and `json_tree_children` / `json_tree_child_count` take optional
  gates. `$.orders[*].items` is now `$.orders`'s token with the gates
  `[s($.orders[*]), s($.orders[*].items)]`.

### How it was run

Same machine, cluster and database (`BENCH_DB=jsonbench`) as the second
measurement. The v4 bundle was rebuilt from the stack tree at dd13d800a (`mise
run --force build` in `packages/eql`; byte-identical to the cached build),
the old `eql_v4` / `eql_v4_internal` schemas and the `public.eql_v4_*` domains
and validators were dropped, and the new bundle installed. Every
`json_tree_*_encrypted_v4_*` table was dropped and re-ingested; the v3 objects
and tables were not touched, so the v3 and flat-plaintext columns in the
Results tables are the second measurement's.

The plaintext twin tables were kept, and the ingest now reseals the document a
twin already holds for an id instead of generating a new one. So both runs
measure the same documents, and the benches sample the same rows and derive the
same needles: every scenario returns the same number of rows before and after
(the right-hand column below), and the flat corpus's operands, SQL and plans
are identical in the metadata sidecars. The ingest also seals under
stack-kms's `DeterministicSource` (fixed seed) instead of `FakeDataKeySource`:
the index key, and so every selector, token and ordering term, is the same, but
the data keys are now a function of the seed, so a later process can open a
stored row. That is what the correctness check below relies on.

Bench crate changes: `stack-encrypt` and `eql-bindings` point at the new
branch; `subtree_at/items` binds the gates as `$2::text[]`; a correctness
binary (`check_json_tree_v4`, `mise run check:v4:json_tree <N>`).

### Before and after (medians)

"Before" is the second measurement (commit 57abcf9, branch
`perf/json-tree-walk-and-projection`), "after" is this run. Query-only, no
decrypt, criterion medians over 10 samples. A change above +10% is marked as a
regression.

#### Flat corpus, 10,000 rows

| scenario | before | after | change | rows before → after |
|---|--:|--:|--:|--:|
| contains/functional | 0.49 ms | 0.53 ms | +8% | 1 → 1 |
| contains/operator | 0.50 ms | 0.54 ms | +7% | 1 → 1 |
| field_eq/extractor | 0.54 ms | 0.59 ms | +9% | 6 → 6 |
| field_eq/unique | 0.42 ms | 0.47 ms | +13% **regression** | 1 → 1 |
| field_gt/functional | 0.11 ms | 0.10 ms | -5% | 10 → 10 |
| field_between/functional | 0.21 ms | 0.21 ms | +1% | 10 → 10 |
| field_order/functional | 0.96 ms | 0.87 ms | -10% | 10 → 10 |
| extract/by_id | 0.20 ms | 0.19 ms | -6% | 1 → 1 |

#### Flat corpus, 100,000 rows

| scenario | before | after | change | rows before → after |
|---|--:|--:|--:|--:|
| contains/functional | 0.48 ms | 0.53 ms | +10% **regression** | 1 → 1 |
| contains/operator | 0.47 ms | 0.50 ms | +7% | 1 → 1 |
| field_eq/extractor | 1.07 ms | 1.16 ms | +8% | 10 → 10 |
| field_eq/unique | 0.25 ms | 0.26 ms | +1% | 1 → 1 |
| field_gt/functional | 0.11 ms | 0.10 ms | -7% | 10 → 10 |
| field_between/functional | 0.21 ms | 0.22 ms | +5% | 10 → 10 |
| field_order/functional | 0.97 ms | 0.87 ms | -10% | 10 → 10 |
| extract/by_id | 0.20 ms | 0.19 ms | -6% | 1 → 1 |

#### Nested corpus, 10,000 rows

| scenario | before | after | change | rows before → after |
|---|--:|--:|--:|--:|
| contains/deep_object | 3.23 ms | 2.89 ms | -10% | 10 → 10 |
| contains/array_element | 0.70 ms | 0.75 ms | +7% | 1 → 1 |
| contains/nested_array | 3.94 ms | 3.48 ms | -12% | 10 → 10 |
| field_eq/nested | 0.50 ms | 0.52 ms | +3% | 1 → 1 |
| field_gt/nested | 0.11 ms | 0.11 ms | -2% | 10 → 10 |
| field_order/nested | 4.81 ms | 3.81 ms | -21% | 10 → 10 |
| subtree/company | 2.84 ms | 1.99 ms | -30% | 6 → 6 |
| subtree/user | 3.90 ms | 2.38 ms | -39% | 12 → 12 |
| subtree/orders | 9.48 ms | 5.71 ms | -40% | 1 → 1 |
| subtree_at/items | 12.95 ms | 8.21 ms | -37% | 1 → 1 |
| subtree/orders_walked | 22.97 ms | 10.92 ms | -52% | 122 → 122 |
| subtree_at/items_walked | 23.12 ms | 10.93 ms | -53% | 87 → 87 |
| children/orders | 1.63 ms | 1.54 ms | -6% | 7 → 7 |
| child_count/orders | 1.62 ms | 1.53 ms | -6% | 1 → 1 |
| path_query/wildcard | 2.24 ms | 1.64 ms | -27% | 7 → 7 |
| extract/nested | 0.59 ms | 0.47 ms | -19% | 1 → 1 |

#### Nested corpus, 100,000 rows

| scenario | before | after | change | rows before → after |
|---|--:|--:|--:|--:|
| contains/deep_object | 7.41 ms | 7.15 ms | -3% | 10 → 10 |
| contains/array_element | 2.04 ms | 2.11 ms | +3% | 1 → 1 |
| contains/nested_array | 9.54 ms | 9.49 ms | -1% | 10 → 10 |
| field_eq/nested | 0.58 ms | 0.55 ms | -5% | 1 → 1 |
| field_gt/nested | 0.11 ms | 0.11 ms | -1% | 10 → 10 |
| field_order/nested | 5.23 ms | 4.12 ms | -21% | 10 → 10 |
| subtree/company | 2.94 ms | 1.96 ms | -33% | 6 → 6 |
| subtree/user | 3.99 ms | 2.45 ms | -39% | 12 → 12 |
| subtree/orders | 8.77 ms | 5.46 ms | -38% | 1 → 1 |
| subtree_at/items | 10.92 ms | 7.18 ms | -34% | 1 → 1 |
| subtree/orders_walked | 24.57 ms | 11.35 ms | -54% | 130 → 130 |
| subtree_at/items_walked | 24.83 ms | 11.27 ms | -55% | 105 → 105 |
| children/orders | 1.64 ms | 1.49 ms | -9% | 5 → 5 |
| child_count/orders | 1.66 ms | 1.52 ms | -9% | 1 → 1 |
| path_query/wildcard | 1.80 ms | 1.41 ms | -22% | 5 → 5 |
| extract/nested | 0.63 ms | 0.54 ms | -15% | 1 → 1 |

The flat 10k tier was run twice; the table holds the second run. The first had
contains/functional 0.57 ms (+15%), contains/operator 0.56 ms (+13%),
field_eq/extractor 0.60 ms (+11%) and field_eq/unique 0.47 ms (+11%); the other
four scenarios were within 8% of the second run.

**What got cheaper, and why.** The walks the change was aimed at: walked out,
`subtree/orders` and `subtree_at/items` are 52 to 55% faster at both tiers
(23 to 25 ms down to 11 ms for 87 to 130 entries), and under the default budget
(which returns the sealed root) 34 to 40% faster; `subtree/company` and
`subtree/user` 30 to 39%. Scenarios the walk does not touch improved too:
`path_query/wildcard` 22 to 27%, `field_order/nested` 21%, `extract/nested` 15
to 19%. Those read entries through `->` and the path functions, which scan a
`sv` that is 30% shorter and a datum that is 24% smaller to decompress.

`children/orders` and `child_count/orders` improved only 6 to 9%, less than
expected. Hypothesis: they are dominated by the fixed per-call cost (detoasting
the 62 KB document and building the parent map over all ~213 entries), which
the change does not touch; probing 5 to 7 children instead of the document's
widest fan-out saves a few dozen hashes on top of it.

**Regressions over 10%.** Two, both on the flat corpus's GIN-served
containment: `field_eq/unique` at 10k (0.42 to 0.47 ms, +13%) and
`contains/functional` at 100k (0.48 to 0.53 ms, +10%); in the first 10k run all
four containment scenarios were +11 to +15%. Hypothesis: not the format. The
flat corpus's operand, SQL text and plan are byte-identical before and after,
the projection `q` the GIN indexes and rechecks is unchanged, the GIN index is
the same size, and flat rows occupy the same heap pages (three per 8 KB page
either way). The native jsonb containment controls on the nested corpus, over
unchanged plaintext with a freshly rebuilt GIN index, moved by similar amounts
(deep_object 0.36 to 0.42 ms and nested_array 0.36 to 0.43 ms at 10k, +17 and
+19%; 1.91 to 2.07 ms and 1.51 to 1.59 ms at 100k, +8 and +5%), which points at
the machine or the freshly built indexes rather than the change. The 100k
nested GIN build went from 58 s to 65 s (+12%, one sample) with the same
projection and a smaller heap; same hypothesis.

### Sizes

Character counts are averaged over ids 1 to 500 of the 10k tier (the same
documents before and after), counting field values only. `pg_column_size` is
averaged over the whole tier; heap includes TOAST.

| | before | after | change |
|---|--:|--:|--:|
| flat document, chars | 2,346 | 2,136 | −9% |
| flat `sv`, chars (5 entries) | 1,730 | 1,520 | −12% |
| flat `q`, chars | 443 | 443 | 0 |
| flat `pg_column_size`, 10k / 100k | 2,452 B / 2,452 B | 2,250 B / 2,250 B | −8% |
| flat heap, 10k / 100k | 27 MB / 270 MB | 27 MB / 270 MB | 0 (three rows per page either way) |
| flat containment GIN, 10k / 100k | 1.0 MB / 7.1 MB | 1.0 MB / 7.1 MB | 0 |
| flat GIN build, 10k / 100k | 0 s / 0 s | 0 s / 0 s | — |
| nested document, chars (213 entries) | 113,458 | 85,157 | −25% |
| nested `sv`, chars | 94,324 | 66,023 | −30% |
| nested `q`, chars | 18,961 | 18,961 | 0 |
| nested `pg_column_size`, 10k / 100k | 81,264 B / 81,207 B | 62,143 B / 62,092 B | −24% |
| nested heap, 10k / 100k | 810 MB / 8,093 MB | 621 MB / 6,201 MB | −23% |
| nested containment GIN, 10k / 100k | 40 MB / 358 MB | 40 MB / 358 MB | 0 |
| nested GIN build, 10k / 100k | 5 s / 58 s | 5 s / 65 s | 0 / +12% |
| nested plaintext, chars | 4,024 | 4,024 | — |

Per-field characters of one nested document (average over the same 500
documents; entries carrying the field in brackets):

| field | before | after |
|---|--:|--:|
| `s` | 6,819 (213) | 6,819 (213) |
| `c` | 20,021 (213) | 20,016 (213) |
| `p` | 6,659 (208) | 6,659 (208) |
| `op` | 24,832 (156) | 24,832 (156) |
| `vs` | 6,819 (213) | — |
| `w` | 6,659 (208) | — |
| `g` | 5,785 (180) | — |
| `n` | 1,356 (42) | — |
| `o` | 208 (208) | — |
| `q` (whole projection) | 18,961 | 18,961 |

The `sv` shrank by 30% where about 15% was expected: the five removed fields
carried 20,827 characters of values per document, and their keys and quotes
another 7,500. The nested document is now 21× its plaintext in characters
(was 28×) and 26× in heap size (was 34×). The largest field is now `op`: the CLLW
OPE term, 159 characters on each of the 156 number and string leaves.

### Correctness

`check_json_tree_v4` (`mise run check:v4:json_tree <N>`), run against both
tiers after ingest. Results in `results/query/v4/json_tree_correctness_<N>.json`.

(a) Walks, nested corpus: 25 rows in a fixed pseudo-random order. For each, the
rows the SQL returns are decrypted and compared with the plaintext twin:
`json_tree_subtree` at `$.company`, `$.user` and `$.orders`, walked out (budget
0) and under the default budget (opening the sealed root with `.at(path)` when
the walk fell back); `json_tree_subtree_at` for `$.user.contact` (one gate) and
`$.orders[*].items` (two gates) walked out, and the latter under the default
budget; `json_tree_children` and `json_tree_child_count` at `$.orders` against
the array's length (every child at `d = 1`); every order by its
position-derived token, `$.orders[k]`; `json_tree_path_query` at
`$.orders[*].status`, each entry opened; and `->` at `$.company.salary`. 14
checks per row, 350 in all.

(b) Predicates: for 10 sampled rows per predicate, the full id set (no LIMIT)
of the encrypted query against native jsonb over the plaintext twin, and for
`ORDER BY … LIMIT 10` the ten key values. Nested corpus: the three containment
needles, `field_eq/nested`, `field_gt/nested`, `field_order/nested` (51
checks). Flat corpus: `contains/functional`, `contains/operator`,
`field_eq` at `$.first_name` and `$.email`, `field_gt`, `field_between`,
`field_order` (61 checks).

| tier | walks (a) | predicates (b) | total |
|---|--:|--:|--:|
| 10,000 | 350 pass, 0 fail | 112 pass, 0 fail | 462 pass, 0 fail |
| 100,000 | 350 pass, 0 fail | 112 pass, 0 fail | 462 pass, 0 fail |

The nested bench's own startup oracle (encrypted `@>` against plaintext `@>`
for its three needles) also agreed at both tiers.

## Summary of the second measurement

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

The fifth measurement's json_tree numbers (derivable tokens, stack 683802bcc,
database `jsonbench_new`, 2026-10-25). Query-only medians (criterion, 10
samples), no decrypt. Delta is v4 relative to v3; negative is faster. The v3
SteVec and flat plaintext columns were not re-run and are the second
measurement's. The nested plaintext column was re-run with the json_tree one.
`subtree/orders` and `subtree_at/items` return the sealed root under the
default budget (one row). The `_walked` twins call the same read with the
budget off and return every entry. Each `subtree/*` read now returns its
node at `d = 0` too (`subtree/company` 7 rows, was 6). The third
measurement's tables, against the ordinal-chained walk, are in this file at
commit cd9643d, and its before/after section above quotes them.

### 10,000 rows

| scenario | v3 SteVec | v4 json_tree | v4 vs v3 | plaintext jsonb | v4 rows | v4 index | v3 index |
|---|--:|--:|--:|--:|--:|---|---|
| contains/functional | 0.23 ms | 0.49 ms | +111% | 0.10 ms | 1 | query_gin_idx | jsonb_array_index |
| contains/operator *(v4-only)* | — | 0.49 ms | — | 0.10 ms | 1 | query_gin_idx | — |
| field_eq/extractor | 0.81 ms | 0.52 ms | -37% | 0.20 ms | 6 | query_gin_idx | seq scan |
| field_eq/unique *(v4-only)* | — | 0.41 ms | — | 0.20 ms | 1 | query_gin_idx | — |
| field_gt/functional | 0.81 ms | 0.15 ms | -82% | — | 10 | field_order_idx | field_order_idx |
| field_between/functional *(v4-only)* | — | 0.20 ms | — | — | 10 | field_order_idx | — |
| field_order/functional | 0.25 ms | 0.78 ms | +211% | — | 10 | field_order_idx | field_order_idx |
| extract/by_id *(v4-only)* | — | 0.16 ms | — | — | 1 | pkey | — |

v3-only scenarios (no v4 surface): `field_eq/bare`, `field_eq/functional`

### 100,000 rows

| scenario | v3 SteVec | v4 json_tree | v4 vs v3 | plaintext jsonb | v4 rows | v4 index | v3 index |
|---|--:|--:|--:|--:|--:|---|---|
| contains/functional | 0.25 ms | 0.41 ms | +63% | 0.20 ms | 1 | query_gin_idx | jsonb_array_index |
| contains/operator *(v4-only)* | — | 0.41 ms | — | 0.20 ms | 1 | query_gin_idx | — |
| field_eq/extractor | 6.06 ms | 1.12 ms | -82% | 0.22 ms | 10 | query_gin_idx | seq scan |
| field_eq/unique *(v4-only)* | — | 0.36 ms | — | 0.22 ms | 1 | query_gin_idx | — |
| field_gt/functional | 0.86 ms | 0.16 ms | -82% | — | 10 | field_order_idx | field_order_idx |
| field_between/functional *(v4-only)* | — | 0.21 ms | — | — | 10 | field_order_idx | — |
| field_order/functional | 0.24 ms | 0.76 ms | +210% | — | 10 | field_order_idx | field_order_idx |
| extract/by_id *(v4-only)* | — | 0.17 ms | — | — | 1 | pkey | — |

v3-only scenarios (no v4 surface): `field_eq/bare`, `field_eq/functional`

### Nested corpus, 10,000 rows (no SteVec twin; baseline is native jsonb over the same rows)

| scenario | v4 json_tree | plaintext jsonb | v4 / plain | rows | v4 index | plain index |
|---|--:|--:|--:|--:|---|---|
| contains/deep_object | 1.92 ms | 0.37 ms | 5.2× | 10 | query_gin_idx | gin_idx |
| contains/array_element | 0.62 ms | 0.08 ms | 7.5× | 1 | query_gin_idx | gin_idx |
| contains/nested_array | 2.62 ms | 0.36 ms | 7.2× | 10 | query_gin_idx | gin_idx |
| field_eq/nested | 0.44 ms | 0.08 ms | 5.6× | 1 | query_gin_idx | gin_idx |
| field_gt/nested | 0.08 ms | 0.13 ms | 0.7× | 10 | field_order_idx | seq scan |
| field_order/nested | 2.37 ms | 0.12 ms | 19.5× | 10 | field_order_idx | salary_idx |
| subtree/company | 2.39 ms | 0.08 ms | 29.8× | 7 | pkey | pkey |
| subtree/user | 3.13 ms | 0.08 ms | 39.4× | 13 | pkey | pkey |
| subtree/orders | 9.19 ms | 0.09 ms | 103.1× | 1 | pkey | pkey |
| subtree_at/items | 8.96 ms | 0.09 ms | 100.0× | 1 | pkey | pkey |
| subtree/orders_walked | 17.89 ms | 0.08 ms | 227.3× | 123 | pkey | pkey |
| subtree_at/items_walked | 12.94 ms | 0.08 ms | 156.5× | 87 | pkey | pkey |
| children/orders | 1.50 ms | 0.09 ms | 17.6× | 7 | pkey | pkey |
| child_count/orders | 2.29 ms | 0.07 ms | 33.6× | 1 | pkey | pkey |
| path_query/wildcard | 1.19 ms | 0.08 ms | 14.9× | 7 | pkey | pkey |
| extract/nested | 0.39 ms | 0.07 ms | 5.3× | 1 | pkey | pkey |

### Nested corpus, 100,000 rows (no SteVec twin; baseline is native jsonb over the same rows)

| scenario | v4 json_tree | plaintext jsonb | v4 / plain | rows | v4 index | plain index |
|---|--:|--:|--:|--:|---|---|
| contains/deep_object | 6.13 ms | 1.96 ms | 3.1× | 10 | query_gin_idx | gin_idx |
| contains/array_element | 1.83 ms | 0.13 ms | 14.6× | 1 | query_gin_idx | gin_idx |
| contains/nested_array | 8.09 ms | 1.56 ms | 5.2× | 10 | query_gin_idx | gin_idx |
| field_eq/nested | 0.46 ms | 0.10 ms | 4.5× | 1 | query_gin_idx | gin_idx |
| field_gt/nested | 0.10 ms | 0.18 ms | 0.6× | 10 | field_order_idx | seq scan |
| field_order/nested | 2.41 ms | 0.14 ms | 17.3× | 10 | field_order_idx | salary_idx |
| subtree/company | 2.35 ms | 0.09 ms | 25.5× | 7 | pkey | pkey |
| subtree/user | 3.06 ms | 0.09 ms | 33.0× | 13 | pkey | pkey |
| subtree/orders | 8.91 ms | 0.10 ms | 85.9× | 1 | pkey | pkey |
| subtree_at/items | 8.92 ms | 0.10 ms | 85.1× | 1 | pkey | pkey |
| subtree/orders_walked | 18.27 ms | 0.10 ms | 175.6× | 131 | pkey | pkey |
| subtree_at/items_walked | 14.98 ms | 0.10 ms | 144.1× | 105 | pkey | pkey |
| children/orders | 1.43 ms | 0.11 ms | 13.5× | 5 | pkey | pkey |
| child_count/orders | 2.24 ms | 0.09 ms | 24.8× | 1 | pkey | pkey |
| path_query/wildcard | 0.93 ms | 0.10 ms | 9.6× | 5 | pkey | pkey |
| extract/nested | 0.38 ms | 0.09 ms | 4.2× | 1 | pkey | pkey |

### Sizes (second measurement)

The third measurement's sizes are in its section above.

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
fake key source's data keys do not leave the ingest process; the third
measurement's ingest seals under a deterministic source, and its correctness
check decrypts, but decryption is still not timed); ingest throughput
against ZeroKMS; the 1M tier (the nested corpus is 8 GB at 100k); any corpus
with wide arrays of scalars.
