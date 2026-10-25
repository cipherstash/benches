//! Shared plumbing for the EQL v4 `json_tree` benches: the cipher and plan
//! the ingest and the query bench derive under, the document generator
//! (the same shape as the root crate's `FakeJsonSmall`), and the result
//! helpers copied from the root `dbbenches` crate so the sidecars match.
//!
//! The crate builds against one of two stored formats, chosen by feature
//! (see Cargo.toml): `derivable` (default; cipherstash/stack#1363) or
//! `gates` (its base). Each format's `stack-encrypt` and `eql-bindings` are
//! re-exported here under their usual names, and every target imports them
//! from this crate (`use json_tree_bench::{eql_bindings, stack_encrypt};`),
//! so the code reads the same under either. What differs between the two
//! is confined to [`ReadKey`]: how a subtree read is derived and called.

#[cfg(any(
    all(feature = "derivable", feature = "gates"),
    all(feature = "derivable", feature = "merged"),
    all(feature = "gates", feature = "merged"),
))]
compile_error!("features `derivable`, `gates` and `merged` are exclusive: build `gates` or `merged` with --no-default-features");
#[cfg(not(any(feature = "derivable", feature = "gates", feature = "merged")))]
compile_error!("enable one of the features `derivable` (default), `gates` or `merged`");

#[cfg(feature = "derivable")]
pub extern crate eql_bindings_derivable as eql_bindings;
#[cfg(feature = "derivable")]
pub extern crate stack_encrypt_derivable as stack_encrypt;
#[cfg(feature = "gates")]
pub extern crate eql_bindings_gates as eql_bindings;
#[cfg(feature = "gates")]
pub extern crate stack_encrypt_gates as stack_encrypt;
#[cfg(feature = "merged")]
pub extern crate eql_bindings_merged as eql_bindings;
#[cfg(feature = "merged")]
pub extern crate stack_encrypt_merged as stack_encrypt;

/// The stored format this build reads and writes: `"derivable"`, `"gates"`
/// or `"merged"`. `merged` is the derivable format as it merged to `next`
/// (cipherstash/stack ebdb5de0a): the same stored bytes and tokens as
/// `derivable`, with the SQL of f94a6eb11 (inlined chain primitives, one
/// read index) and the review fixes after it.
pub const FORMAT: &str = if cfg!(feature = "gates") {
    "gates"
} else if cfg!(feature = "merged") {
    "merged"
} else {
    "derivable"
};

use std::env;
use std::time::{Duration, Instant};
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
    /// The nested corpus sealed with the root OPEN (no sealed root): tables
    /// `json_tree_large_open_*_v4_<N>`, the same documents as `Nested` id
    /// for id. Only for the read of `$` with the root open.
    NestedOpen,
    /// [`fake_json_wide`]: one object of [`WIDE`] keys and one array of
    /// [`WIDE`] elements, for the cost of enumerating a wide node. Tables
    /// `json_tree_wide_*_v4_<N>`.
    Wide,
}

/// The fan-out of the wide corpus's object and array.
pub const WIDE: usize = 300;

