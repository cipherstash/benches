//! SteVec (EQL v3, `public.json`) side of the three-way JSON comparison
//! (`report/JSON_TREE_VS_STEVEC_VS_PLAINTEXT.md`): every scenario of the two
//! json_tree benches (`json-tree-bench/benches/json_tree_v4.rs` and
//! `json_tree_nested_v4.rs`) that SteVec can express, over the same documents
//! (`json_ste_vec_<corpus>_encrypted_v3_<N>` is sealed from the json_tree
//! plaintext twin, id for id — `src/twin.rs`), with the same sampled row and
//! the same plaintext needles and parameters.
//!
//! `CORPUS=small` is the flat corpus, `CORPUS=large` the nested one. Ids are
//! `JSON_COMPARE_V3/<corpus>/<scenario>/<N>`, the scenario tail matching the
//! json_tree bench's.
//!
//! Every operand is derived the way a caller derives it: containment needles
//! through cipherstash-client (`QueryOp::Default`, converted to the v3
//! `{"sv":[…]}` shape by `eql_bindings::from_v2_query`), selectors through
//! `QueryOp::SteVecSelector`. Comparison operands (`>`, `>=`, `<=`) are the
//! stored leaf entries of rows holding the plaintext bound, read through
//! `eql_v3.jsonb_path_query_first` — the same plaintext the json_tree bench
//! derives its `AtPath` operand from; SteVec has no client-side leaf query
//! shape for the alpha.3 domain, and this is what the earlier v3 bench did.
//!
//! The surface, per scenario family:
//!   containment    eql_v3.jsonb_contains(value, $1::jsonb) — inlines to
//!                  `eql_v3.jsonb_array(value) @> eql_v3.jsonb_array($1)`, served
//!                  by the GIN over `eql_v3.jsonb_array(value)`; and the
//!                  operator `value @> $1::jsonb::eql_v3.query_jsonb`, which
//!                  inlines to `eql_v3.to_ste_vec_query(value)::jsonb @> $1` and
//!                  has no index (the documented index recipe is the function)
//!   field compare  eql_v3.jsonb_path_query_first(value, '<sel>') > $1::jsonb::public.jsonb_entry
//!                  (`>` on jsonb_entry inlines to an `ore_cllw` comparison),
//!                  served by a btree on `eql_v3.ore_cllw(jsonb_path_query_first(value, '<sel>'))`
//!                  built at startup, as in `benches/json_v3.rs`
//!   extraction     (value -> '<sel>'::text) — every SteVec entry carries its
//!                  own ciphertext of the node at that path, so `->` on a
//!                  container's selector returns that subtree sealed whole
//!   path query     eql_v3.jsonb_path_query(value, '<sel>') — one entry per
//!                  node matching the selector; a `[*]` selector is the
//!                  wildcard (the table is sealed with `ITEM | WILDCARD`)
//!
//! A scenario whose single execution takes longer than `SLOW_MS` (default
//! 3000) is not handed to criterion (its minimum of 55 iterations would run
//! for hours): it is timed for `SLOW_RUNS` (default 3) executions instead,
//! written to `<RESULTS_DIR>/stevec_<corpus>_slow_<N>.json`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dbbenches::twin::{tables, V3Operands};
use dbbenches::{
    bench_assert, extract_indexes_used, init_scoped_cipher, init_tracing, write_metadata_file_in,
    ScenarioMetadata,
};
use serde_json::{json, Value as JsonValue};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::Row;
use std::time::Instant;
use tokio::runtime::Runtime;

#[derive(Clone)]
enum Arg {
    Text(String),
    Int(i32),
}

struct Scenario {
    name: String,
    sql: String,
    args: Vec<Arg>,
}

fn query_with<'q>(
    sql: &'q str,
    args: &'q [Arg],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    args.iter().fold(sqlx::query(sql), |q, a| match a {
        Arg::Text(t) => q.bind(t),
        Arg::Int(i) => q.bind(*i),
    })
}

