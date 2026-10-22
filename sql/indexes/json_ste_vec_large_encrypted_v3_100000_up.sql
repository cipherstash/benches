-- EQL v3 containment index for the nested SteVec corpus — the same GIN the
-- flat table json_ste_vec_small_encrypted_v3_100000 carries (see its _up.sql):
-- eql_v3.jsonb_contains(value, needle) inlines to
-- eql_v3.jsonb_array(value) @> eql_v3.jsonb_array(needle). The documents are
-- the json_tree plaintext twin's (json_tree_large_plain_v4_100000), id for id.

CREATE INDEX json_ste_vec_large_encrypted_v3_100000_jsonb_array_index
ON json_ste_vec_large_encrypted_v3_100000 USING GIN (
    eql_v3.jsonb_array(value)
);
