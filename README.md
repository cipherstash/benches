# ORE Benchmarks

Performance benchmarks for CipherStash's searchable encryption operations using Order-Revealing Encryption (ORE) and the Encrypt Query Language (EQL).

## 📊 View Results

The latest benchmark results are available in the [`report/`](report/) directory:

- **[Benchmark Report](report/BENCHMARK_REPORT.md)** - the main report (EQL v3): ingest throughput, per-family query performance, SQL/plans/index configs, plus comparison sections vs EQL 2.3 and vs plaintext PostgreSQL
- **[EQL v3 vs v2 Comparison](report/V3_COMPARISON.md)** - full regression tables, index-engagement audit, and docs/marketing charts (`report/v3/`)
- **[EQL 2.3 report archive](report/v2/BENCHMARK_REPORT.md)** - the frozen v2 baseline report

## 🆕 EQL v3 benches

The `*_v3` benches target the EQL v3 release (domain-specific types —
`public.text_search`, `public.integer_ord`, … — replacing the single
`eql_v2_encrypted` composite). They live alongside the v2 benches; the
committed v2 results are the regression baseline and are never overwritten
(v3 results land in `results/query/v3/` and `results/ingest/v3/`).

```bash
# Install the v3 bundle alongside v2. Defaults to the pinned release
# download (eql-3.0.0-alpha.3); override with EQL_V3_VERSION=<tag>,
# EQL_V3_SQL=<prebuilt bundle>, or EQL_V3_DIR=<local checkout to build>.
mise run setup-db-v3

# Populate all v3 tables at a tier
mise run prepare:v3:all 10000

# Query benches: exact | match | ore | ope | group_by | combo | json | plaintext | smoke
mise run bench:v3:query:all 10000     # arg = max tier (10000 | 100000 | 1000000)

# Ingest benches (hyperfine, same tiers as v2)
mise run bench:v3:ingest

# Terminal overviews (v3 siblings of report / report:slow / report:ingest,
# which scan only the v2 baseline results)
mise run report:v3
mise run report:v3:slow [ms]
mise run report:v3:ingest

# v2-vs-v3 side-by-side on the CLI (medians + delta per scenario/tier)
mise run report:v3-compare

# Full comparison artifacts: report/V3_COMPARISON.md + report/v3/ charts
mise run report:build:v3-compare

# Main report (EQL v3 numbers + v2/plaintext comparison sections)
mise run report:build
```

v3 payloads are produced by converting the pinned cipherstash-client's v2.3
output through `eql-bindings::from_v2` (the supported migration path — see
`src/v3.rs` for the details and caveats). The `ope` benches use real
CLLW-OPE `op` terms emitted by cipherstash-client 0.38.1 (`Index::new_ope()`).

## 🌳 EQL v4 `json_tree` benches (stack-encrypt's JSON index)

`json-tree-bench/` benchmarks the JSON tree family — the successor to the
SteVec `public.json` scheme — against the same `FakeJsonSmall` corpus, so the
two JSON schemes compare like for like (`mise run report:json-tree-compare`).
It is a crate of its own because the root package's cipherstash-client 0.38.1
and stack-encrypt cannot share one zerokms-protocol; it pulls stack-encrypt
and eql-bindings from the stack monorepo's `feat/json-index-stack-encrypt`
branch. Documents are sealed under an in-process fake key source (no ZeroKMS
round-trip), so its ingest numbers are not comparable to the v3 ingest; its
query numbers are, since the cost under measurement is Postgres's.

```bash
# Build + install the v4 bundle from ../stack (or EQL_V4_SQL=<bundle>), create tables
mise run setup-db-v4

# Populate (tiers as for v3) and run the query bench
mise run bench:v4:query:json_tree 10000      # prepares json_tree_small_encrypted_v4_10000 first

# Ingest throughput (hyperfine; results/ingest/v4/)
mise run bench:v4:ingest:json-tree-small

# SteVec vs json_tree vs plaintext, per scenario and tier
mise run report:json-tree-compare -- --output report/JSON_TREE_COMPARISON.md
```

### Other benchmarks

- **[ZeroKMS vs AWS KMS](kms-app/REPORT.md)** — bulk field‑encryption throughput
  vs AWS KMS. ZeroKMS encrypts/decrypts a whole batch in one round‑trip; AWS KMS
  has no bulk API and throttle‑fails past a few hundred values per request.

### Headline numbers

Query-only medians (no decrypt) from the latest full run against EQL 2.3, across four row-count tiers. Full per-scenario detail — SQL, planner index choices, EXPLAIN plans — is in [`report/`](report/).

