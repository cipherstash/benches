use anyhow::{Context, Result};
use cipherstash_client::{
    encryption::{Plaintext, QueryOp, ScopedCipher},
    eql::{
        decrypt_eql, encrypt_eql, EqlCiphertext, EqlOperation, EqlOutput, EqlQueryPayload,
        Identifier, PreparedPlaintext,
    },
    schema::{column::IndexType, ColumnConfig},
    zerokms::{EnvKeyProvider, FallbackKeyProvider, ZeroKMSBuilder},
    AutoStrategy,
};
use fake::{Dummy, Fake};
use serde_json::json;
use sqlx::{postgres::PgPoolOptions, types::Json, QueryBuilder};

/// Custom sqlx type for the `eql_v2_encrypted` Postgres composite (single
/// `data jsonb` field). Lets bench scenarios SELECT `value` directly without
/// the historic `value::jsonb` cast, which mattered for `ORDER BY value`:
/// the cast was being folded into the sort key by projection-pushdown
/// (`Sort Key: ((value)::jsonb)`), preventing any functional ORE index
/// from satisfying the sort. See `docs/reference/query-performance.md` §4
/// in the EQL repo. Decode walks the composite via `Json<EqlCiphertext>`
/// for the `data` field.
#[derive(Debug, sqlx::Type)]
#[sqlx(type_name = "eql_v2_encrypted")]
pub struct EqlV2Encrypted {
    pub data: Json<EqlCiphertext>,
}

impl EqlV2Encrypted {
    /// Extract the inner ciphertext, dropping the composite wrapper.
    pub fn into_ciphertext(self) -> EqlCiphertext {
        self.data.0
    }
}
pub mod twin;
pub mod v3;

use stack_profile::ProfileStore;
use std::borrow::Cow;
use std::env;
use std::fmt::Debug;
use std::sync::Arc;

/// Generator for low-cardinality categorical strings of the form `CAT_001`
/// .. `CAT_250`, uniform random over 250 distinct values. Used by the
/// `category_encrypted_*` and `category_plaintext_*` tables that drive the
/// realistic-GROUP-BY scenarios in `benches/group_by.rs`. 250 groups was
/// chosen to roughly match the ISO 3166-1 country-code cardinality (~250) —
/// large enough that the hash-aggregate table is interesting, small enough
/// that the result-set emission cost is negligible compared with the
/// per-row HMAC.
pub struct FakeCategory;

impl Dummy<FakeCategory> for String {
    fn dummy_with_rng<R: fake::Rng + ?Sized>(_: &FakeCategory, rng: &mut R) -> String {
        let n: u32 = (1u32..=250u32).fake_with_rng(rng);
        format!("CAT_{:03}", n)
    }
}

/// Sample a single plaintext string from an encrypted table by decrypting
/// the first row. Used by `benches/exact.rs` to derive a search term that's
/// guaranteed to match at least one record — the previous hardcoded
/// `"Bob Johnson"` returned zero rows at every tier because `fake::Name<EN>`
/// doesn't generate that exact combination, so the EXACT bench was secretly
/// measuring "hash-index lookup + nothing found + LIMIT 1 early exit"
/// rather than realistic equality-query cost.
pub async fn sample_plaintext_string(
    pool: &sqlx::PgPool,
    cipher: Arc<ScopedCipher<AutoStrategy>>,
    table_name: &str,
) -> Result<String> {
    // Select the encrypted column directly — the custom `EqlV2Encrypted`
    // Decode impl (defined above) handles the composite-to-EqlCiphertext
    // conversion, no SQL-level cast needed.
    let row: (EqlV2Encrypted,) =
        sqlx::query_as(&format!("SELECT value FROM {} LIMIT 1", table_name))
            .fetch_one(pool)
            .await
            .with_context(|| format!("sample query failed against {}", table_name))?;

    let decrypted = decrypt_eql(cipher, vec![row.0.into_ciphertext()], &Default::default())
        .await
        .context("sample decrypt failed")?;

    // `Plaintext` implements `Drop` (for zeroizing on drop), so we can't
    // move the inner String out via pattern match — borrow + clone instead.
    let pt = decrypted
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("decrypt_eql returned empty Vec for {}", table_name))?;
    match &pt {
        Plaintext::Text(Some(s)) => Ok(s.clone()),
        other => anyhow::bail!("expected Text sample from {}, got {:?}", table_name, other),
    }
}

