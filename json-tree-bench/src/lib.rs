//! Shared plumbing for the EQL v4 `json_tree` benches: the cipher and plan
//! the ingest and the query bench derive under, the document generator
//! (the same shape as the root crate's `FakeJsonSmall`), and the result
//! helpers copied from the root `dbbenches` crate so the sidecars match.

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};
use eql_bindings::v4::json_tree::{JsonTree, JsonTreeDocument};
use eql_bindings::Identifier;
use fake::Fake;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::QueryBuilder;
use stack_encrypt::json::{JsonDocument, JsonOptions};
use stack_encrypt::kms::DeterministicSource;
use stack_encrypt::plan::{Laid, ValuePlan};
use stack_encrypt::target::{Json as JsonIndex, Layout};
use stack_encrypt::{NonEmpty, Plan, StackCipher};

pub type Cipher = StackCipher<DeterministicSource>;
pub type JsonPlan = ValuePlan<Value, Laid<JsonIndex, JsonDocument>>;

/// The two corpora, each sealed and queried under its own index options.
/// The leaf ordering term `op` is opt-in per path, so each corpus orders
/// only the path its scenarios order or range-compare on: `$.age` for the
/// flat corpus, `$.company.salary` for the nested one. Nothing else carries
/// `op`; an ordering query anywhere else is refused by the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corpus {
    /// `FakeJsonSmall`: tables `json_tree_small_*_v4_<N>`.
    Flat,
    /// `FakeJsonLarge`: tables `json_tree_large_*_v4_<N>`.
    Nested,
}

impl Corpus {
    /// The paths the corpus's index orders.
    pub fn ordered(self) -> &'static [&'static str] {
        match self {
            Corpus::Flat => &["$.age"],
            Corpus::Nested => &["$.company.salary"],
        }
    }

    /// The table-name stem: `json_tree_<stem>_{encrypted,plain}_v4_<N>`.
    pub fn stem(self) -> &'static str {
        match self {
            Corpus::Flat => "small",
            Corpus::Nested => "large",
        }
    }

    /// The JSON index options the corpus is sealed and queried under.
    pub fn options(self) -> JsonOptions {
        JsonOptions::default()
            .ordering(self.ordered().iter().copied())
            .expect("valid ordered paths")
    }

    /// The engine plan a query derives through, laid out under the corpus's
    /// options, so an ordering query resolves through the same index the
    /// documents were sealed under. Its context is the identifier rendered
    /// `table/column` — the same context the typed document was sealed
    /// under, so its selectors are the stored documents' selectors.
    pub fn plan(self) -> JsonPlan {
        Plan::value::<Value>()
            .context(format!("{TABLE}/{COLUMN}"))
            .encrypt_as(JsonIndex::new(self.options()))
            .build()
            .expect("a valid plan")
    }

    /// The EQL layout the ingest seals a document with.
    pub fn layout(self) -> JsonTree {
        JsonTree::new(self.options())
    }
}

/// The context every document is sealed under, whatever the tier table:
/// one identifier for the family, so terms derived for one tier are the
/// terms of every tier (the `i` a stored document carries is informational).
pub const TABLE: &str = "json_tree_small";
pub const COLUMN: &str = "value";

/// The EQL identifier: the context `encrypt_as::<_, JsonTreeDocument>` takes.
pub fn column() -> NonEmpty<Identifier> {
    Identifier::for_column(TABLE, COLUMN).expect("a valid identifier")
}

/// The seed every bench process derives its data keys from. Fixed, so a
/// document sealed by the ingest opens in a later process (the correctness
/// check decrypts what the walks return). Test infrastructure only.
const SEED: [u8; 32] = *b"json-tree-bench::seed::v1\0\0\0\0\0\0\0";