| Family | Scenario | 10k | 100k | 1M | 10M |
|---|---|--:|--:|--:|--:|
| **JSON** | contains/functional | 0.2 ms | 0.3 ms | 0.4 ms | 0.8 ms |
| JSON | field_eq/functional | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms |
| JSON | field_order/functional | 0.3 ms | 0.3 ms | 0.4 ms | 0.4 ms |
| **ORE** | range_gt_100 | 4.0 ms | 4.2 ms | 4.1 ms | 4.0 ms |
| ORE | range_lt_ordered_10 | 0.5 ms | 0.5 ms | 0.5 ms | 0.5 ms |
| **EXACT** | eql_hash | 0.1 ms | 0.1 ms | 0.1 ms | 0.1 ms |
| **MATCH** | eql_bloom | 0.4 ms | 1.8 ms | 15 ms | 144 ms |
| **GROUP_BY** | low_cardinality — encrypted | 2.2 ms | 20 ms | 93 ms | 776 ms |
| GROUP_BY | low_cardinality — plaintext baseline | 1.2 ms | 9.0 ms | 39 ms | 339 ms |
| **COMBO** | top_n_filtered_group_by | 0.2 ms | 1.0 ms | 5.3 ms | 52 ms |

Selective ORE range scenarios are currently disabled (a planner selectivity mis-estimate) — see [encrypt-query-language#230](https://github.com/cipherstash/encrypt-query-language/issues/230). The pathological `ORDER BY value` shape (no extractor; ~60 s at the 10M tier because the sort key can't match any allowed index — a btree directly on `value` would, but the encrypted body trips Postgres's btree entry-size limit) has been removed from the suite — the EQL perf guide covers it as the documented anti-pattern.

## 🔧 Test Setup

### Hardware & Software

The benchmarks are designed to run on a local development machine with the following stack:

- **Database**: PostgreSQL 17 (native — Homebrew `postgresql@17`)
- **Language**: Rust (latest stable)
- **Framework**: Criterion.rs for benchmarking
- **Encryption**: CipherStash EQL v2 with ORE support

### Database Configuration

```yaml
PostgreSQL 17 (native, Homebrew postgresql@17)
Data directory: ~/.eqlbench/pgdata
Port: 5400
User: postgres
Database: postgres
```

### Test Data

The benchmarks use three types of encrypted data:

1. **Integer values** - ORE-encrypted integers for range queries
2. **String values** - Encrypted strings for exact and pattern matching
3. **JSON objects** - Small encrypted JSON documents

### Data Set Sizes

Benchmarks are run against multiple data set sizes:
- 10,000 rows
- 100,000 rows
- 1,000,000 rows
- 10,000,000 rows (optional)

### Query Types

Three categories of queries are benchmarked:

**EXACT Queries** - Exact match lookups
- Using EQL cast operator
- Using EQL HMAC-256 hash

**MATCH Queries** - Pattern matching
- LIKE queries with EQL cast
- Bloom filter containment queries

**ORE Queries** - Range queries on encrypted integers
- Exact match
- Range queries (>, <)
- Ordered range queries with ORDER BY

Each query is tested with and without decryption of results.

## 🚀 Running Benchmarks

### Prerequisites

1. **Install mise** (tool version manager):
   ```bash
   curl https://mise.run | sh
   ```

2. **Install Rust** (via mise):
   ```bash
   mise install
   ```

3. **Set up environment variables**:
   ```bash
   cp .env.example .env
   # Edit .env with your CipherStash credentials
   ```

4. **Install PostgreSQL 17** — the benches run a native local cluster:
   ```bash
   brew install postgresql@17
   ```
   The cluster itself (`~/.eqlbench/pgdata`, port 5400) is created automatically
   on first use by the `postgres-init` task — no manual `initdb` needed.

### Quick Start

```bash
# Start PostgreSQL
mise run postgres

# Set up database (creates tables and installs EQL extension)
mise run setup-db

# Run all ingest benchmarks
mise run bench:ingest

# Run query benchmarks for a specific row count
mise run bench:query:exact 10000
mise run bench:query:match 10000
mise run bench:query:ore 10000

# Run all query benchmarks (all row counts)
mise run bench:query:all

# Generate report
mise run report:build
```

### Step-by-Step Guide

#### 1. Start PostgreSQL

```bash
mise run postgres
```

This starts the native PostgreSQL 17 cluster on port 5400 (data directory `~/.eqlbench/pgdata`), creating and configuring it first if it doesn't yet exist.

#### 2. Initialize Database

```bash
mise run reset-db    # Reset database (if needed)
mise run setup-db    # Install EQL extension and create tables
```

#### 3. Run Ingest Benchmarks

```bash
# Run individual ingest benchmarks
mise run bench:ingest:encrypt_int
mise run bench:ingest:encrypt_string
mise run bench:ingest:encrypt_json_small

# Or run all at once
mise run bench:ingest
```

Results are saved to `results/ingest/*.json`

#### 4. Prepare Tables for Query Benchmarks

Before running query benchmarks, tables need to be populated and indexed:

```bash
# Prepare string_encrypted table with 10,000 rows
mise run prepare:string_encrypted 10000

# Prepare integer_encrypted table with 10,000 rows
mise run prepare:integer_encrypted 10000
```

This process:
1. Checks current row count
2. Drops indexes
3. Inserts additional rows if needed
4. Creates indexes

#### 5. Run Query Benchmarks

```bash
# Run specific query benchmark with specific row count
mise run bench:query:exact 10000
mise run bench:query:match 100000
mise run bench:query:ore 1000000

# Run all query benchmarks for all row counts (10k, 100k, 1M, 10M)
mise run bench:query:all
```

Each query bench writes two files to `results/query/` per row-count tier:

- `<bench>_rows_<N>.json` — criterion's NDJSON message stream (one
  `benchmark-complete` event per scenario, with mean / median / etc.).
