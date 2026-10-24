//! Ingest for the two corpora the derivable-tokens comparison added, chosen by
//! `CORPUS`: `open` (the nested corpus with the root open, resealing
//! `json_tree_large_open_plain_v4<suffix>`, which is filled from the nested
//! twin) or `wide` (`fake_json_wide`). Same environment contract as the other
//! ingest binaries.

use json_tree_bench::{fake_json_large, fake_json_wide, ingest, Corpus};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match std::env::var("CORPUS").as_deref() {
        Ok("open") => {
            ingest(
                "encrypt_json_tree_open_v4",
                Corpus::NestedOpen,
                fake_json_large,
            )
            .await
        }
        Ok("wide") => ingest("encrypt_json_tree_wide_v4", Corpus::Wide, fake_json_wide).await,
        other => anyhow::bail!("CORPUS must be `open` or `wide`, got {other:?}"),
    }
}
