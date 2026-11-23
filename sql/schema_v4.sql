-- EQL v4 bench tables. Requires the v4 bundle (mise run setup-db-v4).
--
-- One table per tier, written by the Rust ingest (v4/src/bin/ingest_v4.rs)
-- through stack-encrypt + eql_bindings::v4 and read back by both the Rust
-- and the Go (rc.6) benches. The 14 encrypted columns mirror the
-- `individual_entity` shape a customer reported decrypt latency on.
--
-- psql -v tier=10000 -f sql/schema_v4.sql

\set tbl individuals_v4_ :tier

DROP TABLE IF EXISTS :tbl;
CREATE TABLE :tbl (
    id             bigint PRIMARY KEY,
    first_name     public.eql_v4_text_eq      NOT NULL,
    last_name      public.eql_v4_text_match   NOT NULL,
    email          public.eql_v4_text_eq      NOT NULL,
    phone          public.eql_v4_text         NOT NULL,
    date_of_birth  public.eql_v4_date         NOT NULL,
    nationality    public.eql_v4_text         NOT NULL,
    tax_id         public.eql_v4_text_eq      NOT NULL,
    address_line1  public.eql_v4_text         NOT NULL,
    address_line2  public.eql_v4_text         NOT NULL,
    city           public.eql_v4_text         NOT NULL,
    postcode       public.eql_v4_text         NOT NULL,
    country        public.eql_v4_text         NOT NULL,
    occupation     public.eql_v4_text         NOT NULL,
    risk_score     public.eql_v4_integer_ord  NOT NULL
);
