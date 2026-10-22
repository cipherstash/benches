//! SteVec (EQL v3) tables sealed from the json_tree plaintext twins.
//!
//! The JSON comparison (`report/JSON_TREE_VS_STEVEC_VS_PLAINTEXT.md`) measures
//! three ways of storing the same documents: native jsonb, SteVec (v3) and
//! json_tree (v4). The json_tree ingest (json-tree-bench) keeps every
//! document it seals in a plaintext twin table, `json_tree_<corpus>_plain_v4_<N>`,
//! keyed by id. This module seals those same documents, id for id, into
//! `json_ste_vec_<corpus>_encrypted_v3_<N>` through the real v3 path
//! (cipherstash-client encrypt against ZeroKMS, then `from_v2` to the
//! `public.json` domain), so the three systems hold identical rows.
//!
//! The column configuration is `encrypt_ste_vec_small_v3`'s (SteVec,
//! `SteVecMode::Standard`, which the alpha.3 `public.json` domain needs for
//! its `oc` terms) except for the array index mode: `ITEM | WILDCARD`, the
//! mode `@cipherstash/stack` configures for a searchable JSON column
//! (`languages/typescript/packages/stack/src/eql/v3/columns.ts`). The Rust
//! default, `ArrayIndexMode::NONE`, which `encrypt_ste_vec_small_v3` passes,
//! indexes no array element at all, so no array query could be expressed; and
//! `POSITION` makes array containment position-dependent, which jsonb `@>` is
//! not. The flat corpus has no arrays, so the mode changes nothing there.
//! The identifier is `(<target table>, value)`, as in the original ingest.

use crate::{init_scoped_cipher, v3};
use anyhow::{Context, Result};
use cipherstash_client::{
    encryption::Plaintext,
    eql::{decrypt_eql, encrypt_eql, EqlOperation, EqlOutput, Identifier, PreparedPlaintext},
    schema::{
        column::{ArrayIndexMode, Index, IndexType, SteVecMode},
        ColumnConfig, ColumnType,
    },
};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, types::Json, QueryBuilder, Row};
use std::borrow::Cow;
use std::env;
use std::time::Instant;

/// The SteVec column configuration (see the module docs for the array mode).
pub fn ste_vec_column_config() -> ColumnConfig {
    ColumnConfig::build("value")
        .casts_as(ColumnType::Json)
        .add_index(Index::new(ste_vec_index_type()))
}

pub fn ste_vec_index_type() -> IndexType {
    IndexType::SteVec {
        prefix: "value".to_string(),
        term_filters: Default::default(),
        array_index_mode: ArrayIndexMode::ITEM | ArrayIndexMode::WILDCARD,
        mode: SteVecMode::Standard,
    }
}

/// `json_ste_vec_<corpus>_encrypted_v3<suffix>` and its twin
/// `json_tree_<corpus>_plain_v4<suffix>`.
pub fn tables(corpus: &str, suffix: &str) -> (String, String) {
    (
        format!("json_ste_vec_{corpus}_encrypted_v3{suffix}"),
        format!("json_tree_{corpus}_plain_v4{suffix}"),
    )
}