/// The stored leaf entry at `sel` of row `id`, as text castable to
/// `public.jsonb_entry`.
async fn entry_at(pool: &sqlx::PgPool, table: &str, sel: &str, id: i32) -> String {
    let row = sqlx::query(&format!(
        "SELECT eql_v3.jsonb_path_query_first(value, '{sel}')::jsonb::text FROM {table} WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("leaf entry at {sel} of row {id}: {e}"));
    row.get::<Option<String>, _>(0)
        .unwrap_or_else(|| panic!("row {id} of {table} has no entry at {sel}"))
}

/// The lowest id of the twin whose integer at `path` (a `#>>` text path) is `v`.
async fn id_with(pool: &sqlx::PgPool, twin: &str, path: &str, v: i64) -> i32 {
    sqlx::query(&format!(
        "SELECT id FROM {twin} WHERE (plain #>> '{path}')::int = $1 ORDER BY id LIMIT 1"
    ))
    .bind(v as i32)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("no row of {twin} has {path} = {v}: {e}"))
    .get(0)
}

async fn functional_btree(pool: &sqlx::PgPool, table: &str, name: &str, sel: &str) {
    let idx = format!("{table}_{name}");
    sqlx::query(&format!("DROP INDEX IF EXISTS {idx}"))
        .execute(pool)
        .await
        .expect("drop stale functional index");
    sqlx::query(&format!(
        "CREATE INDEX {idx} ON {table} \
         USING btree (eql_v3.ore_cllw(eql_v3.jsonb_path_query_first(value, '{sel}')))"
    ))
    .execute(pool)
    .await
    .expect("create functional index");
}

fn criterion_benchmark(c: &mut Criterion) {
    init_tracing();
    let rt = Runtime::new().unwrap();

    let corpus = std::env::var("CORPUS").unwrap_or_else(|_| "small".to_string());
    assert!(corpus == "small" || corpus == "large", "CORPUS is small or large");
    let target_rows = std::env::var("TARGET_ROWS").unwrap_or_else(|_| "unknown".to_string());
    let suffix = match target_rows.as_str() {
        "10000" | "100000" | "1000000" => format!("_{target_rows}"),
        _ => String::new(),
    };
    let (table, twin) = tables(&corpus, &suffix);
    let results_dir =
        std::env::var("RESULTS_DIR").unwrap_or_else(|_| "results/query/compare".to_string());
    let slow_ms: u128 = std::env::var("SLOW_MS")
        .unwrap_or_else(|_| "3000".into())
        .parse()
        .unwrap();
    let slow_runs: usize = std::env::var("SLOW_RUNS")
        .unwrap_or_else(|_| "3".into())
        .parse()
        .unwrap();

    let (pool, scenarios) = rt.block_on(async {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL environment variable must be set");
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to database");
        let cipher = init_scoped_cipher().await.expect("scoped cipher");
        let ops = V3Operands::new(cipher, &table);

        // The json_tree benches' row: the first id at or past the middle.
        let row = sqlx::query(&format!(
            "SELECT id, plain FROM {twin} \
             WHERE id >= (SELECT (MIN(id) + MAX(id)) / 2 FROM {twin}) ORDER BY id LIMIT 1"
        ))
        .fetch_one(&pool)
        .await
        .expect("sample row");
        let id: i32 = row.get("id");
        let sample: JsonValue = row.get::<Json<JsonValue>, _>("plain").0;
        eprintln!("json_compare_v3 ({corpus}): row {id} of `{table}`");

        let t = |s: &str| Arg::Text(s.to_string());
        let i = |n: i32| Arg::Int(n);
        let contains_fn = format!("SELECT id FROM {table} WHERE eql_v3.jsonb_contains(value, $1::jsonb) LIMIT 10");
        let contains_op = format!("SELECT id FROM {table} WHERE value @> $1::jsonb::eql_v3.query_jsonb LIMIT 10");
        let by_path = |sel: &str| {
            format!("SELECT (value -> '{sel}'::text)::jsonb::text FROM {table} WHERE id = $1")
        };
        let path_query = format!(
            "SELECT e::jsonb::text FROM {table}, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2"
        );
        let gt = |sel: &str| {
            format!(
                "SELECT id FROM {table} \
                 WHERE eql_v3.jsonb_path_query_first(value, '{sel}') > $1::jsonb::public.jsonb_entry LIMIT 10"
            )
        };
        let between = |sel: &str| {
            format!(
                "SELECT id FROM {table} \
                 WHERE eql_v3.jsonb_path_query_first(value, '{sel}') >= $1::jsonb::public.jsonb_entry \
                   AND eql_v3.jsonb_path_query_first(value, '{sel}') <= $2::jsonb::public.jsonb_entry LIMIT 10"
            )
        };
        let order = |sel: &str| {
            format!(
                "SELECT id FROM {table} \
                 ORDER BY eql_v3.ore_cllw(eql_v3.jsonb_path_query_first(value, '{sel}')) LIMIT 10"
            )
        };
        let s = |name: &str, sql: String, args: Vec<Arg>| Scenario {
            name: name.to_string(),
            sql,
            args,
        };

        let scenarios = if corpus == "small" {
            let sel_age = ops.selector("$.age").await.unwrap();
            let sel_email = ops.selector("$.email").await.unwrap();
            let doc = ops.containment(&sample).await.unwrap();
            let first = ops
                .containment(&json!({"first_name": sample["first_name"]}))
                .await
                .unwrap();
            let email = ops
                .containment(&json!({"email": sample["email"]}))
                .await
                .unwrap();
            let age = sample["age"].as_i64().unwrap();
            let (lo, hi) = ((age - 5).max(18), (age + 5).min(99));
            let age_entry = entry_at(&pool, &table, &sel_age, id).await;
            let lo_entry = entry_at(&pool, &table, &sel_age, id_with(&pool, &twin, "{age}", lo).await).await;
            let hi_entry = entry_at(&pool, &table, &sel_age, id_with(&pool, &twin, "{age}", hi).await).await;
            let email_entry = entry_at(&pool, &table, &sel_email, id).await;

            eprintln!("json_compare_v3: building the per-selector functional indexes...");
            functional_btree(&pool, &table, "field_order_idx", &sel_age).await;
            sqlx::query(&format!("DROP INDEX IF EXISTS {table}_field_eq_idx"))
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(&format!(
                "CREATE INDEX {table}_field_eq_idx ON {table} \
                 USING btree (eql_v3.eq_term(eql_v3.jsonb_path_query_first(value, '{sel_email}')))"
            ))
            .execute(&pool)
            .await
            .unwrap();

            vec![
                s("contains/functional", contains_fn.clone(), vec![t(&doc)]),
                s("contains/operator", contains_op.clone(), vec![t(&doc)]),
                s("field_eq/extractor", contains_fn.clone(), vec![t(&first)]),
                s("field_eq/unique", contains_fn.clone(), vec![t(&email)]),
                s(
                    "field_eq/eq_term",
                    format!(
                        "SELECT id FROM {table} \
                         WHERE eql_v3.eq_term(eql_v3.jsonb_path_query_first(value, '{sel_email}')) \
                             = eql_v3.eq_term($1::jsonb::public.jsonb_entry) LIMIT 10"
                    ),
                    vec![t(&email_entry)],
                ),
                s("field_gt/functional", gt(&sel_age), vec![t(&age_entry)]),
                s("field_between/functional", between(&sel_age), vec![t(&lo_entry), t(&hi_entry)]),
                s("field_order/functional", order(&sel_age), vec![]),
                s("extract/by_id", by_path(&sel_age), vec![i(id)]),
                s("path_query/field", path_query.clone(), vec![t(&sel_email), i(id)]),
            ]
        } else {
            let deep_object = json!({"user": {"contact": {"emergency_contact": {
                "relationship": sample["user"]["contact"]["emergency_contact"]["relationship"]}}}});
            let address = &sample["addresses"][0];
            let array_element =
                json!({"addresses": [{"city": address["city"], "state": address["state"]}]});
            let order0 = &sample["orders"][0];
            let nested_array = json!({"orders": [{"status": order0["status"],
                "items": [{"product": order0["items"][0]["product"]}]}]});
            let email = json!({"user": {"email": sample["user"]["email"]}});
            let enc_deep = ops.containment(&deep_object).await.unwrap();
            let enc_array = ops.containment(&array_element).await.unwrap();
            let enc_nested = ops.containment(&nested_array).await.unwrap();
            let enc_email = ops.containment(&email).await.unwrap();

            let sel_salary = ops.selector("$.company.salary").await.unwrap();
            let sel_company = ops.selector("$.company").await.unwrap();
            let sel_user = ops.selector("$.user").await.unwrap();
            let sel_orders = ops.selector("$.orders").await.unwrap();
            let sel_items = ops.selector("$.orders[*].items").await.unwrap();
            let sel_order_elems = ops.selector("$.orders[*]").await.unwrap();
            let sel_status = ops.selector("$.orders[*].status").await.unwrap();

            let salary = sample["company"]["salary"].as_i64().unwrap();
            let (hi,): (i32,) = sqlx::query_as(&format!(
                "SELECT MIN((plain #>> '{{company,salary}}')::int) FROM {twin} \
                 WHERE (plain #>> '{{company,salary}}')::int >= $1"
            ))
            .bind(salary as i32 + 10_000)
            .fetch_one(&pool)
            .await
            .unwrap();
            let salary_entry = entry_at(&pool, &table, &sel_salary, id).await;
            let hi_entry = entry_at(
                &pool,
                &table,
                &sel_salary,
                id_with(&pool, &twin, "{company,salary}", i64::from(hi)).await,
            )
            .await;

            eprintln!("json_compare_v3: building the per-selector functional index...");
            functional_btree(&pool, &table, "field_order_idx", &sel_salary).await;

            vec![
                s("contains/deep_object", contains_op.clone(), vec![t(&enc_deep)]),
                s("contains/deep_object_fn", contains_fn.clone(), vec![t(&enc_deep)]),
                s("contains/array_element", contains_op.clone(), vec![t(&enc_array)]),
                s("contains/array_element_fn", contains_fn.clone(), vec![t(&enc_array)]),
                s("contains/nested_array", contains_op.clone(), vec![t(&enc_nested)]),
                s("contains/nested_array_fn", contains_fn.clone(), vec![t(&enc_nested)]),
                s("field_eq/nested", contains_fn.clone(), vec![t(&enc_email)]),
                s("field_gt/nested", gt(&sel_salary), vec![t(&salary_entry)]),
                s("field_between/nested", between(&sel_salary), vec![t(&salary_entry), t(&hi_entry)]),
                s("field_order/nested", order(&sel_salary), vec![]),
                s("subtree/company", by_path(&sel_company), vec![i(id)]),
                s("subtree/user", by_path(&sel_user), vec![i(id)]),
                s("subtree/orders", by_path(&sel_orders), vec![i(id)]),
                s("subtree_at/items", path_query.clone(), vec![t(&sel_items), i(id)]),
                s("children/orders", path_query.clone(), vec![t(&sel_order_elems), i(id)]),
                s(
                    "child_count/orders",
                    format!(
                        "SELECT count(*) FROM {table}, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2"
                    ),
                    vec![t(&sel_order_elems), i(id)],
                ),
                s("path_query/wildcard", path_query.clone(), vec![t(&sel_status), i(id)]),
                s("extract/nested", by_path(&sel_salary), vec![i(id)]),
            ]
        };
        sqlx::query(&format!("ANALYZE {table}"))
            .execute(&pool)
            .await
            .expect("ANALYZE");
        (pool, scenarios)
    });

    // --- Metadata sidecar: EXPLAIN + one timed execution per scenario ---
    let (metadata, slow) = rt.block_on(async {
        let mut out = Vec::with_capacity(scenarios.len());
        let mut slow = Vec::new();
        for s in &scenarios {
            let id = format!("JSON_COMPARE_V3/{corpus}/{}/{target_rows}", s.name);
            let explain: Json<JsonValue> =
                query_with(&format!("EXPLAIN (FORMAT JSON) {}", s.sql), &s.args)
                    .fetch_one(&pool)
                    .await
                    .unwrap_or_else(|e| panic!("EXPLAIN failed for {id}: {e}"))
                    .get(0);
            let started = Instant::now();
            let rows = query_with(&s.sql, &s.args)
                .fetch_all(&pool)
                .await
                .unwrap_or_else(|e| panic!("row-count execute failed for {id}: {e}"));
            let first_ms = started.elapsed().as_millis();
            if first_ms > slow_ms {
                let mut times = Vec::with_capacity(slow_runs);
                for _ in 0..slow_runs {
                    let t0 = Instant::now();
                    query_with(&s.sql, &s.args).fetch_all(&pool).await.unwrap();
                    times.push(t0.elapsed().as_secs_f64() * 1e3);
                }
                let mut sorted = times.clone();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                eprintln!("json_compare_v3: {id} is slow ({first_ms} ms), timed {slow_runs}x: {times:?}");
                slow.push(json!({
                    "id": id, "first_ms": first_ms, "runs_ms": times,
                    "median_ms": sorted[sorted.len() / 2],
                }));
            }
            out.push(ScenarioMetadata {
                id,
                query: s.sql.clone(),
                parameters: s
                    .args
                    .iter()
                    .map(|a| match a {
                        Arg::Text(t) => JsonValue::String(t.clone()),
                        Arg::Int(i) => JsonValue::from(*i),
                    })
                    .collect(),
                indexes_used: extract_indexes_used(&explain.0),
                explain: explain.0,
                rows_returned: rows.len() as u64,
            });
        }
        (out, slow)
    });
    write_metadata_file_in(&results_dir, &format!("stevec_{corpus}"), &target_rows, metadata)
        .expect("failed to write bench metadata sidecar");
    std::fs::write(
        format!("{results_dir}/stevec_{corpus}_slow_{target_rows}.json"),
        serde_json::to_string_pretty(&slow).unwrap(),
    )
    .expect("write slow sidecar");
    let slow_ids: Vec<String> = slow
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();

    // --- Bench loop ---
    let mut group = c.benchmark_group("JSON_COMPARE_V3");
    group.sample_size(10);
    for s in &scenarios {
        let id = format!("JSON_COMPARE_V3/{corpus}/{}/{target_rows}", s.name);
        if slow_ids.contains(&id) {
            continue;
        }
        let sql = s.sql.clone();
        let args = s.args.clone();
        group.bench_function(format!("{corpus}/{}/{target_rows}", s.name), |b| {
            b.to_async(&rt).iter(|| async {
                let rows = bench_assert(query_with(&sql, &args).fetch_all(&pool).await, &id);
                black_box(rows.len())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