/// A cipher over stack-kms's deterministic test source. Its index key is
/// `FakeDataKeySource`'s, deterministic per keyset, so the ingest process and
/// a later bench process derive identical selectors, tokens and ordering
/// terms. Its DATA keys are a function of the seed, the descriptor and the
/// IV, so a later process built from the same seed opens a stored row too.
/// (Until the ordinal-chain measurement the ingest sealed under
/// `FakeDataKeySource`, whose data keys lived only in the ingest process;
/// the terms, and so every measured query, are the same under either.)
pub async fn cipher() -> Cipher {
    StackCipher::builder()
        .kms(DeterministicSource::new(SEED))
        .init()
        .await
        .expect("build cipher over the deterministic key source")
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
/// same id. Where the twin already holds a document for an id (a re-ingest of
/// the encrypted table alone), that document is resealed instead of a new one
/// generated.
///
/// Sealing is under `DeterministicSource`, in-process, with one batched
/// `generate_keys` per batch — no ZeroKMS round-trip. So the hyperfine
/// numbers for these binaries measure "derive terms + AEAD + INSERT" and are
/// NOT comparable to the v3 `encrypt_ste_vec_small_v3` ingest, which pays
/// the KMS call per batch. They are comparable across tiers and corpora, and
/// the index-build time `prepare:_table` logs is comparable to v3's.
///
/// Environment variables: DATABASE_URL, NUM_RECORDS, BATCH_SIZE,
/// TABLE_SUFFIX, HYPERFINE_ITERATION — the root crate's ingest contract, so
/// `prepare:_table` and `bench:ingest:_run` drive these binaries unchanged.
pub async fn ingest(bench_name: &str, corpus: Corpus, generate: fn() -> Value) -> Result<()> {
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
    let stem = corpus.stem();
    let encrypted = format!("json_tree_{stem}_encrypted_v4{suffix}");
    let plain = format!("json_tree_{stem}_plain_v4{suffix}");
    let layout = corpus.layout();

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
        let ids: Vec<i64> = (id..id + count as i64).collect();

        // A plaintext twin that already holds a document for an id is
        // resealed rather than replaced, so a re-ingest after a wire change
        // measures the new format over the same corpus as the run before it.
        // Only the ids with no twin get a generated document (and a twin).
        let id32: Vec<i32> = ids.iter().map(|&i| i as i32).collect();
        let existing: std::collections::HashMap<i32, Value> =
            sqlx::query_as::<_, (i32, Json<Value>)>(&format!(
                "SELECT id, plain FROM {plain} WHERE id = ANY($1)"
            ))
            .bind(&id32)
            .fetch_all(&pool)
            .await?
            .into_iter()
            .map(|(i, Json(v))| (i, v))
            .collect();
        let docs: Vec<Value> = id32
            .iter()
            .map(|i| existing.get(i).cloned().unwrap_or_else(generate))
            .collect();

        // Sealed one document at a time under the corpus's layout (its
        // ordered paths): the layout has no `Vec` form, so each document
        // is one `generate_keys` call. Ingest speed is not what these
        // benches measure, and the key source is in-process.
        let mut sealed: Vec<JsonTreeDocument> = Vec::with_capacity(docs.len());
        for doc in &docs {
            sealed.push(
                keyset
                    .run(layout.encryption(), doc, column())
                    .await
                    .context("seal document")?,
            );
        }

        let mut tx = pool.begin().await?;
        QueryBuilder::new(format!("INSERT INTO {encrypted} (id, value) "))
            .push_values(ids.iter().zip(&sealed), |mut b, (id, doc)| {
                b.push_bind(*id as i32)
                    .push_bind(Json(serde_json::to_value(doc).expect("serialise document")));
            })
            .build()
            .execute(&mut *tx)
            .await?;
        let fresh: Vec<(i32, &Value)> = id32
            .iter()
            .zip(&docs)
            .filter(|(i, _)| !existing.contains_key(i))
            .map(|(i, d)| (*i, d))
            .collect();
        if !fresh.is_empty() {
            QueryBuilder::new(format!("INSERT INTO {plain} (id, plain) "))
                .push_values(fresh, |mut b, (id, doc)| {
                    b.push_bind(id).push_bind(Json(doc.clone()));
                })
                .build()
                .execute(&mut *tx)
                .await?;
        }
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
