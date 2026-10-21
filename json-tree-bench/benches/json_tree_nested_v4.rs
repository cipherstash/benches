//! EQL v4 `json_tree` on the nested corpus (`json_tree_large_encrypted_v4_<N>`,
//! the root crate's `FakeJsonLarge` shape): what the flat corpus cannot
//! exercise — the token-chain walks, the array functions, wildcard path
//! queries and containment below the root — each paired with the native
//! jsonb equivalent over the plaintext twin table (same rows, same ids) as
//! the baseline. The v3 SteVec bench has no nested corpus, so there is no
//! v3 column here; ids are `JSON_TREE_NESTED/<json_tree|plain>/<scenario>/<N>`.
//!
//!   contains/deep_object      @> a needle four objects deep (`$.user.contact.emergency_contact.relationship`, ~1 in 5 rows)
//!   contains/array_element    @> one address object's city + state (selective)
//!   contains/nested_array     @> an order's status with one of its items' product (array in array)
//!   field_eq/nested           AtPath(Eq) on `$.user.email` — containment of the one-field needle
//!   field_gt/nested           (value -> sel) > leaf on `$.company.salary`, functional btree on ord_term
//!   field_order/nested        ORDER BY eql_v4.ord_term(value -> sel) LIMIT 10, same btree
//!   subtree/company           eql_v4.json_tree_subtree(value, token) by id — 6 leaves
//!   subtree/user              … `$.user`: 3 levels, 12 leaves
//!   subtree/orders            … `$.orders`: every order and item, the largest walk (hundreds of entries)
//!   subtree_at/items          eql_v4.json_tree_subtree_at(value, token, sel) for `$.orders[*].items`
//!   subtree/orders_walked     the two above with the budget off (0): the walk returns every
//!   subtree_at/items_walked   entry, where the default budget (64) returns the sealed root
//!   children/orders           eql_v4.json_tree_children(value, token) — one row per order
//!   child_count/orders        eql_v4.json_tree_child_count(value, token)
//!   path_query/wildcard       eql_v4.json_tree_path_query(value::jsonb, sel) for `$.orders[*].status`
//!   extract/nested            (value -> sel) for `$.company.salary` by id
//!
//! The `plain/*` twins: native `@>` under a jsonb_path_ops GIN, `->`/`->>`
//! extraction, `jsonb_array_elements`, `jsonb_array_length`,
//! `jsonb_path_query`, and a btree on `((plain->'company'->>'salary')::int)`.
//! Every by-id scenario uses the same sampled row on both sides.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use eql_bindings::v4::json_tree::{JsonTreeLeafQuery, JsonTreeQuery};
use json_tree_bench::{
    bench_assert, cipher, column, extract_indexes_used, plan, write_metadata_file_in, Cipher,
    JsonPlan, ScenarioMetadata,
};
use serde_json::{json, Value as JsonValue};
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::Row;
use stack_encrypt::json::{JsonOptions, JsonPath};
use stack_encrypt::op;
use tokio::runtime::Runtime;

const CONTAINS_CAST: &str = "::text::jsonb::eql_v4.query_json_tree";
const LEAF_CAST: &str = "::text::jsonb::eql_v4.query_json_tree_leaf";

#[derive(Clone)]
enum Arg {
    Text(String),
    Jsonb(JsonValue),
    Int(i32),
}

struct Scenario {
    name: &'static str,
    sql: String,
    args: Vec<Arg>,
}

fn query_with<'q>(
    sql: &'q str,
    args: &'q [Arg],
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    args.iter().fold(sqlx::query(sql), |q, a| match a {
        Arg::Text(t) => q.bind(t),
        Arg::Jsonb(v) => q.bind(Json(v)),
        Arg::Int(i) => q.bind(*i),
    })
}

async fn contains_operand(cipher: &Cipher, plan: &JsonPlan, needle: &JsonValue) -> String {
    let q = cipher
        .query(op::Contains, needle)
        .on(plan)
        .await
        .expect("derive containment operand");
    serde_json::to_string(&JsonTreeQuery::try_from(&q).expect("typed operand")).unwrap()
}

async fn selector(cipher: &Cipher, path: &str) -> String {
    cipher
        .default_keyset()
        .json_selector(&JsonPath::parse(path).unwrap(), column())
        .await
        .expect("derive selector")
        .to_string()
}

async fn token(cipher: &Cipher, path: &str) -> String {
    cipher
        .default_keyset()
        .json_subtree_key(
            &JsonPath::parse(path).unwrap(),
            JsonOptions::default(),
            column(),
        )
        .await
        .expect("derive subtree key")
        .token()
        .to_string()
}

