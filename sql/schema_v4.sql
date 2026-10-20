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
