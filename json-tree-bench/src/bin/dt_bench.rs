//! The derivable-tokens comparison (cipherstash/stack#1363 against its base):
//! the same reads, over the same documents, in the two stored formats, timed
//! interleaved from one process. Report: `report/JSON_TREE_DERIVABLE_TOKENS.md`.
//!
//! Two steps, because each format's terms come from a different build of
//! stack-encrypt and the timing must not:
//!
//! `dt_bench plan`  (format-specific; run once per build, against its own
//!                   database): derives every scenario's SQL and operands
//!                   (tokens, gates, selectors, containment needles) and
//!                   writes `$OUT_DIR/plan_<format>_<rows>.json`, with the row
//!                   ids every scenario samples.
//! `dt_bench time`  (format-neutral; either build): reads both plans,
//!                   connects to both databases (`GATES_URL`, `DERIVABLE_URL`)
//!                   and, for each pass and each scenario, runs both formats
//!                   back to back, alternating which goes first, then writes
//!                   `$OUT_DIR/time_<rows><TAG>.json`.
//!
//! Per scenario and format, `time` records:
//! - `calls`: client wall time of one prepared-statement call, by id, for
//!   each of the sampled ids, every pass (or of the whole query, `REPS` times
//!   a pass, for a scenario with no id), in microseconds. Includes the local
//!   round trip; the `noop/by_id` scenario measures that floor.
//! - `pass_us_per_doc`: one query applying the scenario to every id in the
//!   pass set (`id = ANY($n)`) and summing its output, divided by the number
//!   of documents: the in-server cost per row, with one round trip in total.
//! - `rows_per_doc`, `bytes_per_doc`: rows and characters of `e::jsonb::text`
//!   the scenario returns per document, from the pass query.
//! - `explain`: `EXPLAIN (ANALYZE, BUFFERS)` of one call on the
//!   representative row.

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use json_tree_bench::{query_with, Arg, FORMAT};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Scenario {
    name: String,
    group: String,
    table: String,
    /// One call: by id (`$<last>` is the id) when `by_id`, else the whole query.
    sql: String,
    args: Vec<Arg>,
    by_id: bool,
    /// For a by-id scenario: `count(*)` and `sum(length(output))` over
    /// `id = ANY($<last>::int[])`.
    pass_sql: Option<String>,
    /// Tokens and gates the read sends, and the characters of query material
    /// in its parameters (tokens, gates, selectors, operands).
    tokens: usize,
    gates: usize,
    param_bytes: usize,
    note: String,
}

#[derive(Serialize, Deserialize)]
struct Plan {
    format: String,
    rows: String,
    /// Per table: the ids timed one call each, the representative id, and the
    /// ids of the pass query.
    sample_ids: BTreeMap<String, Vec<i32>>,
    representative: BTreeMap<String, i32>,
    pass_ids: BTreeMap<String, Vec<i32>>,
    scenarios: Vec<Scenario>,
}

fn env(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

async fn connect(url: &str) -> Result<PgPool> {
    Ok(PgPoolOptions::new().max_connections(1).connect(url).await?)
}

#[tokio::main]
async fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("plan") => plan::run().await,
        Some("time") => time().await,
        other => bail!("usage: dt_bench plan|time (got {other:?})"),
    }
}

mod plan {
    use super::*;
    use json_tree_bench::eql_bindings::v4::json_tree::JsonTreeQuery;
    use json_tree_bench::stack_encrypt::json::JsonPath;
    use json_tree_bench::stack_encrypt::op;
    use json_tree_bench::{cipher, column, Cipher, Corpus, ReadKey};

    const CONTAINS_CAST: &str = "::text::jsonb::eql_v4.query_json_tree";

