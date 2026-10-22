//! Ingest for the EQL v4 `json_tree` bench, nested corpus: the root crate's
//! `FakeJsonLarge` shape (user / company / addresses / orders with items)
//! into `json_tree_large_encrypted_v4<suffix>`, for the token-chain walks,
//! array functions and deep containment the flat corpus cannot exercise.
//! The loop, its environment contract and the fake-key-source caveat are
//! in `json_tree_bench::ingest`.

use anyhow::Result;
use json_tree_bench::{fake_json_large, ingest, Corpus};

#[tokio::main]
async fn main() -> Result<()> {
    ingest("encrypt_json_tree_large_v4", Corpus::Nested, fake_json_large).await
}
