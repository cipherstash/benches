//! Seal the json_tree plaintext twin `json_tree_large_plain_v4<suffix>` into
//! `json_ste_vec_large_encrypted_v3<suffix>` (EQL v3 SteVec), id for id, so
//! SteVec, json_tree and native jsonb hold the same documents. See
//! `dbbenches::twin`.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    dbbenches::init_tracing();
    dbbenches::twin::ingest_from_twin("large").await
}