    struct Ctx<'a> {
        cipher: &'a Cipher,
        out: Vec<Scenario>,
    }

    impl Ctx<'_> {
        /// A subtree read of `path` on `table`, by id.
        #[allow(clippy::too_many_arguments)]
        async fn read(
            &mut self,
            name: &str,
            group: &str,
            table: &str,
            corpus: Corpus,
            path: &str,
            bound: Option<u32>,
            budget: Option<i32>,
            note: &str,
        ) {
            let key = ReadKey::derive(self.cipher, path, &corpus.options(), bound).await;
            let (from, args) = key.read(budget);
            let (tokens, gates) = key.sent();
            self.push_from(name, group, table, &from, args, tokens, gates, note);
        }

        #[allow(clippy::too_many_arguments)]
        fn push_from(
            &mut self,
            name: &str,
            group: &str,
            table: &str,
            from: &str,
            args: Vec<Arg>,
            tokens: usize,
            gates: usize,
            note: &str,
        ) {
            let n = args.len() + 1;
            self.out.push(Scenario {
                name: name.into(),
                group: group.into(),
                table: table.into(),
                sql: format!("SELECT e::jsonb::text FROM {table}, {from} WHERE id = ${n}"),
                pass_sql: Some(format!(
                    "SELECT count(*)::int8, COALESCE(sum(length(e::jsonb::text)), 0)::int8 \
                     FROM {table}, {from} WHERE id = ANY(${n}::int[])"
                )),
                param_bytes: args.iter().map(Arg::text_bytes).sum(),
                args,
                by_id: true,
                tokens,
                gates,
                note: note.into(),
            });
        }

        /// A by-id scalar expression over `value` (one text column).
        #[allow(clippy::too_many_arguments)]
        fn push_expr(
            &mut self,
            name: &str,
            group: &str,
            table: &str,
            expr: &str,
            args: Vec<Arg>,
            tokens: usize,
            gates: usize,
            note: &str,
        ) {
            let n = args.len() + 1;
            self.out.push(Scenario {
                name: name.into(),
                group: group.into(),
                table: table.into(),
                sql: format!("SELECT ({expr})::text FROM {table} WHERE id = ${n}"),
                pass_sql: Some(format!(
                    "SELECT count(*)::int8, COALESCE(sum(length(({expr})::text)), 0)::int8 \
                     FROM {table} WHERE id = ANY(${n}::int[])"
                )),
                param_bytes: args.iter().map(Arg::text_bytes).sum(),
                args,
                by_id: true,
                tokens,
                gates,
                note: note.into(),
            });
        }

        async fn selector(&self, path: &str) -> String {
            self.cipher
                .default_keyset()
                .json_selector(&JsonPath::parse(path).unwrap(), column())
                .await
                .expect("derive selector")
                .to_string()
        }

        async fn contains(&self, corpus: Corpus, needle: &Value) -> String {
            let q = self
                .cipher
                .query(op::Contains, needle)
                .on(&corpus.plan())
                .await
                .expect("derive containment operand");
            serde_json::to_string(&JsonTreeQuery::try_from(&q).expect("typed operand")).unwrap()
        }
    }

    async fn exists(pool: &PgPool, table: &str) -> Result<bool> {
        Ok(sqlx::query("SELECT to_regclass($1) IS NOT NULL")
            .bind(table)
            .fetch_one(pool)
            .await?
            .get(0))
    }

    pub async fn run() -> Result<()> {
        let url = std::env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
        let rows = env("TARGET_ROWS", "10000");
        let wide_rows = env("WIDE_ROWS", "1000");
        let out_dir = env("OUT_DIR", "../results/query/derivable");
        let samples: i64 = env("SAMPLE_IDS", "200").parse()?;
        let pool = connect(&url).await?;
        let cipher = cipher().await;
        let mut cx = Ctx {
            cipher: &cipher,
            out: Vec::new(),
        };

        let nested = format!("json_tree_large_encrypted_v4_{rows}");
        let flat = format!("json_tree_small_encrypted_v4_{rows}");
        let open = format!("json_tree_large_open_encrypted_v4_{rows}");
        let wide = format!("json_tree_wide_encrypted_v4_{wide_rows}");
        let mut tables = vec![
            (nested.clone(), format!("json_tree_large_plain_v4_{rows}")),
            (flat.clone(), format!("json_tree_small_plain_v4_{rows}")),
        ];
        let has_open = exists(&pool, &open).await?;
        let has_wide = exists(&pool, &wide).await?;
        if has_open {
            tables.push((
                open.clone(),
                format!("json_tree_large_open_plain_v4_{rows}"),
            ));
        }
        if has_wide {
            tables.push((wide.clone(), format!("json_tree_wide_plain_v4_{wide_rows}")));
        }

        let gates = cfg!(feature = "gates");
        let mut sample_ids = BTreeMap::new();
        let mut representative = BTreeMap::new();
        let mut pass_ids = BTreeMap::new();
        for (table, plain) in &tables {
            let ids: Vec<i32> = sqlx::query(&format!(
                "SELECT id FROM {plain} ORDER BY md5(id::text || 'derivable-tokens') LIMIT $1"
            ))
            .bind(samples)
            .fetch_all(&pool)
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
            let (count,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM {plain}"))
                .fetch_one(&pool)
                .await?;
            // About 500 documents spread over the table.
            let step = (count / 500).max(1);
            let pass: Vec<i32> = sqlx::query(&format!(
                "SELECT id FROM {plain} WHERE id % $1 = 0 ORDER BY id"
            ))
            .bind(step as i32)
            .fetch_all(&pool)
            .await?
            .iter()
            .map(|r| r.get(0))
            .collect();
            let (rep,): (i32,) = sqlx::query_as(&format!(
                "SELECT id FROM {plain} WHERE id >= (SELECT (MIN(id) + MAX(id)) / 2 FROM {plain}) ORDER BY id LIMIT 1"
            ))
            .fetch_one(&pool)
            .await?;
            sample_ids.insert(table.clone(), ids);
            pass_ids.insert(table.clone(), pass);
            representative.insert(table.clone(), rep);
        }

        // --- floors ---
        cx.push_expr(
            "noop/by_id",
            "floor",
            &nested,
            "id",
            vec![],
            0,
            0,
            "round trip and a primary-key lookup, no detoast",
        );
        cx.push_expr(
            "detoast/nested",
            "floor",
            &nested,
            "jsonb_array_length(value::jsonb -> 'sv')",
            vec![],
            0,
            0,
            "detoast and parse the document, nothing else",
        );
        // The per-call maps every read builds over the whole document: by
        // parent tag (both formats) and, in the derivable format, by self tag.
        cx.push_expr(
            "map/by_parent (nested)",
            "floor",
            &nested,
            "jsonb_typeof(eql_v4_internal.json_tree_by_parent(value::jsonb))",
            vec![],
            0,
            0,
            "the parent-tag map a read builds",
        );
        if !gates {
            cx.push_expr(
                "map/by_self (nested)",
                "floor",
                &nested,
                "jsonb_typeof(eql_v4_internal.json_tree_by_self(value::jsonb))",
                vec![],
                0,
                0,
                "the self-tag map a read builds (derivable only)",
            );
        }

        // --- 3. wildcard-free reads ---
        let n = Corpus::Nested;
        cx.read("read/first_name", "read", &flat, Corpus::Flat, "$.first_name", None, None,
            if gates { "flat corpus; top-level scalar: a head token with no gates returns the node's descendants, so no row (#1356)" }
            else { "flat corpus; top-level scalar" }).await;
        cx.read(
            "read/company.name",
            "read",
            &nested,
            n,
            "$.company.name",
            None,
            None,
            if gates {
                "head token + 1 gate"
            } else {
                "1 token"
            },
        )
        .await;
        cx.read(
            "read/user.contact.emergency_contact.name",
            "read",
            &nested,
            n,
            "$.user.contact.emergency_contact.name",
            None,
            None,
            if gates {
                "head token + 3 gates"
            } else {
                "1 token"
            },
        )
        .await;
        cx.read(
            "read/orders[2]",
            "read",
            &nested,
            n,
            "$.orders[2]",
            None,
            None,
            if gates {
                "position token, no gates: descendants only, not the node (#1356)"
            } else {
                "1 token"
            },
        )
        .await;
        cx.read(
            "read/orders[2].status",
            "read",
            &nested,
            n,
            "$.orders[2].status",
            None,
            None,
            if gates {
                "position token + 1 gate"
            } else {
                "1 token"
            },
        )
        .await;
        cx.read(
            "read/company",
            "read",
            &nested,
            n,
            "$.company",
            None,
            None,
            if gates {
                "head token: descendants only"
            } else {
                "1 token: the node and its descendants"
            },
        )
        .await;
        cx.read(
            "read/user",
            "read",
            &nested,
            n,
            "$.user",
            None,
            None,
            if gates {
                "head token: descendants only"
            } else {
                "1 token: the node and its descendants"
            },
        )
        .await;
        cx.read(
            "read/orders (fallback)",
            "read",
            &nested,
            n,
            "$.orders",
            None,
            None,
            "default budget 64: past it, the sealed root alone",
        )
        .await;
        cx.read(
            "read/orders (walked)",
            "read",
            &nested,
            n,
            "$.orders",
            None,
            Some(0),
            "budget 0: every entry",
        )
        .await;
        if !gates {
            cx.read(
                "read/$ (sealed root)",
                "read",
                &nested,
                n,
                "$",
                None,
                None,
                "the root's token finds the sealed root entry",
            )
            .await;
        }
        if has_open {
            cx.read(
                "read/$ (root open)",
                "read",
                &open,
                Corpus::NestedOpen,
                "$",
                None,
                None,
                if gates {
                    "root token, no gates: every descendant"
                } else {
                    "root token: the root and every descendant"
                },
            )
            .await;
        }

        // --- 4. wildcard reads ---
        for bound in [8, 64, 256] {
            cx.read(
                &format!("wildcard/orders[*].total N={bound}"),
                "wildcard",
                &nested,
                n,
                "$.orders[*].total",
                Some(bound),
                None,
                if gates {
                    "head token + 2 gates (no bound)"
                } else {
                    "N + 1 tokens"
                },
            )
            .await;
        }
        for bound in [8, 64] {
            cx.read(
                &format!("wildcard/orders[*].items[*].price N={bound}"),
                "wildcard",
                &nested,
                n,
                "$.orders[*].items[*].price",
                Some(bound),
                Some(0),
                if gates {
                    "head token + 4 gates (no bound), budget 0"
                } else {
                    "N^2 + N + 1 tokens, budget 0"
                },
            )
            .await;
        }

        // --- 5. enumeration: a small node on the nested corpus, then wide nodes ---
        {
            let key = ReadKey::derive(&cipher, "$.orders", &n.options(), None).await;
            let (from, args) = key.children();
            let (t, g) = key.sent();
            cx.push_from(
                "children/orders (nested)",
                "enumerate",
                &nested,
                &from,
                args,
                t,
                g,
                "one level, 5 to 10 hits",
            );
            let (expr, args) = key.child_count();
            cx.push_expr(
                "child_count/orders (nested)",
                "enumerate",
                &nested,
                &expr,
                args,
                t,
                g,
                "one level, 5 to 10 hits",
            );
        }
        if has_wide {
            let w = Corpus::Wide;
            for (field, path) in [("attrs", "$.attrs"), ("list", "$.list")] {
                let key = ReadKey::derive(&cipher, path, &w.options(), None).await;
                let (from, args) = key.children();
                let (t, g) = key.sent();
                cx.push_from(
                    &format!("children/{field}"),
                    "enumerate",
                    &wide,
                    &from,
                    args,
                    t,
                    g,
                    "one level, 300 hits",
                );
                let (expr, args) = key.child_count();
                cx.push_expr(
                    &format!("child_count/{field}"),
                    "enumerate",
                    &wide,
                    &expr,
                    args,
                    t,
                    g,
                    "one level, 300 hits",
                );
                cx.read(
                    &format!("read/{field} (walked)"),
                    "enumerate",
                    &wide,
                    w,
                    path,
                    None,
                    Some(0),
                    if field == "attrs" {
                        "300 member hits: the member rule's test"
                    } else {
                        "300 element hits: the member test misses, then the element rule's"
                    },
                )
                .await;
            }
        }

        // --- 6. containment and extraction ---
        let rep = representative[&nested];
        let (sample,): (sqlx::types::Json<Value>,) = sqlx::query_as(&format!(
            "SELECT plain FROM json_tree_large_plain_v4_{rows} WHERE id = $1"
        ))
        .bind(rep)
        .fetch_one(&pool)
        .await?;
        let deep = json!({"user": {"contact": {"emergency_contact": {
            "relationship": sample.0["user"]["contact"]["emergency_contact"]["relationship"]}}}});
        let deep_op = cx.contains(n, &deep).await;
        cx.out.push(Scenario {
            name: "contains/deep_object".into(),
            group: "predicate".into(),
            table: nested.clone(),
            sql: format!("SELECT id::text FROM {nested} WHERE value @> $1{CONTAINS_CAST} LIMIT 10"),
            param_bytes: deep_op.len(),
            args: vec![Arg::Text(deep_op)],
            by_id: false,
            pass_sql: None,
            tokens: 0,
            gates: 0,
            note: "GIN, LIMIT 10".into(),
        });
        let address = &sample.0["addresses"][0];
        let array_op = cx
            .contains(
                n,
                &json!({"addresses": [{"city": address["city"], "state": address["state"]}]}),
            )
            .await;
        cx.out.push(Scenario {
            name: "contains/array_element".into(),
            group: "predicate".into(),
            table: nested.clone(),
            sql: format!("SELECT id::text FROM {nested} WHERE value @> $1{CONTAINS_CAST} LIMIT 10"),
            param_bytes: array_op.len(),
            args: vec![Arg::Text(array_op)],
            by_id: false,
            pass_sql: None,
            tokens: 0,
            gates: 0,
            note: "GIN, selective (1 row)".into(),
        });
        let sel_salary_order = cx.selector("$.company.salary").await;
        cx.out.push(Scenario {
            name: "field_order/nested".into(),
            group: "predicate".into(),
            table: nested.clone(),
            sql: format!("SELECT id::text FROM {nested} ORDER BY eql_v4.ord_term(value -> '{sel_salary_order}'::text) LIMIT 10"),
            param_bytes: 0,
            args: vec![],
            by_id: false,
            pass_sql: None,
            tokens: 0,
            gates: 0,
            note: "functional btree on ord_term (created by the nested criterion bench)".into(),
        });
        let frep = representative[&flat];
        let (fsample,): (sqlx::types::Json<Value>,) = sqlx::query_as(&format!(
            "SELECT plain FROM json_tree_small_plain_v4_{rows} WHERE id = $1"
        ))
        .bind(frep)
        .fetch_one(&pool)
        .await?;
        let email_op = cx
            .contains(Corpus::Flat, &json!({"email": fsample.0["email"]}))
            .await;
        cx.out.push(Scenario {
            name: "contains/email".into(),
            group: "predicate".into(),
            table: flat.clone(),
            sql: format!("SELECT id::text FROM {flat} WHERE value @> $1{CONTAINS_CAST} LIMIT 10"),
            param_bytes: email_op.len(),
            args: vec![Arg::Text(email_op)],
            by_id: false,
            pass_sql: None,
            tokens: 0,
            gates: 0,
            note: "flat corpus, GIN, LIMIT 10".into(),
        });
        let sel_salary = cx.selector("$.company.salary").await;
        cx.push_expr(
            "extract/company.salary",
            "predicate",
            &nested,
            &format!("(value -> '{sel_salary}'::text)::jsonb"),
            vec![],
            0,
            0,
            "`->` by selector",
        );
        let sel_age = cx.selector("$.age").await;
        cx.push_expr(
            "extract/age",
            "predicate",
            &flat,
            &format!("(value -> '{sel_age}'::text)::jsonb"),
            vec![],
            0,
            0,
            "flat corpus, `->` by selector",
        );

        let plan = Plan {
            format: FORMAT.into(),
            rows: rows.clone(),
            sample_ids,
            representative,
            pass_ids,
            scenarios: cx.out,
        };
        std::fs::create_dir_all(&out_dir)?;
        let path = format!("{out_dir}/plan_{FORMAT}_{rows}.json");
        std::fs::write(&path, serde_json::to_string_pretty(&plan)?)?;
        eprintln!("{} scenarios written to {path}", plan.scenarios.len());
        Ok(())
    }
}