/// Install a tracing subscriber honouring `RUST_LOG` (defaults to `warn` if
/// unset). Idempotent — calling twice is a no-op. Lets cipherstash-client
/// / zerokms-protocol trace! emissions reach stderr, including the rich
/// internal failure traces in `vitur_client::generate_keys` /
/// `decrypt` / `retrieve_keys`.
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}

pub async fn init_scoped_cipher() -> Result<Arc<ScopedCipher<AutoStrategy>>> {
    // Tuning for the bulk-ingest path. See the "Tuning for bulk ingest"
    // section on `ZeroKMSBuilder` (cipherstash-suite#1960) for the
    // rationale; the short version:
    //
    //   - `connect_timeout(5)`: fast-fail if TCP+TLS to ZeroKMS can't open
    //     in 5 s. Real broken-network signal.
    //   - `request_timeout(60)`: generous total budget. The default (10 s)
    //     trips at scale because cold-pool `generate-data-key` calls can
    //     plausibly exceed it under variable AWS Sydney latency.
    //   - `pool_idle_timeout(600)`: keep warm TLS connections alive across
    //     the bench's idle gaps between batches (default closes after 90
    //     s).
    //   - `max_keys_per_req(100)`: smaller per-request server work →
    //     lower per-call latency → lower chance of any one request
    //     timing out. Trades request count for predictability.
    //   - `max_concurrent_reqs(20)`: more parallelism to compensate for
    //     the smaller batches.
    let zerokms = ZeroKMSBuilder::auto()
        .context("failed to build ZeroKMS client")?
        .with_connect_timeout(5)
        .with_request_timeout(60)
        .with_pool_idle_timeout(600)
        .with_max_keys_per_req(100)
        .with_max_concurrent_reqs(20)
        .with_key_provider(FallbackKeyProvider::new(
            EnvKeyProvider,
            ProfileStore::default(),
        ))
        .build()
        .await
        .context("failed to load client key")?;

    let scoped_cipher = ScopedCipher::init_default(Arc::new(zerokms)).await?;
    Ok(Arc::new(scoped_cipher))
}

pub struct IngestOptions {
    pub bench_name: String,
    pub num_records: i32,
    pub batch_size: usize,
    pub identifier: Identifier,
    pub column_config: ColumnConfig,
    /// When set, every storage payload is converted v2→v3 for this target
    /// domain (via `eql_bindings::from_v2`) before the INSERT — the v3
    /// ingest path. `None` binds the client's v2 ciphertext as-is.
    pub eql_target: Option<v3::TargetDomain>,
}

pub struct IngestOptionsBuilder {
    bench_name: String,
    num_records: Option<i32>,
    batch_size: Option<usize>,
    identifier: Option<Identifier>,
    column_config: Option<ColumnConfig>,
    eql_target: Option<v3::TargetDomain>,
}

impl IngestOptionsBuilder {
    const DEFAULT_BATCH_SIZE: usize = 1000;
    const DEFAULT_NUM_RECORDS: i32 = 100_000;

    pub fn new(bench_name: impl Into<String>) -> Self {
        Self {
            bench_name: bench_name.into(),
            num_records: None,
            batch_size: None,
            identifier: None,
            column_config: None,
            eql_target: None,
        }
    }

