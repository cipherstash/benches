-- EQL v3 bench tables. Parallel to sql/schema.sql (the v2 tables, which stay
-- untouched as the regression baseline) — same row-count tiers, same shapes,
-- but columns are eql_v3 domain types instead of the eql_v2_encrypted
-- composite. Installed by `mise run setup-db-v3` after the v3 bundle.
--
-- Domain choice per table (a v3 column is exactly ONE domain — capability is
-- encoded in the type, unlike v2's single catch-all):
--
--   * string_*:  public.text_search (hm + ob + bf) — the only v3 domain that
--     carries both equality and match terms, matching v2's unique+match
--     string config. Requires the ingest config to add an ORE index on top
--     of v2's unique+match (from_v2 fails closed on a missing `ob`); rows
--     are correspondingly wider than their v2 siblings — noted in the report.
--   * integer_*: public.integer_ord — the default ordering domain (ORE
--     block term, custom btree opclass), v2 ORE bench parity.
--   * integer_*_ope: public.integer_ord_ope — the new CLLW-OPE fast path
--     (hex term, native bytea btree). Real `op` terms from
--     cipherstash-client >= 0.38.1 (Index::new_ope, CIP-3280/CIP-3348).
--   * category_*: public.text_eq (hm only) — GROUP BY / equality parity.
--   * combo_*: per-column domains mirroring the v2 combo configs.
--   * json_*: public.json — the SteVec document domain.
--
-- Tiers stop at 1M for the v3 pass (10M deferred; see plan).

-- Unsuffixed base tables: targets for the hyperfine ingest benchmarks
-- (`bench:v3:ingest:*`), which TRUNCATE + refill per run. The tiered
-- variants below serve the query benches.
CREATE TABLE IF NOT EXISTS string_encrypted_v3 (
    id SERIAL PRIMARY KEY,
    value public.text_search NOT NULL
);

CREATE TABLE IF NOT EXISTS integer_encrypted_v3 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord NOT NULL
);

CREATE TABLE IF NOT EXISTS integer_encrypted_ope_v3 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord_ope NOT NULL
);

CREATE TABLE IF NOT EXISTS category_encrypted_v3 (
    id SERIAL PRIMARY KEY,
    value public.text_eq NOT NULL
);

CREATE TABLE IF NOT EXISTS combo_encrypted_v3 (
    id SERIAL PRIMARY KEY,
    name public.text_search NOT NULL,
    age public.integer_ord NOT NULL,
    category public.text_eq NOT NULL
);

CREATE TABLE IF NOT EXISTS json_ste_vec_small_encrypted_v3 (
    id SERIAL PRIMARY KEY,
    value public.json NOT NULL
);

-- String: equality + match + ordering (text_search).
CREATE TABLE IF NOT EXISTS string_encrypted_v3_10000 (
    id SERIAL PRIMARY KEY,
    value public.text_search NOT NULL
);

CREATE TABLE IF NOT EXISTS string_encrypted_v3_100000 (
    id SERIAL PRIMARY KEY,
    value public.text_search NOT NULL
);

CREATE TABLE IF NOT EXISTS string_encrypted_v3_1000000 (
    id SERIAL PRIMARY KEY,
    value public.text_search NOT NULL
);

-- Integer: ORE ordering (default `_ord` domain).
CREATE TABLE IF NOT EXISTS integer_encrypted_v3_10000 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord NOT NULL
);

CREATE TABLE IF NOT EXISTS integer_encrypted_v3_100000 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord NOT NULL
);

CREATE TABLE IF NOT EXISTS integer_encrypted_v3_1000000 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord NOT NULL
);

-- Integer OPE: the v3 fast ordering path (native bytea btree over the
-- hex-decoded `op` term — no custom comparator anywhere in the chain).
CREATE TABLE IF NOT EXISTS integer_encrypted_ope_v3_10000 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord_ope NOT NULL
);

CREATE TABLE IF NOT EXISTS integer_encrypted_ope_v3_100000 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord_ope NOT NULL
);

CREATE TABLE IF NOT EXISTS integer_encrypted_ope_v3_1000000 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord_ope NOT NULL
);

-- Category: low-cardinality equality (GROUP BY scenarios). Same ~250-bucket
-- generator as v2; shares the v2 category_plaintext_* baselines.
CREATE TABLE IF NOT EXISTS category_encrypted_v3_10000 (
    id SERIAL PRIMARY KEY,
    value public.text_eq NOT NULL
);

