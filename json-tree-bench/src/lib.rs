//! Shared plumbing for the EQL v4 `json_tree` benches: the cipher and plan
//! the ingest and the query bench derive under, the document generator
//! (the same shape as the root crate's `FakeJsonSmall`), and the result
//! helpers copied from the root `dbbenches` crate so the sidecars match.

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};
use eql_bindings::v4::json_tree::JsonTreeDocument;
use eql_bindings::Identifier;
use fake::Fake;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::QueryBuilder;
use stack_encrypt::json::JsonDocument;
use stack_encrypt::kms::FakeDataKeySource;
use stack_encrypt::plan::{Typed, ValuePlan};
use stack_encrypt::{NonEmpty, Plan, StackCipher};

pub type Cipher = StackCipher<FakeDataKeySource>;
pub type JsonPlan = ValuePlan<Value, Typed<JsonDocument>>;

/// The context every document is sealed under, whatever the tier table:
/// one identifier for the family, so terms derived for one tier are the
/// terms of every tier (the `i` a stored document carries is informational).
pub const TABLE: &str = "json_tree_small";
pub const COLUMN: &str = "value";

/// The EQL identifier: the context `encrypt_as::<_, JsonTreeDocument>` takes.
pub fn column() -> NonEmpty<Identifier> {
    Identifier::for_column(TABLE, COLUMN).expect("a valid identifier")
}

/// The engine plan a query derives through. Its context is the identifier
/// rendered `table/column` — the same context the typed document was
/// sealed under, so its selectors are the stored documents' selectors.
pub fn plan() -> JsonPlan {
    Plan::value::<Value>()
        .context(format!("{TABLE}/{COLUMN}"))
        .encrypt_into::<JsonDocument>()
        .build()
        .expect("a valid plan")
}

/// A cipher over the fake in-memory key source. Its index key is
/// deterministic per keyset, so the ingest process and a later bench
/// process derive identical selectors, tokens and ordering terms. Its DATA
/// keys live only in the process that generated them, which is why the
/// bench samples needle plaintexts from the `*_plain_v4` twin tables rather
/// than opening a stored row.
pub async fn cipher() -> Cipher {
    StackCipher::builder()
        .kms(FakeDataKeySource::new())
        .init()
        .await
        .expect("build cipher over the fake key source")
}

/// The small, flat document of the root crate's `FakeJsonSmall` (4 fields),
/// so the json_tree tables hold the same corpus shape as the SteVec tables.
pub fn fake_json_small() -> Value {
    use fake::faker::{internet, name};
    json!({
        "first_name": name::en::FirstName().fake::<String>(),
        "last_name": name::en::LastName().fake::<String>(),
        "age": (18..=99).fake::<i32>(),
        "email": internet::en::FreeEmail().fake::<String>(),
    })
}

/// The nested document of the root crate's `FakeJsonLarge` (user / company /
/// addresses / orders with items): containers at every depth, two arrays
/// of objects, so the token-chain walks and array functions have something
/// to walk. Orders are capped at 5..=10 (the root shape allows 20) to keep
/// a 10k-row tier to a few hundred nodes per document.
pub fn fake_json_large() -> Value {
    use fake::faker::{address, chrono, company, internet, name, phone_number};

    fn pick(options: &[&str]) -> String {
        options[(0..options.len()).fake::<usize>()].to_string()
    }
    let department = pick(&[
        "Engineering",
        "Sales",
        "Marketing",
        "HR",
        "Finance",
        "Operations",
    ]);
    let type_ = pick(&["Home", "Work", "Billing", "Shipping"]);
    let status = pick(&[
        "Pending",
        "Processing",
        "Shipped",
        "Delivered",
        "Cancelled",
        "Returned",
    ]);
    let relationship = pick(&["Spouse", "Parent", "Sibling", "Friend", "Other"]);

    json!({
        "user": {
            "first_name": name::en::FirstName().fake::<String>(),
            "last_name": name::en::LastName().fake::<String>(),
            "age": (18..=99).fake::<i32>(),
            "email": internet::en::FreeEmail().fake::<String>(),
            "username": internet::en::Username().fake::<String>(),
            "contact": {
                "phone": phone_number::en::PhoneNumber().fake::<String>(),
                "mobile": phone_number::en::CellNumber().fake::<String>(),
                "emergency_contact": {
                    "name": name::en::Name().fake::<String>(),
                    "phone": phone_number::en::PhoneNumber().fake::<String>(),
                    "relationship": relationship
                }
            }
        },
        "company": {
            "name": company::en::CompanyName().fake::<String>(),
            "industry": company::en::Industry().fake::<String>(),
            "position": company::en::Profession().fake::<String>(),
            "department": department,
            "salary": (40000..=300000).fake::<i32>(),
            "start_date": chrono::en::Date().fake::<String>()
        },
        "addresses": (0..(1..4).fake::<i32>()).map(|_| {
            json!({
                "type": type_,
                "street": address::en::StreetName().fake::<String>(),
                "city": address::en::CityName().fake::<String>(),
                "state": address::en::StateName().fake::<String>(),
                "zip": address::en::ZipCode().fake::<String>(),
                "country": "United States"
            })
        }).collect::<Vec<_>>(),
        "orders": (0..(5..=10).fake::<i32>()).map(|_| {
            json!({
                "order_id": format!("ORD-{}", (100000..=999999).fake::<i32>()),
                "date": chrono::en::Date().fake::<String>(),
                "total": (10.0..=5000.0).fake::<f64>(),
                "status": status,
                "items": (0..(1..=8).fake::<i32>()).map(|_| {
                    json!({
                        "product": company::en::Buzzword().fake::<String>(),
                        "quantity": (1..=10).fake::<i32>(),
                        "price": (5.0..=500.0).fake::<f64>()
                    })
                }).collect::<Vec<_>>()
            })
        }).collect::<Vec<_>>()
    })
}