    /// Convert every storage payload v2→v3 for `target` before the INSERT
    /// (the v3 ingest path). See [`IngestOptions::eql_target`].
    /// API shape adopted from PR #21 (versioning as configuration on the
    /// single ingest pipeline, typed target resolved at the call site).
    pub fn convert_to_v3(mut self, target: v3::TargetDomain) -> Self {
        self.eql_target = Some(target);
        self
    }

    pub fn num_records(mut self, num_records: i32) -> Self {
        self.num_records = Some(num_records);
        self
    }

    pub fn batch_size(mut self, batch_size: usize) -> Self {
        self.batch_size = Some(batch_size);
        self
    }

    pub fn identifier(mut self, identifier: Identifier) -> Self {
        self.identifier = Some(identifier);
        self
    }

    pub fn column_config(mut self, column_config: ColumnConfig) -> Self {
        self.column_config = Some(column_config);
        self
    }

    pub fn build(self) -> Result<IngestOptions> {
        Ok(IngestOptions {
            bench_name: self.bench_name,
            num_records: self.num_records.unwrap_or(Self::DEFAULT_NUM_RECORDS),
            batch_size: self.batch_size.unwrap_or(Self::DEFAULT_BATCH_SIZE),
            identifier: self.identifier.context("identifier is required")?,
            column_config: self.column_config.context("column_config is required")?,
            eql_target: self.eql_target,
        })
    }
}

impl IngestOptions {
    pub async fn ingest<T, F>(self, f: F) -> Result<()>
    where
        T: Into<Plaintext> + Dummy<F> + Send + Debug,
    {
        let database_url =
            env::var("DATABASE_URL").context("DATABASE_URL environment variable must be set")?;

        let num_records: i32 = env::var("NUM_RECORDS")
            .unwrap_or_else(|_| "10000".to_string())
            .parse()
            .expect("NUM_RECORDS must be a valid integer");

        let hf_iteration: i32 = env::var("HYPERFINE_ITERATION")
            .unwrap_or_else(|_| "0".to_string())
            .parse()
            .expect("HYPERFINE_ITERATION must be a valid integer");

        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await?;

        // Init once and reuse for the binary lifetime. The previous
        // "refresh every 200k rows" loop discarded the warm reqwest
        // connection pool and forced cold-start TLS handshakes — which,
        // with the cipherstash-client default 10s request_timeout,
        // reliably tripped `SendRequest: operation timed out`. Auth
        // tokens auto-refresh through stack-auth's AutoRefresh under the
        // same client; no manual rotation needed.
        let scoped_cipher = init_scoped_cipher().await?;

        let column_config = Cow::Borrowed(&self.column_config);

        for batch_start in (0..self.num_records).step_by(self.batch_size) {
            let batch_end = (batch_start + self.batch_size as i32).min(self.num_records);
            let batch_count = batch_end - batch_start;

            let prepared = (0..batch_count)
                .map(|_| {
                    let x: T = f.fake();

                    PreparedPlaintext::new(
                        // FIXME: take a reference instead of using Cow?
                        column_config.clone(),
                        // FIXME: take a reference instead of owning the identifier
                        self.identifier.clone(),
                        Plaintext::new(x),
                        EqlOperation::Store,
                    )
                })
                .collect::<Vec<_>>();

            let out = encrypt_eql(scoped_cipher.clone(), prepared, &Default::default()).await?;

            match self.eql_target {
                // v2 path: bind the client's storage ciphertext as-is.
                // Every PreparedPlaintext above used EqlOperation::Store, so
                // encrypt_eql yields only EqlOutput::Store. cipherstash-client
                // splits the storage and query payload shapes (since
                // 0.34.1-alpha.9) — unwrap the storage ciphertext for binding.
                None => {
                    QueryBuilder::new(format!(
                        "INSERT INTO {} (value) ",
                        self.identifier.table()
                    ))
                    .push_values(out.into_iter(), |mut b, v| {
                        let EqlOutput::Store(ciphertext) = v else {
                            unreachable!("storage batch must yield EqlOutput::Store");
                        };
                        b.push_bind(Json(ciphertext));
                    })
                    .build()
                    .execute(&pool)
                    .await?;
                }
                // v3 path: convert each storage payload to the target domain
                // before binding. The conversion is deliberately inside the
                // measured path — v3 ingest cost for the intended consumer IS
                // "encrypt + convert + insert". `V3_CONVERT_ONLY=1` skips the
                // INSERT so the conversion overhead is attributable when
                // comparing against the v2 ingest numbers.
                Some(target) => {
                    let converted = out
                        .into_iter()
                        .map(|v| {
                            let EqlOutput::Store(ciphertext) = v else {
                                unreachable!("storage batch must yield EqlOutput::Store");
                            };
                            v3::to_v3_stored_target(&ciphertext, target)
                        })
                        .collect::<Result<Vec<_>>>()?;

                    if env::var("V3_CONVERT_ONLY").is_ok_and(|v| v == "1") {
                        continue;
                    }

                    QueryBuilder::new(format!(
                        "INSERT INTO {} (value) ",
                        self.identifier.table()
                    ))
                    .push_values(converted.into_iter(), |mut b, v| {
                        b.push_bind(Json(v));
                    })
                    .build()
                    .execute(&pool)
                    .await?;
                }
            }
        }

        let result = json!({
            "inserted": num_records
        });
        let filename = format!(
            "target/{}-{num_records}_{hf_iteration}.json",
            self.bench_name
        );
        std::fs::write(&filename, serde_json::to_string(&result)?)?;

        Ok(())
    }
}

