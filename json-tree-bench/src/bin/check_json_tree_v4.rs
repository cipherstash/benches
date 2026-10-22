//! Correctness check for the EQL v4 `json_tree` bench tables, run against a
//! prepared tier (`TARGET_ROWS`, default 10000). Not timed: it answers whether
//! what the benches measure returns the right thing.
//!
//! (a) Walks, nested corpus. For `CHECK_IDS` (default 25) rows chosen by a
//!     fixed pseudo-random order, it runs the bench's walks
//!     (`json_tree_subtree`, `json_tree_subtree_at`, `json_tree_children`,
//!     `json_tree_child_count`, `json_tree_path_query`, `->`), decrypts what
//!     they return, and compares it with the plaintext twin. The documents
//!     are sealed under `DeterministicSource`, so this process opens them.
//! (b) Containment and field predicates, both corpora. For `CHECK_NEEDLES`
//!     (default 10) sampled rows per predicate, the set of ids the encrypted
//!     query returns (no LIMIT) is compared with the equivalent native jsonb
//!     query over the plaintext twin; for `ORDER BY .. LIMIT 10` the ordered
//!     key values are compared, which is robust to ties.
//!
//! Prints one line per failure and a summary, writes the summary to
//! `$RESULTS_DIR/json_tree_correctness_<N>.json`, and exits non-zero on any
//! failure.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use eql_bindings::v4::json_tree::{
    JsonTreeEntry, JsonTreeLeafQuery, JsonTreeQuery, JsonTreeSubtree,
};
use json_tree_bench::{cipher, column, Cipher, Corpus, JsonPlan};
use serde_json::{json, Value};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use stack_encrypt::json::JsonPath;
use stack_encrypt::op;

const CONTAINS_CAST: &str = "::text::jsonb::eql_v4.query_json_tree";
const LEAF_CAST: &str = "::text::jsonb::eql_v4.query_json_tree_leaf";

#[derive(Default)]
struct Tally {
    by_check: BTreeMap<String, (u32, u32)>,
    failures: Vec<String>,
}

impl Tally {
    fn record(&mut self, check: &str, ok: bool, detail: impl FnOnce() -> String) {
        let e = self.by_check.entry(check.to_string()).or_default();
        if ok {
            e.0 += 1;
        } else {
            e.1 += 1;
            let msg = format!("FAIL {check}: {}", detail());
            eprintln!("{msg}");
            self.failures.push(msg);
        }
    }
    fn totals(&self) -> (u32, u32) {
        self.by_check
            .values()
            .fold((0, 0), |(p, f), (a, b)| (p + a, f + b))
    }
}

/// A subtree key for the nested corpus (the only corpus the walks run on),
/// under the options it was sealed with.
async fn subtree_key(cipher: &Cipher, path: &str) -> (String, Vec<String>) {
    let key = cipher
        .default_keyset()
        .json_subtree_key(&JsonPath::parse(path).unwrap(), &Corpus::Nested.options(), column())
        .await
        .expect("derive subtree key");
    (key.token(), key.gates())
}

async fn selector(cipher: &Cipher, path: &str) -> String {
    cipher
        .default_keyset()
        .json_selector(&JsonPath::parse(path).unwrap(), column())
        .await
        .expect("derive selector")
        .to_string()
}

fn entries(rows: Vec<sqlx::postgres::PgRow>) -> Vec<JsonTreeEntry> {
    rows.iter()
        .map(|r| serde_json::from_str(&r.get::<String, _>(0)).expect("an entry"))
        .collect()
}

/// `json_tree_subtree_at(value, token, gates, budget)` for one row.
async fn walk(
    pool: &PgPool,
    table: &str,
    id: i32,
    token: &str,
    gates: &[String],
    budget: Option<i32>,
) -> Result<Vec<JsonTreeEntry>> {
    let with_budget = format!(
        "SELECT e::jsonb::text FROM {table}, \
         eql_v4.json_tree_subtree_at(value, $1::text, $2::text[], $3::int) e WHERE id = $4"
    );
    let default_budget = format!(
        "SELECT e::jsonb::text FROM {table}, \
         eql_v4.json_tree_subtree_at(value, $1::text, $2::text[]) e WHERE id = $3"
    );
    let rows = match budget {
        Some(b) => sqlx::query(&with_budget).bind(token).bind(gates).bind(b).bind(id),
        None => sqlx::query(&default_budget).bind(token).bind(gates).bind(id),
    }
    .fetch_all(pool)
    .await?;
    Ok(entries(rows))
}