/// The ingest loop both binaries run: `NUM_RECORDS` documents from `generate`,
/// sealed in `BATCH_SIZE` batches, into `json_tree_<corpus>_encrypted_v4<suffix>`
/// with the plaintext twin into `json_tree_<corpus>_plain_v4<suffix>` under the
/// same id.
///
/// Sealing is under `FakeDataKeySource`, in-process, with one batched
/// `generate_keys` per batch — no ZeroKMS round-trip. So the hyperfine
/// numbers for these binaries measure "derive terms + AEAD + INSERT" and are
/// NOT comparable to the v3 `encrypt_ste_vec_small_v3` ingest, which pays
/// the KMS call per batch. They are comparable across tiers and corpora, and
/// the index-build time `prepare:_table` logs is comparable to v3's.
///
/// Environment variables: DATABASE_URL, NUM_RECORDS, BATCH_SIZE,
/// TABLE_SUFFIX, HYPERFINE_ITERATION — the root crate's ingest contract, so
/// `prepare:_table` and `bench:ingest:_run` drive these binaries unchanged.
pub async fn ingest(bench_name: &str, corpus: &str, generate: fn() -> Value) -> Result<()> {
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
    let encrypted = format!("json_tree_{corpus}_encrypted_v4{suffix}");
    let plain = format!("json_tree_{corpus}_plain_v4{suffix}");

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
        let docs: Vec<Value> = (0..count).map(|_| generate()).collect();

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
        format!("target/{bench_name}-{num_records}_{hf_iteration}.json"),
        serde_json::to_string(&result)?,
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Result helpers — copies of the root crate's (src/lib.rs), kept in step so
// the sidecars under results/query/v4/ read like those under v3/.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
pub struct ScenarioMetadata {
    pub id: String,
    pub query: String,
    pub parameters: Vec<Value>,
    pub explain: Value,
    pub indexes_used: Vec<String>,
    /// Actual row count returned by one pre-bench execution of the query.
    pub rows_returned: u64,
}

/// Walk an `EXPLAIN (FORMAT JSON)` tree and collect every `Index Name`,
/// deduplicated and sorted.
pub fn extract_indexes_used(explain: &Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_index_names(explain, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_index_names(node: &Value, out: &mut Vec<String>) {
    match node {
        Value::Object(map) => {
            if let Some(Value::String(name)) = map.get("Index Name") {
                out.push(name.clone());
            }
            for v in map.values() {
                collect_index_names(v, out);
            }
        }
        Value::Array(arr) => {
            for v in arr {
                collect_index_names(v, out);
            }
        }
        _ => {}
    }
}

/// Diagnostic wrapper for inner-loop bench failures: names the scenario
/// before panicking, so a failure deep in a run says which query died.
pub fn bench_assert<T, E>(result: std::result::Result<T, E>, scenario: &str) -> T
where
    E: std::fmt::Display + std::fmt::Debug,
{
    match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!();
            eprintln!("==== BENCH FAILURE ====");
            eprintln!("scenario: {scenario}");
            eprintln!("error:    {e}");
            eprintln!("debug:    {e:?}");
            eprintln!("=======================");
            panic!("bench failed in `{scenario}`: {e}");
        }
    }
}

/// Write the scenario metadata sidecar to
/// `<dir>/<prefix>_metadata_<rows>.json`. `cargo criterion` runs the bench
/// with this crate's directory as the working directory, so the default
/// `dir` is `../results/query/v4` (see `RESULTS_DIR` in the bench).
pub fn write_metadata_file_in(
    dir: &str,
    prefix: &str,
    target_rows: &str,
    scenarios: Vec<ScenarioMetadata>,
) -> Result<()> {
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{prefix}_metadata_{target_rows}.json"));
    let payload = json!({
        "target_rows": target_rows,
        "scenarios": scenarios,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&payload)?)?;
    eprintln!("bench metadata written to {}", path.display());
    Ok(())
}
