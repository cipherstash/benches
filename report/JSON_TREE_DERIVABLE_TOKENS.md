# JSON tree: derivable tokens against the gated walk

This report measures cipherstash/stack#1363 (branch `feat/json-tree-derivable-tokens`,
commit 683802bcc, called **derivable** below) against its base (branch
`docs/json-tree-derivable-tokens`, commit edabee625, code-identical to #1358,
called **gates**). Both formats hold the same documents on the same machine and
the same PostgreSQL cluster, resident side by side, and they are timed
interleaved. The design is the last section of
`docs/plans/2026-10-17-json-tree-index.md` in the stack repo, "Design change
(2026-10-24)". The earlier json_tree measurements are in
[JSON_TREE_COMPARISON.md](JSON_TREE_COMPARISON.md).

## Measured (derivable tokens, 2026-10-25, stack edabee625 vs 683802bcc)

### Summary

**Better.**

- **A deep member path** (`$.user.contact.emergency_contact.name`) costs 22%
  less per call (2.48 to 1.93 ms) and sends one 24-byte token where the base
  sent a head token and three gates (96 bytes). The gated walk enumerated
  every sibling at each level it passed through.
- **Single-level wildcards** (`$.orders[*].total`) cost 34 to 58% less at
  N = 8, 64 and 256.
- **Two-level wildcard at N = 8** (73 tokens) costs 80% less.
- **Enumerating a wide node** costs 39 to 49% less, both
  `json_tree_children` and `json_tree_child_count` on a 300-member object
  and a 300-element array, because enumeration no longer hashes a token for
  each child.
- **`$` is now a read under the sealed root** (1.97 ms, the root entry).
  The base refused it.