#[derive(Serialize, Default)]
struct Measured {
    calls_us: Vec<f64>,
    pass_us_per_doc: Vec<f64>,
    rows_per_doc: f64,
    bytes_per_doc: f64,
    rows_rep: usize,
    bytes_rep: usize,
    explain: Vec<String>,
}

fn with_id(args: &[Arg], id: i32) -> Vec<Arg> {
    let mut a = args.to_vec();
    a.push(Arg::Int(id));
    a
}

async fn call(pool: &PgPool, s: &Scenario, id: Option<i32>) -> Result<(f64, usize, usize)> {
    let args = match id {
        Some(id) => with_id(&s.args, id),
        None => s.args.clone(),
    };
    let start = Instant::now();
    let rows = query_with(&s.sql, &args).fetch_all(pool).await?;
    let us = start.elapsed().as_secs_f64() * 1e6;
    let bytes = rows
        .iter()
        .map(|r| {
            r.try_get::<Option<String>, _>(0)
                .ok()
                .flatten()
                .map_or(0, |t| t.len())
        })
        .sum();
    Ok((us, rows.len(), bytes))
}

async fn pass(pool: &PgPool, s: &Scenario, ids: &[i32]) -> Result<(f64, i64, i64)> {
    let sql = s.pass_sql.as_ref().expect("a by-id scenario");
    let mut args = s.args.clone();
    args.push(Arg::IntArray(ids.to_vec()));
    let start = Instant::now();
    let row = query_with(sql, &args).fetch_one(pool).await?;
    let us = start.elapsed().as_secs_f64() * 1e6;
    Ok((us, row.get(0), row.get(1)))
}