/// Whether a walk's rows are the budget fallback: the sealed root alone at d = 0.
fn is_fallback(rows: &[JsonTreeEntry]) -> bool {
    rows.len() == 1 && rows[0].d == Some(0) && rows[0].j == Some(0)
}

async fn open_one(cipher: &Cipher, rows: Vec<JsonTreeEntry>, path: &str) -> Result<Value> {
    let mut sub = JsonTreeSubtree::new(rows.clone());
    if is_fallback(&rows) {
        sub = sub.at(JsonPath::parse(path).unwrap());
    }
    Ok(cipher.decrypt_as::<Value, _>(sub, column()).await?)
}

async fn open_many(cipher: &Cipher, rows: Vec<JsonTreeEntry>, path: &str) -> Result<Vec<Value>> {
    let mut sub = JsonTreeSubtree::new(rows.clone());
    if is_fallback(&rows) {
        sub = sub.at(JsonPath::parse(path).unwrap());
    }
    Ok(cipher.decrypt_as::<Vec<Value>, _>(sub, column()).await?)
}

async fn ids(pool: &PgPool, sql: &str, binds: &[&str]) -> Result<Vec<i32>> {
    let mut q = sqlx::query(sql);
    for b in binds {
        q = q.bind(*b);
    }
    Ok(q.fetch_all(pool).await?.iter().map(|r| r.get(0)).collect())
}

async fn plain_ids(pool: &PgPool, sql: &str, needle: &Value) -> Result<Vec<i32>> {
    Ok(sqlx::query(sql)
        .bind(Json(needle))
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

async fn contains_operand(cipher: &Cipher, plan: &JsonPlan, needle: &Value) -> String {
    let q = cipher
        .query(op::Contains, needle)
        .on(plan)
        .await
        .expect("derive containment operand");
    serde_json::to_string(&JsonTreeQuery::try_from(&q).expect("typed operand")).unwrap()
}

async fn at_path_eq(cipher: &Cipher, plan: &JsonPlan, path: &'static str, v: Value) -> String {
    let q = cipher
        .query(op::AtPath(op::Eq), &(path, v))
        .on(plan)
        .await
        .expect("derive AtPath(Eq) operand");
    serde_json::to_string(&JsonTreeQuery::try_from(&q).expect("typed operand")).unwrap()
}

fn leaf(q: &stack_encrypt::json::JsonLeafQuery) -> String {
    serde_json::to_string(&JsonTreeLeafQuery::try_from(q).expect("typed leaf")).unwrap()
}

async fn sample(pool: &PgPool, plain: &str, n: i64, salt: &str) -> Result<Vec<(i32, Value)>> {
    Ok(sqlx::query(&format!(
        "SELECT id, plain FROM {plain} ORDER BY md5(id::text || $1) LIMIT $2"
    ))
    .bind(salt)
    .bind(n)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.get::<i32, _>(0), r.get::<Json<Value>, _>(1).0))
    .collect())
}

