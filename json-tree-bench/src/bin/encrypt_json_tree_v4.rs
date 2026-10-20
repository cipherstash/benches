//! Ingest for the EQL v4 `json_tree` bench, small corpus: flat documents
//! (the root crate's `FakeJsonSmall` shape) into
//! `json_tree_small_encrypted_v4<suffix>` — the SteVec bench's twin. The
//! loop, its environment contract and the fake-key-source caveat are in
//! `json_tree_bench::ingest`.

use anyhow::Result;
use json_tree_bench::{fake_json_small, ingest};

#[tokio::main]
async fn main() -> Result<()> {
    ingest("encrypt_json_tree_v4", "small", fake_json_small).await
}