/// Seal `NUM_RECORDS` twin documents into the v3 table, continuing from the
/// table's high-water mark (so a re-run after a ZeroKMS transient resumes
/// where the last committed batch ended — the `prepare:_table` retry loop
/// relies on that). Environment: DATABASE_URL, NUM_RECORDS, BATCH_SIZE,
/// TABLE_SUFFIX, CS_* or the stash profile (read by cipherstash-client).
pub async fn ingest_from_twin(corpus: &str) -> Result<()> {
    let database_url =
        env::var("DATABASE_URL").context("DATABASE_URL environment variable must be set")?;
    let num_records: i64 = env::var("NUM_RECORDS")
        .unwrap_or_else(|_| "10000".to_string())
        .parse()
        .context("NUM_RECORDS must be a valid integer")?;
    let batch_size: i64 = env::var("BATCH_SIZE")
        .unwrap_or_else(|_| "500".to_string())
        .parse()
        .context("BATCH_SIZE must be a valid integer")?;
    let suffix = env::var("TABLE_SUFFIX").unwrap_or_default();
    let (target, twin) = tables(corpus, &suffix);

    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    let cipher = init_scoped_cipher().await?;
    let config = ste_vec_column_config();
    let identifier = Identifier::new(&target, "value");
    let domain = v3::TargetDomain::parse("json").expect("json is a v3 domain");

    let (next_id,): (i32,) =
        sqlx::query_as(&format!("SELECT COALESCE(MAX(id), 0) + 1 FROM {target}"))
            .fetch_one(&pool)
            .await?;
    let (twin_max,): (i32,) = sqlx::query_as(&format!("SELECT COALESCE(MAX(id), 0) FROM {twin}"))
        .fetch_one(&pool)
        .await?;
    let last = (i64::from(next_id) + num_records - 1).min(i64::from(twin_max));
    eprintln!("{target}: sealing twin ids {next_id}..={last} from {twin}");

    let started = Instant::now();
    let mut id = i64::from(next_id);
    let mut sealed_total = 0i64;
    while id <= last {
        let hi = (id + batch_size - 1).min(last);
        let rows = sqlx::query(&format!(
            "SELECT id, plain FROM {twin} WHERE id BETWEEN $1 AND $2 ORDER BY id"
        ))
        .bind(id as i32)
        .bind(hi as i32)
        .fetch_all(&pool)
        .await?;
        let docs: Vec<(i32, Value)> = rows
            .into_iter()
            .map(|r| (r.get::<i32, _>("id"), r.get::<Json<Value>, _>("plain").0))
            .collect();
        anyhow::ensure!(
            docs.len() as i64 == hi - id + 1,
            "twin {twin} is missing ids in {id}..={hi}"
        );

        let prepared = docs
            .iter()
            .map(|(_, doc)| {
                PreparedPlaintext::new(
                    Cow::Borrowed(&config),
                    identifier.clone(),
                    Plaintext::Json(Some(doc.clone())),
                    EqlOperation::Store,
                )
            })
            .collect::<Vec<_>>();
        let out = encrypt_eql(cipher.clone(), prepared, &Default::default()).await?;
        let converted = out
            .into_iter()
            .map(|v| {
                let EqlOutput::Store(ciphertext) = v else {
                    unreachable!("storage batch must yield EqlOutput::Store");
                };
                v3::to_v3_stored_target(&ciphertext, domain)
            })
            .collect::<Result<Vec<_>>>()?;

        QueryBuilder::new(format!("INSERT INTO {target} (id, value) "))
            .push_values(docs.iter().zip(converted), |mut b, ((id, _), v)| {
                b.push_bind(*id).push_bind(Json(v));
            })
            .build()
            .execute(&pool)
            .await?;

        sealed_total += docs.len() as i64;
        if (hi / batch_size) % 10 == 0 || hi == last {
            let secs = started.elapsed().as_secs_f64();
            eprintln!(
                "{target}: through id {hi} ({sealed_total} sealed in {secs:.0}s, {:.1} docs/s)",
                sealed_total as f64 / secs
            );
        }
        id = hi + 1;
    }

    sqlx::query(&format!(
        "SELECT setval(pg_get_serial_sequence('{target}', 'id'), (SELECT MAX(id) FROM {target}))"
    ))
    .execute(&pool)
    .await?;

    let secs = started.elapsed().as_secs_f64();
    let line = json!({
        "table": target,
        "twin": twin,
        "sealed": sealed_total,
        "seconds": secs,
        "batch_size": batch_size,
        "recorded_at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::create_dir_all("results/ingest/v3")?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("results/ingest/v3/twin_ingest_times.jsonl")?;
    writeln!(f, "{line}")?;
    eprintln!("{target}: {sealed_total} sealed in {secs:.1}s");
    Ok(())
}

/// Decrypt `ids` from a v3 table and return the plaintext documents in id
/// order (a v3 SteVec document's record ciphertext is `sv[0].c`).
pub async fn decrypt_v3_rows(
    pool: &sqlx::PgPool,
    cipher: std::sync::Arc<cipherstash_client::encryption::ScopedCipher<cipherstash_client::AutoStrategy>>,
    table: &str,
    ids: &[i32],
) -> Result<Vec<(i32, Value)>> {
    let rows = sqlx::query(&format!(
        "SELECT id, value::jsonb AS v FROM {table} WHERE id = ANY($1) ORDER BY id"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await?;
    let ids: Vec<i32> = rows.iter().map(|r| r.get("id")).collect();
    let cts = rows
        .iter()
        .map(|r| v3::v3_row_to_v2_ciphertext(&r.get::<Json<Value>, _>("v").0))
        .collect::<Result<Vec<_>>>()?;
    let pts = decrypt_eql(cipher, cts, &Default::default()).await?;
    ids.into_iter()
        .zip(pts)
        .map(|(id, pt)| match &pt {
            Plaintext::Json(Some(v)) => Ok((id, v.clone())),
            other => anyhow::bail!("row {id}: expected a JSON plaintext, got {other:?}"),
        })
        .collect()
}

/// Query operands for a twin-sealed v3 table, derived by the client the way
/// a caller derives them: a containment needle (`QueryOp::Default`, then
/// `from_v2_query` to the v3 `{"sv":[…]}` shape) and a path selector
/// (`QueryOp::SteVecSelector`). Index keys are per identifier, and the
/// identifier is the tier table, so an operand is derived per table.
pub struct V3Operands {
    cipher: std::sync::Arc<
        cipherstash_client::encryption::ScopedCipher<cipherstash_client::AutoStrategy>,
    >,
    config: ColumnConfig,
    index_type: IndexType,
    identifier: Identifier,
}

impl V3Operands {
    pub fn new(
        cipher: std::sync::Arc<
            cipherstash_client::encryption::ScopedCipher<cipherstash_client::AutoStrategy>,
        >,
        table: &str,
    ) -> Self {
        Self {
            cipher,
            config: ste_vec_column_config(),
            index_type: ste_vec_index_type(),
            identifier: Identifier::new(table, "value"),
        }
    }

    async fn payload(
        &self,
        pt: Plaintext,
        op: cipherstash_client::encryption::QueryOp,
    ) -> Result<Value> {
        let prepared = PreparedPlaintext::new(
            Cow::Borrowed(&self.config),
            self.identifier.clone(),
            pt,
            EqlOperation::Query(&self.index_type, op),
        );
        let mut out = encrypt_eql(self.cipher.clone(), vec![prepared], &Default::default()).await?;
        let EqlOutput::Query(q) = out.remove(0) else {
            anyhow::bail!("query encryption yielded a storage payload");
        };
        Ok(serde_json::to_value(q)?)
    }

    /// The v3 containment needle for `needle`, as the text the SQL binds.
    pub async fn containment(&self, needle: &Value) -> Result<String> {
        let q = self
            .payload(
                Plaintext::Json(Some(needle.clone())),
                cipherstash_client::encryption::QueryOp::Default,
            )
            .await?;
        // cipherstash-client 0.38.1 serialises the containment vector under
        // `q` (to keep it apart from a stored payload's `sv`), each entry a
        // `[selector, {term}]` pair; eql-bindings 0.4.2's `from_v2_query`
        // reads `sv` entries shaped `{"s": selector, <term>}`. Same content,
        // so reshape it and let `from_v2_query` validate the result.
        let mut q = q;
        if let Some(obj) = q.as_object_mut() {
            if !obj.contains_key("sv") {
                if let Some(Value::Array(entries)) = obj.remove("q") {
                    let entries = entries
                        .into_iter()
                        .map(|e| match e {
                            Value::Array(pair) if pair.len() == 2 => {
                                let mut out = serde_json::Map::new();
                                out.insert("s".to_string(), pair[0].clone());
                                if let Value::Object(term) = &pair[1] {
                                    out.extend(term.clone());
                                }
                                Value::Object(out)
                            }
                            other => other,
                        })
                        .collect();
                    obj.insert("sv".to_string(), Value::Array(entries));
                }
            }
        }
        Ok(v3::to_v3_query_json(&q)
            .with_context(|| format!("converting {}", q.to_string().chars().take(300).collect::<String>()))?
            .to_string())
    }

    /// The selector (hex) of a JSONPath such as `$.orders[*].status`.
    pub async fn selector(&self, path: &str) -> Result<String> {
        let q = self
            .payload(
                Plaintext::Text(Some(path.to_string())),
                cipherstash_client::encryption::QueryOp::SteVecSelector,
            )
            .await?;
        q.get("s")
            .and_then(Value::as_str)
            .map(str::to_string)
            .with_context(|| format!("selector payload for {path} has no `s`: {q}"))
    }
}