async fn explain(pool: &PgPool, s: &Scenario, id: Option<i32>) -> Result<Vec<String>> {
    let args = match id {
        Some(id) => with_id(&s.args, id),
        None => s.args.clone(),
    };
    let sql = format!("EXPLAIN (ANALYZE, BUFFERS) {}", s.sql);
    Ok(query_with(&sql, &args)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| r.get::<String, _>(0))
        .collect())
}

async fn time() -> Result<()> {
    let out_dir = env("OUT_DIR", "../results/query/derivable");
    let rows = env("TARGET_ROWS", "10000");
    let tag = env("TAG", "");
    let passes: usize = env("PASSES", "5").parse()?;
    let table_passes: usize = env("TABLE_PASSES", "3").parse()?;
    let reps: usize = env("REPS", "30").parse()?;
    let only = std::env::var("ONLY").ok();
    let formats = ["gates", "derivable"];
    let mut plans = BTreeMap::new();
    let mut pools = BTreeMap::new();
    for f in formats {
        let p: Plan = serde_json::from_str(&std::fs::read_to_string(format!(
            "{out_dir}/plan_{f}_{rows}.json"
        ))?)?;
        let url = std::env::var(format!("{}_URL", f.to_uppercase()))
            .with_context(|| format!("{}_URL", f.to_uppercase()))?;
        pools.insert(f, connect(&url).await?);
        plans.insert(f, p);
    }
    if plans["gates"].sample_ids != plans["derivable"].sample_ids {
        bail!("the two plans sample different ids: the twins differ");
    }
    // Scenario names in plan order, the union of both.
    let mut names: Vec<String> = Vec::new();
    for f in ["derivable", "gates"] {
        for s in &plans[f].scenarios {
            let wanted = only
                .as_ref()
                .is_none_or(|o| o.split(',').any(|o| s.name.contains(o.trim())));
            if !names.contains(&s.name) && wanted {
                names.push(s.name.clone());
            }
        }
    }
    let find = |f: &str, n: &str| plans[f].scenarios.iter().find(|s| s.name == n).cloned();
    let mut measured: BTreeMap<(String, &str), Measured> = BTreeMap::new();

    // Warm both databases: every table each plan reads, detoasted end to end.
    for f in formats {
        let tables: std::collections::BTreeSet<_> =
            plans[f].scenarios.iter().map(|s| s.table.clone()).collect();
        for t in tables {
            let start = Instant::now();
            sqlx::query(&format!("SELECT sum(length(value::jsonb::text)) FROM {t}"))
                .execute(&pools[f])
                .await?;
            eprintln!("warm {f} {t}: {:.1}s", start.elapsed().as_secs_f64());
        }
    }

    // Representative EXPLAIN ANALYZE and row/byte counts, once.
    for name in &names {
        for f in formats {
            let Some(s) = find(f, name) else { continue };
            let p = &plans[f];
            let rep = s.by_id.then(|| p.representative[&s.table]);
            let m = measured.entry((name.clone(), f)).or_default();
            let (_, r, b) = call(&pools[f], &s, rep)
                .await
                .with_context(|| format!("{f} {name}"))?;
            m.rows_rep = r;
            m.bytes_rep = b;
            m.explain = explain(&pools[f], &s, rep).await?;
        }
    }

    let t0 = Instant::now();
    for p in 0..passes {
        for (i, name) in names.iter().enumerate() {
            let order = if (p + i) % 2 == 0 {
                formats
            } else {
                [formats[1], formats[0]]
            };
            for f in order {
                let Some(s) = find(f, name) else { continue };
                let ids = &plans[f].sample_ids[&s.table];
                let m = measured.get_mut(&(name.clone(), f)).unwrap();
                if s.by_id {
                    for id in ids {
                        m.calls_us.push(call(&pools[f], &s, Some(*id)).await?.0);
                    }
                } else {
                    for _ in 0..reps {
                        m.calls_us.push(call(&pools[f], &s, None).await?.0);
                    }
                }
            }
        }
        eprintln!(
            "call pass {}/{passes} done at {:.0}s",
            p + 1,
            t0.elapsed().as_secs_f64()
        );
    }
    for p in 0..table_passes {
        for (i, name) in names.iter().enumerate() {
            let order = if (p + i) % 2 == 0 {
                formats
            } else {
                [formats[1], formats[0]]
            };
            for f in order {
                let Some(s) = find(f, name) else { continue };
                if !s.by_id {
                    continue;
                }
                let ids = &plans[f].pass_ids[&s.table];
                let (us, r, b) = pass(&pools[f], &s, ids)
                    .await
                    .with_context(|| format!("pass {f} {name}"))?;
                let m = measured.get_mut(&(name.clone(), f)).unwrap();
                m.pass_us_per_doc.push(us / ids.len() as f64);
                m.rows_per_doc = r as f64 / ids.len() as f64;
                m.bytes_per_doc = b as f64 / ids.len() as f64;
            }
        }
        eprintln!(
            "table pass {}/{table_passes} done at {:.0}s",
            p + 1,
            t0.elapsed().as_secs_f64()
        );
    }

    let mut out = serde_json::Map::new();
    for name in &names {
        let mut per = serde_json::Map::new();
        for f in formats {
            let Some(s) = find(f, name) else { continue };
            let m = &measured[&(name.clone(), f)];
            per.insert(
                f.to_string(),
                json!({
                    "scenario": s,
                    "measured": m,
                }),
            );
        }
        out.insert(name.clone(), Value::Object(per));
    }
    let doc = json!({
        "rows": rows,
        "passes": passes,
        "table_passes": table_passes,
        "reps": reps,
        "sample_ids": plans["gates"].sample_ids.iter().map(|(k, v)| (k.clone(), v.len())).collect::<BTreeMap<_, _>>(),
        "pass_ids": plans["gates"].pass_ids.iter().map(|(k, v)| (k.clone(), v.len())).collect::<BTreeMap<_, _>>(),
        "seconds": t0.elapsed().as_secs_f64(),
        "scenarios": out,
    });
    let path = format!("{out_dir}/time_{rows}{tag}.json");
    std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    eprintln!("written to {path}");
    Ok(())
}
