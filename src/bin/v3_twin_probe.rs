//! Does a v3 SteVec table hold the documents of its json_tree plaintext twin?
//! Decrypts `PROBE_IDS` (default 20) ids spread over the table and compares
//! each with the twin row of the same id.
//!
//! Usage: v3_twin_probe <small|large> <tier>

use anyhow::{Context, Result};
use sqlx::{postgres::PgPoolOptions, types::Json, Row};
use std::env;

#[tokio::main]
async fn main() -> Result<()> {
    dbbenches::init_tracing();
    let args: Vec<String> = env::args().collect();
    let corpus = args.get(1).context("usage: v3_twin_probe <small|large> <tier>")?;
    let tier = args.get(2).context("usage: v3_twin_probe <small|large> <tier>")?;
    let n: i32 = env::var("PROBE_IDS").unwrap_or_else(|_| "20".into()).parse()?;
    let (table, twin) = dbbenches::twin::tables(corpus, &format!("_{tier}"));

    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&env::var("DATABASE_URL")?)
        .await?;
    let (max,): (i32,) = sqlx::query_as(&format!("SELECT MAX(id) FROM {table}"))
        .fetch_one(&pool)
        .await?;
    let ids: Vec<i32> = (0..n).map(|k| 1 + (k * (max - 1)) / (n - 1).max(1)).collect();
    let cipher = dbbenches::init_scoped_cipher().await?;
    let docs = dbbenches::twin::decrypt_v3_rows(&pool, cipher, &table, &ids).await?;
    let (mut same, mut differ) = (0, 0);
    for (id, doc) in docs {
        let plain: Json<serde_json::Value> =
            sqlx::query(&format!("SELECT plain FROM {twin} WHERE id = $1"))
                .bind(id)
                .fetch_one(&pool)
                .await?
                .get(0);
        if plain.0 == doc {
            same += 1;
        } else {
            differ += 1;
        }
    }
    println!("{table} vs {twin}: {same} same, {differ} differ (of {} probed ids)", ids.len());
    Ok(())
}