CREATE TABLE IF NOT EXISTS category_encrypted_v3_100000 (
    id SERIAL PRIMARY KEY,
    value public.text_eq NOT NULL
);

CREATE TABLE IF NOT EXISTS category_encrypted_v3_1000000 (
    id SERIAL PRIMARY KEY,
    value public.text_eq NOT NULL
);

-- Combo: three encrypted columns per row (composite-predicate scenarios).
CREATE TABLE IF NOT EXISTS combo_encrypted_v3_10000 (
    id SERIAL PRIMARY KEY,
    name public.text_search NOT NULL,
    age public.integer_ord NOT NULL,
    category public.text_eq NOT NULL
);

CREATE TABLE IF NOT EXISTS combo_encrypted_v3_100000 (
    id SERIAL PRIMARY KEY,
    name public.text_search NOT NULL,
    age public.integer_ord NOT NULL,
    category public.text_eq NOT NULL
);

CREATE TABLE IF NOT EXISTS combo_encrypted_v3_1000000 (
    id SERIAL PRIMARY KEY,
    name public.text_search NOT NULL,
    age public.integer_ord NOT NULL,
    category public.text_eq NOT NULL
);

-- JSON SteVec (small documents — 4 flat fields).
CREATE TABLE IF NOT EXISTS json_ste_vec_small_encrypted_v3_10000 (
    id SERIAL PRIMARY KEY,
    value public.json NOT NULL
);

CREATE TABLE IF NOT EXISTS json_ste_vec_small_encrypted_v3_100000 (
    id SERIAL PRIMARY KEY,
    value public.json NOT NULL
);

CREATE TABLE IF NOT EXISTS json_ste_vec_small_encrypted_v3_1000000 (
    id SERIAL PRIMARY KEY,
    value public.json NOT NULL
);

-- New-family smoke table (10k only): one column per v3 scalar family the v2
-- benches never covered. Catches CHECK/operator/inlining breakage per
-- family; not intended to produce tier curves. `boolean` is storage-only by
-- design (two-value cardinality — any index would leak the plaintext), so it
-- gets insert/select-back coverage only.
CREATE TABLE IF NOT EXISTS scalar_smoke_v3 (
    id SERIAL PRIMARY KEY,
    date_val public.date_ord NOT NULL,
    timestamp_val public.timestamp_ord NOT NULL,
    numeric_val public.numeric_ord NOT NULL,
    bigint_val public.bigint_ord NOT NULL,
    double_val public.double_ord NOT NULL,
    boolean_val public.boolean NOT NULL
);

-- 10M tier (added for CIP-3361's 10k -> 10M flat-latency chart; the
-- other families stay capped at 1M).
CREATE TABLE IF NOT EXISTS string_encrypted_v3_10000000 (
    id SERIAL PRIMARY KEY,
    value public.text_search NOT NULL
);

CREATE TABLE IF NOT EXISTS integer_encrypted_v3_10000000 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord NOT NULL
);

CREATE TABLE IF NOT EXISTS integer_encrypted_ope_v3_10000000 (
    id SERIAL PRIMARY KEY,
    value public.integer_ord_ope NOT NULL
);

CREATE TABLE IF NOT EXISTS json_ste_vec_small_encrypted_v3_10000000 (
    id SERIAL PRIMARY KEY,
    value public.json NOT NULL
);

CREATE TABLE IF NOT EXISTS category_encrypted_v3_10000000 (
    id SERIAL PRIMARY KEY,
    value public.text_eq NOT NULL
);

-- JSON SteVec, nested corpus (FakeJsonLarge, ~213 nodes per document):
-- sealed from the json_tree plaintext twins json_tree_large_plain_v4_<N>,
-- id for id (src/twin.rs), for the three-way JSON comparison. Ids are
-- explicit; the serial is resynced after an ingest.
CREATE TABLE IF NOT EXISTS json_ste_vec_large_encrypted_v3_10000 (
    id SERIAL PRIMARY KEY,
    value public.json NOT NULL
);

CREATE TABLE IF NOT EXISTS json_ste_vec_large_encrypted_v3_100000 (
    id SERIAL PRIMARY KEY,
    value public.json NOT NULL
);