- **New answers.** Every read now returns the node it names. The base
  returned only the node's descendants, so a top-level scalar
  (`$.first_name`) came back as no rows (#1356).

**Worse.**

- **Reading a whole subtree** costs 38 to 46% more per call: `$.company`,
  `$.user`, `$.orders[2]`, `$.orders` walked out, and `$` with the root open.
  In-server the cost is 45 to 59% more. Two things cause it. Each hit pays
  the member-or-element self-tag test, two hashes for a member and four for an
  element, where the gated walk paid one. And each read builds a second map
  of the whole document, keyed by `x` (0.41 ms per nested document,
  in-server).
- **Walking a wide array** costs 78% more (31 to 55 ms for 300 elements),
  because every element pays the failed member test before the element rule.
- **The budget fallback** (`$.orders` past the default budget) costs 51% more
  (6.4 to 9.6 ms), because the read walks 65 rows before it falls back to the
  sealed root, and each row pays the test.
- **Two-level wildcard at N = 64** (4,161 tokens, 99,864 bytes of tokens per
  call) costs 48% more.
- **`json_tree_child_count` on a small node** costs 59% more (`$.orders`, 5 to
  10 elements: 1.75 to 2.79 ms). It builds the self-tag map twice per call:
  once to test that the node exists, and again inside `json_tree_children`.
- **Wildcard bound.** A wildcard read now has a bound. At N = 8, 33% of the
  nested rows (those with more than 8 orders) refuse with
  `JsonWildcardTruncated`, and their p95 carries the probe's walk.
- **Storage.** A stored nested document is 11.2% larger on disk (31,691 to
  35,228 bytes at 100,000 rows) and its heap 11.0% larger. A flat document is
  7.4% larger. That is +21 characters and +16 bytes on disk per entry.
- **Ingest.** Sealing costs 3 to 9% more and INSERT 5 to 9% more.
- **Work that detoasts the document** pays for its size: `->` on the nested
  corpus costs 7 to 10% more, `ORDER BY ord_term` at a nested path 7% more,
  and nested containment with rechecks 2 to 6% more.

**Unchanged within noise.**

- Member paths one step below a field (`$.company.name`,
  `$.orders[2].status`): +0.4% and +4%.
- Containment on the flat corpus and selective containment on the nested
  one: −1.5% and +0.9%.
- `->` on the flat corpus.
- `json_tree_children` on a small node (+4%).
- The containment GIN: same size, and its build time moved +3 to +4%.
- Every outer query plan: a primary-key index scan feeding a Function Scan
  in both formats, and the containment and ordering scenarios on the same
  indexes.

### What changed

- **Stored form.** Every entry gains an eight-byte self tag `x`
  (`H8("x" ‖ iv ‖ t(node))`, 12 characters of base64), the root and the
  chain heads included.
- **Tokens.** An object member's token no longer hashes its ordinal, so the
  client derives the token of any path with no wildcard. A read sends one
  token, and the server finds the node by `x`. Gates are gone.
- **Reads.** `json_tree_subtree(doc, tokens text[], budget)` replaces
  `json_tree_subtree_at(doc, token, gates, budget)`. It returns each token's
  node at `d = 0` with everything beneath it. Each call builds a second map
  of the whole document (`json_tree_by_self`, keyed by `x`) next to the
  parent-tag map.
- **Walking beneath a node.** For each hit the server tests the member
  rule's token against the hit's `x`, and on a miss the element rule's
  (`json_tree_hit_token`). That is two hashes for a member and four for an
  element, where the gated walk computed one (`json_tree_child_token`).
- **Wildcards.** The client expands `[*]` to N tokens per level and a probe
  at index N (N = 64 by default). A row where a probe finds a node refuses
  with `JsonWildcardTruncated`. Two levels send N² + N + 1 tokens.
- **Enumeration.** `json_tree_children` and `json_tree_child_count` take any
  derivable node's token and no gates. Enumerating one level needs no child
  token, so no rule choice. Both functions also build the self-tag map, to
  find the node.

### How it was run

- **Machine.** Apple M1 Max, 64 GB. Native PostgreSQL 17.10 (Homebrew) on
  port 5400, `shared_buffers` 4 GB, `work_mem` 128 MB, JIT on: the benches
  cluster of the earlier measurements.
- **Databases.** Two new databases on that cluster.
  - `jsonbench_base` holds the v4 bundle built from edabee625, and
    `jsonbench_new` the bundle built from 683802bcc (`mise run build` in
    `packages/eql` of a worktree at each commit). `sql/schema_v4.sql` was
    applied to both.
  - `jsonbench` (v3 SteVec and the older v4 tables) was not touched.
  - The plaintext twins of the flat and nested corpora, at 10,000 and 100,000
    rows, were copied from `jsonbench` into both databases (`md5` of the
    ordered rows checked), so each format sealed the same documents.
  - Both databases are kept, for re-runs.
- **Two more corpora**, to cover what the flat and nested ones cannot.
  - `json_tree_large_open_*_10000`: the nested corpus, id for id, sealed
    with the root open. It is there for the read of `$` with no sealed root.
  - `json_tree_wide_*_1000`: 1,000 documents, each holding one object of 300
    members (`attrs`) and one array of 300 numbers (`list`). They were
    generated once in SQL and copied, so both formats hold the same documents.
- **Bench crate.** `json-tree-bench` builds either format from one source.
  - The `derivable` feature (the default) builds against 683802bcc. The
    `gates` feature builds against edabee625, into
    `json-tree-bench/target-gates`.
  - Each is a git dependency pinned by `rev`, and both resolve side by side in
    one `Cargo.lock`. A `path` dependency on the two worktrees was the other
    choice; the worktrees are temporary, so a path would not reproduce.
  - What differs between the formats is confined to
    `json_tree_bench::ReadKey`: how a read is derived and which SQL it
    calls.
  - The mise tasks are `bench:dt:build`, `bench:dt:ingest`, `bench:dt:check`,
    `bench:dt:plan`, `bench:dt:time` and `report:dt`.
- **Timing harness** (`json-tree-bench/src/bin/dt_bench.rs`).
  - `plan` runs once per build, against that build's database. It derives
    every scenario's SQL and operands and writes them to a file.
  - `time` reads both files and opens one connection to each database, with
    prepared statements. It warms every table it reads. Then, for each pass
    and each scenario, it runs both formats back to back, alternating which
    goes first.
  - **Per call** is the client wall time of one prepared call by id, on the
    same 200 sampled rows in both databases. Three passes give 600 calls per
    scenario and format, reported as median and p95. A scenario with no id
    (containment, `ORDER BY`) runs 30 times per pass. The local round trip is
    included; `noop/by_id` measures it at about 0.1 ms.
  - **In-server per document** applies the scenario to about 500 documents
    spread over the table in one query (`id = ANY($n)`), sums the output,
    and divides by the number of documents. That is the server's cost per row,
    with one round trip in all. Median of 2 passes.
  - **Runs.** The whole 10,000-row matrix ran twice (`time_10000_r1.json`,
    `time_10000_r2.json`); the tables below are run 1, with the noise floor
    taken from the two runs. Containment, extraction and the enumeration of
    `$.orders` ran once more, with 10 passes and 100 repetitions
    (`time_10000_focus.json`); their tables quote that run.
- **Criterion cross-check.** The existing `json_tree_v4` and
  `json_tree_nested_v4` criterion benches (10 samples, query only) ran
  against both databases at 10,000 rows.
  - The derivable runs went through `bench:v4:query:*` into
    `results/query/v4/`, which the `report:json-tree-compare` tables read.
    The gates runs went directly into `results/query/derivable/criterion/`.
  - These were sequential, not interleaved: derivable first, gates about
    fifteen minutes later. The native jsonb controls in the same benches
    drifted by 3 to 25% between them.
  - They agree in direction with the harness, and are listed at the end.
- **Correctness.** `check_json_tree_v4` was ported to both formats
  (`bench:dt:check`).
  - For 25 rows of the nested corpus it decrypts every read the benches
    time, through the key that derived the read, and compares the result with
    the plaintext twin.
  - The reads it checks: the top-level subtrees, walked out and under the
    default budget; member paths 1 and 4 steps below a field; each
    `$.orders[k]` and `$.orders[k].status`; the wildcard reads at N = 8 and
    64; and `$` under the sealed root (derivable only). At N = 8 the derivable
    format must refuse every row with more than 8 orders.
  - It also checks children, child count, the path query and `->`; `$` with
    the root open; and children, child count and full reads on the wide
    table.
  - For 10 needles per predicate it compares the id sets of containment,
    equality, range and ordering with native jsonb.

| tier | gates | derivable |
|---|---|---|
| 10,000 | 692 passed, 0 failed | 717 passed, 0 failed |
| 100,000 | 672 passed, 0 failed | 697 passed, 0 failed |

The derivable format runs 25 more checks: `$` under the sealed root, which the
base refuses. The 100,000-row tier has no open-root table, which is 20 fewer
checks in each format. Raw: `results/query/derivable/json_tree_correctness_*.json`.

### 1. Stored size

| table | measure | gates | derivable | change |
|---|---|--:|--:|--:|
| json_tree_small_10000 | entries per document | 5.0 | 5.0 | |
| json_tree_small_10000 | characters per document | 1,279 | 1,384 | +8.2% |
| json_tree_small_10000 | `pg_column_size` per document | 1,398 B | 1,502 B | +7.4% |
| json_tree_small_10000 | per entry: characters / bytes on disk | 255.8 / 279.7 | 276.8 / 300.4 | +21.0 chars / +20.7 B |
| json_tree_small_10000 | heap incl. TOAST | 15.7 MiB | 15.7 MiB | +0.0% |
| json_tree_small_10000 | containment GIN | 1.0 MiB | 1.0 MiB | +0.0% |
| json_tree_small_10000 | all indexes | 1.3 MiB | 1.3 MiB | +0.0% |
| json_tree_small_10000 | total | 16.9 MiB | 16.9 MiB | +0.0% |
| json_tree_large_10000 | entries per document | 217.2 | 217.2 | |
| json_tree_large_10000 | characters per document | 49,042 | 53,603 | +9.3% |
| json_tree_large_10000 | `pg_column_size` per document | 31,714 B | 35,256 B | +11.2% |
| json_tree_large_10000 | per entry: characters / bytes on disk | 225.8 / 146.0 | 246.8 / 162.3 | +21.0 chars / +16.3 B |
| json_tree_large_10000 | heap incl. TOAST | 319.5 MiB | 354.5 MiB | +11.0% |
| json_tree_large_10000 | containment GIN | 39.9 MiB | 39.9 MiB | +0.0% |
| json_tree_large_10000 | all indexes | 40.2 MiB | 40.2 MiB | +0.0% |
| json_tree_large_10000 | total | 359.7 MiB | 394.7 MiB | +9.7% |
| json_tree_large_open_10000 | entries per document | 217.2 | 217.2 | |
| json_tree_large_open_10000 | characters per document | 44,519 | 49,081 | +10.2% |
| json_tree_large_open_10000 | `pg_column_size` per document | 26,458 B | 30,013 B | +13.4% |
| json_tree_large_open_10000 | per entry: characters / bytes on disk | 204.9 / 121.8 | 225.9 / 138.2 | +21.0 chars / +16.4 B |
| json_tree_large_open_10000 | heap incl. TOAST | 267.5 MiB | 302.8 MiB | +13.2% |
| json_tree_large_open_10000 | all indexes | 0.2 MiB | 0.2 MiB | +0.0% |
| json_tree_large_open_10000 | total | 267.7 MiB | 303.1 MiB | +13.2% |
| json_tree_wide_1000 | entries per document | 603.0 | 603.0 | |
| json_tree_wide_1000 | characters per document | 125,967 | 138,630 | +10.1% |
| json_tree_wide_1000 | `pg_column_size` per document | 91,195 B | 99,998 B | +9.7% |
| json_tree_wide_1000 | per entry: characters / bytes on disk | 208.9 / 151.2 | 229.9 / 165.8 | +21.0 chars / +14.6 B |
| json_tree_wide_1000 | heap incl. TOAST | 91.0 MiB | 100.2 MiB | +10.2% |
| json_tree_wide_1000 | all indexes | 0.0 MiB | 0.0 MiB | +0.0% |
| json_tree_wide_1000 | total | 91.0 MiB | 100.3 MiB | +10.2% |

At 100,000 rows:

| table | measure | gates | derivable | change |
|---|---|--:|--:|--:|
| json_tree_small_100000 | entries per document | 5.0 | 5.0 | |
| json_tree_small_100000 | characters per document | 1,279 | 1,384 | +8.2% |
| json_tree_small_100000 | `pg_column_size` per document | 1,398 B | 1,502 B | +7.4% |
| json_tree_small_100000 | per entry: characters / bytes on disk | 255.8 / 279.7 | 276.8 / 300.4 | +21.0 chars / +20.7 B |
| json_tree_small_100000 | heap incl. TOAST | 156.3 MiB | 156.3 MiB | +0.0% |
| json_tree_small_100000 | containment GIN | 7.9 MiB | 7.9 MiB | +0.0% |
| json_tree_small_100000 | all indexes | 10.1 MiB | 10.1 MiB | +0.0% |
| json_tree_small_100000 | total | 166.4 MiB | 166.4 MiB | +0.0% |
| json_tree_large_100000 | entries per document | 217.1 | 217.1 | |
| json_tree_large_100000 | characters per document | 49,003 | 53,561 | +9.3% |
| json_tree_large_100000 | `pg_column_size` per document | 31,691 B | 35,228 B | +11.2% |
| json_tree_large_100000 | per entry: characters / bytes on disk | 225.8 / 146.0 | 246.8 / 162.3 | +21.0 chars / +16.3 B |
| json_tree_large_100000 | heap incl. TOAST | 3,192.6 MiB | 3,542.6 MiB | +11.0% |
| json_tree_large_100000 | containment GIN | 358.1 MiB | 358.1 MiB | +0.0% |
| json_tree_large_100000 | all indexes | 360.3 MiB | 360.3 MiB | +0.0% |
| json_tree_large_100000 | total | 3,552.9 MiB | 3,902.9 MiB | +9.9% |

**Per entry.** Each entry gains exactly 21 characters of `::text`: the
`, "x": "…"` framing and 12 characters of base64. On disk it gains about 16
bytes. Base64 of random bytes does not compress, so pglz takes back only
part of the framing.

**Per document.** The nested document grows 9.3% in characters and 11.2% on
disk. The stack unit test's +9.8% counted compact JSON on its own nested
shape, and `::text` here also carries a space after every colon and comma
(19 of the 21 characters per entry are the field itself). A flat document
grows 7.4% (1,398 to 1,502 bytes).

**Heap.**

- Nested: the heap grows with the document, +11.0% (3,193 to 3,543 MiB at
  100,000 rows).
- Flat: the heap does not move. Both sizes fit five rows to an 8 KB page,
  so the page count is the same.
- Indexes: the containment GIN is byte-for-byte the same size, because the
  projection `q` did not change.

Raw: `results/query/derivable/storage_{10000,100000}.json`.

### 2. Ingest

Sealing is client-side, under the in-process `DeterministicSource`, one
document at a time, and the time excludes any KMS round trip. INSERT is the
batched INSERTs (1,000 rows a batch) and their commits, into a table with its
indexes dropped.

- **10,000-row tier.** Three runs per format, alternating which format
  went first: one into the tier table, then two into the unsuffixed table.
- **Other tiers.** One run.
- **GIN builds.** At 10,000 rows, a `REINDEX` of the containment GIN, three
  times per format, interleaved. At 100,000 rows, the one build after the
  load: 43.96 s for gates, 45.13 s for derivable (+2.7%).

| corpus | rows | runs | gates seal | derivable seal | change | gates INSERT | derivable INSERT | change |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| large | 10,000 | 3 / 3 | 8.37 (8.35–8.41) | 8.75 (8.58–8.86) | +4.5% | 23.68 (23.66–23.70) | 25.62 (25.59–25.86) | +8.2% |
| large | 100,000 | 1 / 1 | 80.00 (80.00–80.00) | 82.73 (82.73–82.73) | +3.4% | 224.96 (224.96–224.96) | 245.13 (245.13–245.13) | +9.0% |
| large_open | 10,000 | 1 / 1 | 8.12 (8.12–8.12) | 8.46 (8.46–8.46) | +4.1% | 22.97 (22.97–22.97) | 24.65 (24.65–24.65) | +7.3% |
| small | 10,000 | 3 / 3 | 0.29 (0.29–0.30) | 0.31 (0.30–0.31) | +5.4% | 1.29 (1.28–1.30) | 1.35 (1.35–1.37) | +5.2% |
| small | 100,000 | 1 / 1 | 2.83 (2.83–2.83) | 2.95 (2.95–2.95) | +4.2% | 12.28 (12.28–12.28) | 12.95 (12.95–12.95) | +5.4% |
| wide | 1,000 | 1 / 1 | 1.91 (1.91–1.91) | 2.08 (2.08–2.08) | +8.9% | 5.67 (5.67–5.67) | 6.18 (6.18–6.18) | +8.9% |

| containment GIN build (REINDEX) | runs | gates | derivable | change |
|---|--:|--:|--:|--:|
| json_tree_large_encrypted_v4_10000_query_gin_idx | 3 / 3 | 2.02 s | 2.11 s | +4.3% |
| json_tree_small_encrypted_v4_10000_query_gin_idx | 3 / 3 | 0.07 s | 0.07 s | +5.9% |

Raw: `results/ingest/derivable/ingest.jsonl` and `gin_build.jsonl`.

### 3. Reads with no wildcard (nested corpus unless noted, 10,000 rows)

How to read the table:

- **sent** is what the read binds: tokens, gates, and their characters of
  base64.
- **rows / doc** and **chars returned / doc** count `e::jsonb::text` per
  document, averaged over the pass set.
- **change** is derivable against gates.
- `read/orders (fallback)` runs under the default budget of 64 and returns
  the sealed root. `read/orders (walked)` runs with budget 0 and returns
  every entry.
- `read/$ (root open)` runs on the open-root table.
- `read/first_name` is on the flat corpus. The base returns no row there,
  so its cost is the cost of finding nothing.

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| read/$ (root open) | 1 tok, 24 B → 1 tok, 24 B | 210.9 → 211.9 | 64,291 → 69,001 | 21.08 → 30.69 | 29.21 → 42.18 | +45.6% | 19.79 → 28.69 | +45.0% |
| read/$ (sealed root) | — → 1 tok, 24 B | n/a → 1.0 | n/a → 4,776 | n/a → 1.971 | n/a → 2.417 | n/a | n/a → 1.304 | n/a |
| read/company | 1 tok, 24 B → 1 tok, 24 B | 6.0 → 7.0 | 1,961 → 2,383 | 2.002 → 2.814 | 2.286 → 3.337 | +40.6% | 1.330 → 2.113 | +58.9% |
| read/company.name | 1 tok + 1 gates, 48 B → 1 tok, 24 B | 1.0 → 1.0 | 315 → 336 | 1.968 → 1.977 | 2.265 → 2.422 | +0.4% | 1.251 → 1.285 | +2.7% |
| read/first_name | 1 tok, 24 B → 1 tok, 24 B | 0.0 → 1.0 | 0 → 306 | 0.618 → 0.649 | 0.751 → 0.780 | +5.0% | 0.059 → 0.105 | +79.7% |
| read/orders (fallback) | 1 tok, 24 B → 1 tok, 24 B | 1.0 → 1.0 | 4,755 → 4,776 | 6.357 → 9.615 | 6.696 → 10.09 | +51.3% | 5.568 → 8.673 | +55.8% |
| read/orders (walked) | 1 tok, 24 B → 1 tok, 24 B | 175.1 → 176.1 | 53,245 → 57,217 | 18.10 → 25.94 | 26.24 → 37.81 | +43.4% | 16.62 → 24.34 | +46.4% |
| read/orders[2] | 1 tok, 24 B → 1 tok, 24 B | 23.0 → 24.0 | 7,013 → 7,810 | 3.569 → 5.077 | 4.917 → 7.134 | +42.2% | 2.840 → 4.315 | +51.9% |
| read/orders[2].status | 1 tok + 1 gates, 48 B → 1 tok, 24 B | 1.0 → 1.0 | 305 → 326 | 1.884 → 1.955 | 2.132 → 2.415 | +3.8% | 1.196 → 1.280 | +7.1% |
| read/user | 1 tok, 24 B → 1 tok, 24 B | 12.0 → 13.0 | 3,718 → 4,263 | 2.544 → 3.520 | 2.841 → 4.009 | +38.4% | 1.870 → 2.789 | +49.1% |
| read/user.contact.emergency_contact.name | 1 tok + 3 gates, 96 B → 1 tok, 24 B | 1.0 → 1.0 | 310 → 331 | 2.481 → 1.934 | 2.757 → 2.374 | -22.0% | 1.757 → 1.277 | -27.3% |

**Why whole subtrees got slower, and deep leaves faster.** Two costs are
new on every read, and one cost is gone:

- **The self-tag map.** Each read builds a map of the whole document keyed by
  `x`, in addition to the map keyed by `p`. It costs 0.41 ms per nested
  document in-server (`map/by_self` below), the same as the parent-tag map.
- **The hit test.** Each hit below the node derives the member rule's token
  and compares its self tag; on a miss it derives the element rule's too.
  The micro-timing below puts that at 2× the old per-hit token step for a
  member and 4.9× for an element.
- **What is gone** is the gated walk's enumeration of siblings at every
  level it passes through. A gate admitted only the child it named, but the
  walk probed every child to find it. That saving dominates a path several
  levels deep that ends in a leaf, so `$.user.contact.emergency_contact.name`
  got 22% faster. It is about even at one level below a field
  (`$.company.name`, `$.orders[2].status`). It is absent from a whole-subtree
  read, which visits every node in both formats, so the new costs show
  there in full: 38 to 46% per call.

`read/orders[2]` is a position the base already derived directly. The
difference there is the cost, not the walk.

The derivable format also returns the node itself, one row more per read:
`read/company` returns 7 rows where the base returned 6.

**Per-hit cost in isolation.** Each SQL function the walk calls per hit was
called on 100,000 synthetic inputs per format. The figure is the median of 5
runs, divided by 100,000; reading the inputs alone costs under 0.1 µs a row.

| function (per hit) | gates | derivable |
|---|--:|--:|
| `json_tree_parent_tag` (the probe; both formats) | 7.9 µs | 7.9 µs |
| `json_tree_child_token` (gates: the hit's token) | 7.8 µs | — |
| `json_tree_hit_token`, member test hits | — | 15.5 µs |
| `json_tree_hit_token`, member misses, element hits | — | 38.2 µs |
| `json_tree_hit_token`, neither matches | — | 27.4 µs |

**Per-hit cost inside a walk.** Read off the wide corpus below (300 hits,
in-server):

- A member costs 36 µs more than in the base, and an element 80 µs more.
- Those figures include each read's share of the self-tag map, about 1 ms
  for a 603-entry document.
- They are more than the function costs above account for. The rest is not
  attributed here.

### 4. Wildcard reads (nested corpus, 10,000 rows)

The base sends the head token `$.orders` and one gate per step below it,
whatever N is, so its three or two rows per path are the same query and
serve as a check on run-to-run noise.

- `$.orders[*].total` runs under the default budget.
- `$.orders[*].items[*].price` runs with budget 0. It returns about 33
  entries per document, under the budget either way.

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| wildcard/orders[*].items[*].price N=64 | 1 tok + 4 gates, 120 B → 4161 tok, 99,864 B | 32.8 → 32.8 | 10,355 → 11,043 | 17.43 → 25.81 | 25.20 → 27.01 | +48.0% | 16.08 → 24.47 | +52.1% |
| wildcard/orders[*].items[*].price N=8 | 1 tok + 4 gates, 120 B → 73 tok, 1,752 B | 32.8 → 38.1 | 10,355 → 12,734 | 17.77 → 3.499 | 25.78 → 8.516 | -80.3% | 15.93 → 3.559 | -77.7% |
| wildcard/orders[*].total N=256 | 1 tok + 2 gates, 72 B → 257 tok, 6,168 B | 7.3 → 7.3 | 2,313 → 2,467 | 5.471 → 3.615 | 7.056 → 4.158 | -33.9% | 4.575 → 2.843 | -37.9% |
| wildcard/orders[*].total N=64 | 1 tok + 2 gates, 72 B → 65 tok, 1,560 B | 7.3 → 7.3 | 2,313 → 2,467 | 5.510 → 2.540 | 7.083 → 3.063 | -53.9% | 4.462 → 1.821 | -59.2% |
| wildcard/orders[*].total N=8 | 1 tok + 2 gates, 72 B → 9 tok, 216 B | 7.3 → 14.2 | 2,313 → 4,695 | 5.386 → 2.271 | 6.962 → 7.137 | -57.8% | 4.535 → 2.449 | -46.0% |

- **N = 8.**
  - 33% of the nested rows hold more than 8 orders. On those rows the probe
    `$.orders[8]` finds the ninth order and the server walks its subtree,
    and then the client refuses the read (`JsonWildcardTruncated`).
  - That is the extra rows per document at N = 8, and the p95 equal to the
    base's.
  - The median is a row that fits.
  - No order holds more than 8 items, so the inner probes never hit.
- **N = 64 and 256, one level.** These send 65 and 257 tokens. Each token
  costs a self-tag hash and a map lookup per row, and the read costs 34 to
  54% less than the gated walk, which enumerated every order's five members
  to admit one.
- **N = 64, two levels.** This sends 4,161 tokens, 99,864 characters of
  base64 bound per call. The per-token work (a hash and a lookup, 4,161
  times per row) outweighs the walk it replaces: +48%.
- **The plan does not change.** The outer plan is the same in both formats
  at every N: a primary-key index scan feeding a Function Scan
  (`EXPLAIN ANALYZE` below).

### 5. Enumerating children

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| child_count/orders (nested) | 1 tok, 24 B → 1 tok, 24 B | 1.0 → 1.0 | 1 → 1 | 1.752 → 2.787 | 2.175 → 3.593 | +59.1% | 1.388 → 2.368 | +70.5% |
| children/orders (nested) | 1 tok, 24 B → 1 tok, 24 B | 7.3 → 7.3 | 2,142 → 2,295 | 1.737 → 1.812 | 2.158 → 2.299 | +4.3% | 1.198 → 1.337 | +11.5% |

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| child_count/attrs | 1 tok, 24 B → 1 tok, 24 B | 1.0 → 1.0 | 3 → 3 | 24.89 → 15.10 | 25.96 → 16.06 | -39.3% | 24.13 → 14.25 | -41.0% |
| child_count/list | 1 tok, 24 B → 1 tok, 24 B | 1.0 → 1.0 | 3 → 3 | 24.84 → 15.08 | 25.94 → 15.94 | -39.3% | 24.16 → 14.48 | -40.1% |
| children/attrs | 1 tok, 24 B → 1 tok, 24 B | 300.0 → 300.0 | 90,130 → 96,430 | 25.12 → 12.87 | 26.15 → 13.64 | -48.8% | 24.03 → 12.21 | -49.2% |
| children/list | 1 tok, 24 B → 1 tok, 24 B | 300.0 → 300.0 | 90,237 → 96,537 | 25.08 → 12.84 | 26.10 → 13.43 | -48.8% | 24.23 → 12.29 | -49.3% |
| read/attrs (walked) | 1 tok, 24 B → 1 tok, 24 B | 300.0 → 301.0 | 90,130 → 96,722 | 31.06 → 42.04 | 32.30 → 43.76 | +35.3% | 29.74 → 40.69 | +36.8% |
| read/list (walked) | 1 tok, 24 B → 1 tok, 24 B | 300.0 → 301.0 | 90,237 → 96,830 | 31.06 → 55.43 | 32.37 → 57.59 | +78.4% | 29.72 → 53.84 | +81.2% |

(The first table is the focused run's nested `$.orders`; the second is run 1,
whose wide-corpus rows are the ones to read.)

- **`json_tree_children`.** The derivable version probes parent tags and
  hashes no child token, where the base hashed one per child. It builds the
  self-tag map to find the node.
  - On a 300-wide node, dropping 300 hashes outweighs the map: −49%.
  - On `$.orders` (5 to 10 children) they about cancel: +4% per call, +11.5%
    in-server.
- **`json_tree_child_count`.** The derivable version builds the self-tag map
  to test that the node exists (so that it can return NULL for an absent
  node, decision 7), and then calls `json_tree_children`, which builds it
  again.
  - On the wide node it still gains 39%.
  - On `$.orders` it loses 59% (1.75 to 2.79 ms).
  - Fix: count inside one call, or test existence against the map
    `json_tree_children` already builds. Either removes the second map,
    about 1 ms on a nested document.
- **The member/element test.** Walking the wide nodes in full
  (`read/attrs (walked)`, `read/list (walked)`) isolates it:
  - 300 member hits cost 35% more than the base.
  - 300 element hits cost 78% more, because each element first fails the
    member test.
  - The base's walk costs the same for either kind (31 ms).

### 6. Containment and `->` extraction (10,000 rows, the focused run)

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| detoast/nested | – → – | 1.0 → 1.0 | 3 → 3 | 0.144 → 0.150 | 0.192 → 0.203 | +3.6% | 0.041 → 0.045 | +9.1% |
| map/by_parent (nested) | – → – | 1.0 → 1.0 | 6 → 6 | 0.493 → 0.550 | 0.641 → 0.738 | +11.5% | 0.353 → 0.403 | +14.3% |
| map/by_self (nested) | — → – | n/a → 1.0 | n/a → 6 | n/a → 0.548 | n/a → 0.719 | n/a | n/a → 0.414 | n/a |
| noop/by_id | – → – | 1.0 → 1.0 | 4 → 4 | 0.099 → 0.100 | 0.217 → 0.149 | +0.7% | 0.001 → 0.001 | -4.4% |

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| contains/array_element | 357 B → 357 B | 1.0 → 1.0 | 4 → 4 | 0.637 → 0.643 | 0.787 → 0.750 | +0.9% | n/a → n/a | n/a |
| contains/deep_object | 341 B → 341 B | 10.0 → 10.0 | 20 → 20 | 1.843 → 1.923 | 2.026 → 2.126 | +4.4% | n/a → n/a | n/a |
| contains/email | 146 B → 146 B | 1.0 → 1.0 | 4 → 4 | 0.439 → 0.433 | 0.619 → 0.580 | -1.5% | n/a → n/a | n/a |
| extract/age | – → – | 1.0 → 1.0 | 361 → 382 | 0.182 → 0.182 | 0.307 → 0.318 | +0.1% | 0.057 → 0.060 | +5.5% |
| extract/company.salary | – → – | 1.0 → 1.0 | 392 → 413 | 0.461 → 0.493 | 0.570 → 0.680 | +6.9% | 0.208 → 0.230 | +10.5% |
| field_order/nested | – → – | 10.0 → 10.0 | 36 → 36 | 2.172 → 2.331 | 2.454 → 2.586 | +7.3% | n/a → n/a | n/a |

Nothing here calls a json_tree read, and nothing here changed in SQL. What
changed is the document each query detoasts and parses. `detoast/nested`
reads a document and counts its entries; it costs 9% more in-server, in step
with the 11% larger datum.

- **Unchanged within noise:** selective containment
  (`contains/array_element`, one row) and flat containment (`contains/email`),
  both answered almost entirely by the GIN. Also `->` on the flat corpus
  (+0.1% per call).
- **Moved by the size:** `contains/deep_object` (LIMIT 10, so rechecks that
  detoast documents) is +2 to +6% across three runs (+2.3%, +5.8%, +4.4%).
  `->` on the nested corpus is +7% per call and +10.5% in-server.
  `field_order/nested`, `ORDER BY ord_term(value -> sel) LIMIT 10` on its
  functional btree, is +7%.
- **The parent-tag map:** `map/by_parent` costs 14% more in-server with an
  identical function, because each entry it copies into the map carries `x`.

### 7. 100,000 rows: sizes above; headline reads

The 100,000-row tier was loaded for both formats in about 13 minutes, so
the headline reads were timed there too, with fewer passes:

- 2 passes of 200 sampled rows, and 1 in-server pass over 500 documents.
- Every scenario of sections 3, 4 and 6 that runs on the flat or nested
  corpus, and the enumeration of `$.orders`.
- `field_order/nested` was not timed (its btree exists at 10,000 rows only).
- There is no open-root table at this tier, so neither is `$` with the root
  open.
- The wide corpus has one tier, 1,000 rows, and is not repeated here.

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| detoast/nested | – → – | 1.0 → 1.0 | 3 → 3 | 0.138 → 0.147 | 0.256 → 0.171 | +6.1% | 0.064 → 0.046 | -27.5% |
| map/by_parent (nested) | – → – | 1.0 → 1.0 | 6 → 6 | 0.474 → 0.558 | 0.628 → 0.767 | +17.6% | 0.361 → 0.411 | +13.7% |
| map/by_self (nested) | — → – | n/a → 1.0 | n/a → 6 | n/a → 0.603 | n/a → 0.831 | n/a | n/a → 0.424 | n/a |
| noop/by_id | – → – | 1.0 → 1.0 | 5 → 5 | 0.105 → 0.094 | 0.248 → 0.119 | -9.9% | 0.008 → 0.004 | -47.1% |

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| read/$ (sealed root) | — → 1 tok, 24 B | n/a → 1.0 | n/a → 4,844 | n/a → 1.838 | n/a → 2.284 | n/a | n/a → 1.274 | n/a |
| read/company | 1 tok, 24 B → 1 tok, 24 B | 6.0 → 7.0 | 1,962 → 2,384 | 1.903 → 2.677 | 2.154 → 3.140 | +40.6% | 1.271 → 2.043 | +60.7% |
| read/company.name | 1 tok + 1 gates, 48 B → 1 tok, 24 B | 1.0 → 1.0 | 315 → 336 | 1.891 → 1.828 | 2.253 → 2.283 | -3.4% | 1.239 → 1.253 | +1.1% |
| read/first_name | 1 tok, 24 B → 1 tok, 24 B | 0.0 → 1.0 | 0 → 306 | 0.560 → 0.637 | 0.775 → 0.833 | +13.7% | 0.094 → 0.178 | +88.7% |
| read/orders (fallback) | 1 tok, 24 B → 1 tok, 24 B | 1.0 → 1.0 | 4,823 → 4,844 | 6.026 → 9.193 | 6.345 → 9.749 | +52.6% | 5.404 → 8.805 | +62.9% |
| read/orders (walked) | 1 tok, 24 B → 1 tok, 24 B | 177.8 → 178.8 | 54,089 → 58,119 | 16.61 → 24.14 | 24.78 → 36.12 | +45.4% | 16.40 → 24.05 | +46.6% |
| read/orders[2] | 1 tok, 24 B → 1 tok, 24 B | 22.3 → 23.3 | 6,780 → 7,561 | 3.405 → 4.942 | 4.714 → 6.946 | +45.1% | 2.676 → 4.202 | +57.0% |
| read/orders[2].status | 1 tok + 1 gates, 48 B → 1 tok, 24 B | 1.0 → 1.0 | 305 → 326 | 1.773 → 1.851 | 2.023 → 2.312 | +4.4% | 1.161 → 1.251 | +7.8% |
| read/user | 1 tok, 24 B → 1 tok, 24 B | 12.0 → 13.0 | 3,718 → 4,263 | 2.402 → 3.357 | 2.673 → 3.851 | +39.8% | 1.791 → 2.742 | +53.1% |
| read/user.contact.emergency_contact.name | 1 tok + 3 gates, 96 B → 1 tok, 24 B | 1.0 → 1.0 | 310 → 331 | 2.370 → 1.842 | 2.646 → 2.316 | -22.3% | 1.731 → 1.257 | -27.4% |

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| wildcard/orders[*].items[*].price N=64 | 1 tok + 4 gates, 120 B → 4161 tok, 99,864 B | 33.3 → 33.3 | 10,503 → 11,201 | 16.42 → 24.84 | 24.34 → 26.14 | +51.3% | 15.95 → 24.00 | +50.4% |
| wildcard/orders[*].items[*].price N=8 | 1 tok + 4 gates, 120 B → 73 tok, 1,752 B | 33.3 → 39.0 | 10,503 → 13,037 | 16.22 → 3.243 | 23.82 → 8.323 | -80.0% | 15.94 → 3.547 | -77.8% |
| wildcard/orders[*].total N=256 | 1 tok + 2 gates, 72 B → 257 tok, 6,168 B | 7.5 → 7.5 | 2,360 → 2,517 | 4.987 → 3.546 | 6.723 → 4.146 | -28.9% | 4.528 → 2.804 | -38.1% |
| wildcard/orders[*].total N=64 | 1 tok + 2 gates, 72 B → 65 tok, 1,560 B | 7.5 → 7.5 | 2,360 → 2,517 | 4.946 → 2.467 | 6.626 → 3.121 | -50.1% | 4.556 → 1.824 | -60.0% |
| wildcard/orders[*].total N=8 | 1 tok + 2 gates, 72 B → 9 tok, 216 B | 7.5 → 14.9 | 2,360 → 4,923 | 4.902 → 2.082 | 6.588 → 7.149 | -57.5% | 4.479 → 2.480 | -44.6% |

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| child_count/orders (nested) | 1 tok, 24 B → 1 tok, 24 B | 1.0 → 1.0 | 1 → 1 | 1.736 → 2.748 | 2.131 → 3.582 | +58.3% | 1.404 → 2.378 | +69.4% |
| children/orders (nested) | 1 tok, 24 B → 1 tok, 24 B | 7.5 → 7.5 | 2,186 → 2,342 | 1.742 → 1.772 | 2.145 → 2.281 | +1.7% | 1.206 → 1.351 | +12.0% |

| scenario | sent (gates → derivable) | rows / doc | chars returned / doc | per call, median ms | per call, p95 ms | change (median) | in-server per doc, ms | change |
|---|---|--:|--:|--:|--:|--:|--:|--:|
| contains/array_element | 357 B → 357 B | 1.0 → 1.0 | 5 → 5 | 1.867 → 1.893 | 1.986 → 2.044 | +1.4% | n/a → n/a | n/a |
| contains/deep_object | 341 B → 341 B | 10.0 → 10.0 | 17 → 17 | 6.119 → 6.196 | 6.287 → 6.249 | +1.2% | n/a → n/a | n/a |
| contains/email | 146 B → 146 B | 1.0 → 1.0 | 5 → 5 | 0.493 → 0.481 | 0.626 → 0.563 | -2.5% | n/a → n/a | n/a |
| extract/age | – → – | 1.0 → 1.0 | 361 → 382 | 0.180 → 0.184 | 0.350 → 0.305 | +2.1% | 0.058 → 0.062 | +6.3% |
| extract/company.salary | – → – | 1.0 → 1.0 | 392 → 413 | 0.460 → 0.478 | 0.541 → 0.537 | +3.9% | 0.210 → 0.228 | +8.7% |

The per-document costs at 100,000 rows match 10,000 rows: a read touches one
document, so its cost does not grow with the table.

### 8. Noise floor

Run 1 against run 2 of the full 10,000-row matrix, same format. The figure is
the relative difference of each statistic, over every scenario and both
formats.

| metric | median over scenarios | largest | scenario |
|---|--:|--:|---|
| per call, median | 0.7% | 28.6% | noop/by_id (derivable) |
| per call, p95 | 1.0% | 31.8% | noop/by_id (derivable) |
| in-server per doc | 0.9% | 5.8% | detoast/nested (gates) |

- **Above 1 ms**, every scenario's change from gates to derivable reproduced
  within 3.5 percentage points between the two runs. The largest gaps were
  `contains/deep_object` (+2.3% in run 1, +5.8% in run 2) and
  `read/company` (+40.6% and +37.6%).
- **Below 1 ms** (`noop`, `extract/age`, `read/first_name`,
  `contains/email`), the per-call figures are dominated by the round trip
  and move by up to ±20% between runs. Read their in-server column instead.
  It is the same effect the earlier measurements saw when they ran the
  flat 10,000-row tier twice: sub-millisecond criterion medians moved by up to
  15%.

### 9. `EXPLAIN ANALYZE`, representative row

The outer plan is identical in shape for every read in both formats. Three
examples follow; every scenario's plan is in the time files under
`explain`. The json_tree functions are SQL functions with recursive CTEs that
the planner does not inline, so `EXPLAIN` shows a Function Scan and its time,
not the walk inside it.

`read/company`, representative row:

gates:

```
Nested Loop  (cost=0.54..33.55 rows=1000 width=32) (actual time=1.482..1.488 rows=6 loops=1)
  Buffers: shared hit=59
  ->  Index Scan using json_tree_large_encrypted_v4_10000_pkey on json_tree_large_encrypted_v4_10000  (cost=0.29..8.30 rows=1 width=18) (actual time=0.002..0.002 rows=1 loops=1)
        Index Cond: (id = 5000)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree_at e  (cost=0.25..10.25 rows=1000 width=32) (actual time=1.478..1.478 rows=6 loops=1)
        Buffers: shared hit=56
Planning Time: 0.017 ms
Execution Time: 1.492 ms
```

derivable:

```
Nested Loop  (cost=0.54..33.55 rows=1000 width=32) (actual time=2.193..2.201 rows=7 loops=1)
  Buffers: shared hit=66
  ->  Index Scan using json_tree_large_encrypted_v4_10000_pkey on json_tree_large_encrypted_v4_10000  (cost=0.29..8.30 rows=1 width=18) (actual time=0.003..0.004 rows=1 loops=1)
        Index Cond: (id = 5000)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree e  (cost=0.25..10.25 rows=1000 width=32) (actual time=2.187..2.188 rows=7 loops=1)
        Buffers: shared hit=63
Planning Time: 0.021 ms
Execution Time: 2.206 ms
```


`wildcard/orders[*].items[*].price N=64`, representative row:

gates:

```
Nested Loop  (cost=0.54..33.55 rows=1000 width=32) (actual time=12.009..12.032 rows=20 loops=1)
  Buffers: shared hit=52
  ->  Index Scan using json_tree_large_encrypted_v4_10000_pkey on json_tree_large_encrypted_v4_10000  (cost=0.29..8.30 rows=1 width=18) (actual time=0.007..0.009 rows=1 loops=1)
        Index Cond: (id = 5000)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree_at e  (cost=0.25..10.25 rows=1000 width=32) (actual time=11.999..11.999 rows=20 loops=1)
        Buffers: shared hit=49
Planning Time: 0.041 ms
Execution Time: 12.041 ms
```

derivable:

```
Nested Loop  (cost=0.54..33.55 rows=1000 width=32) (actual time=25.247..25.273 rows=20 loops=1)
  Buffers: shared hit=59
  ->  Index Scan using json_tree_large_encrypted_v4_10000_pkey on json_tree_large_encrypted_v4_10000  (cost=0.29..8.30 rows=1 width=18) (actual time=0.008..0.010 rows=1 loops=1)
        Index Cond: (id = 5000)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree e  (cost=0.25..10.25 rows=1000 width=32) (actual time=25.235..25.236 rows=20 loops=1)
        Buffers: shared hit=56
Planning Time: 0.042 ms
Execution Time: 25.282 ms
```


`read/list (walked)`, representative row:

gates:

```
Nested Loop  (cost=0.53..33.54 rows=1000 width=32) (actual time=31.351..31.686 rows=300 loops=1)
  Buffers: shared hit=101
  ->  Index Scan using json_tree_wide_encrypted_v4_1000_pkey on json_tree_wide_encrypted_v4_1000  (cost=0.28..8.29 rows=1 width=18) (actual time=0.010..0.012 rows=1 loops=1)
        Index Cond: (id = 500)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree_at e  (cost=0.25..10.25 rows=1000 width=32) (actual time=31.337..31.345 rows=300 loops=1)
        Buffers: shared hit=98
Planning Time: 0.050 ms
Execution Time: 31.704 ms
```

derivable:

```
Nested Loop  (cost=0.53..33.54 rows=1000 width=32) (actual time=56.980..57.328 rows=301 loops=1)
  Buffers: shared hit=131
  ->  Index Scan using json_tree_wide_encrypted_v4_1000_pkey on json_tree_wide_encrypted_v4_1000  (cost=0.28..8.29 rows=1 width=18) (actual time=0.009..0.011 rows=1 loops=1)
        Index Cond: (id = 500)
        Buffers: shared hit=3
  ->  Function Scan on json_tree_subtree e  (cost=0.25..10.25 rows=1000 width=32) (actual time=56.967..56.975 rows=301 loops=1)
        Buffers: shared hit=128
Planning Time: 0.050 ms
Execution Time: 57.345 ms
```

### 10. Criterion cross-check (10,000 rows, not interleaved)

The two formats ran the existing criterion benches about fifteen minutes
apart, derivable first. The native jsonb scenarios in the same benches
(`plain/*`) have the same data and SQL in both databases. They came out
between 25% faster and 21% slower in the derivable run, most of them 12 to
25% faster. Treat a difference under about 20% here as noise.

| bench | scenario | gates | derivable | change |
|---|---|--:|--:|--:|
| nested | json_tree/contains/deep_object | 1.75 ms | 1.92 ms | +10% |
| nested | json_tree/contains/array_element | 0.59 ms | 0.62 ms | +5% |
| nested | json_tree/contains/nested_array | 2.40 ms | 2.62 ms | +9% |
| nested | json_tree/contains/deep_object_fn | 1.76 ms | 1.93 ms | +9% |
| nested | json_tree/contains/array_element_fn | 0.59 ms | 0.63 ms | +7% |
| nested | json_tree/contains/nested_array_fn | 2.41 ms | 2.61 ms | +8% |
| nested | json_tree/field_eq/nested | 0.39 ms | 0.44 ms | +12% |
| nested | json_tree/field_gt/nested | 0.10 ms | 0.08 ms | -17% |
| nested | json_tree/field_between/nested | 0.21 ms | 0.20 ms | -3% |
| nested | json_tree/field_order/nested | 2.08 ms | 2.37 ms | +14% |
| nested | json_tree/subtree/company | 1.61 ms | 2.39 ms | +49% |
| nested | json_tree/subtree/user | 2.13 ms | 3.13 ms | +47% |
| nested | json_tree/subtree/orders | 5.80 ms | 9.19 ms | +58% |
| nested | json_tree/subtree_at/items | 8.65 ms | 8.96 ms | +4% |
| nested | json_tree/subtree/orders_walked | 11.73 ms | 17.89 ms | +52% |
| nested | json_tree/subtree_at/items_walked | 11.59 ms | 12.94 ms | +12% |
| nested | json_tree/children/orders | 1.47 ms | 1.50 ms | +2% |
| nested | json_tree/child_count/orders | 1.47 ms | 2.29 ms | +56% |
| nested | json_tree/path_query/wildcard | 1.05 ms | 1.19 ms | +13% |
| nested | json_tree/extract/nested | 0.36 ms | 0.39 ms | +8% |
| nested | plain/contains/deep_object | 0.34 ms | 0.37 ms | +8% |
| nested | plain/contains/array_element | 0.10 ms | 0.08 ms | -15% |
| nested | plain/contains/nested_array | 0.30 ms | 0.36 ms | +20% |
| nested | plain/field_eq/nested | 0.09 ms | 0.08 ms | -16% |
| nested | plain/field_gt/nested | 0.13 ms | 0.13 ms | -3% |
| nested | plain/field_between/nested | 0.13 ms | 0.12 ms | -4% |
| nested | plain/field_order/nested | 0.13 ms | 0.12 ms | -5% |
| nested | plain/subtree/company | 0.09 ms | 0.08 ms | -12% |
| nested | plain/subtree/user | 0.09 ms | 0.08 ms | -14% |
| nested | plain/subtree/orders | 0.10 ms | 0.09 ms | -13% |
| nested | plain/subtree_at/items | 0.10 ms | 0.09 ms | -13% |
| nested | plain/subtree/orders_walked | 0.10 ms | 0.08 ms | -22% |
| nested | plain/subtree_at/items_walked | 0.10 ms | 0.08 ms | -19% |
| nested | plain/children/orders | 0.10 ms | 0.09 ms | -19% |
| nested | plain/child_count/orders | 0.09 ms | 0.07 ms | -24% |
| nested | plain/path_query/wildcard | 0.10 ms | 0.08 ms | -18% |
| nested | plain/extract/nested | 0.09 ms | 0.07 ms | -19% |
| flat | json_tree/contains/functional | 0.46 ms | 0.49 ms | +5% |
| flat | json_tree/contains/operator | 0.47 ms | 0.49 ms | +5% |
| flat | json_tree/field_eq/extractor | 0.50 ms | 0.52 ms | +3% |
| flat | json_tree/field_eq/unique | 0.38 ms | 0.41 ms | +6% |
| flat | json_tree/field_gt/functional | 0.15 ms | 0.15 ms | -4% |
| flat | json_tree/field_between/functional | 0.20 ms | 0.20 ms | -4% |
| flat | json_tree/field_order/functional | 0.73 ms | 0.78 ms | +6% |
| flat | json_tree/extract/by_id | 0.16 ms | 0.16 ms | -3% |
| flat | json_tree/path_query/field | 0.16 ms | 0.16 ms | -3% |
| flat | plain/contains/operator | 0.10 ms | 0.08 ms | -20% |
| flat | plain/field_eq/extractor | 0.10 ms | 0.08 ms | -19% |
| flat | plain/field_eq/unique | 0.09 ms | 0.07 ms | -22% |
| flat | plain/field_gt/functional | 0.09 ms | 0.07 ms | -23% |
| flat | plain/field_between/functional | 0.12 ms | 0.11 ms | -10% |
| flat | plain/field_order/functional | 0.09 ms | 0.08 ms | -15% |
| flat | plain/extract/by_id | 0.09 ms | 0.07 ms | -25% |
| flat | plain/path_query/field | 0.09 ms | 0.07 ms | -17% |

The two runs agree with the harness on every scenario that moved by more than
the controls drifted:

- whole subtrees: +47 to +59%;
- the budget fallback: +58%;
- `child_count/orders`: +56%;
- `children/orders`: +2%;
- the wildcard read `$.orders[*].items` at the default N = 64 (65 tokens):
  +4% under the budget (both formats fall back to the sealed root) and +12%
  walked out.

Containment, equality, ordering and extraction on the nested corpus came
out +5 to +14%, and on the flat corpus −5 to +6%. The interleaved harness
puts the nested ones lower, at +1 to +10%, and is the figure to use.