async fn check_walks(
    pool: &PgPool,
    cipher: &Cipher,
    tier: &str,
    n: i64,
    t: &mut Tally,
) -> Result<()> {
    let table = format!("json_tree_large_encrypted_v4_{tier}");
    let plain = format!("json_tree_large_plain_v4_{tier}");
    let rows = sample(pool, &plain, n, "walks").await?;

    let (tok_company, g_company) = subtree_key(cipher, "$.company").await;
    let (tok_user, g_user) = subtree_key(cipher, "$.user").await;
    let (tok_orders, g_orders) = subtree_key(cipher, "$.orders").await;
    let (tok_items, g_items) = subtree_key(cipher, "$.orders[*].items").await;
    let (tok_contact, g_contact) = subtree_key(cipher, "$.user.contact").await;
    let sel_status = selector(cipher, "$.orders[*].status").await;
    let sel_salary = selector(cipher, "$.company.salary").await;
    assert!(g_company.is_empty() && g_user.is_empty() && g_orders.is_empty());
    assert_eq!(tok_items, tok_orders);
    assert_eq!(g_items.len(), 2);
    assert_eq!(g_contact.len(), 1);

    for (id, doc) in &rows {
        let id = *id;
        // Subtrees at chain heads: walked out (budget 0) and under the default budget.
        for (name, path, tok, key) in [
            ("subtree/company", "$.company", &tok_company, "company"),
            ("subtree/user", "$.user", &tok_user, "user"),
            ("subtree/orders", "$.orders", &tok_orders, "orders"),
        ] {
            for (mode, budget) in [("walked", Some(0)), ("default_budget", None)] {
                let got = walk(pool, &table, id, tok, &[], budget).await?;
                let fell_back = is_fallback(&got);
                let v = open_one(cipher, got, path).await;
                let label = format!("{name} ({mode})");
                t.record(&label, matches!(&v, Ok(v) if *v == doc[key]), || {
                    format!("row {id}: fallback={fell_back} got {v:?}")
                });
            }
        }
        // A gated walk below a key step.
        let got = walk(pool, &table, id, &tok_contact, &g_contact, Some(0)).await?;
        let v = open_one(cipher, got, "$.user.contact").await;
        t.record("subtree_at/user.contact (walked)", matches!(&v, Ok(v) if *v == doc["user"]["contact"]), || {
            format!("row {id}: got {v:?}")
        });
        // The wildcard walk: every order's items, in order.
        let expected: Vec<Value> = doc["orders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["items"].clone())
            .collect();
        for (mode, budget) in [("walked", Some(0)), ("default_budget", None)] {
            let got = walk(pool, &table, id, &tok_items, &g_items, budget).await?;
            let fell_back = is_fallback(&got);
            let v = open_many(cipher, got, "$.orders[*].items").await;
            t.record(&format!("subtree_at/items ({mode})"), matches!(&v, Ok(v) if *v == expected), || {
                format!("row {id}: fallback={fell_back} got {v:?}")
            });
        }
        // Children and child count of `$.orders`, against the array.
        let orders = doc["orders"].as_array().unwrap();
        let children = entries(
            sqlx::query(&format!(
                "SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_children(value, $1::text) e WHERE id = $2"
            ))
            .bind(&tok_orders)
            .bind(id)
            .fetch_all(pool)
            .await?,
        );
        let count: i32 = sqlx::query(&format!(
            "SELECT eql_v4.json_tree_child_count(value, $1::text) FROM {table} WHERE id = $2"
        ))
        .bind(&tok_orders)
        .bind(id)
        .fetch_one(pool)
        .await?
        .get(0);
        t.record(
            "children/orders (count, d = 1)",
            children.len() == orders.len() && children.iter().all(|e| e.d == Some(1)),
            || format!("row {id}: {} children, {} orders", children.len(), orders.len()),
        );
        t.record("child_count/orders", count as usize == orders.len(), || {
            format!("row {id}: count {count}, {} orders", orders.len())
        });
        // Each child, by the position-derived token: `$.orders[k]` opens to the k-th order.
        let mut all = true;
        let mut detail = String::new();
        for (k, order) in orders.iter().enumerate() {
            let path = format!("$.orders[{k}]");
            let (tok, gates) = subtree_key(cipher, &path).await;
            let got = walk(pool, &table, id, &tok, &gates, Some(0)).await?;
            let v = open_one(cipher, got, &path).await;
            if !matches!(&v, Ok(v) if v == order) {
                all = false;
                detail = format!("row {id}: {path} got {v:?}");
            }
        }
        t.record("subtree/orders[k] (each child)", all, || detail);
        // The wildcard path query: every order's status, in order.
        let statuses: Vec<Value> = orders.iter().map(|o| o["status"].clone()).collect();
        let found = entries(
            sqlx::query(&format!(
                "SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_path_query(value::jsonb, $1::text) e WHERE id = $2"
            ))
            .bind(&sel_status)
            .bind(id)
            .fetch_all(pool)
            .await?,
        );
        let mut opened = Vec::new();
        for e in found {
            opened.push(cipher.decrypt_as::<Value, _>(e, column()).await);
        }
        let ok = opened.len() == statuses.len()
            && opened.iter().zip(&statuses).all(|(a, b)| matches!(a, Ok(a) if a == b));
        t.record("path_query/wildcard", ok, || format!("row {id}: got {opened:?}"));
        // `->` at a nested leaf.
        let e: Option<String> = sqlx::query(&format!(
            "SELECT (value -> '{sel_salary}'::text)::jsonb::text FROM {table} WHERE id = $1"
        ))
        .bind(id)
        .fetch_one(pool)
        .await?
        .get(0);
        let v = match e {
            Some(e) => Some(
                cipher
                    .decrypt_as::<Value, _>(serde_json::from_str::<JsonTreeEntry>(&e)?, column())
                    .await,
            ),
            None => None,
        };
        t.record("extract/nested", matches!(&v, Some(Ok(v)) if *v == doc["company"]["salary"]), || {
            format!("row {id}: got {v:?}")
        });
    }
    Ok(())
}

async fn check_nested_predicates(
    pool: &PgPool,
    cipher: &Cipher,
    plan: &JsonPlan,
    tier: &str,
    n: i64,
    t: &mut Tally,
) -> Result<()> {
    let table = format!("json_tree_large_encrypted_v4_{tier}");
    let plain = format!("json_tree_large_plain_v4_{tier}");
    let enc_sql = format!("SELECT id FROM {table} WHERE value @> $1{CONTAINS_CAST} ORDER BY id");
    let plain_sql = format!("SELECT id FROM {plain} WHERE plain @> $1 ORDER BY id");
    let sel_salary = selector(cipher, "$.company.salary").await;

    for (id, s) in sample(pool, &plain, n, "nested-needles").await? {
        let address = &s["addresses"][0];
        let order = &s["orders"][0];
        let needles = [
            ("nested contains/deep_object", json!({"user": {"contact": {"emergency_contact": {
                "relationship": s["user"]["contact"]["emergency_contact"]["relationship"]}}}})),
            ("nested contains/array_element",
             json!({"addresses": [{"city": address["city"], "state": address["state"]}]})),
            ("nested contains/nested_array", json!({"orders": [{"status": order["status"],
                "items": [{"product": order["items"][0]["product"]}]}]})),
        ];
        for (name, needle) in needles {
            let enc = ids(pool, &enc_sql, &[&contains_operand(cipher, plan, &needle).await]).await?;
            let pl = plain_ids(pool, &plain_sql, &needle).await?;
            t.record(name, enc == pl && enc.contains(&id), || {
                format!("row {id}: encrypted {} ids, plain {} ids", enc.len(), pl.len())
            });
        }
        let email = at_path_eq(cipher, plan, "$.user.email", s["user"]["email"].clone()).await;
        let enc = ids(pool, &enc_sql, &[&email]).await?;
        let pl = plain_ids(pool, &plain_sql, &json!({"user": {"email": s["user"]["email"]}})).await?;
        t.record("nested field_eq/nested", enc == pl && enc.contains(&id), || {
            format!("row {id}: encrypted {enc:?}, plain {pl:?}")
        });
        let salary = s["company"]["salary"].as_i64().unwrap();
        let gt = cipher
            .query(op::AtPath(op::Gt), &("$.company.salary", salary))
            .on(plan)
            .await?;
        let enc = ids(
            pool,
            &format!("SELECT id FROM {table} WHERE (value -> '{sel_salary}'::text) > $1{LEAF_CAST} ORDER BY id"),
            &[&leaf(&gt)],
        )
        .await?;
        let pl: Vec<i32> = sqlx::query(&format!(
            "SELECT id FROM {plain} WHERE (plain -> 'company' ->> 'salary')::int > $1 ORDER BY id"
        ))
        .bind(salary as i32)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
        t.record("nested field_gt/nested", enc == pl, || {
            format!("row {id}: encrypted {} ids, plain {} ids", enc.len(), pl.len())
        });
    }
    // ORDER BY at a path: the ten smallest salaries, in order.
    let enc: Vec<i64> = sqlx::query(&format!(
        "SELECT (p.plain -> 'company' ->> 'salary')::bigint FROM \
         (SELECT id FROM {table} ORDER BY eql_v4.ord_term(value -> '{sel_salary}'::text) LIMIT 10) e \
         JOIN {plain} p USING (id)"
    ))
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect();
    let mut enc_sorted = enc.clone();
    enc_sorted.sort();
    let pl: Vec<i64> = sqlx::query(&format!(
        "SELECT (plain -> 'company' ->> 'salary')::bigint s FROM {plain} ORDER BY s LIMIT 10"
    ))
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect();
    t.record("nested field_order/nested (top 10)", enc_sorted == pl, || {
        format!("encrypted {enc:?}, plain {pl:?}")
    });
    Ok(())
}

async fn check_flat_predicates(
    pool: &PgPool,
    cipher: &Cipher,
    plan: &JsonPlan,
    tier: &str,
    n: i64,
    t: &mut Tally,
) -> Result<()> {
    let table = format!("json_tree_small_encrypted_v4_{tier}");
    let plain = format!("json_tree_small_plain_v4_{tier}");
    let enc_sql = format!(
        "SELECT id FROM {table} WHERE eql_v4.json_tree_contains(value, $1{CONTAINS_CAST}) ORDER BY id"
    );
    let op_sql = format!("SELECT id FROM {table} WHERE value @> $1{CONTAINS_CAST} ORDER BY id");
    let plain_sql = format!("SELECT id FROM {plain} WHERE plain @> $1 ORDER BY id");
    let sel_age = selector(cipher, "$.age").await;

    for (id, s) in sample(pool, &plain, n, "flat-needles").await? {
        let doc_q = contains_operand(cipher, plan, &s).await;
        let pl = plain_ids(pool, &plain_sql, &s).await?;
        let enc = ids(pool, &enc_sql, &[&doc_q]).await?;
        t.record("flat contains/functional", enc == pl && enc.contains(&id), || {
            format!("row {id}: encrypted {enc:?}, plain {pl:?}")
        });
        let enc = ids(pool, &op_sql, &[&doc_q]).await?;
        t.record("flat contains/operator", enc == pl && enc.contains(&id), || {
            format!("row {id}: encrypted {enc:?}, plain {pl:?}")
        });
        for (name, path, key) in [
            ("flat field_eq/extractor ($.first_name)", "$.first_name", "first_name"),
            ("flat field_eq/unique ($.email)", "$.email", "email"),
        ] {
            let q = at_path_eq(cipher, plan, path, s[key].clone()).await;
            let enc = ids(pool, &enc_sql, &[&q]).await?;
            let pl = plain_ids(pool, &plain_sql, &json!({key: s[key]})).await?;
            t.record(name, enc == pl && enc.contains(&id), || {
                format!("row {id}: encrypted {} ids, plain {} ids", enc.len(), pl.len())
            });
        }
        let age = s["age"].as_i64().unwrap();
        let gt = cipher.query(op::AtPath(op::Gt), &("$.age", age)).on(plan).await?;
        let enc = ids(
            pool,
            &format!("SELECT id FROM {table} WHERE (value -> '{sel_age}'::text) > $1{LEAF_CAST} ORDER BY id"),
            &[&leaf(&gt)],
        )
        .await?;
        let pl: Vec<i32> = sqlx::query(&format!(
            "SELECT id FROM {plain} WHERE (plain ->> 'age')::int > $1 ORDER BY id"
        ))
        .bind(age as i32)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
        t.record("flat field_gt/functional", enc == pl, || {
            format!("row {id}: encrypted {} ids, plain {} ids", enc.len(), pl.len())
        });
        let (lo, hi) = ((age - 5).max(18), (age + 5).min(99));
        let between = cipher
            .query(op::AtPath(op::Between), &("$.age", (lo, hi)))
            .on(plan)
            .await?;
        let enc = ids(
            pool,
            &format!(
                "SELECT id FROM {table} WHERE (value -> '{sel_age}'::text) >= $1{LEAF_CAST} \
                 AND (value -> '{sel_age}'::text) <= $2{LEAF_CAST} ORDER BY id"
            ),
            &[&leaf(between.lo()), &leaf(between.hi())],
        )
        .await?;
        let pl: Vec<i32> = sqlx::query(&format!(
            "SELECT id FROM {plain} WHERE (plain ->> 'age')::int BETWEEN $1 AND $2 ORDER BY id"
        ))
        .bind(lo as i32)
        .bind(hi as i32)
        .fetch_all(pool)
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
        t.record("flat field_between/functional", enc == pl, || {
            format!("row {id}: encrypted {} ids, plain {} ids", enc.len(), pl.len())
        });
    }
    // ORDER BY at a path: the ages of the first ten rows the index returns.
    let enc: Vec<i64> = sqlx::query(&format!(
        "SELECT (p.plain ->> 'age')::bigint FROM \
         (SELECT id FROM {table} ORDER BY eql_v4.ord_term(value -> '{sel_age}'::text) LIMIT 10) e \
         JOIN {plain} p USING (id)"
    ))
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect();
    let pl: Vec<i64> = sqlx::query(&format!(
        "SELECT (plain ->> 'age')::bigint a FROM {plain} ORDER BY a LIMIT 10"
    ))
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect();
    let mut enc_sorted = enc.clone();
    enc_sorted.sort();
    t.record("flat field_order/functional (top 10)", enc_sorted == pl, || {
        format!("encrypted {enc:?}, plain {pl:?}")
    });
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
    let tier = std::env::var("TARGET_ROWS").unwrap_or_else(|_| "10000".into());
    let n_ids: i64 = std::env::var("CHECK_IDS").unwrap_or_else(|_| "25".into()).parse()?;
    let n_needles: i64 = std::env::var("CHECK_NEEDLES").unwrap_or_else(|_| "10".into()).parse()?;
    let results_dir =
        std::env::var("RESULTS_DIR").unwrap_or_else(|_| "../results/query/v4".to_string());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect(&database_url)
        .await?;
    let cipher = cipher().await;
    // Each corpus is queried through the plan it was sealed under: `$.age`
    // ordered on the flat corpus, `$.company.salary` on the nested one.
    let nested_plan = Corpus::Nested.plan();
    let flat_plan = Corpus::Flat.plan();
    let mut t = Tally::default();

    check_walks(&pool, &cipher, &tier, n_ids, &mut t).await?;
    check_nested_predicates(&pool, &cipher, &nested_plan, &tier, n_needles, &mut t).await?;
    check_flat_predicates(&pool, &cipher, &flat_plan, &tier, n_needles, &mut t).await?;

    let (pass, fail) = t.totals();
    println!("json_tree v4 correctness, {tier} rows: {pass} passed, {fail} failed");
    for (check, (p, f)) in &t.by_check {
        println!("  {check:<45} {p:>4} pass {f:>4} fail");
    }
    std::fs::create_dir_all(&results_dir)?;
    let summary = json!({
        "target_rows": tier,
        "walk_rows_checked": n_ids,
        "needle_rows_per_predicate": n_needles,
        "passed": pass,
        "failed": fail,
        "checks": t.by_check.iter().map(|(k, (p, f))| (k.clone(), json!({"pass": p, "fail": f}))).collect::<serde_json::Map<_, _>>(),
        "failures": t.failures,
    });
    let path = format!("{results_dir}/json_tree_correctness_{tier}.json");
    std::fs::write(&path, serde_json::to_string_pretty(&summary)?)?;
    eprintln!("written to {path}");
    if fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}
