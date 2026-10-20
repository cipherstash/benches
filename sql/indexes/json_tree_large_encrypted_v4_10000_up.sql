-- EQL v4 containment index for the nested json_tree corpus (see the small
-- corpus's _up.sql for why one jsonb_path_ops GIN over the projection
-- serves every containment scenario). The plaintext twin gets the native
-- equivalent so the nested bench's `plain/*` baseline scenarios are
-- indexed the same way.

CREATE INDEX json_tree_large_encrypted_v4_10000_query_gin_idx
ON json_tree_large_encrypted_v4_10000 USING GIN (
    (eql_v4.json_tree_query(value)::jsonb) jsonb_path_ops
);

CREATE INDEX IF NOT EXISTS json_tree_large_plain_v4_10000_gin_idx
ON json_tree_large_plain_v4_10000 USING GIN (plain jsonb_path_ops);