- `<bench>_metadata_<N>.json` — JSON sidecar with one record per
  scenario: the exact SQL the bench ran, the bound parameter (encrypted
  payload as JSON, or empty for raw-SQL benches), the
  `EXPLAIN (FORMAT JSON)` plan captured once at startup, and the list of
  indexes the planner picked. Join the two files by the `id` field.

#### 6. Generate Report

Quick overview of all results in the terminal — median runtime per scenario, slowest first:

```bash
mise run report
```

Or build the full Markdown report file:

```bash
mise run report:build
```

This generates:
- `report/BENCHMARK_REPORT.md` - Markdown report
- `report/*_chart.png` - Performance charts (requires matplotlib)

To enable chart generation:
```bash
pip3 install matplotlib
```

## 📁 Project Structure

```
ore-benches/
├── benches/              # Criterion benchmark definitions
│   ├── exact.rs          # EXACT query benchmarks
│   ├── match.rs          # MATCH query benchmarks
│   └── ore.rs            # ORE range query benchmarks
├── src/
│   ├── bin/              # Binary utilities
│   │   ├── encrypt_int.rs
│   │   ├── encrypt_string.rs
│   │   └── combine_benchmark.rs
│   └── lib.rs            # Shared benchmark code
├── sql/
│   ├── schema.sql        # Database schema
│   └── indexes/          # Index creation scripts
├── results/              # Benchmark results (JSON)
│   ├── ingest/           # Ingest throughput results
│   └── query/            # Query performance results
├── report/               # Generated reports
│   ├── BENCHMARK_REPORT.md
│   └── *.png             # Charts
├── report_benchmarks.py  # Report generator script
├── mise.toml             # Task definitions
└── README.md             # This file
```

## 🛠️ Advanced Usage

### Custom Row Counts

```bash
# Prepare and benchmark custom row count
mise run prepare:string_encrypted 50000
TARGET_ROWS=50000 cargo criterion --bench exact
```

### Individual Benchmark Runs

```bash
# Build in release mode
mise run bench:build

# Run specific benchmark manually
TARGET_ROWS=10000 cargo criterion --bench ore --message-format json > results/query/ore_rows_10000.json
```

### Database Management

```bash
# Connect to database
mise run psql

# View PostgreSQL logs
mise run postgres-logs

# Stop PostgreSQL
mise run postgres-stop
```

### Report Generation Options

```bash
# Generate report with custom filename
mise run report:build custom_report.md

# Or use Python script directly
python3 report_benchmarks.py --output report/my_report.md

# Specify custom directories
python3 report_benchmarks.py \
  --results-dir results \
  --sql-dir sql \
  --output report/BENCHMARK_REPORT.md
```

## 📈 Understanding Results

### Ingest Throughput

Measures how many encrypted records can be inserted per second. Higher is better.

### Query Performance

Query times are reported both:
- **Without decryption** - Time to execute query and retrieve encrypted results
- **With decryption** - Time including client-side decryption

Times exceeding 100ms are marked with ⚠️ for easy identification.

### Performance Factors

Query performance is affected by:
1. **Data set size** - Larger datasets generally increase query time
2. **Index type** - Hash indexes are faster for exact matches; ORE indexes enable range queries
3. **Query complexity** - Pattern matching is slower than exact lookups
4. **Result set size** - LIMIT clause affects decryption overhead

## 🔍 Troubleshooting

### PostgreSQL Connection Issues

```bash
# Check / start PostgreSQL (idempotent — reports if already running)
mise run postgres

# Restart PostgreSQL
mise run postgres-stop
mise run postgres
```

### Missing EQL Extension

```bash
mise run setup-db
```

### Benchmark Failures

Check that:
1. Database is running and accessible
2. Tables have been prepared with correct row counts
3. Environment variables are set in `.env`
4. CipherStash credentials are valid

## 📚 Additional Documentation

- [Report Generator Documentation](README_REPORT.md) - Detailed guide for the report generation script
- [Report Directory](report/README.md) - Information about generated reports
- [CipherStash Documentation](https://cipherstash.com/docs) - Official CipherStash docs

## 🤝 Contributing

When adding new benchmarks:

1. Add benchmark definition to `benches/`
2. Update `mise.toml` with new tasks
3. Add query descriptions to `report_benchmarks.py`
4. Document the benchmark in this README
5. Run benchmarks and commit results to `report/`

## 📄 License

MIT License - see [LICENSE](LICENSE) for details.
