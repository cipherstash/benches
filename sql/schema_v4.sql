-- EQL v4 bench tables: the JSON tree family (`public.eql_v4_json_tree`),
-- produced by stack-encrypt's `Json` index rather than cipherstash-client's
-- SteVec. Parallel to the `json_ste_vec_small_encrypted_v3_*` tables in
-- sql/schema_v3.sql (same row-count tiers, same FakeJsonSmall documents) so
-- the two JSON schemes compare like for like. Installed by
-- `mise run setup-db-v4` after the v4 bundle, which coexists with v2 and v3.

-- Unsuffixed base table: target for the hyperfine ingest benchmark
-- (`bench:v4:ingest:json-tree-small`), which TRUNCATEs + refills per run.
CREATE TABLE IF NOT EXISTS json_tree_small_encrypted_v4 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

-- Tiered variants for the query benches.
CREATE TABLE IF NOT EXISTS json_tree_small_encrypted_v4_10000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_small_encrypted_v4_100000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_small_encrypted_v4_1000000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_small_encrypted_v4_10000000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

-- Plaintext twins, keyed by the encrypted row's id. The json_tree ingest
-- seals under a fake in-memory key source (no ZeroKMS round-trip: the cost
-- under measurement is Postgres's), so a stored document cannot be opened
-- by a later process. The query bench samples its needle plaintexts from
-- here instead, and can check an encrypted result set against `plain @>`.
-- These tables are never read by a measured query.
CREATE TABLE IF NOT EXISTS json_tree_small_plain_v4 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_small_plain_v4_10000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_small_plain_v4_100000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_small_plain_v4_1000000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_small_plain_v4_10000000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

-- Nested corpus: the root crate's FakeJsonLarge shape (user / company /
-- addresses / orders with items) — containers at every depth and two
-- arrays of objects — for the token-chain walks (json_tree_subtree,
-- json_tree_children, json_tree_child_count), wildcard path queries and
-- deep containment that the flat corpus cannot exercise. No SteVec twin:
-- the v3 JSON bench is small-corpus only. The plaintext twin tables
-- double as the jsonb baseline for these scenarios (same rows, native
-- operators, jsonb_path_ops GIN), since no json_large_plaintext baseline
-- exists.

CREATE TABLE IF NOT EXISTS json_tree_large_encrypted_v4 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_large_plain_v4 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_large_encrypted_v4_10000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_large_plain_v4_10000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_large_encrypted_v4_100000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_large_plain_v4_100000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_large_encrypted_v4_1000000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_large_plain_v4_1000000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

-- The derivable-tokens comparison (report/JSON_TREE_DERIVABLE_TOKENS.md):
-- the nested corpus sealed with the root OPEN, for the read of `$` with no
-- sealed root (its twin is filled from json_tree_large_plain_v4_<N>, so the
-- documents are the nested corpus's, id for id), and the wide corpus (one
-- 300-member object, one 300-element array) for the cost of enumerating a
-- wide node. No containment index on either: only reads run on them.

CREATE TABLE IF NOT EXISTS json_tree_large_open_encrypted_v4_10000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_large_open_plain_v4_10000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_wide_encrypted_v4_1000 (
    id SERIAL PRIMARY KEY,
    value public.eql_v4_json_tree NOT NULL
);

CREATE TABLE IF NOT EXISTS json_tree_wide_plain_v4_1000 (
    id INTEGER PRIMARY KEY,
    plain JSONB NOT NULL
);