fn criterion_benchmark(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let target_rows = std::env::var("TARGET_ROWS").unwrap_or_else(|_| "unknown".to_string());
    let table_suffix = match target_rows.as_str() {
        "10000" | "100000" | "1000000" | "10000000" => format!("_{target_rows}"),
        _ => String::new(),
    };
    let table = format!("json_tree_large_encrypted_v4{table_suffix}");
    let plain_table = format!("json_tree_large_plain_v4{table_suffix}");
    let results_dir =
        std::env::var("RESULTS_DIR").unwrap_or_else(|_| "../results/query/v4".to_string());

    let (pool, scenarios) = rt.block_on(async {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL environment variable must be set");
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to database");
        let cipher = cipher().await;
        let plan = plan();

        // A row from the middle of the table.
        let row = sqlx::query(&format!(
            "SELECT id, plain FROM {plain_table} \
             WHERE id >= (SELECT (MIN(id) + MAX(id)) / 2 FROM {plain_table}) \
             ORDER BY id LIMIT 1"
        ))
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|e| panic!("sampling a row from `{plain_table}` failed: {e}"));
        let id: i32 = row.get("id");
        let sample: Json<JsonValue> = row.get("plain");
        let sample = sample.0;
        eprintln!("json_tree_nested_v4 bench picked row {id} from `{table}`");

        // --- Needles, from the sample ---
        let deep_object = json!({"user": {"contact": {"emergency_contact": {
            "relationship": sample["user"]["contact"]["emergency_contact"]["relationship"]}}}});
        let address = &sample["addresses"][0];
        let array_element = json!({"addresses": [{"city": address["city"], "state": address["state"]}]});
        let order = &sample["orders"][0];
        let nested_array = json!({"orders": [{"status": order["status"],
            "items": [{"product": order["items"][0]["product"]}]}]});
        let email = json!({"user": {"email": sample["user"]["email"]}});
        let salary = sample["company"]["salary"].as_i64().expect("integer salary");

        let enc_deep = contains_operand(&cipher, &plan, &deep_object).await;
        let enc_array = contains_operand(&cipher, &plan, &array_element).await;
        let enc_nested = contains_operand(&cipher, &plan, &nested_array).await;
        let enc_email = {
            let q = cipher
                .query(op::AtPath(op::Eq), &("$.user.email", sample["user"]["email"].clone()))
                .on(&plan)
                .await
                .expect("derive AtPath(Eq) operand");
            serde_json::to_string(&JsonTreeQuery::try_from(&q).expect("typed operand")).unwrap()
        };
        let salary_gt = {
            let q = cipher
                .query(op::AtPath(op::Gt), &("$.company.salary", salary))
                .on(&plan)
                .await
                .expect("derive AtPath(Gt) operand");
            serde_json::to_string(&JsonTreeLeafQuery::try_from(&q).expect("typed leaf")).unwrap()
        };
        let sel_salary = selector(&cipher, "$.company.salary").await;
        let sel_status = selector(&cipher, "$.orders[*].status").await;
        let tok_company = token(&cipher, "$.company").await;
        let tok_user = token(&cipher, "$.user").await;
        let tok_orders = token(&cipher, "$.orders").await;
        let items_key = cipher
            .default_keyset()
            .json_subtree_key(
                &JsonPath::parse("$.orders[*].items").unwrap(),
                JsonOptions::default(),
                column(),
            )
            .await
            .expect("derive subtree key");
        let tok_items = items_key.token().to_string();
        let sel_items = items_key
            .selector()
            .expect("a wildcard path carries a selector")
            .to_string();

        // --- Per-path functional indexes for the ordering scenarios ---
        for (name, expr) in [
            (
                format!("{table}_field_order_idx"),
                format!("{table} USING btree (eql_v4.ord_term(value -> '{sel_salary}'::text))"),
            ),
            (
                format!("{plain_table}_salary_idx"),
                format!("{plain_table} USING btree (((plain -> 'company' ->> 'salary')::int))"),
            ),
        ] {
            sqlx::query(&format!("DROP INDEX IF EXISTS {name}"))
                .execute(&pool)
                .await
                .expect("drop stale index");
            sqlx::query(&format!("CREATE INDEX {name} ON {expr}"))
                .execute(&pool)
                .await
                .expect("create functional index");
        }
        for t in [&table, &plain_table] {
            sqlx::query(&format!("ANALYZE {t}"))
                .execute(&pool)
                .await
                .expect("ANALYZE");
        }

        // --- Oracle: encrypted containment agrees with plaintext containment ---
        for (label, enc, needle) in [
            ("deep_object", &enc_deep, &deep_object),
            ("array_element", &enc_array, &array_element),
            ("nested_array", &enc_nested, &nested_array),
        ] {
            let encrypted: Vec<i32> = query_with(
                &format!("SELECT id FROM {table} WHERE value @> $1{CONTAINS_CAST} ORDER BY id"),
                &[Arg::Text(enc.clone())],
            )
            .fetch_all(&pool)
            .await
            .expect("oracle (encrypted)")
            .iter()
            .map(|r| r.get(0))
            .collect();
            let plain: Vec<i32> = query_with(
                &format!("SELECT id FROM {plain_table} WHERE plain @> $1 ORDER BY id"),
                &[Arg::Jsonb(needle.clone())],
            )
            .fetch_all(&pool)
            .await
            .expect("oracle (plain)")
            .iter()
            .map(|r| r.get(0))
            .collect();
            assert_eq!(encrypted, plain, "{label}: encrypted @> must agree with plaintext @>");
            assert!(encrypted.contains(&id));
            eprintln!("json_tree_nested_v4 bench: {label} oracle agrees ({} rows)", encrypted.len());
        }

        let i = |n: i32| Arg::Int(n);
        let t = |s: &str| Arg::Text(s.to_string());
        let j = |v: &JsonValue| Arg::Jsonb(v.clone());
        let mut scenarios = vec![
            // ---- encrypted ----
            Scenario { name: "json_tree/contains/deep_object",
                sql: format!("SELECT id FROM {table} WHERE value @> $1{CONTAINS_CAST} LIMIT 10"),
                args: vec![t(&enc_deep)] },
            Scenario { name: "json_tree/contains/array_element",
                sql: format!("SELECT id FROM {table} WHERE value @> $1{CONTAINS_CAST} LIMIT 10"),
                args: vec![t(&enc_array)] },
            Scenario { name: "json_tree/contains/nested_array",
                sql: format!("SELECT id FROM {table} WHERE value @> $1{CONTAINS_CAST} LIMIT 10"),
                args: vec![t(&enc_nested)] },
            Scenario { name: "json_tree/field_eq/nested",
                sql: format!("SELECT id FROM {table} WHERE value @> $1{CONTAINS_CAST} LIMIT 10"),
                args: vec![t(&enc_email)] },
            Scenario { name: "json_tree/field_gt/nested",
                sql: format!("SELECT id FROM {table} WHERE (value -> '{sel_salary}'::text) > $1{LEAF_CAST} LIMIT 10"),
                args: vec![t(&salary_gt)] },
            Scenario { name: "json_tree/field_order/nested",
                sql: format!("SELECT id FROM {table} ORDER BY eql_v4.ord_term(value -> '{sel_salary}'::text) LIMIT 10"),
                args: vec![] },
            Scenario { name: "json_tree/subtree/company",
                sql: format!("SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_subtree(value, $1::text) e WHERE id = $2"),
                args: vec![t(&tok_company), i(id)] },
            Scenario { name: "json_tree/subtree/user",
                sql: format!("SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_subtree(value, $1::text) e WHERE id = $2"),
                args: vec![t(&tok_user), i(id)] },
            Scenario { name: "json_tree/subtree/orders",
                sql: format!("SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_subtree(value, $1::text) e WHERE id = $2"),
                args: vec![t(&tok_orders), i(id)] },
            Scenario { name: "json_tree/subtree_at/items",
                sql: format!("SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_subtree_at(value, $1::text, $2::text) e WHERE id = $3"),
                args: vec![t(&tok_items), t(&sel_items), i(id)] },
            // The same two walks with the budget off (0): every entry of the
            // subtree, where the default budget returns the sealed root.
            Scenario { name: "json_tree/subtree/orders_walked",
                sql: format!("SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_subtree(value, $1::text, 0) e WHERE id = $2"),
                args: vec![t(&tok_orders), i(id)] },
            Scenario { name: "json_tree/subtree_at/items_walked",
                sql: format!("SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_subtree_at(value, $1::text, $2::text, 0) e WHERE id = $3"),
                args: vec![t(&tok_items), t(&sel_items), i(id)] },
            Scenario { name: "json_tree/children/orders",
                sql: format!("SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_children(value, $1::text) e WHERE id = $2"),
                args: vec![t(&tok_orders), i(id)] },
            Scenario { name: "json_tree/child_count/orders",
                sql: format!("SELECT eql_v4.json_tree_child_count(value, $1::text) FROM {table} WHERE id = $2"),
                args: vec![t(&tok_orders), i(id)] },
            Scenario { name: "json_tree/path_query/wildcard",
                sql: format!("SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_path_query(value::jsonb, $1::text) e WHERE id = $2"),
                args: vec![t(&sel_status), i(id)] },
            Scenario { name: "json_tree/extract/nested",
                sql: format!("SELECT (value -> '{sel_salary}'::text)::jsonb::text FROM {table} WHERE id = $1"),
                args: vec![i(id)] },
            // ---- plaintext twins ----
            Scenario { name: "plain/contains/deep_object",
                sql: format!("SELECT id FROM {plain_table} WHERE plain @> $1 LIMIT 10"),
                args: vec![j(&deep_object)] },
            Scenario { name: "plain/contains/array_element",
                sql: format!("SELECT id FROM {plain_table} WHERE plain @> $1 LIMIT 10"),
                args: vec![j(&array_element)] },
            Scenario { name: "plain/contains/nested_array",
                sql: format!("SELECT id FROM {plain_table} WHERE plain @> $1 LIMIT 10"),
                args: vec![j(&nested_array)] },
            Scenario { name: "plain/field_eq/nested",
                sql: format!("SELECT id FROM {plain_table} WHERE plain @> $1 LIMIT 10"),
                args: vec![j(&email)] },
            Scenario { name: "plain/field_gt/nested",
                sql: format!("SELECT id FROM {plain_table} WHERE (plain -> 'company' ->> 'salary')::int > $1 LIMIT 10"),
                args: vec![i(salary as i32)] },
            Scenario { name: "plain/field_order/nested",
                sql: format!("SELECT id FROM {plain_table} ORDER BY (plain -> 'company' ->> 'salary')::int LIMIT 10"),
                args: vec![] },
            Scenario { name: "plain/subtree/company",
                sql: format!("SELECT (plain -> 'company')::text FROM {plain_table} WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/subtree/user",
                sql: format!("SELECT (plain -> 'user')::text FROM {plain_table} WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/subtree/orders",
                sql: format!("SELECT (plain -> 'orders')::text FROM {plain_table} WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/subtree_at/items",
                sql: format!("SELECT e::text FROM {plain_table}, jsonb_path_query(plain, '$.orders[*].items') e WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/subtree/orders_walked",
                sql: format!("SELECT (plain -> 'orders')::text FROM {plain_table} WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/subtree_at/items_walked",
                sql: format!("SELECT e::text FROM {plain_table}, jsonb_path_query(plain, '$.orders[*].items') e WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/children/orders",
                sql: format!("SELECT e::text FROM {plain_table}, jsonb_array_elements(plain -> 'orders') e WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/child_count/orders",
                sql: format!("SELECT jsonb_array_length(plain -> 'orders') FROM {plain_table} WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/path_query/wildcard",
                sql: format!("SELECT e::text FROM {plain_table}, jsonb_path_query(plain, '$.orders[*].status') e WHERE id = $1"),
                args: vec![i(id)] },
            Scenario { name: "plain/extract/nested",
                sql: format!("SELECT (plain -> 'company' -> 'salary')::text FROM {plain_table} WHERE id = $1"),
                args: vec![i(id)] },
        ];
        scenarios.shrink_to_fit();
        (pool, scenarios)
    });

    // --- Metadata sidecar: EXPLAIN + one execution per scenario ---
    let metadata = rt.block_on(async {
        let mut out = Vec::with_capacity(scenarios.len());
        for s in &scenarios {
            let id = format!("JSON_TREE_NESTED/{}/{}", s.name, target_rows);
            let explain_sql = format!("EXPLAIN (FORMAT JSON) {}", s.sql);
            let explain: Json<JsonValue> = query_with(&explain_sql, &s.args)
                .fetch_one(&pool)
                .await
                .unwrap_or_else(|e| panic!("EXPLAIN failed for {id}: {e}"))
                .get(0);
            let rows = query_with(&s.sql, &s.args)
                .fetch_all(&pool)
                .await
                .unwrap_or_else(|e| panic!("row-count execute failed for {id}: {e}"));
            let parameters = s
                .args
                .iter()
                .map(|a| match a {
                    Arg::Text(t) => JsonValue::String(t.clone()),
                    Arg::Jsonb(v) => v.clone(),
                    Arg::Int(i) => JsonValue::from(*i),
                })
                .collect();
            out.push(ScenarioMetadata {
                id,
                query: s.sql.clone(),
                parameters,
                indexes_used: extract_indexes_used(&explain.0),
                explain: explain.0,
                rows_returned: rows.len() as u64,
            });
        }
        out
    });
    write_metadata_file_in(&results_dir, "json_tree_nested", &target_rows, metadata)
        .expect("failed to write bench metadata sidecar");

    // --- Bench loop ---
    let mut group = c.benchmark_group("JSON_TREE_NESTED");
    group.sample_size(10);
    for s in &scenarios {
        let id = format!("JSON_TREE_NESTED/{}/{}", s.name, target_rows);
        let sql = s.sql.clone();
        let args = s.args.clone();
        group.bench_function(format!("{}/{}", s.name, target_rows), |b| {
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