impl Corpus {
    /// The paths the corpus's index orders.
    pub fn ordered(self) -> &'static [&'static str] {
        match self {
            Corpus::Flat => &["$.age"],
            Corpus::Nested | Corpus::NestedOpen => &["$.company.salary"],
            Corpus::Wide => &[],
        }
    }

    /// The table-name stem: `json_tree_<stem>_{encrypted,plain}_v4_<N>`.
    pub fn stem(self) -> &'static str {
        match self {
            Corpus::Flat => "small",
            Corpus::Nested => "large",
            Corpus::NestedOpen => "large_open",
            Corpus::Wide => "wide",
        }
    }

    /// The JSON index options the corpus is sealed and queried under.
    pub fn options(self) -> JsonOptions {
        let mut options = JsonOptions::default()
            .ordering(self.ordered().iter().copied())
            .expect("valid ordered paths");
        options.sealed_root = self != Corpus::NestedOpen;
        options
    }

    /// The engine plan a query derives through, laid out under the corpus's
    /// options, so an ordering query resolves through the same index the
    /// documents were sealed under. Its context is the identifier rendered
    /// `table/column` — the same context the typed document was sealed
    /// under, so its selectors are the stored documents' selectors.
    pub fn plan(self) -> JsonPlan {
        Plan::value::<Value>()
            .context(format!("{TABLE}/{COLUMN}"))
            .encrypt_with_layout(JsonIndex::new(self.options()))
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

/// The wide document: `attrs`, an object of [`WIDE`] members (`k000` …),
/// and `list`, an array of [`WIDE`] numbers. Enumerating either is one
/// level of [`WIDE`] hits, members in one and elements in the other, which
/// is what separates the member rule's self-tag test from the element
/// rule's fallback in the derivable format's walk.
pub fn fake_json_wide() -> Value {
    let attrs: serde_json::Map<String, Value> = (0..WIDE)
        .map(|k| (format!("k{k:03}"), json!((0..1_000_000).fake::<i32>())))
        .collect();
    let list: Vec<Value> = (0..WIDE).map(|_| json!((0..1_000_000).fake::<i32>())).collect();
    json!({ "attrs": attrs, "list": list })
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

    // Time spent sealing (client, in-process key source) and inserting
    // (the INSERTs and their commit), reported separately.
    let mut seal_time = Duration::ZERO;
    let mut insert_time = Duration::ZERO;
    let started = Instant::now();

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
        let seal_start = Instant::now();
        let mut sealed: Vec<JsonTreeDocument> = Vec::with_capacity(docs.len());
        for doc in &docs {
            sealed.push(
                keyset
                    .run(layout.encryption(), doc, column())
                    .await
                    .context("seal document")?,
            );
        }
        seal_time += seal_start.elapsed();

        let insert_start = Instant::now();
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
        insert_time += insert_start.elapsed();

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
    let result = json!({
        "inserted": num_records,
        "format": FORMAT,
        "table": encrypted,
        "seal_seconds": seal_time.as_secs_f64(),
        "insert_seconds": insert_time.as_secs_f64(),
        "total_seconds": started.elapsed().as_secs_f64(),
    });
    eprintln!("ingest {encrypted} ({FORMAT}): {result}");
    if let Ok(log) = env::var("INGEST_LOG") {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
        writeln!(f, "{result}")?;
    }
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

// ---------------------------------------------------------------------------
// Reads, by format
// ---------------------------------------------------------------------------

/// A bound SQL parameter, as the benches and the check bind them.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum Arg {
    Text(String),
    Jsonb(Value),
    Int(i32),
    TextArray(Vec<String>),
    IntArray(Vec<i32>),
}

impl Arg {
    /// The parameter as JSON, for the metadata sidecars and plan files.
    pub fn to_json(&self) -> Value {
        match self {
            Arg::Text(t) => Value::String(t.clone()),
            Arg::Jsonb(v) => v.clone(),
            Arg::Int(i) => Value::from(*i),
            Arg::TextArray(v) => Value::from(v.clone()),
            Arg::IntArray(v) => Value::from(v.clone()),
        }
    }

    /// The bytes of query material the parameter carries: the characters of
    /// a text, the sum of an array's elements' characters.
    pub fn text_bytes(&self) -> usize {
        match self {
            Arg::Text(t) => t.len(),
            Arg::TextArray(v) => v.iter().map(String::len).sum(),
            Arg::Jsonb(v) => v.to_string().len(),
            Arg::Int(_) => 4,
            Arg::IntArray(v) => 4 * v.len(),
        }
    }
}

/// Bind `args` to `sql` in order.
pub fn query_with<'q>(
    sql: &'q str,
    args: &'q [Arg],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    args.iter().fold(sqlx::query(sql), |q, a| match a {
        Arg::Text(t) => q.bind(t),
        Arg::Jsonb(v) => q.bind(Json(v)),
        Arg::Int(i) => q.bind(*i),
        Arg::TextArray(v) => q.bind(v),
        Arg::IntArray(v) => q.bind(v),
    })
}

pub use stack_encrypt::json::JsonSubtreeKey;

