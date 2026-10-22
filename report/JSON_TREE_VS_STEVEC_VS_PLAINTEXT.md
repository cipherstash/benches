# JSON in PostgreSQL: json_tree vs SteVec vs plaintext

Three ways of storing and querying the same JSON documents in PostgreSQL,
measured side by side on one quiet machine on 2026-10-22:

1. **plaintext**: native `jsonb`, native operators, a `jsonb_path_ops` GIN.
   The control.
2. **SteVec**: EQL v3 encrypted JSON (`public.json`), sealed by
   cipherstash-client through ZeroKMS.
3. **json_tree**: EQL v4 (`public.eql_v4_json_tree`), sealed by stack-encrypt's
   `Json` index under a local deterministic keyset.

All three hold the same documents, id for id: two corpora (flat, 5 nodes per
document; nested, 213 nodes per document), each at 10,000 and 100,000 rows.

## Summary

- **Flat documents.** json_tree and SteVec are in the same band, within 0.1 to
  0.5 ms of each other on every indexed query. json_tree is faster on ranges at
  a path (0.11 ms for `>` and 0.21 ms for between, against SteVec's 0.85 and 1.4
  to 1.8 ms; most of SteVec's time there is planning). SteVec is faster on
  containment (0.23 to 0.51 ms against 0.40 to 1.09 ms), on `ORDER BY` at a path
  (0.25 against 0.79 ms) and on equality through its per-path `eq_term` btree
  (0.11 ms), for which json_tree has no equivalent. SteVec's `@>` operator has
  no index: 255 ms at 10k and 2.6 s at 100k, where json_tree's `@>` is 0.49 ms.
- **Nested documents.** Where both have an index, they are again within a
  factor of about 3 of each other. SteVec's equality and array-element
  containment are 1.4 to 3.3× faster, and json_tree's ranges 2 to 9× faster.
  json_tree uses its index for every containment needle. SteVec does not:
  its `@>` operator never does (81 ms to 143 s), and the planner skips its GIN
  for the deep-object needle in both spellings (49 to 81 ms against json_tree's
  2.6 to 6.9 ms). SteVec's `->` on a container returns that subtree in one
  ciphertext (0.5 ms), 3 to 10× cheaper than json_tree's subtree walk.
- **Native jsonb** is 0.10 to 0.14 ms on almost everything. It reaches 2.8 ms
  where it seq-scans a range, and 10 ms where the prepared statement's generic
  plan dropped its GIN (flat 100k; 0.12 ms with custom plans). The encrypted systems are 1 to 2× native on
  by-id reads of flat documents, 5 to 15× on by-id reads of nested ones, 2 to
  5× on indexed equality, and 3 to 15× on json_tree containment where native
  jsonb uses its GIN.
- **Storage.** A nested SteVec document is 108.5 KB on disk against
  json_tree's 62.1 KB and plaintext's 2.2 KB. SteVec's containment GIN at 100k
  nested rows is 5.5 GiB and took 27 minutes to build, against json_tree's
  358 MiB in 34 s and plaintext's 355 MiB in 18 s. On flat documents the
  heaps are similar (209 against 270 MiB at 100k), and SteVec's GIN is
  13× json_tree's (92 against 7.1 MiB).
- **Correctness.** json_tree passed all 462 checks at both tiers. SteVec
  returned extra rows for array-of-object containment at 100k (4 of 10 needles,
  5 rows; the known false positive, cipherstash/stack#1231), and agreed with the
  control everywhere else: 272 checks per tier, 264 agreeing at 100k and all 272
  at 10k.
- **Noise floor.** Two back-to-back runs of the flat 10k tier differ by a median
  of 0.4% per scenario (90th percentile 0.9%, maximum 9.3%). Differences under
  about 10% are not findings.

## Environment

| | |
|---|---|
| Machine | Apple M1 Max, 10 cores, 64 GB, macOS 26.5.1; the machine was reserved for the run, with nothing else running during the timing session |
| PostgreSQL | 17.10 (Homebrew), native cluster on port 5400, database `jsonbench` |
| Settings | `shared_buffers` 4 GB, `work_mem` 128 MB, `maintenance_work_mem` 4 GB, `effective_cache_size` 24 GB, `max_parallel_workers_per_gather` 2, `jit` on, `random_page_cost` 4, `plan_cache_mode` auto, TOAST compression pglz |
| EQL v3 bundle | 3.0.0-alpha.3 (`COMMENT ON SCHEMA eql_v3`), the `public.json` SteVec domain |
| EQL v4 bundle | built from cipherstash/stack `feat/json-tree-ordinal-chain` at 6adf37fcc (`COMMENT ON SCHEMA eql_v4` reads `DEV`). The installed bundle was built at dd13d800a. Its 3,417 `eql_v4` / `eql_v4_internal` function definitions and its domain constraints are identical to those of a fresh install of the tree's bundle, compared in a scratch database. The two commits differ only in SQL comments, so the bundle was not reinstalled |
| SteVec client | cipherstash-client 0.38.1, eql-bindings 0.4.2 (`from_v2` to the v3 wire), real workspace through ZeroKMS |
| json_tree client | stack-encrypt and eql-bindings from the same stack commit 6adf37fcc (`json-tree-bench/Cargo.lock`), `DeterministicSource` keyset |
| Toolchain | rustc 1.94.1, criterion 0.4 through cargo-criterion 1.1.0, sqlx 0.8 |

## Method

**Corpora.** The documents are those of the json_tree plaintext twin tables,
`json_tree_{small,large}_plain_v4_<N>`. These were filled by the json_tree ingest
(`json-tree-bench/src/lib.rs`, the root crate's `FakeJsonSmall` and
`FakeJsonLarge` shapes) and are the plaintext system's tables.

- *Flat*: `first_name`, `last_name`, `age`, `email` (5 nodes with the root).
- *Nested*: user, contact and emergency contact, company, 1 to 3 addresses,
  and 5 to 10 orders of 1 to 8 items each. That is 213 nodes on average, with
  two arrays of objects.

The json_tree tables are the existing ones, ingested on the current wire and
not re-ingested. The SteVec tables were sealed from the twins for this report,
id for id (`src/twin.rs`, binaries `encrypt_ste_vec_twin_{small,large}_v3`,
tasks `prepare:json_ste_vec_{small,large}_v3`). The flat SteVec tables that
existed before held other documents: 20 of 20 probed ids differed from the
twin. They were truncated and resealed. Afterwards `v3_twin_probe` decrypted 20
ids spread over each of the four SteVec tables, and all 80 matched the twin.

The SteVec column configuration is the existing v3 ingest's: SteVec index,
`SteVecMode::Standard`, which the alpha.3 domain needs for its `oc` terms. The
exception is the array mode, set to `ITEM | WILDCARD`, which is what
`@cipherstash/stack` configures for a searchable JSON column. The existing
ingest passes the Rust default, `ArrayIndexMode::NONE`, which indexes no array
element, so no query on `addresses` or `orders` could be expressed. `POSITION`
would make array containment depend on position, which jsonb `@>` does not.
The flat corpus has no arrays, so the mode changes nothing there.
`ITEM | WILDCARD` writes every array element twice, each copy with its own
ciphertext of the subtree below it. A nested SteVec document therefore carries
668 entries for its 213 nodes.

**Which SteVec.** This is the EQL 3.0.0-alpha.3 bundle installed in
`jsonbench`, with its `public.json` domain and the existing
`json_ste_vec_*_encrypted_v3_*` tables. The current EQL 3.0.x SteVec domain,
`public.eql_v3_json_search`, is not installed there and was not measured.
3.0.x orders a document's entries by a CLLW-OPE `op` term (client compat mode)
where alpha.3 uses CLLW-ORE `oc`, so the range and `ORDER BY` rows may differ
under it.

**Which plaintext.** The plaintext system is the twin tables. The older
`json_small_plaintext_<N>` baseline holds a different, md5-generated corpus, so
it is not used here.

**Ingest, wall clock** (SteVec only; the json_tree tables were not re-ingested).
`seal` is the binary: read the twin, encrypt through ZeroKMS in batches of 500,
insert. `GIN` is the index build that `prepare:_table` then runs.

| table | seal | GIN build | task total |
|---|--:|--:|--:|
| `json_ste_vec_small_encrypted_v3_10000` | 10 s | 1 s | 16 s |
| `json_ste_vec_small_encrypted_v3_100000` | 86 s | 11 s | 103 s |
| `json_ste_vec_large_encrypted_v3_10000` | 141 s | 141 s | 306 s |
| `json_ste_vec_large_encrypted_v3_100000` | 1,444 s | 1,634 s | 3,200 s |

The nested 10k ingest overlapped a few minutes of compilation, and the nested
100k ingest overlapped two short smoke runs of the benches, so those two seal
times are slightly pessimistic. All four completed. Raw:
`results/ingest/v3/twin_ingest_times.jsonl` and
`results/ingest/index_build_times.jsonl`.

**Scenarios.** The scenario list is the two json_tree benches'
(`json-tree-bench/benches/json_tree_v4.rs`, `json_tree_nested_v4.rs`), extended:

- the flat bench gained a plaintext twin for each scenario and a path query;
- the nested bench gained the function spelling of its three containment
  needles and a between on `$.company.salary`;
- a new SteVec bench (`benches/json_compare_v3.rs`) mirrors both, scenario for
  scenario.

All three systems use the same sampled row: the first id at or past the middle
of the table, which is id 5,000 at 10k and id 50,000 at 100k. They also use the
same plaintext needles and parameters:

- the whole sampled document;
- `first_name`, `email` and `user.email` of the sampled row;
- `age` ± 5 (clamped to 18 to 99);
- for the salary between, the sampled salary and the smallest salary in the
  corpus at least 10,000 above it;
- the needles of the nested containment scenarios, built from the sampled row's
  first address and first order.

Each system derives its own operand from these, the way a caller would:

- json_tree: `cipher.query(op::…).on(&plan)` and `json_selector` /
  `json_subtree_key`;
- SteVec: cipherstash-client `QueryOp::Default` and `QueryOp::SteVecSelector`;
  for `>`, `>=` and `<=`, the stored leaf entry of a row holding the bound,
  because the alpha.3 domain has no client-side leaf query shape;
- plaintext: the jsonb value itself.

Indexes:

- the containment GIN of each table (listed in the appendix);
- per-path functional btrees, built by the benches at startup:
  `eql_v4.ord_term(value -> sel)`,
  `eql_v3.ore_cllw(eql_v3.jsonb_path_query_first(value, sel))` and
  `eql_v3.eq_term(…)` for `$.email`, and `((plain ->> 'age')::int)` /
  `((plain -> 'company' ->> 'salary')::int)`.

**Timing.** criterion with `sample_size(10)`, the same as every bench in this
repository: 10 samples, 3 s warm-up and 5 s measurement per scenario, one
sqlx connection pool of 5, prepared statements as sqlx issues them. The tables
report the median of the 10 samples, in milliseconds, query only and with no
decryption. A scenario whose first execution took over 3 s was not given to
criterion, whose minimum of 55 iterations would run for hours. It was timed for
3 executions instead, and its median is marked [^s]. Only SteVec's `@>`
operator on the array-element needle (14 s and 143 s) fell under this rule.

Before the session every containment GIN was dropped and rebuilt once (timed;
see Storage) and every table was `VACUUM (ANALYZE)`d. The whole matrix then ran
in one quiet session, 02:10 to 02:56 UTC (`mise run bench:compare:json`), in
this order: flat 10k (twice), flat 100k, nested 10k, nested 100k. Within each
tier SteVec ran first, then json_tree with its plaintext twins in one process.

One supplementary run came after the session, described under the flat
results: the flat 100k json_tree and plaintext bench under
`plan_cache_mode = force_custom_plan`.

## Noise floor

**The noise floor is under 1%.** The flat 10k tier ran twice, back to back, at
the start of the session. Over its 27 scenarios the median absolute change
between the runs is 0.4%, the 90th percentile 0.9% and the maximum 9.3%
(json_tree `@>`, 0.54 to 0.49 ms). No scenario moved more than 10%. The tables
below hold the second run.

## Results

What each system runs, per scenario family (the exact SQL is in the appendix):

| family | plaintext | SteVec | json_tree |
|---|---|---|---|
| containment, function | — | `eql_v3.jsonb_contains(value, $1::jsonb)`, GIN on `eql_v3.jsonb_array(value)` | `eql_v4.json_tree_contains(value, $1::…query_json_tree)`, GIN on `eql_v4.json_tree_query(value)::jsonb` |
| containment, operator | `plain @> $1`, `jsonb_path_ops` GIN | `value @> $1::jsonb::eql_v3.query_jsonb`: inlines to `eql_v3.to_ste_vec_query(value)::jsonb @> $1`, no index | `value @> $1::…query_json_tree`, same plan as the function |
| field `>`, between | `(plain ->> 'age')::int > $1` | `eql_v3.jsonb_path_query_first(value, '<sel>') > $1::jsonb::public.jsonb_entry` | `(value -> '<sel>'::text) > $1::…query_json_tree_leaf` |
| `ORDER BY` a path | btree on the expression | btree on `eql_v3.ore_cllw(jsonb_path_query_first(…))` | btree on `eql_v4.ord_term(value -> '<sel>')` |
| one value by id | `plain -> 'age'` | `value -> '<sel>'::text` | `value -> '<sel>'::text` |
| path query by id | `jsonb_path_query(plain, '$…')` | `eql_v3.jsonb_path_query(value, sel)` | `eql_v4.json_tree_path_query(value::jsonb, sel)` |
| subtree by id | `plain -> 'company'` | `value -> '<sel of $.company>'`: the node's own ciphertext | `eql_v4.json_tree_subtree(value, token[, budget])` |
| subtree at a wildcard | `jsonb_path_query(plain, '$.orders[*].items')` | `eql_v3.jsonb_path_query(value, sel of $.orders[*].items)` | `eql_v4.json_tree_subtree_at(value, token, gates[, budget])` |
| children, child count | `jsonb_array_elements`, `jsonb_array_length` | `eql_v3.jsonb_path_query(value, sel of $.orders[*])`, and `count(*)` over it | `eql_v4.json_tree_children`, `eql_v4.json_tree_child_count` |

SteVec's array accessors, `eql_v3.jsonb_array_elements` and
`eql_v3.jsonb_array_length`, do not work on this shape. They accept only a
document whose root is an array, and on `value -> '<sel of $.orders>'` they
raise `cannot extract elements from non-array` and `cannot get array length of
a non-array`. SteVec's children and child count are therefore measured through
the wildcard path query `$.orders[*]`, which returns one entry per element,
each carrying the element's own ciphertext. The correctness check decrypts
those entries and compares them with the twin.

The flat corpus has no container below its root, so the subtree, wildcard,
children and child-count scenarios exist only on the nested corpus.

### Flat corpus

**json_tree and SteVec are within 0.1 to 0.5 ms of each other on every indexed
query.** Each wins a different half:

- json_tree wins ranges: 0.11 against 0.85 ms for `>`, and 0.21 against 1.4 to
  1.8 ms for between. SteVec executes these in 0.13 to 0.20 ms. The rest of its
  time is planning, 0.6 to 1.1 ms: the operand is a stored entry inlined into a
  custom plan on every execution.
- SteVec wins containment (0.23 to 0.36 against 0.46 to 0.49 ms for the whole
  document) and `ORDER BY` (0.25 against 0.79 ms, both index scans, the open
  item from the earlier report).
- SteVec's `eq_term` btree answers equality at a path in 0.11 ms; json_tree
  has no entry equality.

SteVec's `@>` operator has no index and seq-scans: 255 ms at 10k, 2.6 s at
100k. Native jsonb is 0.10 to 0.12 ms throughout, except where noted below.

#### Flat corpus, 10,000 rows

| scenario | plaintext | SteVec | json_tree | SteVec / plain | json_tree / plain | json_tree / SteVec | rows (p / S / j) | index (p / S / j) |
|---|--:|--:|--:|--:|--:|--:|--:|---|
| contains/functional — whole document, function spelling | —[^a] | 0.23 | 0.49 |  |  | 2.1× | –/1/1 | – / jsonb_array_index / query_gin_idx |
| contains/operator — whole document, `@>` | 0.10 | 255 | 0.49 | 2,457× | 4.7× | <0.01× | 1/1/1 | gin_idx / seq scan / query_gin_idx |
| field_eq/extractor — `$.first_name` = value (containment) | 0.10 | 0.19 | 0.53 | 1.8× | 5.1× | 2.8× | 6/6/6 | gin_idx / jsonb_array_index / query_gin_idx |
| field_eq/unique — `$.email` = value (containment) | 0.10 | 0.20 | 0.40 | 2.0× | 4.0× | 2.1× | 1/1/1 | gin_idx / jsonb_array_index / query_gin_idx |
| field_eq/eq_term — `$.email` = value (SteVec `eq_term` btree) | —[^b] | 0.11 | —[^d] |  |  |  | –/1/– | – / field_eq_idx / – |
| field_gt/functional — `$.age` > value | 0.10 | 0.85 | 0.11 | 8.4× | 1.1× | 0.13× | 10/10/10 | seq scan / field_order_idx / field_order_idx |
| field_between/functional — `$.age` between lo and hi | 0.12 | 1.84 | 0.21 | 15× | 1.7× | 0.11× | 10/10/10 | seq scan / field_order_idx / field_order_idx |
| field_order/functional — ORDER BY `$.age` LIMIT 10 | 0.10 | 0.26 | 0.79 | 2.6× | 7.7× | 3.0× | 10/10/10 | age_idx / field_order_idx / field_order_idx |
| extract/by_id — `$.age` of one row | 0.10 | 0.12 | 0.18 | 1.3× | 1.9× | 1.5× | 1/1/1 | pkey / pkey / pkey |
| path_query/field — path query `$.email`, one row | 0.10 | 0.12 | 0.18 | 1.3× | 1.8× | 1.5× | 1/1/1 | pkey / pkey / pkey |

#### Flat corpus, 100,000 rows

| scenario | plaintext | SteVec | json_tree | SteVec / plain | json_tree / plain | json_tree / SteVec | rows (p / S / j) | index (p / S / j) |
|---|--:|--:|--:|--:|--:|--:|--:|---|
| contains/functional — whole document, function spelling | —[^a] | 0.36 | 0.46 |  |  | 1.3× | –/1/1 | – / jsonb_array_index / query_gin_idx |
| contains/operator — whole document, `@>` | 10.4 | 2,600 | 0.46 | 250× | 0.04× | <0.01× | 1/1/1 | gin_idx / seq scan / query_gin_idx |
| field_eq/extractor — `$.first_name` = value (containment) | 3.05 | 0.51 | 1.09 | 0.17× | 0.36× | 2.1× | 10/10/10 | gin_idx / jsonb_array_index / query_gin_idx |
| field_eq/unique — `$.email` = value (containment) | 9.23 | 0.22 | 0.26 | 0.02× | 0.03× | 1.2× | 1/1/1 | gin_idx / jsonb_array_index / query_gin_idx |
| field_eq/eq_term — `$.email` = value (SteVec `eq_term` btree) | —[^b] | 0.11 | —[^d] |  |  |  | –/1/– | – / field_eq_idx / – |
| field_gt/functional — `$.age` > value | 0.10 | 0.85 | 0.11 | 8.5× | 1.1× | 0.13× | 10/10/10 | seq scan / field_order_idx / field_order_idx |
| field_between/functional — `$.age` between lo and hi | 0.12 | 1.44 | 0.21 | 12× | 1.7× | 0.14× | 10/10/10 | seq scan / field_order_idx / field_order_idx |
| field_order/functional — ORDER BY `$.age` LIMIT 10 | 0.10 | 0.24 | 0.79 | 2.4× | 7.7× | 3.3× | 10/10/10 | age_idx / field_order_idx / field_order_idx |
| extract/by_id — `$.age` of one row | 0.10 | 0.12 | 0.18 | 1.3× | 1.9× | 1.5× | 1/1/1 | pkey / pkey / pkey |
| path_query/field — path query `$.email`, one row | 0.10 | 0.12 | 0.18 | 1.3× | 1.8× | 1.5× | 1/1/1 | pkey / pkey / pkey |

**At 100k, native containment lost its GIN to the generic plan.** The
plaintext containment rows (10.4, 3.05 and 9.23 ms) are sequential scans, not
the GIN. sqlx executes a prepared statement, and after five executions the
`auto` plan cache tries a generic plan. That plan estimates `plain @> $1` at
the default selectivity (1,000 of 100,000 rows), so `LIMIT 10` makes a
sequential scan look cheaper; the appendix shows the generic plan. With the
needle inlined, the planner takes the GIN. The flat 100k json_tree + plaintext
bench was rerun once after the session, with `plan_cache_mode =
force_custom_plan` set on the database and reset afterwards. Native
containment is then 0.12 ms, and json_tree pays a custom plan per execution
(+0.15 to +0.4 ms on containment):

| scenario | session (`auto`), ms | `force_custom_plan`, ms |
|---|--:|--:|
| json_tree/contains/functional | 0.46 | 0.62 |
| json_tree/contains/operator | 0.46 | 0.63 |
| json_tree/extract/by_id | 0.18 | 0.20 |
| json_tree/field_between/functional | 0.21 | 0.21 |
| json_tree/field_eq/extractor | 1.09 | 1.50 |
| json_tree/field_eq/unique | 0.26 | 0.51 |
| json_tree/field_gt/functional | 0.11 | 0.16 |
| json_tree/field_order/functional | 0.79 | 0.79 |
| json_tree/path_query/field | 0.18 | 0.19 |
| plain/contains/operator | 10.40 | 0.12 |
| plain/extract/by_id | 0.10 | 0.11 |
| plain/field_between/functional | 0.12 | 0.12 |
| plain/field_eq/extractor | 3.05 | 0.12 |
| plain/field_eq/unique | 9.23 | 0.12 |
| plain/field_gt/functional | 0.10 | 0.11 |
| plain/field_order/functional | 0.10 | 0.10 |
| plain/path_query/field | 0.10 | 0.11 |

The encrypted systems kept their indexes under the session's `auto` mode at
both tiers. The headline ratios against plaintext at flat 100k should be read
against the custom-plan column.

### Nested corpus

**Where both encrypted systems use an index, they are within a factor of about
3.** SteVec is faster on equality at a path (0.22 to 0.27 against 0.45 to
0.51 ms) and on the array-element and nested-array needles through its
function (0.47 to 6.4 against 0.64 to 8.9 ms). json_tree is faster on `>` and
between (0.11 and 0.21 against 0.21 to 1.9 ms). `ORDER BY` is 3.5 to 4.6 ms
on both, against native's 0.13 ms.

**json_tree's containment always uses its index, and SteVec's often does not:**

- SteVec's `@>` operator never uses the GIN: 81 ms to 143 s.
- For the deep-object needle, the planner skips SteVec's GIN even through the
  function. It estimates the needle at 19% of rows and seq-scans for the
  `LIMIT 10`, which costs 49 to 81 ms against json_tree's 2.6 to 6.9 ms and
  native's 0.36 to 1.97 ms.

**By-id reads: SteVec's subtree `->` is cheapest, path reads are level.**

- Every SteVec entry carries its own ciphertext, so a subtree is one `->`:
  0.50 to 0.55 ms for `$.company`, `$.user` or the whole `$.orders` array.
- json_tree walks: 1.7 to 2.3 ms for 6 to 12 leaves, 5.1 to 5.2 ms to the
  sealed-root fallback for `$.orders`, and 10 to 10.7 ms walked out (122 to 130
  entries).
- At a wildcard, SteVec returns one sealed subtree per order (1.25 to 1.46 ms).
  json_tree falls back to the sealed root (6.8 to 7.4 ms) or walks out (9.9 to
  10.5 ms).
- Path query, children, child count and one value by id cost the same on both,
  0.46 to 1.6 ms, against native's 0.10 ms.

The SteVec subtree comes at a cost the Storage section shows: every node's
subtree is sealed again at every ancestor, so the document is 1.7× json_tree's
on disk.

#### Nested corpus, 10,000 rows

| scenario | plaintext | SteVec | json_tree | SteVec / plain | json_tree / plain | json_tree / SteVec | rows (p / S / j) | index (p / S / j) |
|---|--:|--:|--:|--:|--:|--:|--:|---|
| contains/deep_object — `@>` 4 objects deep | 0.36 | 81.0 | 2.59 | 223× | 7.1× | 0.03× | 10/10/10 | gin_idx / seq scan / query_gin_idx |
| contains/deep_object_fn — same, function spelling | —[^a] | 64.1 | 2.59 |  |  | 0.04× | –/10/10 | – / seq scan / query_gin_idx |
| contains/array_element — `@>` one address (city + state) | 0.10 | 14,122[^s] | 0.64 | 134,988× | 6.1× | <0.01× | 1/1/1 | gin_idx / seq scan / query_gin_idx |
| contains/array_element_fn — same, function spelling | —[^a] | 0.47 | 0.65 |  |  | 1.4× | –/1/1 | – / jsonb_array_index / query_gin_idx |
| contains/nested_array — `@>` order status + item product | 0.28 | 163 | 3.28 | 591× | 12× | 0.02× | 10/10/10 | gin_idx / seq scan / query_gin_idx |
| contains/nested_array_fn — same, function spelling | —[^a] | 0.99 | 3.27 |  |  | 3.3× | –/10/10 | – / jsonb_array_index / query_gin_idx |
| field_eq/nested — `$.user.email` = value (containment) | 0.10 | 0.22 | 0.45 | 2.2× | 4.4× | 2.0× | 1/1/1 | gin_idx / jsonb_array_index / query_gin_idx |
| field_gt/nested — `$.company.salary` > value | 0.14 | 0.21 | 0.11 | 1.5× | 0.76× | 0.50× | 10/10/10 | seq scan / field_order_idx / field_order_idx |
| field_between/nested — `$.company.salary` between lo and hi | 2.77 | 1.90 | 0.22 | 0.69× | 0.08× | 0.12× | 10/10/10 | seq scan / field_order_idx / field_order_idx |
| field_order/nested — ORDER BY `$.company.salary` LIMIT 10 | 0.13 | 4.05 | 3.50 | 30× | 26× | 0.86× | 10/10/10 | salary_idx / field_order_idx / field_order_idx |
| extract/nested — `$.company.salary` of one row | 0.10 | 0.50 | 0.46 | 5.0× | 4.6× | 0.92× | 1/1/1 | pkey / pkey / pkey |
| path_query/wildcard — `$.orders[*].status`, one row | 0.11 | 1.52 | 1.58 | 14× | 15× | 1.0× | 7/7/7 | pkey / pkey / pkey |
| subtree/company — subtree `$.company` (6 leaves) | 0.10 | 0.50 | 1.72 | 5.0× | 17× | 3.5× | 1/1/6 | pkey / pkey / pkey |
| subtree/user — subtree `$.user` (12 leaves) | 0.10 | 0.50 | 2.15 | 4.8× | 21× | 4.3× | 1/1/12 | pkey / pkey / pkey |
| subtree/orders — subtree `$.orders`, default budget | 0.11 | 0.50 | 5.06 | 4.6× | 46× | 10× | 1/1/1 | pkey / pkey / pkey |
| subtree/orders_walked — subtree `$.orders`, budget 0 | 0.11 | —[^c] | 10.0 |  | 92× |  | 1/–/122 | pkey / – / pkey |
| subtree_at/items — `$.orders[*].items`, default budget | 0.11 | 1.46 | 7.38 | 13× | 67× | 5.1× | 7/7/1 | pkey / pkey / pkey |
| subtree_at/items_walked — `$.orders[*].items`, budget 0 | 0.11 | —[^c] | 9.85 |  | 89× |  | 7/–/87 | pkey / – / pkey |
| children/orders — children of `$.orders` | 0.11 | 1.51 | 1.41 | 14× | 13× | 0.93× | 7/7/7 | pkey / pkey / pkey |
| child_count/orders — child count of `$.orders` | 0.10 | 1.49 | 1.46 | 15× | 15× | 0.98× | 1/1/1 | pkey / pkey / pkey |

#### Nested corpus, 100,000 rows

| scenario | plaintext | SteVec | json_tree | SteVec / plain | json_tree / plain | json_tree / SteVec | rows (p / S / j) | index (p / S / j) |
|---|--:|--:|--:|--:|--:|--:|--:|---|
| contains/deep_object — `@>` 4 objects deep | 1.97 | 56.8 | 6.85 | 29× | 3.5× | 0.12× | 10/10/10 | gin_idx / seq scan / query_gin_idx |
| contains/deep_object_fn — same, function spelling | —[^a] | 48.6 | 6.80 |  |  | 0.14× | –/10/10 | – / seq scan / query_gin_idx |
| contains/array_element — `@>` one address (city + state) | 0.13 | 142,711[^s] | 1.92 | 1,069,400× | 14× | <0.01× | 1/2/1 | gin_idx / seq scan / query_gin_idx |
| contains/array_element_fn — same, function spelling | —[^a] | 1.02 | 1.92 |  |  | 1.9× | –/2/1 | – / jsonb_array_index / query_gin_idx |
| contains/nested_array — `@>` order status + item product | 1.58 | 154 | 8.83 | 98× | 5.6× | 0.06× | 10/10/10 | gin_idx / seq scan / query_gin_idx |
| contains/nested_array_fn — same, function spelling | —[^a] | 6.42 | 8.87 |  |  | 1.4× | –/10/10 | – / jsonb_array_index / query_gin_idx |
| field_eq/nested — `$.user.email` = value (containment) | 0.11 | 0.27 | 0.51 | 2.5× | 4.6× | 1.9× | 1/1/1 | gin_idx / jsonb_array_index / query_gin_idx |
| field_gt/nested — `$.company.salary` > value | 0.18 | 0.25 | 0.11 | 1.3× | 0.58× | 0.44× | 10/10/10 | seq scan / field_order_idx / field_order_idx |
| field_between/nested — `$.company.salary` between lo and hi | 2.39 | 0.64 | 0.21 | 0.27× | 0.09× | 0.33× | 10/10/10 | seq scan / field_order_idx / field_order_idx |
| field_order/nested — ORDER BY `$.company.salary` LIMIT 10 | 0.14 | 4.56 | 3.78 | 32× | 26× | 0.83× | 10/10/10 | salary_idx / field_order_idx / field_order_idx |
| extract/nested — `$.company.salary` of one row | 0.10 | 0.53 | 0.49 | 5.3× | 5.0× | 0.93× | 1/1/1 | pkey / pkey / pkey |
| path_query/wildcard — `$.orders[*].status`, one row | 0.10 | 1.27 | 1.33 | 12× | 13× | 1.0× | 5/5/5 | pkey / pkey / pkey |
| subtree/company — subtree `$.company` (6 leaves) | 0.10 | 0.55 | 1.77 | 5.5× | 18× | 3.2× | 1/1/6 | pkey / pkey / pkey |
| subtree/user — subtree `$.user` (12 leaves) | 0.10 | 0.55 | 2.26 | 5.3× | 22× | 4.1× | 1/1/12 | pkey / pkey / pkey |
| subtree/orders — subtree `$.orders`, default budget | 0.11 | 0.54 | 5.19 | 5.0× | 47× | 9.5× | 1/1/1 | pkey / pkey / pkey |
| subtree/orders_walked — subtree `$.orders`, budget 0 | 0.11 | —[^c] | 10.7 |  | 98× |  | 1/–/130 | pkey / – / pkey |
| subtree_at/items — `$.orders[*].items`, default budget | 0.11 | 1.25 | 6.81 | 11× | 62× | 5.4× | 5/5/1 | pkey / pkey / pkey |
| subtree_at/items_walked — `$.orders[*].items`, budget 0 | 0.11 | —[^c] | 10.5 |  | 95× |  | 5/–/105 | pkey / – / pkey |
| children/orders — children of `$.orders` | 0.11 | 1.25 | 1.41 | 11× | 13× | 1.1× | 5/5/5 | pkey / pkey / pkey |
| child_count/orders — child count of `$.orders` | 0.10 | 1.25 | 1.43 | 13× | 14× | 1.1× | 1/1/1 | pkey / pkey / pkey |

In both nested tables, the plaintext `_walked` rows run the same query as the
row above them. Native jsonb has no walk.

At 100k, SteVec's array-element needle returns 2 rows where the control
returns 1. That is the false positive of cipherstash/stack#1231; see
Correctness.

[^a]: Native jsonb has one containment spelling, `@>`, measured in the
    operator row. (`jsonb_contains()` exists but no index serves it.)
[^b]: Not built. The native equivalent would be a btree on `plain ->> 'email'`;
    the containment rows are the native comparison.
[^c]: SteVec has no walk and no budget. Every SteVec entry carries its own
    ciphertext of the node at its path, so `->` on a container returns the
    whole subtree in one ciphertext. That is the default-budget row above;
    there is no second query to time.
[^d]: json_tree has no equality operator on an entry. Equality at a path is
    containment of a one-field needle (the `field_eq/*` rows).
[^s]: Over 3 s on its first execution, so not timed by criterion. Median of 3
    executions.

Columns: medians in ms (criterion, 10 samples); the three ratios (a ratio below
1.0× means the first system is faster); rows returned by one execution
(plaintext / SteVec / json_tree); the index the planner chose, from the
`EXPLAIN` each bench captures at startup (suffix after the table name). That
`EXPLAIN` sees the statement's first, custom-planned execution. Where the
steady state switched to a generic plan with a different shape, the appendix's
6th-execution plan shows it; that happened only for plaintext containment at
flat 100k.

## Storage

**SteVec's nested documents and their index are the outlier.**

- A nested SteVec document is 108.5 KB on disk (238 K characters as text). That
  is 1.7× json_tree's 62.1 KB and 50× plaintext's 2.2 KB.
- SteVec's containment GIN at 100k nested rows is 5.5 GiB and took 27 minutes
  to build. json_tree's is 358 MiB in 34 s, and plaintext's 355 MiB in 18 s.
- At 100k nested rows the three systems total 16.1 GiB, 6.4 GiB and 0.6 GiB.

The reasons:

- `ITEM | WILDCARD` writes each array element twice.
- Every entry carries the ciphertext of its whole subtree, so a node is sealed
  again at every ancestor. The root's entry alone is the whole document.
- SteVec's GIN indexes every entry's selector-and-term pair, 668 per document.

On flat documents the heaps are similar (1.9 against 2.3 KB per document), and
SteVec's GIN is 13× json_tree's (92 against 7.1 MiB at 100k).

| corpus | rows | system | avg column, bytes (`pg_column_size`) | avg column, chars (`::text`) | heap incl. TOAST, MiB (`pg_table_size`) | of which TOAST, MiB | containment GIN, MiB | GIN build, s | other indexes, MiB | total, MiB |
|---|--:|---|--:|--:|--:|--:|--:|--:|--:|--:|
| flat | 10,000 | plaintext | 109 | 95 | 1.4 | 0.0 | 0.9 | 0.0 | 0.3 | 2.6 |
| flat | 10,000 | SteVec | 1,942 | 1,889 | 20.9 | 5.1 | 10.8 | 0.9 | 3.8 | 35.5 |
| flat | 10,000 | json_tree | 2,250 | 2,132 | 27.1 | 26.5 | 1.0 | 0.1 | 0.3 | 28.4 |
| flat | 100,000 | plaintext | 109 | 95 | 14.0 | 0.0 | 6.6 | 0.2 | 2.8 | 23.4 |
| flat | 100,000 | SteVec | 1,942 | 1,890 | 208.8 | 51.1 | 92.2 | 10.9 | 36.3 | 337.2 |
| flat | 100,000 | json_tree | 2,250 | 2,132 | 269.8 | 264.8 | 7.1 | 0.6 | 2.9 | 279.9 |
| nested | 10,000 | plaintext | 2,156 | 4,024 | 24.1 | 17.2 | 38.9 | 1.2 | 0.5 | 63.5 |
| nested | 10,000 | SteVec | 108,574 | 238,359 | 1,080.1 | 1,079.6 | 873.3 | 139.8 | 1.3 | 1,954.7 |
| nested | 10,000 | json_tree | 62,143 | 86,821 | 620.9 | 620.4 | 39.9 | 2.3 | 1.2 | 662.0 |
| nested | 100,000 | plaintext | 2,154 | 4,021 | 240.1 | 170.3 | 354.8 | 18.1 | 4.2 | 599.1 |
| nested | 100,000 | SteVec | 108,500 | 238,159 | 10,792.8 | 10,787.8 | 5,644.3 | 1616.1 | 13.1 | 16,450.1 |
| nested | 100,000 | json_tree | 62,092 | 86,743 | 6,201.2 | 6,196.2 | 358.2 | 33.7 | 10.0 | 6,569.4 |

How to read the columns:

- *Avg column* is `avg(pg_column_size(col))` (on-disk, compressed) and
  `avg(length(col::text))` (characters) over every row.
- *Heap incl. TOAST* is `pg_table_size`: main fork, TOAST table with its index,
  free-space and visibility maps.
- *Of which TOAST* is the TOAST relation's total size. Nested documents of all
  three systems, and json_tree's flat documents, live mostly in TOAST.
- *Containment GIN* is the table's one GIN index after the rebuild.
- *GIN build* is the wall-clock time of one `DROP INDEX` + `CREATE INDEX` per
  table, run back to back before the timing session (`report_json_three_way.py
  gin-rebuild`). PostgreSQL 17 builds GIN indexes single-threaded.
- *Other indexes* are the primary key plus the per-path btrees the benches
  build: `field_order_idx` on each encrypted table, `field_eq_idx` on the flat
  SteVec table, `age_idx` / `salary_idx` on the plaintext twins.
- *Total* is `pg_total_relation_size`.

Raw: `results/query/compare/storage.json`, `gin_rebuild.json`.

## Correctness

**json_tree agreed with the control everywhere. SteVec agreed everywhere except
array-of-object containment at 100k, the known false positive of #1231.** The
plaintext bench queries agreed with the control.

**json_tree** (`mise run check:v4:json_tree <N>`,
`json-tree-bench/src/bin/check_json_tree_v4.rs`):

- 25 nested rows, 14 checks each: the walks, path queries and `->`, decrypted
  and compared with the twin.
- 10 needles per predicate, comparing full id sets without `LIMIT`, plus the
  `ORDER BY` keys. Nested: the three containment needles, equality, `>`,
  `ORDER BY`. Flat: containment in both spellings, equality at `first_name` and
  `email`, `>`, between, `ORDER BY`.

| tier | walks | predicates | total |
|---|--:|--:|--:|
| 10,000 | 350 pass, 0 fail | 112 pass, 0 fail | 462 pass, 0 fail |
| 100,000 | 350 pass, 0 fail | 112 pass, 0 fail | 462 pass, 0 fail |

Raw: `results/query/v4/json_tree_correctness_<N>.json`.

**SteVec and plaintext** (`mise run check:compare:json <N>`,
`src/bin/check_json_v3_twin.rs`): 10 needle rows per predicate, from a fixed
pseudo-random order of each twin.

- Each predicate scenario runs without its `LIMIT`. Its id set is compared with
  the control: `plain @> needle` for containment, `plain #>> path` compared as
  text or integer for the field predicates. Both SteVec spellings of every
  containment needle are checked.
- For `ORDER BY … LIMIT 10`, the plaintext keys of the returned rows are
  compared with the control's ten smallest keys.
- The nested by-id scenarios are checked by decrypting what they return and
  comparing it with the twin's value at that path, as a multiset:
  `->` on `$.company`, `$.user`, `$.orders` and `$.company.salary`, and the
  wildcard path queries for items, children and statuses. Child count is
  compared with the array's length.
- The plaintext bench's equality queries (`@>` of a one-field needle) are
  checked against `#>>` equality. Its other predicate queries are the control
  itself.

**10,000 rows** (`check_json_v3_twin`, 10 needles per predicate)

| check | needles | agree | disagree | extra rows | missing rows | control rows |
|---|--:|--:|--:|--:|--:|--:|
| flat/plain/field_eq/extractor | 10 | 10 | 0 | 0 | 0 | 41 |
| flat/plain/field_eq/unique | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/contains/functional | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/contains/operator | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/field_between/functional | 10 | 10 | 0 | 0 | 0 | 13028 |
| flat/stevec/field_eq/eq_term | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/field_eq/extractor | 10 | 10 | 0 | 0 | 0 | 41 |
| flat/stevec/field_eq/unique | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/field_gt/functional | 10 | 10 | 0 | 0 | 0 | 57391 |
| flat/stevec/field_order/functional | 1 | 1 | 0 |  |  |  |
| nested/plain/field_eq/nested | 10 | 10 | 0 | 0 | 0 | 10 |
| nested/stevec/child_count/orders | 10 | 10 | 0 |  |  |  |
| nested/stevec/children/orders | 10 | 10 | 0 |  |  |  |
| nested/stevec/contains/array_element | 10 | 10 | 0 | 0 | 0 | 11 |
| nested/stevec/contains/array_element_fn | 10 | 10 | 0 | 0 | 0 | 11 |
| nested/stevec/contains/deep_object | 10 | 10 | 0 | 0 | 0 | 19842 |
| nested/stevec/contains/deep_object_fn | 10 | 10 | 0 | 0 | 0 | 19842 |
| nested/stevec/contains/nested_array | 10 | 10 | 0 | 0 | 0 | 4665 |
| nested/stevec/contains/nested_array_fn | 10 | 10 | 0 | 0 | 0 | 4665 |
| nested/stevec/extract/nested | 10 | 10 | 0 |  |  |  |
| nested/stevec/field_between/nested | 10 | 10 | 0 | 0 | 0 | 3880 |
| nested/stevec/field_eq/nested | 10 | 10 | 0 | 0 | 0 | 10 |
| nested/stevec/field_gt/nested | 10 | 10 | 0 | 0 | 0 | 52235 |
| nested/stevec/field_order/nested | 1 | 1 | 0 |  |  |  |
| nested/stevec/path_query/wildcard | 10 | 10 | 0 |  |  |  |
| nested/stevec/subtree/company | 10 | 10 | 0 |  |  |  |
| nested/stevec/subtree/orders | 10 | 10 | 0 |  |  |  |
| nested/stevec/subtree/user | 10 | 10 | 0 |  |  |  |
| nested/stevec/subtree_at/items | 10 | 10 | 0 |  |  |  |

**100,000 rows** (`check_json_v3_twin`, 10 needles per predicate)

| check | needles | agree | disagree | extra rows | missing rows | control rows |
|---|--:|--:|--:|--:|--:|--:|
| flat/plain/field_eq/extractor | 10 | 10 | 0 | 0 | 0 | 360 |
| flat/plain/field_eq/unique | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/contains/functional | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/contains/operator | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/field_between/functional | 10 | 10 | 0 | 0 | 0 | 129462 |
| flat/stevec/field_eq/eq_term | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/field_eq/extractor | 10 | 10 | 0 | 0 | 0 | 360 |
| flat/stevec/field_eq/unique | 10 | 10 | 0 | 0 | 0 | 10 |
| flat/stevec/field_gt/functional | 10 | 10 | 0 | 0 | 0 | 426351 |
| flat/stevec/field_order/functional | 1 | 1 | 0 |  |  |  |
| nested/plain/field_eq/nested | 10 | 10 | 0 | 0 | 0 | 11 |
| nested/stevec/child_count/orders | 10 | 10 | 0 |  |  |  |
| nested/stevec/children/orders | 10 | 10 | 0 |  |  |  |
| nested/stevec/contains/array_element | 10 | 6 | 4 | 5 | 0 | 11 |
| nested/stevec/contains/array_element_fn | 10 | 6 | 4 | 5 | 0 | 11 |
| nested/stevec/contains/deep_object | 10 | 10 | 0 | 0 | 0 | 199462 |
| nested/stevec/contains/deep_object_fn | 10 | 10 | 0 | 0 | 0 | 199462 |
| nested/stevec/contains/nested_array | 10 | 10 | 0 | 0 | 0 | 47278 |
| nested/stevec/contains/nested_array_fn | 10 | 10 | 0 | 0 | 0 | 47278 |
| nested/stevec/extract/nested | 10 | 10 | 0 |  |  |  |
| nested/stevec/field_between/nested | 10 | 10 | 0 | 0 | 0 | 38929 |
| nested/stevec/field_eq/nested | 10 | 10 | 0 | 0 | 0 | 11 |
| nested/stevec/field_gt/nested | 10 | 10 | 0 | 0 | 0 | 582257 |
| nested/stevec/field_order/nested | 1 | 1 | 0 |  |  |  |
| nested/stevec/path_query/wildcard | 10 | 10 | 0 |  |  |  |
| nested/stevec/subtree/company | 10 | 10 | 0 |  |  |  |
| nested/stevec/subtree/orders | 10 | 10 | 0 |  |  |  |
| nested/stevec/subtree/user | 10 | 10 | 0 |  |  |  |
| nested/stevec/subtree_at/items | 10 | 10 | 0 |  |  |  |
At 100k, 4 of the 10 array-element needles returned extra rows. Both spellings
returned the same rows (the operator and the function share the term set):

| needle row | SteVec ids | control ids | extra |
|--:|--:|--:|--:|
| 11213 | 2 | 1 | 1 |
| 35146 | 2 | 1 | 1 |
| 50591 | 3 | 2 | 1 |
| 32601 | 3 | 1 | 2 |

This is cipherstash/stack#1231. SteVec indexes `$.addresses[*].city` and
`$.addresses[*].state` as independent entries, so a document with the needle's
city in one address and its state in another satisfies the needle. The
sampled 10k needles happened to draw no such document. The bench's own needle
at 100k (row 50000) is one of the affected kind: SteVec returned 2 rows
against 1. json_tree's containment projection nests each element's fields
inside that element, so the same needle matches only within one address. It
returned exactly the control's rows. No disagreement was found on the
deep-object or nested-array needles, on any field predicate, or in any
decrypted by-id result.

Raw: `results/query/compare/stevec_correctness_<N>.json`.

## Appendix A: every query, verbatim, with its plan

Every scenario and system at the 100,000-row tier.

- The SQL is exactly what the bench prepared. Selectors are interpolated as
  `'<hex>'::text` literals, so that the index expressions match.
- Parameter kinds:
  - json_tree's `text` operands are JSON-serialised `JsonTreeQuery` /
    `JsonTreeLeafQuery` values.
  - SteVec's `text` operands are a `{"sv":[…]}` needle or a stored
    `jsonb_entry`.
  - `int4` is a row id or a plaintext bound.
  - `jsonb` is a plaintext needle.
  - `text[]` is json_tree's gates.
- The plans were captured once by `report_json_three_way.py explain 100000`
  after the timing session. Each scenario was `PREPARE`d with the bench's own
  bound parameters (the same needle and id), executed 5 times, then
  `EXPLAIN (ANALYZE, BUFFERS) EXECUTE`d. That is the 6th execution, the first
  at which the `auto` plan cache may switch to a generic plan, as the bench's
  steady state does.
- In a custom plan the operand is inlined into the plan text. Literals over 200
  characters are cut to their first 100 characters, with their length noted.
  The full plans and parameters are in `results/query/compare/explain_100000.json`.

### Flat corpus, 100,000 rows

#### flat: contains/functional — whole document, function spelling

*plaintext:* no query of its own: native jsonb has one containment spelling, `@>`, measured in the operator row.

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_small_encrypted_v3_100000 WHERE eql_v3.jsonb_contains(value, $1::jsonb) LIMIT 10
```

```
Limit  (cost=24.22..64.28 rows=10 width=4) (actual time=0.185..0.185 rows=1 loops=1)
  Buffers: shared hit=32
  ->  Bitmap Heap Scan on json_ste_vec_small_encrypted_v3_100000  (cost=24.22..2027.10 rows=500 width=4) (actual time=0.185..0.185 rows=1 loops=1)
        Recheck Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
        Heap Blocks: exact=1
        Buffers: shared hit=32
        ->  Bitmap Index Scan on json_ste_vec_small_encrypted_v3_100000_jsonb_array_index  (cost=0.00..24.09 rows=500 width=0) (actual time=0.183..0.184 rows=1 loops=1)
              Index Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
              Buffers: shared hit=31
Planning:
  Buffers: shared hit=1
Planning Time: 0.052 ms
Execution Time: 0.192 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_encrypted_v4_100000 WHERE eql_v4.json_tree_contains(value, $1::text::jsonb::eql_v4.query_json_tree) LIMIT 10
```

```
Limit  (cost=44.70..150.92 rows=10 width=4) (actual time=0.262..0.263 rows=1 loops=1)
  Buffers: shared hit=64
  ->  Bitmap Heap Scan on json_tree_small_encrypted_v4_100000  (cost=44.70..10772.70 rows=1010 width=4) (actual time=0.262..0.262 rows=1 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=1
        Buffers: shared hit=64
        ->  BitmapOr  (cost=44.70..44.70 rows=2000 width=0) (actual time=0.228..0.228 rows=0 loops=1)
              Buffers: shared hit=55
              ->  Bitmap Index Scan on json_tree_small_encrypted_v4_100000_query_gin_idx  (cost=0.00..22.10 rows=1000 width=0) (actual time=0.215..0.215 rows=1 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=55
              ->  Bitmap Index Scan on json_tree_small_encrypted_v4_100000_query_gin_idx  (cost=0.00..22.10 rows=1000 width=0) (actual time=0.012..0.012 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.028 ms
Execution Time: 0.269 ms
```

#### flat: contains/operator — whole document, `@>`

*plaintext* — parameters: $1 jsonb; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_plain_v4_100000 WHERE plain @> $1 LIMIT 10
```

```
Limit  (cost=0.00..30.35 rows=10 width=4) (actual time=8.622..15.471 rows=1 loops=1)
  Buffers: shared hit=1 read=1784
  ->  Seq Scan on json_tree_small_plain_v4_100000  (cost=0.00..3035.00 rows=1000 width=4) (actual time=8.622..15.470 rows=1 loops=1)
        Filter: (plain @> $1)
        Rows Removed by Filter: 99999
        Buffers: shared hit=1 read=1784
Planning:
  Buffers: shared hit=1
Planning Time: 0.011 ms
Execution Time: 15.477 ms
```

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_small_encrypted_v3_100000 WHERE value @> $1::jsonb::eql_v3.query_jsonb LIMIT 10
```

```
Limit  (cost=0.00..471.81 rows=10 width=4) (actual time=1369.605..2740.285 rows=1 loops=1)
  Buffers: shared hit=78009
  ->  Seq Scan on json_ste_vec_small_encrypted_v3_100000  (cost=0.00..47181.00 rows=1000 width=4) (actual time=1369.604..2740.282 rows=1 loops=1)
        Filter: ((eql_v3.to_ste_vec_query(value))::jsonb @> ((($1)::jsonb)::eql_v3.query_jsonb)::jsonb)
        Rows Removed by Filter: 99999
        Buffers: shared hit=78009
Planning Time: 0.094 ms
Execution Time: 2740.293 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_encrypted_v4_100000 WHERE value @> $1::text::jsonb::eql_v4.query_json_tree LIMIT 10
```

```
Limit  (cost=44.70..150.92 rows=10 width=4) (actual time=0.264..0.264 rows=1 loops=1)
  Buffers: shared hit=64
  ->  Bitmap Heap Scan on json_tree_small_encrypted_v4_100000  (cost=44.70..10772.70 rows=1010 width=4) (actual time=0.264..0.264 rows=1 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=1
        Buffers: shared hit=64
        ->  BitmapOr  (cost=44.70..44.70 rows=2000 width=0) (actual time=0.229..0.229 rows=0 loops=1)
              Buffers: shared hit=55
              ->  Bitmap Index Scan on json_tree_small_encrypted_v4_100000_query_gin_idx  (cost=0.00..22.10 rows=1000 width=0) (actual time=0.217..0.217 rows=1 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=55
              ->  Bitmap Index Scan on json_tree_small_encrypted_v4_100000_query_gin_idx  (cost=0.00..22.10 rows=1000 width=0) (actual time=0.012..0.012 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.031 ms
Execution Time: 0.271 ms
```

#### flat: field_eq/extractor — `$.first_name` = value (containment)

*plaintext* — parameters: $1 jsonb; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_plain_v4_100000 WHERE plain @> $1 LIMIT 10
```

```
Limit  (cost=0.00..30.35 rows=10 width=4) (actual time=0.059..3.928 rows=10 loops=1)
  Buffers: shared hit=545
  ->  Seq Scan on json_tree_small_plain_v4_100000  (cost=0.00..3035.00 rows=1000 width=4) (actual time=0.059..3.927 rows=10 loops=1)
        Filter: (plain @> $1)
        Rows Removed by Filter: 30447
        Buffers: shared hit=545
Planning:
  Buffers: shared hit=1
Planning Time: 0.011 ms
Execution Time: 3.931 ms
```

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_small_encrypted_v3_100000 WHERE eql_v3.jsonb_contains(value, $1::jsonb) LIMIT 10
```

```
Limit  (cost=24.22..64.28 rows=10 width=4) (actual time=0.328..0.330 rows=10 loops=1)
  Buffers: shared hit=51
  ->  Bitmap Heap Scan on json_ste_vec_small_encrypted_v3_100000  (cost=24.22..2027.10 rows=500 width=4) (actual time=0.328..0.329 rows=10 loops=1)
        Recheck Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
        Heap Blocks: exact=10
        Buffers: shared hit=51
        ->  Bitmap Index Scan on json_ste_vec_small_encrypted_v3_100000_jsonb_array_index  (cost=0.00..24.09 rows=500 width=0) (actual time=0.325..0.325 rows=30 loops=1)
              Index Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
              Buffers: shared hit=41
Planning:
  Buffers: shared hit=1
Planning Time: 0.053 ms
Execution Time: 0.337 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_encrypted_v4_100000 WHERE eql_v4.json_tree_contains(value, $1::text::jsonb::eql_v4.query_json_tree) LIMIT 10
```

```
Limit  (cost=56.03..142.58 rows=10 width=4) (actual time=0.680..0.857 rows=10 loops=1)
  Buffers: shared hit=175
  ->  Bitmap Heap Scan on json_tree_small_encrypted_v4_100000  (cost=56.03..142.58 rows=10 width=4) (actual time=0.680..0.856 rows=10 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "9f7f562232761fd6f7624026c36859c9", "vs": "cdc46799430eea809fc8da821fddded8"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "9f7f562232761fd6f7624026c36859c9", "vs": "cdc46799430eea809fc8da821fddded8"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "9f7f562232761fd6f7624026c36859c9", "vs": "cdc46799430eea809fc8da821fddded8"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "9f7f562232761fd6f7624026c36859c9", "vs": "cdc46799430eea809fc8da821fddded8"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "9f7f562232761fd6f7624026c36859c9", "vs": "cdc46799430eea809fc8da821fddded8"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=10
        Buffers: shared hit=175
        ->  BitmapOr  (cost=56.03..56.03 rows=10 width=0) (actual time=0.654..0.655 rows=0 loops=1)
              Buffers: shared hit=85
              ->  Bitmap Index Scan on json_tree_small_encrypted_v4_100000_query_gin_idx  (cost=0.00..56.03 rows=10 width=0) (actual time=0.646..0.646 rows=30 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "9f7f562232761fd6f7624026c36859c9", "vs": "cdc46799430eea809fc8da821fddded8"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=85
              ->  Bitmap Index Scan on json_tree_small_encrypted_v4_100000_query_gin_idx  (cost=0.00..0.00 rows=1 width=0) (actual time=0.008..0.008 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "9f7f562232761fd6f7624026c36859c9", "vs": "cdc46799430eea809fc8da821fddded8"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=4
Planning Time: 0.128 ms
Execution Time: 0.863 ms
```

#### flat: field_eq/unique — `$.email` = value (containment)

*plaintext* — parameters: $1 jsonb; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_plain_v4_100000 WHERE plain @> $1 LIMIT 10
```

```
Limit  (cost=0.00..30.35 rows=10 width=4) (actual time=6.012..11.735 rows=1 loops=1)
  Buffers: shared hit=1785
  ->  Seq Scan on json_tree_small_plain_v4_100000  (cost=0.00..3035.00 rows=1000 width=4) (actual time=6.012..11.735 rows=1 loops=1)
        Filter: (plain @> $1)
        Rows Removed by Filter: 99999
        Buffers: shared hit=1785
Planning:
  Buffers: shared hit=1
Planning Time: 0.012 ms
Execution Time: 11.739 ms
```

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_small_encrypted_v3_100000 WHERE eql_v3.jsonb_contains(value, $1::jsonb) LIMIT 10
```

```
Limit  (cost=24.22..64.28 rows=10 width=4) (actual time=0.131..0.131 rows=1 loops=1)
  Buffers: shared hit=16
  ->  Bitmap Heap Scan on json_ste_vec_small_encrypted_v3_100000  (cost=24.22..2027.10 rows=500 width=4) (actual time=0.131..0.131 rows=1 loops=1)
        Recheck Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
        Heap Blocks: exact=1
        Buffers: shared hit=16
        ->  Bitmap Index Scan on json_ste_vec_small_encrypted_v3_100000_jsonb_array_index  (cost=0.00..24.09 rows=500 width=0) (actual time=0.129..0.129 rows=1 loops=1)
              Index Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
              Buffers: shared hit=15
Planning:
  Buffers: shared hit=1
Planning Time: 0.053 ms
Execution Time: 0.138 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_encrypted_v4_100000 WHERE eql_v4.json_tree_contains(value, $1::text::jsonb::eql_v4.query_json_tree) LIMIT 10
```

```
Limit  (cost=56.03..142.58 rows=10 width=4) (actual time=0.145..0.145 rows=1 loops=1)
  Buffers: shared hit=34
  ->  Bitmap Heap Scan on json_tree_small_encrypted_v4_100000  (cost=56.03..142.58 rows=10 width=4) (actual time=0.144..0.145 rows=1 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "7349633426741ad6074ff9e7ffa69599", "vs": "d566b7e03127d5532141d303eee2bb26"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "7349633426741ad6074ff9e7ffa69599", "vs": "d566b7e03127d5532141d303eee2bb26"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "7349633426741ad6074ff9e7ffa69599", "vs": "d566b7e03127d5532141d303eee2bb26"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "7349633426741ad6074ff9e7ffa69599", "vs": "d566b7e03127d5532141d303eee2bb26"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "7349633426741ad6074ff9e7ffa69599", "vs": "d566b7e03127d5532141d303eee2bb26"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=1
        Buffers: shared hit=34
        ->  BitmapOr  (cost=56.03..56.03 rows=10 width=0) (actual time=0.119..0.119 rows=0 loops=1)
              Buffers: shared hit=25
              ->  Bitmap Index Scan on json_tree_small_encrypted_v4_100000_query_gin_idx  (cost=0.00..56.03 rows=10 width=0) (actual time=0.110..0.110 rows=1 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "7349633426741ad6074ff9e7ffa69599", "vs": "d566b7e03127d5532141d303eee2bb26"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=25
              ->  Bitmap Index Scan on json_tree_small_encrypted_v4_100000_query_gin_idx  (cost=0.00..0.00 rows=1 width=0) (actual time=0.009..0.009 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> ((('{"el": [], "sv": [{"s": "1a5317b64b04a9136157a95c8a631430", "vs": "398f6c6dccf8f490da2077e1b0922cdc"}, {"s": "7349633426741ad6074ff9e7ffa69599", "vs": "d566b7e03127d5532141d303eee2bb26"}]}'::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=4
Planning Time: 0.129 ms
Execution Time: 0.151 ms
```

#### flat: field_eq/eq_term — `$.email` = value (SteVec `eq_term` btree)

*plaintext:* not built: the native equivalent would be a btree on `plain ->> 'email'`; the containment rows are the native comparison.

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_small_encrypted_v3_100000 WHERE eql_v3.eq_term(eql_v3.jsonb_path_query_first(value, 'b81940706ae05004a9b95b6dbd1aaa60')) = eql_v3.eq_term($1::jsonb::public.jsonb_entry) LIMIT 10
```

```
Limit  (cost=0.80..8.82 rows=1 width=4) (actual time=0.011..0.012 rows=1 loops=1)
  Buffers: shared hit=5
  ->  Index Scan using json_ste_vec_small_encrypted_v3_100000_field_eq_idx on json_ste_vec_small_encrypted_v3_100000  (cost=0.80..8.82 rows=1 width=4) (actual time=0.011..0.011 rows=1 loops=1)
        Index Cond: (eql_v3.eq_term(eql_v3.jsonb_path_query_first((value)::jsonb, 'b81940706ae05004a9b95b6dbd1aaa60'::text)) = eql_v3.eq_term((($1)::jsonb)::jsonb_entry))
        Buffers: shared hit=5
Planning Time: 0.028 ms
Execution Time: 0.017 ms
```

*json_tree:* not expressible: json_tree has no equality operator on an entry; equality at a path is containment (`field_eq/unique`).

#### flat: field_gt/functional — `$.age` > value

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_plain_v4_100000 WHERE (plain ->> 'age')::int > $1 LIMIT 10
```

```
Limit  (cost=0.00..1.14 rows=10 width=4) (actual time=0.002..0.004 rows=10 loops=1)
  Buffers: shared hit=2
  ->  Seq Scan on json_tree_small_plain_v4_100000  (cost=0.00..3785.00 rows=33333 width=4) (actual time=0.002..0.004 rows=10 loops=1)
        Filter: (((plain ->> 'age'::text))::integer > $1)
        Rows Removed by Filter: 6
        Buffers: shared hit=2
Planning Time: 0.011 ms
Execution Time: 0.007 ms
```

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_small_encrypted_v3_100000 WHERE eql_v3.jsonb_path_query_first(value, 'cc6e1824ab3319e8f92dc6199ce671b9') > $1::jsonb::public.jsonb_entry LIMIT 10
```

```
Limit  (cost=0.67..13.85 rows=10 width=4) (actual time=0.125..0.127 rows=10 loops=1)
  Buffers: shared hit=12
  ->  Index Scan using json_ste_vec_small_encrypted_v3_100000_field_order_idx on json_ste_vec_small_encrypted_v3_100000  (cost=0.67..85484.38 rows=64847 width=4) (actual time=0.125..0.126 rows=10 loops=1)
        Index Cond: (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, 'cc6e1824ab3319e8f92dc6199ce671b9'::text)) > eql_v3.ore_cllw(('{"a": false, "c": "mBbKAFJFwNsBbDQAbcb_8YhCp6b|jw5kWim9anNPR=F2h_vBCP+{7Rx#f^<OkCUWRMY!Iqa`>5mYz9zQW…[375 chars]'::jsonb)::jsonb_entry))
        Buffers: shared hit=12
Planning Time: 0.565 ms
Execution Time: 0.132 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_encrypted_v4_100000 WHERE (value -> 'a7b53d9b98181a98c49559f86362fcce'::text) > $1::text::jsonb::eql_v4.query_json_tree_leaf LIMIT 10
```

```
Limit  (cost=0.43..1.41 rows=10 width=4) (actual time=0.004..0.006 rows=10 loops=1)
  Buffers: shared hit=7
  ->  Index Scan using json_tree_small_encrypted_v4_100000_field_order_idx on json_tree_small_encrypted_v4_100000  (cost=0.43..3271.31 rows=33333 width=4) (actual time=0.004..0.005 rows=10 loops=1)
        Index Cond: (decode(((eql_v4.json_tree_path_query_first((value)::jsonb, 'a7b53d9b98181a98c49559f86362fcce'::text))::jsonb ->> 'op'::text), 'hex'::text) > decode((((($1)::jsonb)::eql_v4.query_json_tree_leaf)::jsonb ->> 'op'::text), 'hex'::text))
        Buffers: shared hit=7
Planning Time: 0.058 ms
Execution Time: 0.010 ms
```

#### flat: field_between/functional — `$.age` between lo and hi

*plaintext* — parameters: $1 int4, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_plain_v4_100000 WHERE (plain ->> 'age')::int BETWEEN $1 AND $2 LIMIT 10
```

```
Limit  (cost=0.00..3.49 rows=10 width=4) (actual time=0.003..0.012 rows=10 loops=1)
  Buffers: shared hit=3
  ->  Seq Scan on json_tree_small_plain_v4_100000  (cost=0.00..4785.00 rows=13727 width=4) (actual time=0.003..0.011 rows=10 loops=1)
        Filter: ((((plain ->> 'age'::text))::integer >= 41) AND (((plain ->> 'age'::text))::integer <= 51))
        Rows Removed by Filter: 51
        Buffers: shared hit=3
Planning Time: 0.028 ms
Execution Time: 0.015 ms
```

*SteVec* — parameters: $1 text, $2 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_small_encrypted_v3_100000 WHERE eql_v3.jsonb_path_query_first(value, 'cc6e1824ab3319e8f92dc6199ce671b9') >= $1::jsonb::public.jsonb_entry AND eql_v3.jsonb_path_query_first(value, 'cc6e1824ab3319e8f92dc6199ce671b9') <= $2::jsonb::public.jsonb_entry LIMIT 10
```

```
Limit  (cost=0.92..31.65 rows=10 width=4) (actual time=0.167..0.196 rows=10 loops=1)
  Buffers: shared hit=13
  ->  Index Scan using json_ste_vec_small_encrypted_v3_100000_field_order_idx on json_ste_vec_small_encrypted_v3_100000  (cost=0.92..41529.53 rows=13517 width=4) (actual time=0.167..0.196 rows=10 loops=1)
        Index Cond: ((eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, 'cc6e1824ab3319e8f92dc6199ce671b9'::text)) >= eql_v3.ore_cllw(('{"a": false, "c": "mBbLFTBlai!-dTZX+*;IuEgNP6hOkGW{U{tG^=X{=8>#?J2TS9WW*qib+NlmVxl&Ug<$l_uyZN2V@Si>-…[375 chars]'::jsonb)::jsonb_entry)) AND (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, 'cc6e1824ab3319e8f92dc6199ce671b9'::text)) <= eql_v3.ore_cllw(('{"a": false, "c": "mBbK4NuUb&y8^(|Sd$6EF1qE!6g_B^+q)F|cR|#=<++=|!}8BNUBn=y`WDV!J&u;2U2OrS_3xW7v*A)E@…[375 chars]'::jsonb)::jsonb_entry)))
        Buffers: shared hit=13
Planning Time: 1.093 ms
Execution Time: 0.204 ms
```

*json_tree* — parameters: $1 text, $2 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_encrypted_v4_100000 WHERE (value -> 'a7b53d9b98181a98c49559f86362fcce'::text) >= $1::text::jsonb::eql_v4.query_json_tree_leaf AND (value -> 'a7b53d9b98181a98c49559f86362fcce'::text) <= $2::text::jsonb::eql_v4.query_json_tree_leaf LIMIT 10
```

```
Limit  (cost=0.43..2.57 rows=10 width=4) (actual time=0.006..0.007 rows=10 loops=1)
  Buffers: shared hit=7
  ->  Index Scan using json_tree_small_encrypted_v4_100000_field_order_idx on json_tree_small_encrypted_v4_100000  (cost=0.43..2876.82 rows=13443 width=4) (actual time=0.006..0.007 rows=10 loops=1)
        Index Cond: ((decode(((eql_v4.json_tree_path_query_first((value)::jsonb, 'a7b53d9b98181a98c49559f86362fcce'::text))::jsonb ->> 'op'::text), 'hex'::text) >= decode(((('{"s": "a7b53d9b98181a98c49559f86362fcce", "op": "0126916d0e57e88459877d695fcdfa76f550148b1cd6aa60645b6578b9126ba62ad7557c151e9fc048a0abe06487878fd8b6e14f516e68434c01dd986c42a572dc"}'::jsonb)::eql_v4.query_json_tree_leaf)::jsonb ->> 'op'::text), 'hex'::text)) AND (decode(((eql_v4.json_tree_path_query_first((value)::jsonb, 'a7b53d9b98181a98c49559f86362fcce'::text))::jsonb ->> 'op'::text), 'hex'::text) <= decode(((('{"s": "a7b53d9b98181a98c49559f86362fcce", "op": "0126916d0e57e88459877d69604d820bfc063d0dd23a14bc64ba6382d3e53b91f1f8e9ce1f20ee44a19810e5a5e586f9ddd78df28075c226b617a96d6b50050b5f"}'::jsonb)::eql_v4.query_json_tree_leaf)::jsonb ->> 'op'::text), 'hex'::text)))
        Buffers: shared hit=7
Planning Time: 0.194 ms
Execution Time: 0.014 ms
```

#### flat: field_order/functional — ORDER BY `$.age` LIMIT 10

*plaintext* — parameters: no parameters; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_plain_v4_100000 ORDER BY (plain ->> 'age')::int LIMIT 10
```

```
Limit  (cost=0.29..1.27 rows=10 width=8) (actual time=0.003..0.006 rows=10 loops=1)
  Buffers: shared hit=12
  ->  Index Scan using json_tree_small_plain_v4_100000_age_idx on json_tree_small_plain_v4_100000  (cost=0.29..9741.00 rows=100000 width=8) (actual time=0.003..0.005 rows=10 loops=1)
        Buffers: shared hit=12
Planning Time: 0.000 ms
Execution Time: 0.008 ms
```

*SteVec* — parameters: no parameters; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_small_encrypted_v3_100000 ORDER BY eql_v3.ore_cllw(eql_v3.jsonb_path_query_first(value, 'cc6e1824ab3319e8f92dc6199ce671b9')) LIMIT 10
```

```
Limit  (cost=0.42..14.20 rows=10 width=36) (actual time=0.040..0.145 rows=10 loops=1)
  Buffers: shared hit=22
  ->  Index Scan using json_ste_vec_small_encrypted_v3_100000_field_order_idx on json_ste_vec_small_encrypted_v3_100000  (cost=0.42..137813.64 rows=100000 width=36) (actual time=0.040..0.145 rows=10 loops=1)
        Buffers: shared hit=22
Planning Time: 0.000 ms
Execution Time: 0.148 ms
```

*json_tree* — parameters: no parameters; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_small_encrypted_v4_100000 ORDER BY eql_v4.ord_term(value -> 'a7b53d9b98181a98c49559f86362fcce'::text) LIMIT 10
```

```
Limit  (cost=0.42..25.91 rows=10 width=36) (actual time=0.081..0.627 rows=10 loops=1)
  Buffers: shared hit=169
  ->  Index Scan using json_tree_small_encrypted_v4_100000_field_order_idx on json_tree_small_encrypted_v4_100000  (cost=0.42..254956.05 rows=100000 width=36) (actual time=0.081..0.627 rows=10 loops=1)
        Buffers: shared hit=169
Planning Time: 0.000 ms
Execution Time: 0.630 ms
```

#### flat: extract/by_id — `$.age` of one row

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (plain -> 'age')::text FROM json_tree_small_plain_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_small_plain_v4_100000_pkey on json_tree_small_plain_v4_100000  (cost=0.29..8.32 rows=1 width=32) (actual time=0.002..0.003 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=3
Planning Time: 0.011 ms
Execution Time: 0.006 ms
```

*SteVec* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (value -> 'cc6e1824ab3319e8f92dc6199ce671b9'::text)::jsonb::text FROM json_ste_vec_small_encrypted_v3_100000 WHERE id = $1
```

```
Index Scan using json_ste_vec_small_encrypted_v3_100000_pkey on json_ste_vec_small_encrypted_v3_100000  (cost=0.29..8.57 rows=1 width=32) (actual time=0.028..0.028 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=3
Planning Time: 0.019 ms
Execution Time: 0.031 ms
```

*json_tree* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (value -> 'a7b53d9b98181a98c49559f86362fcce'::text)::jsonb::text FROM json_tree_small_encrypted_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_small_encrypted_v4_100000_pkey on json_tree_small_encrypted_v4_100000  (cost=0.29..10.82 rows=1 width=32) (actual time=0.083..0.083 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=19
Planning Time: 0.022 ms
Execution Time: 0.086 ms
```

#### flat: path_query/field — path query `$.email`, one row

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::text FROM json_tree_small_plain_v4_100000, jsonb_path_query(plain, '$.email') e WHERE id = $1
```

```
Nested Loop  (cost=0.29..33.31 rows=1000 width=32) (actual time=0.005..0.005 rows=1 loops=1)
  Buffers: shared hit=3
  ->  Index Scan using json_tree_small_plain_v4_100000_pkey on json_tree_small_plain_v4_100000  (cost=0.29..8.31 rows=1 width=108) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $1)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_path_query e  (cost=0.00..10.00 rows=1000 width=32) (actual time=0.003..0.003 rows=1 loops=1)
Planning Time: 0.015 ms
Execution Time: 0.008 ms
```

*SteVec* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_ste_vec_small_encrypted_v3_100000, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=0.030..0.031 rows=1 loops=1)
  Buffers: shared hit=3
  ->  Index Scan using json_ste_vec_small_encrypted_v3_100000_pkey on json_ste_vec_small_encrypted_v3_100000  (cost=0.29..8.31 rows=1 width=1547) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_path_query e  (cost=0.25..10.25 rows=1000 width=32) (actual time=0.027..0.027 rows=1 loops=1)
Planning Time: 0.015 ms
Execution Time: 0.034 ms
```

*json_tree* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_small_encrypted_v4_100000, eql_v4.json_tree_path_query(value::jsonb, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=0.085..0.085 rows=1 loops=1)
  Buffers: shared hit=19
  ->  Index Scan using json_tree_small_encrypted_v4_100000_pkey on json_tree_small_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_path_query e  (cost=0.25..10.25 rows=1000 width=32) (actual time=0.081..0.081 rows=1 loops=1)
        Buffers: shared hit=16
Planning Time: 0.015 ms
Execution Time: 0.088 ms
```

### Nested corpus, 100,000 rows

#### nested: contains/deep_object — `@>` 4 objects deep

*plaintext* — parameters: $1 jsonb; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_plain_v4_100000 WHERE plain @> $1 LIMIT 10
```

```
Limit  (cost=30.87..59.62 rows=10 width=4) (actual time=1.643..1.702 rows=10 loops=1)
  Buffers: shared hit=33
  ->  Bitmap Heap Scan on json_tree_large_plain_v4_100000  (cost=30.87..2905.68 rows=1000 width=4) (actual time=1.643..1.701 rows=10 loops=1)
        Recheck Cond: (plain @> $1)
        Heap Blocks: exact=3
        Buffers: shared hit=33
        ->  Bitmap Index Scan on json_tree_large_plain_v4_100000_gin_idx  (cost=0.00..30.62 rows=1000 width=0) (actual time=0.996..0.996 rows=19761 loops=1)
              Index Cond: (plain @> $1)
              Buffers: shared hit=9
Planning:
  Buffers: shared hit=1
Planning Time: 0.012 ms
Execution Time: 1.707 ms
```

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE value @> $1::jsonb::eql_v3.query_jsonb LIMIT 10
```

```
Limit  (cost=0.00..271.37 rows=10 width=4) (actual time=2.435..57.693 rows=10 loops=1)
  Buffers: shared hit=671
  ->  Seq Scan on json_ste_vec_large_encrypted_v3_100000  (cost=0.00..27137.00 rows=1000 width=4) (actual time=2.435..57.692 rows=10 loops=1)
        Filter: ((eql_v3.to_ste_vec_query(value))::jsonb @> (('{"sv": [{"s": "0062378a2a2037bf4cdd8c78c2a1aeb0", "hm": "25612f1751b8619090a88a84e96ef32a"}, {"s": "…[531 chars]'::jsonb)::eql_v3.query_jsonb)::jsonb)
        Rows Removed by Filter: 30
        Buffers: shared hit=671
Planning Time: 0.127 ms
Execution Time: 57.700 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE value @> $1::text::jsonb::eql_v4.query_json_tree LIMIT 10
```

```
Limit  (cost=61.76..167.97 rows=10 width=4) (actual time=4.978..6.511 rows=10 loops=1)
  Buffers: shared hit=458
  ->  Bitmap Heap Scan on json_tree_large_encrypted_v4_100000  (cost=61.76..10788.76 rows=1010 width=4) (actual time=4.978..6.510 rows=10 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=1
        Buffers: shared hit=458
        ->  BitmapOr  (cost=61.76..61.76 rows=2000 width=0) (actual time=4.783..4.783 rows=0 loops=1)
              Buffers: shared hit=233
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=4.770..4.770 rows=19761 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=233
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=0.013..0.013 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.034 ms
Execution Time: 6.519 ms
```

#### nested: contains/deep_object_fn — same, function spelling

*plaintext:* no query of its own: native jsonb has one containment spelling, `@>`, measured in the operator row.

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE eql_v3.jsonb_contains(value, $1::jsonb) LIMIT 10
```

```
Limit  (cost=0.00..13.98 rows=10 width=4) (actual time=3.928..95.423 rows=10 loops=1)
  Buffers: shared hit=1340
  ->  Seq Scan on json_ste_vec_large_encrypted_v3_100000  (cost=0.00..26887.00 rows=19227 width=4) (actual time=3.928..95.422 rows=10 loops=1)
        Filter: (eql_v3.jsonb_array((value)::jsonb) @> '{"{\"s\": \"3d2bf30af54965e84e8ad8f985efc949\", \"hm\": \"8441ca23e973b1301a7026fb46ccb84c\"}","{\"s…[569 chars]'::jsonb[])
        Rows Removed by Filter: 30
        Buffers: shared hit=1340
Planning:
  Buffers: shared hit=2
Planning Time: 0.337 ms
Execution Time: 95.429 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE eql_v4.json_tree_contains(value, $1::text::jsonb::eql_v4.query_json_tree) LIMIT 10
```

```
Limit  (cost=61.76..167.97 rows=10 width=4) (actual time=4.974..6.527 rows=10 loops=1)
  Buffers: shared hit=458
  ->  Bitmap Heap Scan on json_tree_large_encrypted_v4_100000  (cost=61.76..10788.76 rows=1010 width=4) (actual time=4.974..6.526 rows=10 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=1
        Buffers: shared hit=458
        ->  BitmapOr  (cost=61.76..61.76 rows=2000 width=0) (actual time=4.779..4.780 rows=0 loops=1)
              Buffers: shared hit=233
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=4.767..4.767 rows=19761 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=233
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=0.012..0.012 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.034 ms
Execution Time: 6.535 ms
```

#### nested: contains/array_element — `@>` one address (city + state)

*plaintext* — parameters: $1 jsonb; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_plain_v4_100000 WHERE plain @> $1 LIMIT 10
```

```
Limit  (cost=30.87..59.62 rows=10 width=4) (actual time=0.032..0.032 rows=1 loops=1)
  Buffers: shared hit=13
  ->  Bitmap Heap Scan on json_tree_large_plain_v4_100000  (cost=30.87..2905.68 rows=1000 width=4) (actual time=0.032..0.032 rows=1 loops=1)
        Recheck Cond: (plain @> $1)
        Rows Removed by Index Recheck: 1
        Heap Blocks: exact=2
        Buffers: shared hit=13
        ->  Bitmap Index Scan on json_tree_large_plain_v4_100000_gin_idx  (cost=0.00..30.62 rows=1000 width=0) (actual time=0.020..0.020 rows=2 loops=1)
              Index Cond: (plain @> $1)
              Buffers: shared hit=8
Planning:
  Buffers: shared hit=1
Planning Time: 0.012 ms
Execution Time: 0.035 ms
```

*SteVec* — parameters: $1 text; EXPLAIN single execution (slow scenario).

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE value @> $1::jsonb::eql_v3.query_jsonb LIMIT 10
```

```
Limit  (cost=0.00..271.37 rows=10 width=4) (actual time=35635.854..143512.405 rows=2 loops=1)
  Buffers: shared hit=364746 read=1380619
  ->  Seq Scan on json_ste_vec_large_encrypted_v3_100000  (cost=0.00..27137.00 rows=1000 width=4) (actual time=35635.853..143512.401 rows=2 loops=1)
        Filter: ((eql_v3.to_ste_vec_query(value))::jsonb @> (('{"sv": [{"s": "0062378a2a2037bf4cdd8c78c2a1aeb0", "hm": "25612f1751b8619090a88a84e96ef32a"}, {"s": "…[1,304 chars]'::jsonb)::eql_v3.query_jsonb)::jsonb)
        Rows Removed by Filter: 99998
        Buffers: shared hit=364746 read=1380619
Planning:
  Buffers: shared hit=1188
Planning Time: 1.421 ms
Execution Time: 143513.055 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE value @> $1::text::jsonb::eql_v4.query_json_tree LIMIT 10
```

```
Limit  (cost=61.76..167.97 rows=10 width=4) (actual time=1.674..1.675 rows=1 loops=1)
  Buffers: shared hit=224
  ->  Bitmap Heap Scan on json_tree_large_encrypted_v4_100000  (cost=61.76..10788.76 rows=1010 width=4) (actual time=1.674..1.674 rows=1 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Rows Removed by Index Recheck: 1
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=2
        Buffers: shared hit=224
        ->  BitmapOr  (cost=61.76..61.76 rows=2000 width=0) (actual time=1.319..1.319 rows=0 loops=1)
              Buffers: shared hit=176
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=1.300..1.300 rows=2 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=176
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=0.019..0.019 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.031 ms
Execution Time: 1.682 ms
```

#### nested: contains/array_element_fn — same, function spelling

*plaintext:* no query of its own: native jsonb has one containment spelling, `@>`, measured in the operator row.

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE eql_v3.jsonb_contains(value, $1::jsonb) LIMIT 10
```

```
Limit  (cost=41.26..58.99 rows=10 width=4) (actual time=0.836..0.837 rows=2 loops=1)
  Buffers: shared hit=125
  ->  Bitmap Heap Scan on json_ste_vec_large_encrypted_v3_100000  (cost=41.26..928.10 rows=500 width=4) (actual time=0.836..0.836 rows=2 loops=1)
        Recheck Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
        Heap Blocks: exact=2
        Buffers: shared hit=125
        ->  Bitmap Index Scan on json_ste_vec_large_encrypted_v3_100000_jsonb_array_index  (cost=0.00..41.13 rows=500 width=0) (actual time=0.834..0.834 rows=2 loops=1)
              Index Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
              Buffers: shared hit=123
Planning:
  Buffers: shared hit=1
Planning Time: 0.052 ms
Execution Time: 0.847 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE eql_v4.json_tree_contains(value, $1::text::jsonb::eql_v4.query_json_tree) LIMIT 10
```

```
Limit  (cost=61.76..167.97 rows=10 width=4) (actual time=1.691..1.691 rows=1 loops=1)
  Buffers: shared hit=224
  ->  Bitmap Heap Scan on json_tree_large_encrypted_v4_100000  (cost=61.76..10788.76 rows=1010 width=4) (actual time=1.691..1.691 rows=1 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Rows Removed by Index Recheck: 1
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=2
        Buffers: shared hit=224
        ->  BitmapOr  (cost=61.76..61.76 rows=2000 width=0) (actual time=1.337..1.337 rows=0 loops=1)
              Buffers: shared hit=176
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=1.318..1.318 rows=2 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=176
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=0.019..0.019 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.030 ms
Execution Time: 1.699 ms
```

#### nested: contains/nested_array — `@>` order status + item product

*plaintext* — parameters: $1 jsonb; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_plain_v4_100000 WHERE plain @> $1 LIMIT 10
```

```
Limit  (cost=30.87..59.62 rows=10 width=4) (actual time=1.233..1.314 rows=10 loops=1)
  Buffers: shared hit=56
  ->  Bitmap Heap Scan on json_tree_large_plain_v4_100000  (cost=30.87..2905.68 rows=1000 width=4) (actual time=1.233..1.314 rows=10 loops=1)
        Recheck Cond: (plain @> $1)
        Heap Blocks: exact=7
        Buffers: shared hit=56
        ->  Bitmap Index Scan on json_tree_large_plain_v4_100000_gin_idx  (cost=0.00..30.62 rows=1000 width=0) (actual time=0.983..0.983 rows=4617 loops=1)
              Index Cond: (plain @> $1)
              Buffers: shared hit=22
Planning:
  Buffers: shared hit=1
Planning Time: 0.013 ms
Execution Time: 1.319 ms
```

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE value @> $1::jsonb::eql_v3.query_jsonb LIMIT 10
```

```
Limit  (cost=0.00..271.37 rows=10 width=4) (actual time=7.922..157.199 rows=10 loops=1)
  Buffers: shared hit=1859
  ->  Seq Scan on json_ste_vec_large_encrypted_v3_100000  (cost=0.00..27137.00 rows=1000 width=4) (actual time=7.921..157.197 rows=10 loops=1)
        Filter: ((eql_v3.to_ste_vec_query(value))::jsonb @> (('{"sv": [{"s": "0062378a2a2037bf4cdd8c78c2a1aeb0", "hm": "25612f1751b8619090a88a84e96ef32a"}, {"s": "…[2,436 chars]'::jsonb)::eql_v3.query_jsonb)::jsonb)
        Rows Removed by Filter: 97
        Buffers: shared hit=1859
Planning Time: 0.125 ms
Execution Time: 157.205 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE value @> $1::text::jsonb::eql_v4.query_json_tree LIMIT 10
```

```
Limit  (cost=61.76..167.97 rows=10 width=4) (actual time=6.266..8.486 rows=10 loops=1)
  Buffers: shared hit=572
  ->  Bitmap Heap Scan on json_tree_large_encrypted_v4_100000  (cost=61.76..10788.76 rows=1010 width=4) (actual time=6.266..8.485 rows=10 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=1
        Buffers: shared hit=572
        ->  BitmapOr  (cost=61.76..61.76 rows=2000 width=0) (actual time=6.041..6.041 rows=0 loops=1)
              Buffers: shared hit=331
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=6.011..6.011 rows=4617 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=331
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=0.030..0.030 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.034 ms
Execution Time: 8.495 ms
```

#### nested: contains/nested_array_fn — same, function spelling

*plaintext:* no query of its own: native jsonb has one containment spelling, `@>`, measured in the operator row.

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE eql_v3.jsonb_contains(value, $1::jsonb) LIMIT 10
```

```
Limit  (cost=41.26..58.99 rows=10 width=4) (actual time=6.101..6.101 rows=10 loops=1)
  Buffers: shared hit=370
  ->  Bitmap Heap Scan on json_ste_vec_large_encrypted_v3_100000  (cost=41.26..928.10 rows=500 width=4) (actual time=6.100..6.101 rows=10 loops=1)
        Recheck Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
        Heap Blocks: exact=1
        Buffers: shared hit=370
        ->  Bitmap Index Scan on json_ste_vec_large_encrypted_v3_100000_jsonb_array_index  (cost=0.00..41.13 rows=500 width=0) (actual time=6.064..6.064 rows=4617 loops=1)
              Index Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
              Buffers: shared hit=369
Planning:
  Buffers: shared hit=1
Planning Time: 0.055 ms
Execution Time: 6.112 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE eql_v4.json_tree_contains(value, $1::text::jsonb::eql_v4.query_json_tree) LIMIT 10
```

```
Limit  (cost=61.76..167.97 rows=10 width=4) (actual time=6.266..8.501 rows=10 loops=1)
  Buffers: shared hit=572
  ->  Bitmap Heap Scan on json_tree_large_encrypted_v4_100000  (cost=61.76..10788.76 rows=1010 width=4) (actual time=6.266..8.500 rows=10 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=1
        Buffers: shared hit=572
        ->  BitmapOr  (cost=61.76..61.76 rows=2000 width=0) (actual time=6.038..6.038 rows=0 loops=1)
              Buffers: shared hit=331
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=6.010..6.010 rows=4617 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=331
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=0.028..0.028 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.032 ms
Execution Time: 8.510 ms
```

#### nested: field_eq/nested — `$.user.email` = value (containment)

*plaintext* — parameters: $1 jsonb; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_plain_v4_100000 WHERE plain @> $1 LIMIT 10
```

```
Limit  (cost=30.87..59.62 rows=10 width=4) (actual time=0.006..0.006 rows=1 loops=1)
  Buffers: shared hit=5
  ->  Bitmap Heap Scan on json_tree_large_plain_v4_100000  (cost=30.87..2905.68 rows=1000 width=4) (actual time=0.006..0.006 rows=1 loops=1)
        Recheck Cond: (plain @> $1)
        Heap Blocks: exact=1
        Buffers: shared hit=5
        ->  Bitmap Index Scan on json_tree_large_plain_v4_100000_gin_idx  (cost=0.00..30.62 rows=1000 width=0) (actual time=0.002..0.002 rows=1 loops=1)
              Index Cond: (plain @> $1)
              Buffers: shared hit=4
Planning:
  Buffers: shared hit=1
Planning Time: 0.012 ms
Execution Time: 0.009 ms
```

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE eql_v3.jsonb_contains(value, $1::jsonb) LIMIT 10
```

```
Limit  (cost=41.26..58.99 rows=10 width=4) (actual time=0.164..0.165 rows=1 loops=1)
  Buffers: shared hit=28
  ->  Bitmap Heap Scan on json_ste_vec_large_encrypted_v3_100000  (cost=41.26..928.10 rows=500 width=4) (actual time=0.164..0.164 rows=1 loops=1)
        Recheck Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
        Heap Blocks: exact=1
        Buffers: shared hit=28
        ->  Bitmap Index Scan on json_ste_vec_large_encrypted_v3_100000_jsonb_array_index  (cost=0.00..41.13 rows=500 width=0) (actual time=0.163..0.163 rows=1 loops=1)
              Index Cond: (eql_v3.jsonb_array((value)::jsonb) @> eql_v3.jsonb_array(($1)::jsonb))
              Buffers: shared hit=27
Planning:
  Buffers: shared hit=1
Planning Time: 0.051 ms
Execution Time: 0.171 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE value @> $1::text::jsonb::eql_v4.query_json_tree LIMIT 10
```

```
Limit  (cost=61.76..167.97 rows=10 width=4) (actual time=0.327..0.328 rows=1 loops=1)
  Buffers: shared hit=62
  ->  Bitmap Heap Scan on json_tree_large_encrypted_v4_100000  (cost=61.76..10788.76 rows=1010 width=4) (actual time=0.327..0.327 rows=1 loops=1)
        Recheck Cond: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text)))
        Filter: ((eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text)) OR ((((($1)::jsonb)::eql_v4.query_json_tree)::jsonb ? 'alt'::text) AND (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))))
        Heap Blocks: exact=1
        Buffers: shared hit=62
        ->  BitmapOr  (cost=61.76..61.76 rows=2000 width=0) (actual time=0.188..0.189 rows=0 loops=1)
              Buffers: shared hit=39
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=0.178..0.178 rows=1 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb - 'alt'::text))
                    Buffers: shared hit=39
              ->  Bitmap Index Scan on json_tree_large_encrypted_v4_100000_query_gin_idx  (cost=0.00..30.63 rows=1000 width=0) (actual time=0.010..0.010 rows=0 loops=1)
                    Index Cond: (eql_v4.json_tree_query(value) @> (((($1)::jsonb)::eql_v4.query_json_tree)::jsonb -> 'alt'::text))
Planning:
  Buffers: shared hit=2
Planning Time: 0.031 ms
Execution Time: 0.335 ms
```

#### nested: field_gt/nested — `$.company.salary` > value

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_plain_v4_100000 WHERE (plain -> 'company' ->> 'salary')::int > $1 LIMIT 10
```

```
Limit  (cost=0.00..3.35 rows=10 width=4) (actual time=0.006..0.081 rows=10 loops=1)
  Buffers: shared hit=32
  ->  Seq Scan on json_tree_large_plain_v4_100000  (cost=0.00..11170.00 rows=33333 width=4) (actual time=0.005..0.081 rows=10 loops=1)
        Filter: ((((plain -> 'company'::text) ->> 'salary'::text))::integer > $1)
        Rows Removed by Filter: 4
        Buffers: shared hit=32
Planning Time: 0.012 ms
Execution Time: 0.083 ms
```

*SteVec* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE eql_v3.jsonb_path_query_first(value, '7a3882d0f44d16913361486af5f74096') > $1::jsonb::public.jsonb_entry LIMIT 10
```

```
Limit  (cost=0.68..2.17 rows=10 width=4) (actual time=0.126..0.128 rows=10 loops=1)
  Buffers: shared hit=13
  ->  Index Scan using json_ste_vec_large_encrypted_v3_100000_field_order_idx on json_ste_vec_large_encrypted_v3_100000  (cost=0.68..4995.76 rows=33333 width=4) (actual time=0.126..0.127 rows=10 loops=1)
        Index Cond: (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, '7a3882d0f44d16913361486af5f74096'::text)) > eql_v3.ore_cllw((($1)::jsonb)::jsonb_entry))
        Buffers: shared hit=13
Planning Time: 0.043 ms
Execution Time: 0.133 ms
```

*json_tree* — parameters: $1 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE (value -> 'f37909387e6b7f65fbb88f01ff94e2cc'::text) > $1::text::jsonb::eql_v4.query_json_tree_leaf LIMIT 10
```

```
Limit  (cost=0.43..1.77 rows=10 width=4) (actual time=0.004..0.006 rows=10 loops=1)
  Buffers: shared hit=13
  ->  Index Scan using json_tree_large_encrypted_v4_100000_field_order_idx on json_tree_large_encrypted_v4_100000  (cost=0.43..4475.76 rows=33333 width=4) (actual time=0.004..0.005 rows=10 loops=1)
        Index Cond: (decode(((eql_v4.json_tree_path_query_first((value)::jsonb, 'f37909387e6b7f65fbb88f01ff94e2cc'::text))::jsonb ->> 'op'::text), 'hex'::text) > decode((((($1)::jsonb)::eql_v4.query_json_tree_leaf)::jsonb ->> 'op'::text), 'hex'::text))
        Buffers: shared hit=13
Planning Time: 0.053 ms
Execution Time: 0.011 ms
```

#### nested: field_between/nested — `$.company.salary` between lo and hi

*plaintext* — parameters: $1 int4, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_plain_v4_100000 WHERE (plain -> 'company' ->> 'salary')::int BETWEEN $1 AND $2 LIMIT 10
```

```
Limit  (cost=0.00..31.99 rows=10 width=4) (actual time=0.049..2.125 rows=10 loops=1)
  Buffers: shared hit=874
  ->  Seq Scan on json_tree_large_plain_v4_100000  (cost=0.00..12420.00 rows=3882 width=4) (actual time=0.048..2.124 rows=10 loops=1)
        Filter: (((((plain -> 'company'::text) ->> 'salary'::text))::integer >= 151704) AND ((((plain -> 'company'::text) ->> 'salary'::text))::integer <= 161707))
        Rows Removed by Filter: 235
        Buffers: shared hit=874
Planning Time: 0.028 ms
Execution Time: 2.129 ms
```

*SteVec* — parameters: $1 text, $2 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 WHERE eql_v3.jsonb_path_query_first(value, '7a3882d0f44d16913361486af5f74096') >= $1::jsonb::public.jsonb_entry AND eql_v3.jsonb_path_query_first(value, '7a3882d0f44d16913361486af5f74096') <= $2::jsonb::public.jsonb_entry LIMIT 10
```

```
Limit  (cost=0.92..10.17 rows=10 width=4) (actual time=0.258..0.260 rows=10 loops=1)
  Buffers: shared hit=13
  ->  Index Scan using json_ste_vec_large_encrypted_v3_100000_field_order_idx on json_ste_vec_large_encrypted_v3_100000  (cost=0.92..2776.68 rows=3001 width=4) (actual time=0.258..0.260 rows=10 loops=1)
        Index Cond: ((eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, '7a3882d0f44d16913361486af5f74096'::text)) >= eql_v3.ore_cllw(('{"a": false, "c": "mBbLn><D1!#x(SI;&dMPa~SHx7|MtKCEI3f*yq|b;sawkI@BGBV?=;lti&KSNkBrC#Qf_2)L<bqT|9d2>…[380 chars]'::jsonb)::jsonb_entry)) AND (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, '7a3882d0f44d16913361486af5f74096'::text)) <= eql_v3.ore_cllw(('{"a": false, "c": "mBbLjT5gez2F!#RaRzr<ynlAY7-=Hu$z@pEWh-(VYOk_5jURhhzTbsRIm93~)G{jx(27s++E+Y4B^4Zhy…[380 chars]'::jsonb)::jsonb_entry)))
        Buffers: shared hit=13
Planning Time: 0.243 ms
Execution Time: 0.267 ms
```

*json_tree* — parameters: $1 text, $2 text; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 WHERE (value -> 'f37909387e6b7f65fbb88f01ff94e2cc'::text) >= $1::text::jsonb::eql_v4.query_json_tree_leaf AND (value -> 'f37909387e6b7f65fbb88f01ff94e2cc'::text) <= $2::text::jsonb::eql_v4.query_json_tree_leaf LIMIT 10
```

```
Limit  (cost=0.43..9.52 rows=10 width=4) (actual time=0.005..0.007 rows=10 loops=1)
  Buffers: shared hit=13
  ->  Index Scan using json_tree_large_encrypted_v4_100000_field_order_idx on json_tree_large_encrypted_v4_100000  (cost=0.43..2732.53 rows=3005 width=4) (actual time=0.005..0.006 rows=10 loops=1)
        Index Cond: ((decode(((eql_v4.json_tree_path_query_first((value)::jsonb, 'f37909387e6b7f65fbb88f01ff94e2cc'::text))::jsonb ->> 'op'::text), 'hex'::text) >= decode(((('{"s": "f37909387e6b7f65fbb88f01ff94e2cc", "op": "0126916d0e57e884d9861df5286c033ca0b564d003e7ba587767e24c912cfcf37b90b2be93543039e1e045f39e7668c8d78fa429514cce9fd1c677e492fa827285"}'::jsonb)::eql_v4.query_json_tree_leaf)::jsonb ->> 'op'::text), 'hex'::text)) AND (decode(((eql_v4.json_tree_path_query_first((value)::jsonb, 'f37909387e6b7f65fbb88f01ff94e2cc'::text))::jsonb ->> 'op'::text), 'hex'::text) <= decode(((('{"s": "f37909387e6b7f65fbb88f01ff94e2cc", "op": "0126916d0e57e884d9861df5286c033d20efc08b66d08428f9cf688936131e12f2af630627a0dcfa9efd749488a622b5de6822d091e05d23e48bf1da22cf08e993"}'::jsonb)::eql_v4.query_json_tree_leaf)::jsonb ->> 'op'::text), 'hex'::text)))
        Buffers: shared hit=13
Planning Time: 0.192 ms
Execution Time: 0.014 ms
```

#### nested: field_order/nested — ORDER BY `$.company.salary` LIMIT 10

*plaintext* — parameters: no parameters; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_plain_v4_100000 ORDER BY (plain -> 'company' ->> 'salary')::int LIMIT 10
```

```
Limit  (cost=0.29..4.22 rows=10 width=8) (actual time=0.009..0.056 rows=10 loops=1)
  Buffers: shared hit=30
  ->  Index Scan using json_tree_large_plain_v4_100000_salary_idx on json_tree_large_plain_v4_100000  (cost=0.29..39236.27 rows=100000 width=8) (actual time=0.009..0.055 rows=10 loops=1)
        Buffers: shared hit=30
Planning Time: 0.000 ms
Execution Time: 0.058 ms
```

*SteVec* — parameters: no parameters; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_ste_vec_large_encrypted_v3_100000 ORDER BY eql_v3.ore_cllw(eql_v3.jsonb_path_query_first(value, '7a3882d0f44d16913361486af5f74096')) LIMIT 10
```

```
Limit  (cost=0.42..6.38 rows=10 width=36) (actual time=0.485..4.195 rows=10 loops=1)
  Buffers: shared hit=529
  ->  Index Scan using json_ste_vec_large_encrypted_v3_100000_field_order_idx on json_ste_vec_large_encrypted_v3_100000  (cost=0.42..59636.22 rows=100000 width=36) (actual time=0.484..4.194 rows=10 loops=1)
        Buffers: shared hit=529
Planning Time: 0.000 ms
Execution Time: 4.198 ms
```

*json_tree* — parameters: no parameters; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT id FROM json_tree_large_encrypted_v4_100000 ORDER BY eql_v4.ord_term(value -> 'f37909387e6b7f65fbb88f01ff94e2cc'::text) LIMIT 10
```

```
Limit  (cost=0.42..26.28 rows=10 width=36) (actual time=0.390..3.487 rows=10 loops=1)
  Buffers: shared hit=465
  ->  Index Scan using json_tree_large_encrypted_v4_100000_field_order_idx on json_tree_large_encrypted_v4_100000  (cost=0.42..258580.42 rows=100000 width=36) (actual time=0.390..3.486 rows=10 loops=1)
        Buffers: shared hit=465
Planning Time: 0.000 ms
Execution Time: 3.490 ms
```

#### nested: extract/nested — `$.company.salary` of one row

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (plain -> 'company' -> 'salary')::text FROM json_tree_large_plain_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.32 rows=1 width=32) (actual time=0.007..0.007 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=3
Planning Time: 0.011 ms
Execution Time: 0.010 ms
```

*SteVec* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (value -> '7a3882d0f44d16913361486af5f74096'::text)::jsonb::text FROM json_ste_vec_large_encrypted_v3_100000 WHERE id = $1
```

```
Index Scan using json_ste_vec_large_encrypted_v3_100000_pkey on json_ste_vec_large_encrypted_v3_100000  (cost=0.29..8.57 rows=1 width=32) (actual time=0.340..0.341 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=48
Planning Time: 0.019 ms
Execution Time: 0.343 ms
```

*json_tree* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (value -> 'f37909387e6b7f65fbb88f01ff94e2cc'::text)::jsonb::text FROM json_tree_large_encrypted_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..10.82 rows=1 width=32) (actual time=0.304..0.304 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=47
Planning Time: 0.023 ms
Execution Time: 0.307 ms
```

#### nested: path_query/wildcard — `$.orders[*].status`, one row

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::text FROM json_tree_large_plain_v4_100000, jsonb_path_query(plain, '$.orders[*].status') e WHERE id = $1
```

```
Nested Loop  (cost=0.29..33.31 rows=1000 width=32) (actual time=0.008..0.009 rows=5 loops=1)
  Buffers: shared hit=3
  ->  Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.31 rows=1 width=656) (actual time=0.001..0.002 rows=1 loops=1)
        Index Cond: (id = $1)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_path_query e  (cost=0.00..10.00 rows=1000 width=32) (actual time=0.006..0.006 rows=5 loops=1)
Planning Time: 0.015 ms
Execution Time: 0.012 ms
```

*SteVec* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_ste_vec_large_encrypted_v3_100000, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=1.051..1.056 rows=5 loops=1)
  Buffers: shared hit=168
  ->  Index Scan using json_ste_vec_large_encrypted_v3_100000_pkey on json_ste_vec_large_encrypted_v3_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_path_query e  (cost=0.25..10.25 rows=1000 width=32) (actual time=1.047..1.047 rows=5 loops=1)
        Buffers: shared hit=165
Planning Time: 0.015 ms
Execution Time: 1.059 ms
```

*json_tree* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_large_encrypted_v4_100000, eql_v4.json_tree_path_query(value::jsonb, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=1.098..1.104 rows=5 loops=1)
  Buffers: shared hit=179
  ->  Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_path_query e  (cost=0.25..10.25 rows=1000 width=32) (actual time=1.094..1.094 rows=5 loops=1)
        Buffers: shared hit=176
Planning Time: 0.016 ms
Execution Time: 1.107 ms
```

#### nested: subtree/company — subtree `$.company` (6 leaves)

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (plain -> 'company')::text FROM json_tree_large_plain_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.32 rows=1 width=32) (actual time=0.005..0.005 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=3
Planning Time: 0.010 ms
Execution Time: 0.008 ms
```

*SteVec* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (value -> 'aa06852b97a000d2a2ed9169b3e2bfb3'::text)::jsonb::text FROM json_ste_vec_large_encrypted_v3_100000 WHERE id = $1
```

```
Index Scan using json_ste_vec_large_encrypted_v3_100000_pkey on json_ste_vec_large_encrypted_v3_100000  (cost=0.29..8.57 rows=1 width=32) (actual time=0.338..0.338 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=48
Planning Time: 0.018 ms
Execution Time: 0.341 ms
```

*json_tree* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_large_encrypted_v4_100000, eql_v4.json_tree_subtree(value, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=1.526..1.534 rows=6 loops=1)
  Buffers: shared hit=80
  ->  Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree e  (cost=0.25..10.25 rows=1000 width=32) (actual time=1.522..1.522 rows=6 loops=1)
        Buffers: shared hit=77
Planning Time: 0.018 ms
Execution Time: 1.537 ms
```

#### nested: subtree/user — subtree `$.user` (12 leaves)

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (plain -> 'user')::text FROM json_tree_large_plain_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.32 rows=1 width=32) (actual time=0.006..0.006 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=3
Planning Time: 0.011 ms
Execution Time: 0.009 ms
```

*SteVec* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (value -> 'e664932c6ea787b9719aac08f464437b'::text)::jsonb::text FROM json_ste_vec_large_encrypted_v3_100000 WHERE id = $1
```

```
Index Scan using json_ste_vec_large_encrypted_v3_100000_pkey on json_ste_vec_large_encrypted_v3_100000  (cost=0.29..8.57 rows=1 width=32) (actual time=0.339..0.339 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=48
Planning Time: 0.019 ms
Execution Time: 0.342 ms
```

*json_tree* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_large_encrypted_v4_100000, eql_v4.json_tree_subtree(value, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=1.979..1.995 rows=12 loops=1)
  Buffers: shared hit=80
  ->  Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree e  (cost=0.25..10.25 rows=1000 width=32) (actual time=1.975..1.975 rows=12 loops=1)
        Buffers: shared hit=77
Planning Time: 0.019 ms
Execution Time: 1.999 ms
```

#### nested: subtree/orders — subtree `$.orders`, default budget

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (plain -> 'orders')::text FROM json_tree_large_plain_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.32 rows=1 width=32) (actual time=0.013..0.013 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=3
Planning Time: 0.011 ms
Execution Time: 0.017 ms
```

*SteVec* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (value -> 'b418a3fd4b4547815842585c07002390'::text)::jsonb::text FROM json_ste_vec_large_encrypted_v3_100000 WHERE id = $1
```

```
Index Scan using json_ste_vec_large_encrypted_v3_100000_pkey on json_ste_vec_large_encrypted_v3_100000  (cost=0.29..8.57 rows=1 width=32) (actual time=0.344..0.344 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=48
Planning Time: 0.019 ms
Execution Time: 0.347 ms
```

*json_tree* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_large_encrypted_v4_100000, eql_v4.json_tree_subtree(value, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=5.042..5.043 rows=1 loops=1)
  Buffers: shared hit=80
  ->  Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree e  (cost=0.25..10.25 rows=1000 width=32) (actual time=5.032..5.032 rows=1 loops=1)
        Buffers: shared hit=77
Planning Time: 0.021 ms
Execution Time: 5.047 ms
```

#### nested: subtree/orders_walked — subtree `$.orders`, budget 0

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT (plain -> 'orders')::text FROM json_tree_large_plain_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.32 rows=1 width=32) (actual time=0.013..0.013 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=3
Planning Time: 0.011 ms
Execution Time: 0.017 ms
```

*SteVec:* no query of its own: SteVec has no walk and no budget; `->` on the container returns the subtree sealed whole, which is the default-budget row.

*json_tree* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_large_encrypted_v4_100000, eql_v4.json_tree_subtree(value, $1::text, 0) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=10.222..10.389 rows=130 loops=1)
  Buffers: shared hit=69
  ->  Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree e  (cost=0.25..10.25 rows=1000 width=32) (actual time=10.218..10.221 rows=130 loops=1)
        Buffers: shared hit=66
Planning Time: 0.019 ms
Execution Time: 10.396 ms
```

#### nested: subtree_at/items — `$.orders[*].items`, default budget

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::text FROM json_tree_large_plain_v4_100000, jsonb_path_query(plain, '$.orders[*].items') e WHERE id = $1
```

```
Nested Loop  (cost=0.29..33.31 rows=1000 width=32) (actual time=0.008..0.014 rows=5 loops=1)
  Buffers: shared hit=3
  ->  Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.31 rows=1 width=656) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $1)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_path_query e  (cost=0.00..10.00 rows=1000 width=32) (actual time=0.004..0.004 rows=5 loops=1)
Planning Time: 0.015 ms
Execution Time: 0.017 ms
```

*SteVec* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_ste_vec_large_encrypted_v3_100000, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=1.048..1.055 rows=5 loops=1)
  Buffers: shared hit=168
  ->  Index Scan using json_ste_vec_large_encrypted_v3_100000_pkey on json_ste_vec_large_encrypted_v3_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_path_query e  (cost=0.25..10.25 rows=1000 width=32) (actual time=1.045..1.045 rows=5 loops=1)
        Buffers: shared hit=165
Planning Time: 0.015 ms
Execution Time: 1.058 ms
```

*json_tree* — parameters: $1 text, $2 text[], $3 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_large_encrypted_v4_100000, eql_v4.json_tree_subtree_at(value, $1::text, $2::text[]) e WHERE id = $3
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=6.517..6.518 rows=1 loops=1)
  Buffers: shared hit=80
  ->  Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $3)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree_at e  (cost=0.25..10.25 rows=1000 width=32) (actual time=6.507..6.507 rows=1 loops=1)
        Buffers: shared hit=77
Planning Time: 0.021 ms
Execution Time: 6.522 ms
```

#### nested: subtree_at/items_walked — `$.orders[*].items`, budget 0

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::text FROM json_tree_large_plain_v4_100000, jsonb_path_query(plain, '$.orders[*].items') e WHERE id = $1
```

```
Nested Loop  (cost=0.29..33.31 rows=1000 width=32) (actual time=0.007..0.014 rows=5 loops=1)
  Buffers: shared hit=3
  ->  Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.31 rows=1 width=656) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $1)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_path_query e  (cost=0.00..10.00 rows=1000 width=32) (actual time=0.004..0.004 rows=5 loops=1)
Planning Time: 0.015 ms
Execution Time: 0.017 ms
```

*SteVec:* no query of its own: SteVec has no walk and no budget; `->` on the container returns the subtree sealed whole, which is the default-budget row.

*json_tree* — parameters: $1 text, $2 text[], $3 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_large_encrypted_v4_100000, eql_v4.json_tree_subtree_at(value, $1::text, $2::text[], 0) e WHERE id = $3
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=10.091..10.225 rows=105 loops=1)
  Buffers: shared hit=69
  ->  Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $3)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree_at e  (cost=0.25..10.25 rows=1000 width=32) (actual time=10.087..10.090 rows=105 loops=1)
        Buffers: shared hit=66
Planning Time: 0.021 ms
Execution Time: 10.232 ms
```

#### nested: children/orders — children of `$.orders`

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::text FROM json_tree_large_plain_v4_100000, jsonb_array_elements(plain -> 'orders') e WHERE id = $1
```

```
Nested Loop  (cost=0.30..10.81 rows=100 width=32) (actual time=0.009..0.017 rows=5 loops=1)
  Buffers: shared hit=3
  ->  Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.31 rows=1 width=656) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $1)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_array_elements e  (cost=0.01..1.00 rows=100 width=32) (actual time=0.005..0.005 rows=5 loops=1)
Planning Time: 0.014 ms
Execution Time: 0.020 ms
```

*SteVec* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_ste_vec_large_encrypted_v3_100000, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=1.050..1.058 rows=5 loops=1)
  Buffers: shared hit=168
  ->  Index Scan using json_ste_vec_large_encrypted_v3_100000_pkey on json_ste_vec_large_encrypted_v3_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on jsonb_path_query e  (cost=0.25..10.25 rows=1000 width=32) (actual time=1.046..1.046 rows=5 loops=1)
        Buffers: shared hit=165
Planning Time: 0.015 ms
Execution Time: 1.061 ms
```

*json_tree* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT e::jsonb::text FROM json_tree_large_encrypted_v4_100000, eql_v4.json_tree_children(value, $1::text) e WHERE id = $2
```

```
Nested Loop  (cost=0.54..33.56 rows=1000 width=32) (actual time=1.188..1.193 rows=5 loops=1)
  Buffers: shared hit=58
  ->  Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = $2)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_children e  (cost=0.25..10.25 rows=1000 width=32) (actual time=1.185..1.185 rows=5 loops=1)
        Buffers: shared hit=55
Planning Time: 0.019 ms
Execution Time: 1.197 ms
```

#### nested: child_count/orders — child count of `$.orders`

*plaintext* — parameters: $1 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT jsonb_array_length(plain -> 'orders') FROM json_tree_large_plain_v4_100000 WHERE id = $1
```

```
Index Scan using json_tree_large_plain_v4_100000_pkey on json_tree_large_plain_v4_100000  (cost=0.29..8.32 rows=1 width=4) (actual time=0.004..0.005 rows=1 loops=1)
  Index Cond: (id = $1)
  Buffers: shared hit=3
Planning Time: 0.011 ms
Execution Time: 0.008 ms
```

*SteVec* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT count(*) FROM json_ste_vec_large_encrypted_v3_100000, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2
```

```
Aggregate  (cost=31.06..31.07 rows=1 width=8) (actual time=1.049..1.049 rows=1 loops=1)
  Buffers: shared hit=168
  ->  Nested Loop  (cost=0.54..28.56 rows=1000 width=0) (actual time=1.048..1.048 rows=5 loops=1)
        Buffers: shared hit=168
        ->  Index Scan using json_ste_vec_large_encrypted_v3_100000_pkey on json_ste_vec_large_encrypted_v3_100000  (cost=0.29..8.31 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
              Index Cond: (id = $2)
              Buffers: shared hit=3
        ->  Function Scan on jsonb_path_query e  (cost=0.25..10.25 rows=1000 width=0) (actual time=1.046..1.046 rows=5 loops=1)
              Buffers: shared hit=165
Planning Time: 0.015 ms
Execution Time: 1.053 ms
```

*json_tree* — parameters: $1 text, $2 int4; EXPLAIN after 5 warm executions of the prepared statement.

```sql
SELECT eql_v4.json_tree_child_count(value, $1::text) FROM json_tree_large_encrypted_v4_100000 WHERE id = $2
```

```
Index Scan using json_tree_large_encrypted_v4_100000_pkey on json_tree_large_encrypted_v4_100000  (cost=0.29..8.56 rows=1 width=4) (actual time=1.197..1.198 rows=1 loops=1)
  Index Cond: (id = $2)
  Buffers: shared hit=58
Planning Time: 0.020 ms
Execution Time: 1.200 ms
```

## Appendix B: indexes

Every index on every table, as present after the timing session (`pg_indexes`,
with sizes).

`json_tree_small_plain_v4_10000`:

```
CREATE INDEX json_tree_small_plain_v4_10000_age_idx ON public.json_tree_small_plain_v4_10000 USING btree ((((plain ->> 'age'::text))::integer))   -- 0.1 MiB
CREATE INDEX json_tree_small_plain_v4_10000_gin_idx ON public.json_tree_small_plain_v4_10000 USING gin (plain jsonb_path_ops)   -- 0.9 MiB
CREATE UNIQUE INDEX json_tree_small_plain_v4_10000_pkey ON public.json_tree_small_plain_v4_10000 USING btree (id)   -- 0.2 MiB
```

`json_ste_vec_small_encrypted_v3_10000`:

```
CREATE INDEX json_ste_vec_small_encrypted_v3_10000_field_eq_idx ON public.json_ste_vec_small_encrypted_v3_10000 USING btree (eql_v3.eq_term(eql_v3.jsonb_path_query_first((value)::jsonb, 'b81940706ae05004a9b95b6dbd1aaa60'::text)))   -- 2.4 MiB
CREATE INDEX json_ste_vec_small_encrypted_v3_10000_field_order_idx ON public.json_ste_vec_small_encrypted_v3_10000 USING btree (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, 'cc6e1824ab3319e8f92dc6199ce671b9'::text)))   -- 1.1 MiB
CREATE INDEX json_ste_vec_small_encrypted_v3_10000_jsonb_array_index ON public.json_ste_vec_small_encrypted_v3_10000 USING gin (eql_v3.jsonb_array((value)::jsonb))   -- 10.8 MiB
CREATE UNIQUE INDEX json_ste_vec_small_encrypted_v3_10000_pkey ON public.json_ste_vec_small_encrypted_v3_10000 USING btree (id)   -- 0.2 MiB
```

`json_tree_small_encrypted_v4_10000`:

```
CREATE INDEX json_tree_small_encrypted_v4_10000_field_order_idx ON public.json_tree_small_encrypted_v4_10000 USING btree (eql_v4.ord_term((value -> 'a7b53d9b98181a98c49559f86362fcce'::text)))   -- 0.1 MiB
CREATE UNIQUE INDEX json_tree_small_encrypted_v4_10000_pkey ON public.json_tree_small_encrypted_v4_10000 USING btree (id)   -- 0.2 MiB
CREATE INDEX json_tree_small_encrypted_v4_10000_query_gin_idx ON public.json_tree_small_encrypted_v4_10000 USING gin (eql_v4.json_tree_query(value) jsonb_path_ops)   -- 1.0 MiB
```

`json_tree_large_plain_v4_10000`:

```
CREATE INDEX json_tree_large_plain_v4_10000_gin_idx ON public.json_tree_large_plain_v4_10000 USING gin (plain jsonb_path_ops)   -- 38.9 MiB
CREATE UNIQUE INDEX json_tree_large_plain_v4_10000_pkey ON public.json_tree_large_plain_v4_10000 USING btree (id)   -- 0.2 MiB
CREATE INDEX json_tree_large_plain_v4_10000_salary_idx ON public.json_tree_large_plain_v4_10000 USING btree (((((plain -> 'company'::text) ->> 'salary'::text))::integer))   -- 0.2 MiB
```

`json_ste_vec_large_encrypted_v3_10000`:

```
CREATE INDEX json_ste_vec_large_encrypted_v3_10000_field_order_idx ON public.json_ste_vec_large_encrypted_v3_10000 USING btree (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, '7a3882d0f44d16913361486af5f74096'::text)))   -- 1.1 MiB
CREATE INDEX json_ste_vec_large_encrypted_v3_10000_jsonb_array_index ON public.json_ste_vec_large_encrypted_v3_10000 USING gin (eql_v3.jsonb_array((value)::jsonb))   -- 873.3 MiB
CREATE UNIQUE INDEX json_ste_vec_large_encrypted_v3_10000_pkey ON public.json_ste_vec_large_encrypted_v3_10000 USING btree (id)   -- 0.2 MiB
```

`json_tree_large_encrypted_v4_10000`:

```
CREATE INDEX json_tree_large_encrypted_v4_10000_field_order_idx ON public.json_tree_large_encrypted_v4_10000 USING btree (eql_v4.ord_term((value -> 'f37909387e6b7f65fbb88f01ff94e2cc'::text)))   -- 0.9 MiB
CREATE UNIQUE INDEX json_tree_large_encrypted_v4_10000_pkey ON public.json_tree_large_encrypted_v4_10000 USING btree (id)   -- 0.2 MiB
CREATE INDEX json_tree_large_encrypted_v4_10000_query_gin_idx ON public.json_tree_large_encrypted_v4_10000 USING gin (eql_v4.json_tree_query(value) jsonb_path_ops)   -- 39.9 MiB
```

`json_tree_small_plain_v4_100000`:

```
CREATE INDEX json_tree_small_plain_v4_100000_age_idx ON public.json_tree_small_plain_v4_100000 USING btree ((((plain ->> 'age'::text))::integer))   -- 0.7 MiB
CREATE INDEX json_tree_small_plain_v4_100000_gin_idx ON public.json_tree_small_plain_v4_100000 USING gin (plain jsonb_path_ops)   -- 6.6 MiB
CREATE UNIQUE INDEX json_tree_small_plain_v4_100000_pkey ON public.json_tree_small_plain_v4_100000 USING btree (id)   -- 2.2 MiB
```

`json_ste_vec_small_encrypted_v3_100000`:

```
CREATE INDEX json_ste_vec_small_encrypted_v3_100000_field_eq_idx ON public.json_ste_vec_small_encrypted_v3_100000 USING btree (eql_v3.eq_term(eql_v3.jsonb_path_query_first((value)::jsonb, 'b81940706ae05004a9b95b6dbd1aaa60'::text)))   -- 23.2 MiB
CREATE INDEX json_ste_vec_small_encrypted_v3_100000_field_order_idx ON public.json_ste_vec_small_encrypted_v3_100000 USING btree (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, 'cc6e1824ab3319e8f92dc6199ce671b9'::text)))   -- 10.9 MiB
CREATE INDEX json_ste_vec_small_encrypted_v3_100000_jsonb_array_index ON public.json_ste_vec_small_encrypted_v3_100000 USING gin (eql_v3.jsonb_array((value)::jsonb))   -- 92.2 MiB
CREATE UNIQUE INDEX json_ste_vec_small_encrypted_v3_100000_pkey ON public.json_ste_vec_small_encrypted_v3_100000 USING btree (id)   -- 2.2 MiB
```

`json_tree_small_encrypted_v4_100000`:

```
CREATE INDEX json_tree_small_encrypted_v4_100000_field_order_idx ON public.json_tree_small_encrypted_v4_100000 USING btree (eql_v4.ord_term((value -> 'a7b53d9b98181a98c49559f86362fcce'::text)))   -- 0.8 MiB
CREATE UNIQUE INDEX json_tree_small_encrypted_v4_100000_pkey ON public.json_tree_small_encrypted_v4_100000 USING btree (id)   -- 2.2 MiB
CREATE INDEX json_tree_small_encrypted_v4_100000_query_gin_idx ON public.json_tree_small_encrypted_v4_100000 USING gin (eql_v4.json_tree_query(value) jsonb_path_ops)   -- 7.1 MiB
```

`json_tree_large_plain_v4_100000`:

```
CREATE INDEX json_tree_large_plain_v4_100000_gin_idx ON public.json_tree_large_plain_v4_100000 USING gin (plain jsonb_path_ops)   -- 354.8 MiB
CREATE UNIQUE INDEX json_tree_large_plain_v4_100000_pkey ON public.json_tree_large_plain_v4_100000 USING btree (id)   -- 2.2 MiB
CREATE INDEX json_tree_large_plain_v4_100000_salary_idx ON public.json_tree_large_plain_v4_100000 USING btree (((((plain -> 'company'::text) ->> 'salary'::text))::integer))   -- 2.1 MiB
```

`json_ste_vec_large_encrypted_v3_100000`:

```
CREATE INDEX json_ste_vec_large_encrypted_v3_100000_field_order_idx ON public.json_ste_vec_large_encrypted_v3_100000 USING btree (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first((value)::jsonb, '7a3882d0f44d16913361486af5f74096'::text)))   -- 10.9 MiB
CREATE INDEX json_ste_vec_large_encrypted_v3_100000_jsonb_array_index ON public.json_ste_vec_large_encrypted_v3_100000 USING gin (eql_v3.jsonb_array((value)::jsonb))   -- 5,644.3 MiB
CREATE UNIQUE INDEX json_ste_vec_large_encrypted_v3_100000_pkey ON public.json_ste_vec_large_encrypted_v3_100000 USING btree (id)   -- 2.2 MiB
```

`json_tree_large_encrypted_v4_100000`:

```
CREATE INDEX json_tree_large_encrypted_v4_100000_field_order_idx ON public.json_tree_large_encrypted_v4_100000 USING btree (eql_v4.ord_term((value -> 'f37909387e6b7f65fbb88f01ff94e2cc'::text)))   -- 7.9 MiB
CREATE UNIQUE INDEX json_tree_large_encrypted_v4_100000_pkey ON public.json_tree_large_encrypted_v4_100000 USING btree (id)   -- 2.2 MiB
CREATE INDEX json_tree_large_encrypted_v4_100000_query_gin_idx ON public.json_tree_large_encrypted_v4_100000 USING gin (eql_v4.json_tree_query(value) jsonb_path_ops)   -- 358.2 MiB
```