#[derive(Debug)]
pub struct WrappedJson(pub serde_json::Value);

impl From<WrappedJson> for Plaintext {
    fn from(WrappedJson(value): WrappedJson) -> Self {
        Plaintext::Json(Some(value))
    }
}

/// Generator config for small JSON payloads (4 flat fields).
pub struct FakeJsonSmall;

impl Dummy<FakeJsonSmall> for WrappedJson {
    fn dummy_with_rng<R: fake::Rng + ?Sized>(_config: &FakeJsonSmall, _: &mut R) -> Self {
        use fake::faker::{internet, name};

        let value = json!({
            "first_name": name::en::FirstName().fake::<String>(),
            "last_name": name::en::LastName().fake::<String>(),
            "age": (18..=99).fake::<i32>(),
            "email": internet::en::FreeEmail().fake::<String>(),
        });
        WrappedJson(value)
    }
}

/// Generator config for large nested JSON payloads (users / company / addresses / orders).
pub struct FakeJsonLarge;

impl Dummy<FakeJsonLarge> for WrappedJson {
    fn dummy_with_rng<R: fake::Rng + ?Sized>(_config: &FakeJsonLarge, _: &mut R) -> Self {
        use fake::faker::{address, chrono, company, internet, name, phone_number};

        let department = [
            "Engineering",
            "Sales",
            "Marketing",
            "HR",
            "Finance",
            "Operations",
        ]
        .iter()
        .take((1..6).fake())
        .next_back()
        .unwrap()
        .to_string();
        let type_ = ["Home", "Work", "Billing", "Shipping"]
            .iter()
            .take((1..4).fake())
            .next_back()
            .unwrap()
            .to_string();
        let status = [
            "Pending",
            "Processing",
            "Shipped",
            "Delivered",
            "Cancelled",
            "Returned",
        ]
        .iter()
        .take((1..6).fake())
        .next_back()
        .unwrap()
        .to_string();
        let relationship = ["Spouse", "Parent", "Sibling", "Friend", "Other"]
            .iter()
            .take((1..5).fake())
            .next_back()
            .unwrap()
            .to_string();

        let value = json!({
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
            "orders": (0..(5..=20).fake::<i32>()).map(|_| {
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
        });

        WrappedJson(value)
    }
}

pub struct EncryptedQueryBuilder {
    pub column_config: ColumnConfig,
    pub identifier: Identifier,
    pub index_type: Option<IndexType>,
    pub statement: Option<String>,
}

impl EncryptedQueryBuilder {
    pub fn new(column_config: ColumnConfig, identifier: Identifier) -> Self {
        Self {
            column_config,
            identifier,
            index_type: None,
            statement: None,
        }
    }

    pub fn index_type(mut self, index_type: IndexType) -> Self {
        self.index_type = Some(index_type);
        self
    }

    pub fn statement(mut self, statement: impl Into<String>) -> Self {
        self.statement = Some(statement.into());
        self
    }

    pub async fn build_query<T>(
        self,
        plaintext: T,
        cipher: Arc<ScopedCipher<AutoStrategy>>,
    ) -> Result<EncryptedQuery>
    where
        T: Into<Plaintext> + Send + Debug,
    {
        let index_type = self
            .index_type
            .context("index_type must be set to build query")?;

        let prepared = PreparedPlaintext::new(
            Cow::Owned(self.column_config),
            self.identifier.clone(),
            plaintext.into(),
            EqlOperation::Query(&index_type, QueryOp::Default),
        );

        let mut out = encrypt_eql(Arc::clone(&cipher), vec![prepared], &Default::default()).await?;

        // build_query uses EqlOperation::Query, so the single output is always
        // EqlOutput::Query. cipherstash-client splits storage / query payloads
        // (since 0.34.1-alpha.9): a query carries an EqlQueryPayload (partial
        // payload, no `c` ciphertext).
        let EqlOutput::Query(eql) = out.remove(0) else {
            unreachable!("build_query encrypts with EqlOperation::Query");
        };

        Ok(EncryptedQuery {
            eql,
            statement: self.statement.context("statement must be set")?,
            scoped_cipher: cipher,
        })
    }
}

pub struct EncryptedQuery {
    pub eql: EqlQueryPayload,
    pub statement: String,
    scoped_cipher: Arc<ScopedCipher<AutoStrategy>>,
}

impl EncryptedQuery {
    pub async fn execute(&self, pool: &sqlx::PgPool) -> Result<Vec<(i32, EqlV2Encrypted)>> {
        let results: Vec<(i32, EqlV2Encrypted)> = sqlx::query_as(&self.statement)
            .bind(Json(&self.eql))
            .fetch_all(pool)
            .await?;

        Ok(results)
    }

    pub async fn execute_and_decrypt<T>(&self, pool: &sqlx::PgPool) -> Result<Vec<T>>
    where
        T: TryFrom<Plaintext>,
        <T as TryFrom<Plaintext>>::Error: Debug,
    {
        let results: Vec<(i32, EqlV2Encrypted)> = self.execute(pool).await?;

        let decrypted = decrypt_eql(
            Arc::clone(&self.scoped_cipher),
            results
                .into_iter()
                .map(|(_, value)| value.into_ciphertext()),
            &Default::default(),
        )
        .await?
        .into_iter()
        .map(|pt| T::try_from(pt).expect("failed to convert plaintext"))
        .collect();

        Ok(decrypted)
    }

    /// Run `EXPLAIN (FORMAT JSON)` on the bound query and return the parsed
    /// plan as `serde_json::Value`. Used by each bench's startup pass to
    /// record what the planner did with the canonical scenario shape — see
    /// `ScenarioMetadata` / `write_metadata_file` for the captured fields.
    pub async fn explain(&self, pool: &sqlx::PgPool) -> Result<serde_json::Value> {
        let explain_sql = format!("EXPLAIN (FORMAT JSON) {}", self.statement);
        let row: (Json<serde_json::Value>,) = sqlx::query_as(&explain_sql)
            .bind(Json(&self.eql))
            .fetch_one(pool)
            .await?;
        Ok(row.0 .0)
    }

    /// Serialise the bound parameter as a JSON value for metadata logging.
    pub fn parameter_json(&self) -> Result<serde_json::Value> {
        Ok(serde_json::to_value(&self.eql)?)
    }
}

// --- Bench metadata sidecar ----------------------------------------------
//
// Each query bench writes a `results/query/<prefix>_metadata_<rows>.json`
// sidecar alongside the criterion-generated `*_rows_*.json` file. The
// sidecar captures, per scenario:
//
//   * `id`             — same string as criterion's benchmark id, so the
//                        two files can be joined by id.
//   * `query`          — the exact SQL the bench ran (template with $1
//                        placeholders intact; placeholders are filled by
//                        sqlx at execute time).
//   * `parameters`     — list of bound values, serialised as JSON. For
//                        the encrypted benches the parameter is the
//                        EqlQueryPayload; for plaintext / json the
//                        list is typically empty.
//   * `explain`        — output of `EXPLAIN (FORMAT JSON)` against the
//                        bound query, captured once at startup before
//                        the criterion loop runs.
//   * `indexes_used`   — flat sorted list of every `Index Name` value
//                        found anywhere in the EXPLAIN tree. Useful for
//                        downstream analysis without re-parsing the
//                        whole plan.

#[derive(serde::Serialize)]
pub struct ScenarioMetadata {
    pub id: String,
    pub query: String,
    pub parameters: Vec<serde_json::Value>,
    pub explain: serde_json::Value,
    pub indexes_used: Vec<String>,
    /// Actual row count returned by a single pre-bench execution of the
    /// query. The cost (one extra round-trip per scenario at startup) is
    /// trivial relative to criterion's warmup phase, and gives us a real
    /// number rather than the planner's estimate from `Plan Rows`.
    pub rows_returned: u64,
}

/// Walk an `EXPLAIN (FORMAT JSON)` tree and collect every `Index Name`.
///
/// PG's plan emits `"Index Name": "<name>"` on Index Scan, Bitmap Index
/// Scan, and Index Only Scan nodes. We don't need to interpret which kind
/// of scan it is here — just surface the names so the report (and a human
/// debugging a slow bench) can see what the planner picked. Deduplicated
/// and sorted for stable output.
pub fn extract_indexes_used(explain: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    collect_index_names(explain, &mut out);
    out.sort();
    out.dedup();
    out
}

fn collect_index_names(node: &serde_json::Value, out: &mut Vec<String>) {
    match node {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(name)) = map.get("Index Name") {
                out.push(name.clone());
            }
            for v in map.values() {
                collect_index_names(v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_index_names(v, out);
            }
        }
        _ => {}
    }
}

/// Diagnostic wrapper for inner-loop bench failures. Prints a structured
/// error block before panicking, including the scenario id, the error's
/// `Display` *and* `Debug` forms (so the anyhow chain shows up), and a
/// hint about the common failure modes we've seen.
///
/// Replaces bare `.unwrap()` / `.expect("...")` in criterion `iter`
/// closures — when a bench fails an hour into a 10M run, the
/// difference between "called unwrap on Err" and "ORE/ore_decrypt/.../1M
/// — ZeroKMS decrypt failure, here's what to do" matters.
pub fn bench_assert<T, E>(result: Result<T, E>, scenario: &str) -> T
where
    E: std::fmt::Display + std::fmt::Debug,
{
    match result {
        Ok(v) => v,
        Err(e) => {
            print_bench_failure(scenario, &e);
            panic!("bench failed in `{}`: {}", scenario, e);
        }
    }
}

fn print_bench_failure<E>(scenario: &str, e: &E)
where
    E: std::fmt::Display + std::fmt::Debug,
{
    let msg = format!("{}", e);
    eprintln!();
    eprintln!("==== BENCH FAILURE ====");
    eprintln!("scenario: {}", scenario);
    eprintln!("error:    {}", e);
    eprintln!("debug:    {:?}", e);

    // The most common failure on long bench runs is the ZeroKMS scoped
    // cipher's TTL expiring mid-iteration. We've hit this several times;
    // the underlying message is the bit after "Could not decrypt data
    // using keyset", which on its own doesn't tell a fresh reader what
    // to do next.
    if msg.contains("Could not decrypt") || msg.contains("keyset") {
        eprintln!();
        eprintln!("hint: ZeroKMS decrypt failure. Most likely the scoped-cipher TTL");
        eprintln!("      expired mid-run — ScopedCipher::init_default binds the");
        eprintln!("      session to a fixed lifetime (~10-15 minutes on ZeroKMS) and");
        eprintln!("      criterion's full sample sweep at 1M/10M can outlive it.");
        eprintln!("      Re-running the bench picks up fresh credentials.");
        eprintln!("      If the failure persists across a fresh run:");
        eprintln!("        1. `mise run truncate` to drop rows that may have been");
        eprintln!("           encrypted under an earlier generation.");
        eprintln!("        2. Re-populate via the relevant `prepare:*` task.");
        eprintln!("        3. Re-run the bench.");
        eprintln!("      If still failing, verify CS_CLIENT_ID / CS_CLIENT_KEY /");
        eprintln!("      CS_WORKSPACE_CRN are valid and check the CipherStash");
        eprintln!("      console for rate limits / quota.");
    } else if msg.contains("Unexpected error") {
        eprintln!();
        eprintln!("hint: opaque cipherstash-client error. The message itself is");
        eprintln!("      unhelpful but the underlying cause is often a ZeroKMS");
        eprintln!("      authentication issue (TTL expiry, rate limit, expired");
        eprintln!("      credential). Same recovery as the decrypt-failure hint:");
        eprintln!("      re-run; if persistent, check the CipherStash console.");
    } else if msg.contains("Connection refused") || msg.contains("Connection reset") {
        eprintln!();
        eprintln!("hint: database connection error. The postgres container may have");
        eprintln!("      stopped — try `mise run postgres` to start it.");
    } else if msg.contains("relation") && msg.contains("does not exist") {
        eprintln!();
        eprintln!("hint: missing table. The schema isn't set up — run `mise run");
        eprintln!("      setup-db` or, for query benches, the `prepare:*` task");
        eprintln!("      that creates the target row-count variant table.");
    } else if msg.contains("query failed") || msg.contains("syntax error") {
        eprintln!();
        eprintln!("hint: SQL execution error. Inspect the recorded query in the");
        eprintln!("      `*_metadata_<rows>.json` sidecar for the scenario above");
        eprintln!("      and try running it against the bench database directly.");
    }
    eprintln!("==== END BENCH FAILURE ====");
    eprintln!();
}

/// Write the scenario metadata sidecar to
/// `results/query/<prefix>_metadata_<rows>.json` (path relative to the
/// bench process's current working directory, which `cargo criterion`
/// sets to the package root).
pub fn write_metadata_file(
    prefix: &str,
    target_rows: &str,
    scenarios: Vec<ScenarioMetadata>,
) -> Result<()> {
    write_metadata_file_in("results/query", prefix, target_rows, scenarios)
}

/// [`write_metadata_file`] with an explicit output directory — the v3
/// benches keep their sidecars under `results/query/v3/` so the committed
/// v2 results stay untouched as the regression baseline.
pub fn write_metadata_file_in(
    dir: &str,
    prefix: &str,
    target_rows: &str,
    scenarios: Vec<ScenarioMetadata>,
) -> Result<()> {
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}_metadata_{}.json", prefix, target_rows));
    let payload = serde_json::json!({
        "target_rows": target_rows,
        "scenarios": scenarios,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&payload)?)?;
    eprintln!("bench metadata written to {}", path.display());
    Ok(())
}
