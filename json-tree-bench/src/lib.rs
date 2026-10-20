//! Shared plumbing for the EQL v4 `json_tree` benches: the cipher and plan
//! the ingest and the query bench derive under, the document generator
//! (the same shape as the root crate's `FakeJsonSmall`), and the result
//! helpers copied from the root `dbbenches` crate so the sidecars match.

use std::path::PathBuf;

use anyhow::Result;
use eql_bindings::Identifier;
use fake::Fake;
use serde_json::{json, Value};
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
