# JSON tree on `next`: the merged derivable-token format on an M4

This report measures `json_tree`, EQL v4's encrypted JSON document type, as it
merged to `next` in cipherstash/stack on 2026-10-25 (commit **ebdb5de0a**, with
#1358, #1362, #1363 and #1366 on it). It is called **merged** below. Two
earlier builds were timed beside it, interleaved, on the same machine and the
same PostgreSQL:

- **gates**: edabee625, the gated walk before #1363 (code-identical to #1358).
- **derivable**: 683802bcc, #1363 as [JSON_TREE_DERIVABLE_TOKENS.md](JSON_TREE_DERIVABLE_TOKENS.md)
  first measured it on an M1 Max.

`derivable` and `merged` store the same bytes and send the same tokens. What
differs is the SQL. f94a6eb11 ("inline the json_tree chain primitives and
build the read's index once") landed after the M1 measurement and before the
merge, along with the review fixes. So `derivable` against `merged` isolates
that commit, and `gates` against `merged` is the net effect of the format
change as shipped.

## Summary

**The merged build is as good as or better than every earlier measurement
on every json_tree read and enumeration, by a wide margin.** The regressions
the M1 measurement found in #1363 are gone. Each one is now faster than the
gated walk it replaced.

Against **gates**, same machine, per call (in-server per document in brackets):

- **Whole-subtree reads**: −25% to −75% (−63% to −85%). `$.orders` walked out
  12.92 → 3.42 ms. `$.company` 1.67 → 1.25 ms.
- **The budget fallback** (`$.orders` past the default budget): 4.79 → 1.52 ms,
  −68% (−85%). On the M1, #1363 had made this 51% slower.
- **Deep member paths**: −27% to −42% (−68% to −78%).
- **Wildcards**: −66% to −88% (−77% to −94%). That includes the two-level
  wildcard at N = 64 (4,161 tokens): 12.83 → 3.90 ms. On the M1 that case was
  48% slower than gates.
- **Enumeration**: `json_tree_children` −43% to −78%. `json_tree_child_count`
  −46% on `$.orders` and −91% on a 300-wide node. On the M1, #1363 had made
  child count 59% slower.
- **Walking a 300-element array**: 22.88 → 6.68 ms, −71%. On the M1, #1363 had
  made it 78% slower.

Against **derivable** (#1363 before f94a6eb11), every read is 29% to 87% faster
per call.

Against the **M1 Max figures**, the merged build's in-server cost per document
is lower than the best M1 figure on every json_tree scenario, by 70% to 93%
for anything that walks. The M4 does the unchanged gates code in 0.74× the
M1's time, so that is not the machine: allowing for it, the merged build still
costs 60% to 90% less. Sub-millisecond per-call figures are higher here than on
the M1, because this PostgreSQL runs in Docker (see "Machine").

**What is not better.**

1. **`read/first_name` per call**: 0.48 → 0.71 ms (+46%), reproduced in every
   run and at 100k rows. This is the flat corpus's top-level scalar, a
   five-entry document. Two causes:
   - gates returned **no row** for it (#1356), so its figure is the cost of
     finding nothing;
   - every merged `json_tree_subtree` call carries about **0.2 ms more fixed
     cost per statement** than the older SQL, whatever it reads (section 5).
     On every other read the in-server saving outweighs it many times over. On
     a five-entry document nothing does.

   In-server per document it is 0.037 → 0.047 ms against gates and 0.070 →
   0.047 ms against derivable.
2. **The stored-format costs of #1363 stand, as the M1 measured them.**
   f94a6eb11 changed no stored byte, so these are the self tag `x` and not the
   SQL:
   - nested documents +11.2% on disk, flat +7.4%;
   - sealing +5% to +10%, INSERT +4% to +9%;
   - work that detoasts the document pays for its size: `->` +5% to +12%
     in-server, `ORDER BY ord_term` at a nested path +6%, nested containment
     with rechecks +1% to +4%.

   The containment GIN is the same size and builds within 3%.

**Correctness.** All three builds pass `check_json_tree_v4` at both tiers with
the M1's counts: 10,000 rows gates 692 / derivable 717 / merged 717 passed, 0
failed; 100,000 rows 672 / 697 / 697, 0 failed. Every format sealed
byte-identical plaintext twins (md5-checked on every copy).

## Machine

| | this run | the M1 measurement |
|---|---|---|
| machine | Apple M4 laptop, 10 cores, 24 GB | Apple M1 Max, 64 GB |
| PostgreSQL | 17.10 (`postgres:17.10`, Debian aarch64) in Docker (OrbStack, 10 CPUs, 12 GB VM) on port 5400 | 17.10 native (Homebrew) on port 5400 |
| settings | `shared_buffers` 4 GB, `work_mem` 128 MB, `maintenance_work_mem` 2 GB, `effective_cache_size` 8 GB, JIT on | `shared_buffers` 4 GB, `work_mem` 128 MB, `maintenance_work_mem` 4 GB, `effective_cache_size` 24 GB, JIT on |
| databases | `jsonbench_base` (gates), `jsonbench_new` (derivable), `jsonbench_merged` (merged), one cluster | `jsonbench_base`, `jsonbench_new` |

What that means for reading the numbers:

- **Round trip.** The client reaches the server through OrbStack's VM network.
  `noop/by_id` (a primary-key lookup) costs 0.25 ms per call here against 0.10 ms
  on the M1, and every per-call figure carries that extra ~0.15 ms. For anything
  under a millisecond, read the in-server column.
- **Compute.** On the unchanged gates code the M4 takes 0.75× the M1's time per
  call (median over the 24 scenarios above 1 ms) and 0.74× in-server (22
  scenarios above 0.5 ms). That is the factor to apply when comparing an M4
  figure with an M1 one.
- **Memory at 100k rows.** The three databases' nested 100k tables are about
  10.6 GB, beside a 12 GB VM, so not all of it stays cached. The 100k in-server
  pass ran once and shows misses on some floors (gates `detoast/nested` 0.89 ms
  against 0.05 ms at 10k). At 100k, read the per-call column, which ran over two
  passes after a warm-up and matches the 10k figures.
- **Documents.** The corpora are generated at random, so this machine's
  documents are not the M1's, though their averages match (217.2 entries per
  nested document in both, and per-document sizes within 0.1%). Every format
  here seals the same documents. This matters for the single-row criterion
  benches: their representative row (`id = 5000`) holds 294 entries under
  `$.orders` here, so those benches' walked figures are not comparable across
  machines. The interleaved harness averages 200 sampled rows.

## How it was run

The method is the derivable-tokens measurement's (see
[JSON_TREE_DERIVABLE_TOKENS.md](JSON_TREE_DERIVABLE_TOKENS.md), "How it was
run"), extended to three formats:

- **Bundles.** Each database holds the EQL v4 bundle built from its commit
  (`mise run build` in `packages/eql` of a worktree at edabee625, 683802bcc and
  ebdb5de0a), plus `sql/schema_v4.sql`. In the merged database the seven chain
  primitives and `json_tree_index` carry no pinned `search_path`; in the
  derivable database they do. That is f94a6eb11's mechanism, checked in
  `pg_proc.proconfig` before any timing.
- **Bench crate.** `json-tree-bench` gains a third feature, `merged`, a git
  dependency on ebdb5de0a built into `json-tree-bench/target-merged`. It uses
  the derivable code path: the client API did not change between 683802bcc and
  ebdb5de0a. The `map/by_parent` and `map/by_self` floors name functions
  f94a6eb11 removed, so the merged plan times `map/index`, the one
  `json_tree_index` pass that replaced them.
- **Tasks.** The `bench:dt:*` tasks take `DT_FORMATS` (here
  `gates derivable merged`), `DT_OUT` and `DT_INGEST_LOG`.
  - The new `bench:dt:twin` copies a plaintext twin from one database to the
    others and checks its md5.
  - `bench:dt:ingest` copies the twin after the first format of every rep.
  - `dt_bench time` reads `FORMATS` and rotates which format goes first, pass by
    pass and scenario by scenario.
- **Ingest.**
  - At 10k rows: flat and nested three reps each, the order rotated; the
    open-root nested corpus and the 1,000-row wide corpus once each.
  - At 100k rows: flat and nested once each.
  - Each GIN at 10k was then rebuilt with `REINDEX` three times per format,
    interleaved.
- **Criterion.** `json_tree_v4` and `json_tree_nested_v4`, 10k rows, one
  database after another. They also create the functional btrees that
  `field_order/nested` uses.
- **Timing.**
  - 10k: two full runs (`time_10000_r1.json`, `time_10000_r2.json`), each 5 call
    passes of 200 sampled rows and 3 in-server passes of 500 documents (the M1
    used 3 and 2).
  - 10k focused run: the floors, containment, extraction, ordering and
    enumeration of `$.orders`, at 10 passes, 5 in-server passes and 100
    repetitions (`time_10000_focus.json`).
  - 100k: 2 passes and 1 in-server pass (`time_100000_r1.json`).
    `field_order/nested` was dropped from the 100k plans, as on the M1: there is
    no btree at that tier, and each call sorts 100,000 TOASTed documents.
- **Correctness.** `bench:dt:check` at both tiers, every format against its
  own database.
- **Report.** `report_json_tree_merged.py render <rows> <time files>` prints
  every table below. The M1 columns read `results/query/derivable/`.

The whole matrix took about two and a half hours.

## 1. Stored size and ingest
#### Stored size, 10,000 rows

| table | measure | gates | derivable | merged | merged vs gates | M1: gates → derivable |
|---|---|--:|--:|--:|--:|--:|
| json_tree_small_10000 | entries per document | 5.0 | 5.0 | 5.0 | | 5.0 → 5.0 |
| json_tree_small_10000 | `pg_column_size` per document | 1,398 B | 1,502 B | 1,502 B | +7.4% | 1,398 B → 1,502 B |
| json_tree_small_10000 | characters per document | 1,279 | 1,384 | 1,384 | +8.2% | 1,279 → 1,384 |
| json_tree_small_10000 | heap incl. TOAST | 15.7 MiB | 15.7 MiB | 15.7 MiB | +0.0% | 15.7 MiB → 15.7 MiB |
| json_tree_small_10000 | containment GIN | 1.0 MiB | 1.0 MiB | 1.0 MiB | +0.0% | 1.0 MiB → 1.0 MiB |
| json_tree_small_10000 | total | 17.0 MiB | 17.0 MiB | 17.0 MiB | +0.0% | 16.9 MiB → 16.9 MiB |
| json_tree_large_10000 | entries per document | 217.2 | 217.2 | 217.2 | | 217.2 → 217.2 |
| json_tree_large_10000 | `pg_column_size` per document | 31,712 B | 35,248 B | 35,253 B | +11.2% | 31,714 B → 35,256 B |
| json_tree_large_10000 | characters per document | 49,036 | 53,589 | 53,596 | +9.3% | 49,042 → 53,603 |
| json_tree_large_10000 | heap incl. TOAST | 319.7 MiB | 354.5 MiB | 354.6 MiB | +10.9% | 319.5 MiB → 354.5 MiB |
| json_tree_large_10000 | containment GIN | 39.9 MiB | 39.9 MiB | 39.9 MiB | +0.0% | 39.9 MiB → 39.9 MiB |
| json_tree_large_10000 | total | 360.8 MiB | 395.6 MiB | 395.7 MiB | +9.7% | 359.7 MiB → 394.7 MiB |
| json_tree_large_open_10000 | entries per document | 217.2 | 217.2 | 217.2 | | 217.2 → 217.2 |
| json_tree_large_open_10000 | `pg_column_size` per document | 26,450 B | 30,005 B | 30,006 B | +13.4% | 26,458 B → 30,013 B |
| json_tree_large_open_10000 | characters per document | 44,508 | 49,069 | 49,072 | +10.3% | 44,519 → 49,081 |
| json_tree_large_open_10000 | heap incl. TOAST | 267.6 MiB | 302.7 MiB | 302.8 MiB | +13.1% | 267.5 MiB → 302.8 MiB |
| json_tree_large_open_10000 | total | 267.8 MiB | 303.0 MiB | 303.0 MiB | +13.1% | 267.7 MiB → 303.1 MiB |
| json_tree_wide_1000 | entries per document | 603.0 | 603.0 | 603.0 | | 603.0 → 603.0 |
| json_tree_wide_1000 | `pg_column_size` per document | 91,194 B | 99,998 B | 99,997 B | +9.7% | 91,195 B → 99,998 B |
| json_tree_wide_1000 | characters per document | 125,968 | 138,631 | 138,631 | +10.1% | 125,967 → 138,630 |
| json_tree_wide_1000 | heap incl. TOAST | 91.0 MiB | 100.2 MiB | 100.2 MiB | +10.2% | 91.0 MiB → 100.2 MiB |
| json_tree_wide_1000 | total | 91.0 MiB | 100.3 MiB | 100.3 MiB | +10.2% | 91.0 MiB → 100.3 MiB |

#### Ingest: seal and INSERT, seconds (median of the runs; min–max)

| corpus | rows | runs | step | gates | derivable | merged | merged vs gates | M1: gates → derivable |
|---|--:|--:|---|--:|--:|--:|--:|--:|
| large | 10,000 | 3 / 3 / 3 | seal | 6.11 (6.09–6.11) | 6.46 (6.42–6.48) | 6.43 (6.36–6.43) | +5.2% | 8.37 → 8.75 |
| large | 10,000 | 3 / 3 / 3 | INSERT | 15.29 (15.14–16.01) | 16.64 (16.54–17.30) | 16.49 (16.34–16.51) | +7.9% | 23.68 → 25.62 |
| large | 100,000 | 1 / 1 / 1 | seal | 61.02 (61.02–61.02) | 64.18 (64.18–64.18) | 63.74 (63.74–63.74) | +4.5% | 80.00 → 82.73 |
| large | 100,000 | 1 / 1 / 1 | INSERT | 160.12 (160.12–160.12) | 166.74 (166.74–166.74) | 166.47 (166.47–166.47) | +4.0% | 224.96 → 245.13 |
| large_open | 10,000 | 1 / 1 / 1 | seal | 5.95 (5.95–5.95) | 6.27 (6.27–6.27) | 6.26 (6.26–6.26) | +5.3% | 8.12 → 8.46 |
| large_open | 10,000 | 1 / 1 / 1 | INSERT | 14.49 (14.49–14.49) | 16.09 (16.09–16.09) | 15.72 (15.72–15.72) | +8.5% | 22.97 → 24.65 |
| small | 10,000 | 3 / 3 / 3 | seal | 0.28 (0.24–0.30) | 0.30 (0.30–0.33) | 0.31 (0.28–0.31) | +10.5% | 0.29 → 0.31 |
| small | 10,000 | 3 / 3 / 3 | INSERT | 0.92 (0.91–0.94) | 0.98 (0.95–0.99) | 0.99 (0.96–1.00) | +7.3% | 1.29 → 1.35 |
| small | 100,000 | 1 / 1 / 1 | seal | 2.94 (2.94–2.94) | 3.19 (3.19–3.19) | 3.20 (3.20–3.20) | +8.7% | 2.83 → 2.95 |
| small | 100,000 | 1 / 1 / 1 | INSERT | 9.21 (9.21–9.21) | 9.57 (9.57–9.57) | 9.65 (9.65–9.65) | +4.7% | 12.28 → 12.95 |
| wide | 1,000 | 1 / 1 / 1 | seal | 1.43 (1.43–1.43) | 1.49 (1.49–1.49) | 1.51 (1.51–1.51) | +5.7% | 1.91 → 2.08 |
| wide | 1,000 | 1 / 1 / 1 | INSERT | 4.00 (4.00–4.00) | 4.01 (4.01–4.01) | 3.87 (3.87–3.87) | -3.3% | 5.67 → 6.18 |

| containment GIN build (REINDEX, median of 3) | gates | derivable | merged | merged vs gates |
|---|--:|--:|--:|--:|
| json_tree_large_encrypted_v4_10000_query_gin_idx | 1.49 s | 1.67 s | 1.52 s | +2.6% |
| json_tree_small_encrypted_v4_10000_query_gin_idx | 0.04 s | 0.04 s | 0.04 s | -2.6% |


At 100,000 rows:
#### Stored size, 100,000 rows

| table | measure | gates | derivable | merged | merged vs gates | M1: gates → derivable |
|---|---|--:|--:|--:|--:|--:|
| json_tree_small_100000 | entries per document | 5.0 | 5.0 | 5.0 | | 5.0 → 5.0 |
| json_tree_small_100000 | `pg_column_size` per document | 1,399 B | 1,502 B | 1,502 B | +7.4% | 1,398 B → 1,502 B |
| json_tree_small_100000 | characters per document | 1,279 | 1,384 | 1,384 | +8.2% | 1,279 → 1,384 |
| json_tree_small_100000 | heap incl. TOAST | 156.3 MiB | 156.3 MiB | 156.3 MiB | +0.0% | 156.3 MiB → 156.3 MiB |
| json_tree_small_100000 | containment GIN | 7.9 MiB | 7.9 MiB | 7.9 MiB | +0.0% | 7.9 MiB → 7.9 MiB |
| json_tree_small_100000 | total | 166.4 MiB | 166.4 MiB | 166.4 MiB | +0.0% | 166.4 MiB → 166.4 MiB |
| json_tree_large_100000 | entries per document | 216.9 | 216.9 | 216.9 | | 217.1 → 217.1 |
| json_tree_large_100000 | `pg_column_size` per document | 31,678 B | 35,206 B | 35,212 B | +11.2% | 31,691 B → 35,228 B |
| json_tree_large_100000 | characters per document | 48,975 | 53,523 | 53,530 | +9.3% | 49,003 → 53,561 |
| json_tree_large_100000 | heap incl. TOAST | 3,191.6 MiB | 3,540.7 MiB | 3,541.4 MiB | +11.0% | 3,192.6 MiB → 3,542.6 MiB |
| json_tree_large_100000 | containment GIN | 357.9 MiB | 357.9 MiB | 357.9 MiB | +0.0% | 358.1 MiB → 358.1 MiB |
| json_tree_large_100000 | total | 3,551.7 MiB | 3,900.8 MiB | 3,901.5 MiB | +9.8% | 3,552.9 MiB → 3,902.9 MiB |


The merged format stores what #1363 stores. The 5 to 7 bytes per nested
document between them are #1366's exact float round trip in the sealed numbers.
Ingest is 25% to 35% faster on this machine than on the M1 in absolute terms,
and the format's relative cost (+5% to +10%) is what the M1 found.

## 2. Reads, wildcards and enumeration (10,000 rows, run 1)

#### Floors (no json_tree read)

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| detoast/nested | 0.247 / 0.295 / 0.255 | +3.2% | -13.5% | 0.036 / 0.040 / 0.040 | +10.3% | 0.144 / 0.150 |
| map/by_parent (nested) | 0.550 / 0.610 / – | n/a | n/a | 0.283 / 0.331 / – | n/a | 0.493 / 0.550 |
| map/by_self (nested) | – / 0.607 / – | n/a | n/a | – / 0.327 / – | n/a | – / 0.548 |
| map/index (nested) | – / – / 0.377 | n/a | n/a | – / – / 0.126 | n/a | – |
| noop/by_id | 0.254 / 0.252 / 0.247 | -2.7% | -2.2% | 0.001 / 0.001 / 0.001 | -0.4% | 0.099 / 0.100 |

#### Reads with no wildcard

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| read/$ (root open) | 15.24 / 21.81 / 3.857 | -74.7% | -82.3% | 14.48 / 21.01 / 2.950 | -79.6% | 21.08 / 30.69 |
| read/$ (sealed root) | – / 1.651 / 1.163 | n/a | -29.6% | – / 1.019 / 0.299 | n/a | – / 1.971 |
| read/company | 1.673 / 2.265 / 1.253 | -25.1% | -44.7% | 0.999 / 1.590 / 0.372 | -62.7% | 2.002 / 2.814 |
| read/company.name | 1.645 / 1.639 / 1.136 | -30.9% | -30.7% | 0.980 / 1.007 / 0.294 | -70.0% | 1.968 / 1.977 |
| read/first_name | 0.484 / 0.545 / 0.706 | +45.7% | +29.4% | 0.037 / 0.070 / 0.047 | +27.0% | 0.618 / 0.649 |
| read/orders (fallback) | 4.787 / 7.192 / 1.522 | -68.2% | -78.8% | 4.057 / 6.457 / 0.625 | -84.6% | 6.357 / 9.615 |
| read/orders (walked) | 12.92 / 18.65 / 3.423 | -73.5% | -81.7% | 12.17 / 17.92 / 2.516 | -79.3% | 18.10 / 25.94 |
| read/orders[2] | 2.837 / 4.002 / 1.475 | -48.0% | -63.1% | 2.091 / 3.217 / 0.597 | -71.5% | 3.569 / 5.077 |
| read/orders[2].status | 1.572 / 1.664 / 1.142 | -27.4% | -31.4% | 0.924 / 1.009 / 0.295 | -68.1% | 1.884 / 1.955 |
| read/user | 2.059 / 2.787 / 1.321 | -35.8% | -52.6% | 1.383 / 2.112 / 0.452 | -67.3% | 2.544 / 3.520 |
| read/user.contact.emergency_contact.name | 2.005 / 1.656 / 1.167 | -41.8% | -29.5% | 1.359 / 1.010 / 0.299 | -78.0% | 2.481 / 1.934 |

#### Wildcard reads

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| wildcard/orders[*].items[*].price N=64 | 12.83 / 18.47 / 3.896 | -69.6% | -78.9% | 11.88 / 17.26 / 2.769 | -76.7% | 17.43 / 25.81 |
| wildcard/orders[*].items[*].price N=8 | 12.67 / 2.688 / 1.522 | -88.0% | -43.4% | 11.85 / 2.598 / 0.686 | -94.2% | 17.77 / 3.499 |
| wildcard/orders[*].total N=256 | 4.085 / 2.836 / 1.383 | -66.1% | -51.2% | 3.366 / 2.126 / 0.492 | -85.4% | 5.471 / 3.615 |
| wildcard/orders[*].total N=64 | 4.027 / 2.076 / 1.272 | -68.4% | -38.7% | 3.363 / 1.397 / 0.397 | -88.2% | 5.510 / 2.540 |
| wildcard/orders[*].total N=8 | 4.028 / 1.866 / 1.216 | -69.8% | -34.9% | 3.406 / 1.822 / 0.439 | -87.1% | 5.386 / 2.271 |

#### Enumerating children: `$.orders` on the nested corpus (5 to 10 elements), and the wide corpus (1,000 rows: 300 members, 300 elements)

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| child_count/attrs | 18.19 / 11.67 / 1.550 | -91.5% | -86.7% | 17.45 / 11.15 / 0.958 | -94.5% | 24.89 / 15.10 |
| child_count/list | 18.16 / 11.66 / 1.544 | -91.5% | -86.8% | 17.44 / 11.17 / 0.980 | -94.4% | 24.84 / 15.08 |
| child_count/orders (nested) | 1.552 / 2.435 / 0.842 | -45.7% | -65.4% | 1.055 / 1.887 / 0.237 | -77.5% | 1.752 / 2.787 |
| children/attrs | 18.48 / 9.678 / 4.119 | -77.7% | -57.4% | 17.53 / 9.267 / 3.604 | -79.4% | 25.12 / 12.87 |
| children/list | 18.51 / 9.783 / 4.294 | -76.8% | -56.1% | 17.59 / 9.270 / 3.586 | -79.6% | 25.08 / 12.84 |
| children/orders (nested) | 1.546 / 1.625 / 0.886 | -42.7% | -45.5% | 0.934 / 1.060 / 0.321 | -65.6% | 1.737 / 1.812 |
| read/attrs (walked) | 22.87 / 31.25 / 6.185 | -73.0% | -80.2% | 22.02 / 30.20 / 5.125 | -76.7% | 31.06 / 42.04 |
| read/list (walked) | 22.88 / 40.56 / 6.678 | -70.8% | -83.5% | 21.95 / 39.63 / 5.619 | -74.4% | 31.06 / 55.43 |


How to read these tables:

- The `M1` column is the derivable-tokens measurement's per-call median (its
  focused run where it had the scenario, else run 1). Multiply it by about 0.75
  to compare it with an M4 figure, and add about 0.15 ms of round trip.
- `read/$ (sealed root)` has no gates figure: the gated walk refused `$`.
- At N = 8 the wildcard rows refuse on the 33% of documents with more than
  eight orders (`JsonWildcardTruncated`), as before. That is why derivable and
  merged return more rows per document there.

**Where f94a6eb11's saving shows.**

- **Floors.** `map/index`, the one pass that builds both maps, costs 0.13 ms
  in-server per nested document. Each of the two maps it replaced cost 0.28 to
  0.33 ms.
- **Buffers.** The `EXPLAIN (ANALYZE, BUFFERS)` of `read/company` touches 10
  shared buffers in the merged build against 80 (gates) and 90 (derivable).
  The document is decompressed once, into a `MATERIALIZED` CTE, where every
  reference to the argument used to fetch it again.
- **Per hit.** The member-or-element test that cost #1363 its whole-subtree
  regression is now cheaper than the gated walk's single token step. Walking
  300 members costs 6.19 ms against 22.87 ms (gates) and 31.25 ms
  (derivable); 300 elements 6.68 ms against 22.88 and 40.56.

The `EXPLAIN (ANALYZE, BUFFERS)` of every scenario's representative row is in
the time files, under `explain`. The outer plan is the same shape in all three
formats: a primary-key index scan feeding a Function Scan.

## 3. Containment, extraction and ordering (10,000 rows, the focused run)

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| detoast/nested | 0.290 / 0.280 / 0.290 | -0.0% | +3.4% | 0.038 / 0.041 / 0.040 | +4.2% | 0.144 / 0.150 |
| map/by_parent (nested) | 0.548 / 0.599 / – | n/a | n/a | 0.283 / 0.322 / – | n/a | 0.493 / 0.550 |
| map/by_self (nested) | – / 0.597 / – | n/a | n/a | – / 0.327 / – | n/a | – / 0.548 |
| map/index (nested) | – / – / 0.374 | n/a | n/a | – / – / 0.129 | n/a | – |
| noop/by_id | 0.238 / 0.214 / 0.250 | +5.1% | +16.9% | 0.001 / 0.001 / 0.001 | -0.4% | 0.099 / 0.100 |
| contains/array_element | 0.707 / 0.705 / 0.721 | +1.9% | +2.2% | n/a / n/a / n/a | n/a | 0.637 / 0.643 |
| contains/deep_object | 1.619 / 1.652 / 1.657 | +2.3% | +0.3% | n/a / n/a / n/a | n/a | 1.843 / 1.923 |
| contains/email | 0.533 / 0.530 / 0.533 | -0.0% | +0.6% | n/a / n/a / n/a | n/a | 0.439 / 0.433 |
| extract/age | 0.324 / 0.324 / 0.320 | -1.5% | -1.5% | 0.044 / 0.045 / 0.047 | +8.1% | 0.182 / 0.182 |
| extract/company.salary | 0.652 / 0.662 / 0.669 | +2.6% | +1.1% | 0.184 / 0.195 / 0.198 | +7.3% | 0.461 / 0.493 |
| field_order/nested | 2.153 / 2.284 / 2.275 | +5.7% | -0.4% | n/a / n/a / n/a | n/a | 2.172 / 2.331 |

None of these calls a json_tree read, and none of their SQL changed between
derivable and merged. What moved between gates and the other two is the
document each query detoasts and parses, now 11% larger. That is the M1's
finding again: selective containment within noise, deep containment with
rechecks +2% to +4%, `->` on the nested corpus +7% to +12% in-server,
`ORDER BY ord_term` at a nested path +6%.

## 4. 100,000 rows

#### Floors (no json_tree read)

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| detoast/nested | 0.346 / 0.337 / 0.292 | -15.8% | -13.4% | 0.891 / 0.634 / 0.049 | -94.5% | 0.138 / 0.147 |
| map/by_parent (nested) | 0.520 / 0.603 / – | n/a | n/a | 0.287 / 0.330 / – | n/a | 0.474 / 0.558 |
| map/by_self (nested) | – / 0.627 / – | n/a | n/a | – / 0.333 / – | n/a | – / 0.603 |
| map/index (nested) | – / – / 0.422 | n/a | n/a | – / – / 0.138 | n/a | – |
| noop/by_id | 0.227 / 0.262 / 0.224 | -1.7% | -14.6% | 0.015 / 0.009 / 0.010 | -35.4% | 0.105 / 0.094 |

#### Reads with no wildcard

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| read/$ (sealed root) | – / 1.677 / 1.161 | n/a | -30.7% | – / 1.018 / 0.327 | n/a | – / 1.838 |
| read/company | 1.658 / 2.252 / 1.270 | -23.4% | -43.6% | 1.009 / 1.657 / 0.375 | -62.8% | 1.903 / 2.677 |
| read/company.name | 1.629 / 1.673 / 1.144 | -29.8% | -31.6% | 0.978 / 1.019 / 0.295 | -69.8% | 1.891 / 1.828 |
| read/first_name | 0.573 / 0.626 / 0.864 | +50.9% | +38.0% | 0.239 / 0.143 / 0.088 | -63.2% | 0.560 / 0.637 |
| read/orders (fallback) | 4.791 / 7.230 / 1.520 | -68.3% | -79.0% | 4.061 / 6.458 / 0.700 | -82.8% | 6.026 / 9.193 |
| read/orders (walked) | 12.91 / 18.46 / 3.392 | -73.7% | -81.6% | 12.31 / 18.12 / 2.568 | -79.1% | 16.61 / 24.14 |
| read/orders[2] | 2.560 / 3.591 / 1.451 | -43.3% | -59.6% | 2.091 / 3.194 / 0.580 | -72.3% | 3.405 / 4.942 |
| read/orders[2].status | 1.543 / 1.692 / 1.130 | -26.8% | -33.2% | 0.934 / 1.021 / 0.296 | -68.3% | 1.773 / 1.851 |
| read/user | 2.048 / 2.808 / 1.360 | -33.6% | -51.6% | 1.386 / 2.120 / 0.478 | -65.5% | 2.402 / 3.357 |
| read/user.contact.emergency_contact.name | 2.031 / 1.671 / 1.130 | -44.3% | -32.4% | 1.375 / 1.023 / 0.304 | -77.9% | 2.370 / 1.842 |

#### Wildcard reads

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| wildcard/orders[*].items[*].price N=64 | 12.61 / 18.42 / 3.869 | -69.3% | -79.0% | 11.98 / 17.20 / 2.755 | -77.0% | 16.42 / 24.84 |
| wildcard/orders[*].items[*].price N=8 | 12.67 / 2.706 / 1.510 | -88.1% | -44.2% | 12.04 / 2.759 / 0.706 | -94.1% | 16.22 / 3.243 |
| wildcard/orders[*].total N=256 | 4.195 / 2.848 / 1.393 | -66.8% | -51.1% | 3.380 / 2.146 / 0.508 | -85.0% | 4.987 / 3.546 |
| wildcard/orders[*].total N=64 | 4.213 / 2.099 / 1.291 | -69.4% | -38.5% | 3.456 / 1.401 / 0.392 | -88.7% | 4.946 / 2.467 |
| wildcard/orders[*].total N=8 | 4.205 / 1.848 / 1.224 | -70.9% | -33.8% | 3.374 / 1.982 / 0.463 | -86.3% | 4.902 / 2.082 |

#### Enumerating children: `$.orders` on the nested corpus (5 to 10 elements), and the wide corpus (1,000 rows: 300 members, 300 elements)

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| child_count/attrs | 18.09 / 11.61 / 1.556 | -91.4% | -86.6% | 17.50 / 11.03 / 1.007 | -94.2% | – |
| child_count/list | 18.12 / 11.66 / 1.544 | -91.5% | -86.8% | 17.42 / 10.99 / 0.940 | -94.6% | – |
| child_count/orders (nested) | 1.570 / 2.413 / 0.783 | -50.1% | -67.5% | 1.113 / 1.883 / 0.244 | -78.1% | 1.736 / 2.748 |
| children/attrs | 18.65 / 9.839 / 4.160 | -77.7% | -57.7% | 17.74 / 9.225 / 3.581 | -79.8% | – |
| children/list | 18.45 / 9.814 / 4.323 | -76.6% | -55.9% | 17.48 / 9.150 / 3.625 | -79.3% | – |
| children/orders (nested) | 1.546 / 1.603 / 0.952 | -38.4% | -40.6% | 0.950 / 1.080 / 0.323 | -66.0% | 1.742 / 1.772 |
| read/attrs (walked) | 22.81 / 31.28 / 6.136 | -73.1% | -80.4% | 21.84 / 30.23 / 5.166 | -76.3% | – |
| read/list (walked) | 22.80 / 40.67 / 6.677 | -70.7% | -83.6% | 21.89 / 39.62 / 5.629 | -74.3% | – |

#### Containment and `->` extraction

Per call is the client wall time of one prepared call, median (p95 below it in the raw files). In-server is per document over the pass set.

| scenario | per call ms: gates / derivable / merged | merged vs gates | merged vs derivable | in-server ms/doc: gates / derivable / merged | merged vs gates | M1 per call: gates / derivable |
|---|--:|--:|--:|--:|--:|--:|
| contains/array_element | 0.699 / 0.702 / 0.689 | -1.5% | -1.9% | n/a / n/a / n/a | n/a | 1.867 / 1.893 |
| contains/deep_object | 4.372 / 4.468 / 4.438 | +1.5% | -0.7% | n/a / n/a / n/a | n/a | 6.119 / 6.196 |
| contains/email | 0.508 / 0.499 / 0.476 | -6.1% | -4.5% | n/a / n/a / n/a | n/a | 0.493 / 0.481 |
| extract/age | 0.270 / 0.319 / 0.271 | +0.6% | -15.0% | 0.043 / 0.046 / 0.046 | +4.8% | 0.180 / 0.184 |
| extract/company.salary | 0.651 / 0.682 / 0.646 | -0.7% | -5.3% | 0.185 / 0.202 / 0.201 | +8.3% | 0.460 / 0.478 |

The per-call figures match the 10k tier: a read touches one document. The
in-server column at this tier ran once and carries cache misses (see
"Machine"). The first format to read a table in the pass paid them, which is
why gates' `detoast/nested` reads 0.89 ms. Use the per-call column here.

## 5. The fixed cost per `json_tree_subtree` call

Per-call time minus in-server time per document is what a call pays once,
however much it reads: the round trip, binding, and setting up the read
function. From run 1 at 10k rows:

| scenario | gates | derivable | merged |
|---|--:|--:|--:|
| `noop/by_id` | 0.252 | 0.251 | 0.246 |
| `read/first_name` | 0.447 | 0.475 | 0.658 |
| `read/company` | 0.674 | 0.675 | 0.881 |
| `read/company.name` | 0.666 | 0.632 | 0.843 |
| `read/orders[2].status` | 0.648 | 0.655 | 0.847 |
| `read/user` | 0.676 | 0.675 | 0.869 |
| `read/orders (walked)` | 0.741 | 0.729 | 0.906 |
| `wildcard/orders[*].total N=64` | 0.664 | 0.679 | 0.876 |
| `children/orders (nested)` | 0.612 | 0.565 | 0.565 |
| `child_count/orders (nested)` | 0.496 | 0.548 | 0.605 |

(ms per call.) Every merged `json_tree_subtree` call pays 0.17 to 0.21 ms more
than either older build. `json_tree_children` does not.

**Likely cause: planning the read's body.** It is consistent with the following,
though not proven:

- `json_tree_subtree` is a `LANGUAGE sql` function that PostgreSQL 17 does not
  inline (it is recursive and set-returning). So its body is planned again for
  every statement that calls it.
- f94a6eb11 made the body plan the inlined primitives and the
  `json_tree_index` subquery rather than calling them.
- The body, prepared afresh under a generic plan (parameters, not constants,
  as the SQL-function executor plans it), takes this long to plan, median of
  45 (`python3 report_json_tree_planning.py 10000`, output in
  `results/query/merged/planning_10000.txt`):

| read | gates | derivable | merged |
|---|--:|--:|--:|
| `read/first_name` | 0.221 ms | 0.194 ms | 0.402 ms |
| `read/company` | 1.464 ms | 1.597 ms | 1.790 ms |
| `read/orders[2].status` | 1.467 ms | 1.583 ms | 1.790 ms |

The +0.2 ms on `read/first_name` matches the per-call delta exactly. The
absolute planning times of the nested reads are higher than a real call pays,
because the standalone body is planned with the whole document bound as a
parameter of the outer statement. The deltas are the reliable part.

**What would remove it**, untested:

- a `plpgsql` wrapper, which caches its plan for the session;
- PostgreSQL 18, which caches SQL-function plans.

It costs about 0.2 ms on calls that otherwise save 0.5 to 15 ms, so it matters
only for very small documents read one at a time.

## 6. Criterion cross-check (10,000 rows, not interleaved)

One representative row per scenario, one database after another, 10 samples.
The direction agrees with the harness on every json_tree read. Medians, ms:

| bench | scenario | gates | derivable | merged | merged vs gates |
|---|---|--:|--:|--:|--:|
| flat | json_tree/contains/functional | 0.47 | 0.48 | 0.48 | +1% |
| flat | json_tree/contains/operator | 0.49 | 0.47 | 0.48 | -3% |
| flat | json_tree/field_eq/extractor | 0.46 | 0.45 | 0.45 | -2% |
| flat | json_tree/field_eq/unique | 0.42 | 0.42 | 0.41 | -2% |
| flat | json_tree/field_gt/functional | 0.25 | 0.25 | 0.25 | -0% |
| flat | json_tree/field_between/functional | 0.29 | 0.28 | 0.29 | +1% |
| flat | json_tree/field_order/functional | 0.77 | 0.78 | 0.78 | +2% |
| flat | json_tree/extract/by_id | 0.26 | 0.26 | 0.26 | -2% |
| flat | json_tree/path_query/field | 0.26 | 0.26 | 0.26 | -2% |
| nested | json_tree/contains/deep_object | 1.50 | 1.54 | 1.54 | +3% |
| nested | json_tree/contains/array_element | 0.57 | 0.57 | 0.58 | +2% |
| nested | json_tree/contains/nested_array | 2.06 | 2.12 | 2.14 | +4% |
| nested | json_tree/contains/deep_object_fn | 1.50 | 1.54 | 1.54 | +2% |
| nested | json_tree/contains/array_element_fn | 0.57 | 0.58 | 0.59 | +4% |
| nested | json_tree/contains/nested_array_fn | 2.06 | 2.13 | 2.12 | +3% |
| nested | json_tree/field_eq/nested | 0.50 | 0.51 | 0.52 | +3% |
| nested | json_tree/field_gt/nested | 0.20 | 0.20 | 0.20 | +1% |
| nested | json_tree/field_between/nested | 0.29 | 0.29 | 0.29 | +0% |
| nested | json_tree/field_order/nested | 2.06 | 2.21 | 2.21 | +7% |
| nested | json_tree/subtree/company | 1.85 | 2.66 | 1.26 | -32% |
| nested | json_tree/subtree/user | 2.23 | 3.20 | 1.33 | -40% |
| nested | json_tree/subtree/orders | 4.88 | 7.66 | 1.51 | -69% |
| nested | json_tree/subtree_at/items | 7.19 | 7.37 | 1.48 | -79% |
| nested | json_tree/subtree/orders_walked | 21.11 | 30.34 | 4.93 | -77% |
| nested | json_tree/subtree_at/items_walked | 21.14 | 25.64 | 4.40 | -79% |
| nested | json_tree/children/orders | 1.78 | 1.97 | 0.92 | -48% |
| nested | json_tree/child_count/orders | 1.74 | 3.25 | 0.82 | -53% |
| nested | json_tree/path_query/wildcard | 1.95 | 2.12 | 2.15 | +10% |
| nested | json_tree/extract/nested | 0.56 | 0.58 | 0.59 | +4% |

`subtree/orders_walked` reads 294 entries on this machine's representative row,
against an average of 175, which is why its gates figure is higher than the
M1's (11.7 ms). `path_query/wildcard` (+10%) and `extract/nested` (+4%) do not
call the walk. They read the larger document, and derivable shows the same
increase. The criterion `plain/*` controls (native jsonb) are 0.19 to 0.26 ms
here against 0.07 to 0.13 ms on the M1: that is the Docker round trip.

## Raw files

- `results/query/merged/time_10000_r1.json`, `time_10000_r2.json`,
  `time_10000_focus.json`, `time_100000_r1.json`: every call and pass, with
  each scenario's SQL, operands and `EXPLAIN (ANALYZE, BUFFERS)`.
- `results/query/merged/plan_{gates,derivable,merged}_{10000,100000}.json`:
  what each format sent.
- `results/query/merged/json_tree_correctness_*.json`: the checks.
- `results/query/merged/storage_{10000,100000}.json`: sizes.
- `results/ingest/merged/ingest.jsonl`, `gin_build.jsonl`: ingest and index
  builds.
- `results/query/merged/criterion/<format>/`: the criterion runs.
