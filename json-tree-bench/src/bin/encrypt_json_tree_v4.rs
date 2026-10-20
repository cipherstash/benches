//! Ingest for the EQL v4 `json_tree` bench: small flat JSON documents (the
//! root crate's `FakeJsonSmall` shape) sealed by stack-encrypt's JSON index
//! into `json_tree_small_encrypted_v4<suffix>` as `public.eql_v4_json_tree`,
//! with the plaintext twin written to `json_tree_small_plain_v4<suffix>`
//! under the same id (needle sourcing for the query bench; see lib.rs).
//!
//! Sealing is under `FakeDataKeySource`, in-process, with one batched
//! `generate_keys` per `BATCH_SIZE` documents — no ZeroKMS round-trip. So
//! the hyperfine numbers for this binary measure "derive terms + AEAD +
//! INSERT" and are NOT comparable to the v3 `encrypt_ste_vec_small_v3`
//! ingest, which pays the KMS call per batch. They are comparable to each
//! other across tiers, and the index-build time `prepare:_table` logs is
//! comparable to v3's.
//!
//! Environment variables: DATABASE_URL, NUM_RECORDS, BATCH_SIZE,
//! TABLE_SUFFIX, HYPERFINE_ITERATION. Same contract as the root crate's
//! ingest binaries, so `prepare:_table` and `bench:ingest:_run` drive it.

use std::env;

use anyhow::{Context, Result};
use eql_bindings::v4::json_tree::JsonTreeDocument;
use json_tree_bench::{cipher, column, fake_json_small};
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::QueryBuilder;

#[tokio::main]
async fn main() -> Result<()> {
    let database_url =
        env::var("DATABASE_URL").context("DATABASE_URL environment variable must be set")?;
    let num_records: i64 = env::var("NUM_RECORDS")
        .unwrap_or_else(|_| "10000".to_string())
        .parse()
        .context("NUM_RECORDS must be a valid integer")?;
    let batch_size: usize = env::var("BATCH_SIZE")
        .unwrap_or_else(|_| "1000".to_string())
        .parse()
        .context("BATCH_SIZE must be a valid integer")?;
    let hf_iteration: i32 = env::var("HYPERFINE_ITERATION")
        .unwrap_or_else(|_| "0".to_string())
        .parse()
        .context("HYPERFINE_ITERATION must be a valid integer")?;
    let suffix = env::var("TABLE_SUFFIX").unwrap_or_default();
    let encrypted = format!("json_tree_small_encrypted_v4{suffix}");
    let plain = format!("json_tree_small_plain_v4{suffix}");

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;
    let cipher = cipher().await;
    let keyset = cipher.default_keyset();

    // Explicit ids, continuing from the table's high-water mark, so the
    // plaintext twin pairs with its encrypted row by id (RETURNING order is
    // not a contract). The serial is resynced at the end.
    let (next_id,): (i32,) =
        sqlx::query_as(&format!("SELECT COALESCE(MAX(id), 0) + 1 FROM {encrypted}"))
            .fetch_one(&pool)
            .await?;

    let mut id = i64::from(next_id);
    let mut remaining = num_records;
    while remaining > 0 {
        let count = remaining.min(batch_size as i64) as usize;
        let docs: Vec<Value> = (0..count).map(|_| fake_json_small()).collect();

        // One batched derivation: a `Vec<JsonTreeDocument>` is settled in
        // one `generate_keys` call for the whole batch.
        let sealed: Vec<JsonTreeDocument> = keyset
            .encrypt_as::<Vec<Value>, Vec<JsonTreeDocument>>(&docs, column())
            .await
            .context("seal batch")?;

        let ids: Vec<i64> = (id..id + count as i64).collect();
        let mut tx = pool.begin().await?;
        QueryBuilder::new(format!("INSERT INTO {encrypted} (id, value) "))
            .push_values(ids.iter().zip(&sealed), |mut b, (id, doc)| {
                b.push_bind(*id as i32)
                    .push_bind(Json(serde_json::to_value(doc).expect("serialise document")));
            })
            .build()
            .execute(&mut *tx)
            .await?;
        QueryBuilder::new(format!("INSERT INTO {plain} (id, plain) "))
            .push_values(ids.iter().zip(&docs), |mut b, (id, doc)| {
                b.push_bind(*id as i32).push_bind(Json(doc.clone()));
            })
            .build()
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        id += count as i64;
        remaining -= count as i64;
    }

    sqlx::query(&format!(
        "SELECT setval(pg_get_serial_sequence('{encrypted}', 'id'), (SELECT MAX(id) FROM {encrypted}))"
    ))
    .execute(&pool)
    .await?;

    // The marker the root crate's combine_benchmark reads after a hyperfine
    // run, at the path it expects (this crate's directory is the cwd).
    let result = json!({ "inserted": num_records });
    std::fs::create_dir_all("target")?;
    std::fs::write(
        format!("target/encrypt_json_tree_v4-{num_records}_{hf_iteration}.json"),
        serde_json::to_string(&result)?,
    )?;
    Ok(())
}