/// A subtree read's key and how this build's format calls it. The one
/// place the two formats differ for the benches:
///
/// - `gates`: the key is the deepest derivable token (a chain head, or an
///   array position just below one) and one gate per step below it; the read
///   is `json_tree_subtree_at(value, token, gates[, budget])`, and with no
///   gates it returns the token's node's descendants, not the node (#1356).
///   A wildcard is one gate; `bound` is ignored.
/// - `derivable`: the key is one token per concrete path (one for a path
///   with no wildcard; `bound` per wildcard level plus a probe), and the read
///   is `json_tree_subtree(value, tokens[, budget])`, which returns each
///   token's node at `d = 0` with its descendants.
pub struct ReadKey {
    pub path: String,
    pub key: JsonSubtreeKey,
}

impl ReadKey {
    /// Derive the key for `path` under `options`, a wildcard expanded to
    /// `bound` (the derivable format's default, 64, when `None`).
    pub async fn derive(cipher: &Cipher, path: &str, options: &JsonOptions, bound: Option<u32>) -> Self {
        let parsed = stack_encrypt::json::JsonPath::parse(path).expect("a valid path");
        let keyset = cipher.default_keyset();
        #[cfg(feature = "gates")]
        let key = {
            let _ = bound;
            keyset.json_subtree_key(&parsed, options, column()).await
        };
        #[cfg(not(feature = "gates"))]
        let key = keyset
            .json_subtree_key_with_bound(
                &parsed,
                options,
                bound.unwrap_or(JsonSubtreeKey::DEFAULT_BOUND),
                column(),
            )
            .await;
        ReadKey {
            path: path.to_string(),
            key: key.unwrap_or_else(|e| panic!("derive subtree key for {path}: {e}")),
        }
    }

    /// The read in a FROM clause, `eql_v4.json_tree_subtree…(value, …) e`,
    /// its parameters numbered from `$1`, and the parameters. `budget`
    /// `None` is the function's default (64).
    pub fn read(&self, budget: Option<i32>) -> (String, Vec<Arg>) {
        let b = budget.map(|b| format!(", {b}")).unwrap_or_default();
        #[cfg(feature = "gates")]
        return (
            format!("eql_v4.json_tree_subtree_at(value, $1::text, $2::text[]{b}) e"),
            vec![Arg::Text(self.key.token()), Arg::TextArray(self.key.gates())],
        );
        #[cfg(not(feature = "gates"))]
        return (
            format!("eql_v4.json_tree_subtree(value, $1::text[]{b}) e"),
            vec![Arg::TextArray(self.key.tokens())],
        );
    }

    /// `json_tree_children` of the key's node, as [`read`](Self::read).
    pub fn children(&self) -> (String, Vec<Arg>) {
        #[cfg(feature = "gates")]
        return (
            "eql_v4.json_tree_children(value, $1::text, $2::text[]) e".to_string(),
            vec![Arg::Text(self.key.token()), Arg::TextArray(self.key.gates())],
        );
        #[cfg(not(feature = "gates"))]
        return (
            "eql_v4.json_tree_children(value, $1::text) e".to_string(),
            vec![Arg::Text(self.single_token())],
        );
    }

    /// `json_tree_child_count` of the key's node: an expression, not a FROM
    /// item, and its parameters.
    pub fn child_count(&self) -> (String, Vec<Arg>) {
        #[cfg(feature = "gates")]
        return (
            "eql_v4.json_tree_child_count(value, $1::text, $2::text[])".to_string(),
            vec![Arg::Text(self.key.token()), Arg::TextArray(self.key.gates())],
        );
        #[cfg(not(feature = "gates"))]
        return (
            "eql_v4.json_tree_child_count(value, $1::text)".to_string(),
            vec![Arg::Text(self.single_token())],
        );
    }

    #[cfg(not(feature = "gates"))]
    fn single_token(&self) -> String {
        let tokens = self.key.tokens();
        assert_eq!(tokens.len(), 1, "{}: a wildcard-free path sends one token", self.path);
        tokens.into_iter().next().unwrap()
    }

    /// How many tokens and gates the read sends.
    pub fn sent(&self) -> (usize, usize) {
        #[cfg(feature = "gates")]
        return (1, self.key.gates().len());
        #[cfg(not(feature = "gates"))]
        return (self.key.tokens().len(), 0);
    }
}
