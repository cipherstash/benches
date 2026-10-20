-- EQL v4 containment index for the json_tree bench: the one index the
-- family documents. eql_v4.json_tree_contains(value, needle) inlines to
-- native jsonb @> over eql_v4.json_tree_query(value)::jsonb (the
-- element-nested projection), so a jsonb_path_ops GIN over that expression
-- serves every containment scenario (whole-document and single-path
-- needles alike). GIN is lossy; the executor rechecks the real operator.
-- The path-query / children / child_count scenarios are per-row extractions
-- and take no index.

CREATE INDEX json_tree_small_encrypted_v4_10000_query_gin_idx
ON json_tree_small_encrypted_v4_10000 USING GIN (
    (eql_v4.json_tree_query(value)::jsonb) jsonb_path_ops
);
