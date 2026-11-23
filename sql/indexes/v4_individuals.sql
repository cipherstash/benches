-- Indexes for an individuals_v4_<tier> table, built after the ingest so
-- the load is not slowed by index maintenance.
--
-- psql -v tier=10000 -f sql/indexes/v4_individuals.sql

\set tbl individuals_v4_ :tier
\set ix_email :tbl _email_eq
\set ix_last :tbl _last_name_match
\set ix_risk :tbl _risk_score_ord

CREATE INDEX IF NOT EXISTS :ix_email ON :tbl USING hash  (eql_v4.eq_term(email));
CREATE INDEX IF NOT EXISTS :ix_last  ON :tbl USING gin   (eql_v4.match_term(last_name));
CREATE INDEX IF NOT EXISTS :ix_risk  ON :tbl USING btree (eql_v4.ord_term(risk_score));
ANALYZE :tbl;
