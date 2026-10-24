//! Correctness check for the EQL v4 `json_tree` bench tables, run against a
//! prepared tier (`TARGET_ROWS`, default 10000), in whichever stored format
//! the crate is built for (`derivable` or `gates`, see Cargo.toml). Not
//! timed: it answers whether what the benches measure returns the right
//! thing.
//!
//! (a) Reads, nested corpus. For `CHECK_IDS` (default 25) rows chosen by a
//!     fixed pseudo-random order, it runs the subtree reads the benches time
//!     (`json_tree_subtree` with a token list in the derivable format,
//!     `json_tree_subtree_at` with a token and gates in the gates format;
//!     see `ReadKey`), `json_tree_children`, `json_tree_child_count`,
//!     `json_tree_path_query` and `->`, opens what they return through the
//!     key that derived the read, and compares it with the plaintext twin.
//!     The documents are sealed under `DeterministicSource`, so this process
//!     opens them.
//! (b) Containment and field predicates, both corpora. For `CHECK_NEEDLES`
//!     (default 10) sampled rows per predicate, the set of ids the encrypted
//!     query returns (no LIMIT) is compared with the equivalent native jsonb
//!     query over the plaintext twin; for `ORDER BY .. LIMIT 10` the ordered
//!     key values are compared, which is robust to ties.
//! (c) Where they exist, the open-root nested table
//!     (`json_tree_large_open_encrypted_v4_<TARGET_ROWS>`: a read of `$`) and
//!     the wide table (`json_tree_wide_encrypted_v4_<WIDE_ROWS>`, default
//!     1000: children, child count and reads of a 300-member object and a
//!     300-element array).
//!
//! Prints one line per failure and a summary, writes the summary to
//! `$RESULTS_DIR/json_tree_correctness_<N>[_gates].json`, and exits non-zero
//! on any failure.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use eql_bindings::v4::json_tree::{
    JsonTreeEntry, JsonTreeExtracted, JsonTreeLeafQuery, JsonTreeQuery, JsonTreeSubtree,
};
use json_tree_bench::{
    cipher, column, eql_bindings, query_with, stack_encrypt, Arg, Cipher, Corpus, JsonPlan,
    ReadKey, FORMAT,
};
use serde_json::{json, Value};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use stack_encrypt::json::{JsonPath, JsonSelector};
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

async fn json_selector(cipher: &Cipher, path: &str) -> JsonSelector {
    cipher
        .default_keyset()
        .json_selector(&JsonPath::parse(path).unwrap(), column())
        .await
        .expect("derive selector")
}

async fn selector(cipher: &Cipher, path: &str) -> String {
    json_selector(cipher, path).await.to_string()
}

fn entries(rows: Vec<sqlx::postgres::PgRow>) -> Vec<JsonTreeEntry> {
    rows.iter()
        .map(|r| serde_json::from_str(&r.get::<String, _>(0)).expect("an entry"))
        .collect()
}

/// The subtree read `key` derives, for one row.
async fn read(
    pool: &PgPool,
    table: &str,
    id: i32,
    key: &ReadKey,
    budget: Option<i32>,
) -> Result<Vec<JsonTreeEntry>> {
    let (from, mut args) = key.read(budget);
    let sql = format!("SELECT e::jsonb::text FROM {table}, {from} WHERE id = ${}", args.len() + 1);
    args.push(Arg::Int(id));
    Ok(entries(query_with(&sql, &args).fetch_all(pool).await?))
}

/// Whether a read's rows are the budget fallback: the sealed root alone at d = 0.
fn is_fallback(rows: &[JsonTreeEntry]) -> bool {
    rows.len() == 1 && rows[0].d == Some(0) && rows[0].j == Some(0)
}

async fn open_one(cipher: &Cipher, key: &ReadKey, rows: Vec<JsonTreeEntry>) -> Result<Value> {
    let sub = JsonTreeSubtree::from_key(&key.key, rows);
    Ok(cipher.decrypt_as::<Value, _>(sub, column()).await?)
}

async fn open_many(cipher: &Cipher, key: &ReadKey, rows: Vec<JsonTreeEntry>) -> Result<Vec<Value>> {
    let sub = JsonTreeSubtree::from_key(&key.key, rows);
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

/// Read `key` on row `id` under `budget` and record whether it opens to `want`.
#[allow(clippy::too_many_arguments)]
async fn check_one(
    pool: &PgPool,
    cipher: &Cipher,
    table: &str,
    id: i32,
    key: &ReadKey,
    budget: Option<i32>,
    want: &Value,
    label: &str,
    t: &mut Tally,
) -> Result<()> {
    let got = read(pool, table, id, key, budget).await?;
    let fell_back = is_fallback(&got);
    let v = open_one(cipher, key, got).await;
    t.record(label, matches!(&v, Ok(v) if v == want), || {
        format!("row {id}: fallback={fell_back} got {v:?}")
    });
    Ok(())
}

/// The wildcard read `key` on row `id`: opens to `want` when every array it
/// crosses is within the bound (`fits`), and refuses otherwise (derivable
/// format only: the gates format has no bound).
#[allow(clippy::too_many_arguments)]
async fn check_many(
    pool: &PgPool,
    cipher: &Cipher,
    table: &str,
    id: i32,
    key: &ReadKey,
    budget: Option<i32>,
    want: &[Value],
    fits: bool,
    label: &str,
    t: &mut Tally,
) -> Result<()> {
    let got = read(pool, table, id, key, budget).await?;
    let fell_back = is_fallback(&got);
    let v = open_many(cipher, key, got).await;
    let ok = if fits { matches!(&v, Ok(v) if v == want) } else { v.is_err() };
    t.record(label, ok, || format!("row {id}: fits={fits} fallback={fell_back} got {v:?}"));
    Ok(())
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
    let options = Corpus::Nested.options();
    let key = |path: &'static str, bound: Option<u32>| {
        let options = options.clone();
        async move { ReadKey::derive(cipher, path, &options, bound).await }
    };

    let key_company = key("$.company", None).await;
    let key_user = key("$.user", None).await;
    let key_orders = key("$.orders", None).await;
    let key_items = key("$.orders[*].items", None).await;
    let key_contact = key("$.user.contact", None).await;
    let key_ec_name = key("$.user.contact.emergency_contact.name", None).await;
    let key_company_name = key("$.company.name", None).await;
    let key_totals_8 = key("$.orders[*].total", Some(8)).await;
    let key_totals_64 = key("$.orders[*].total", Some(64)).await;
    let key_prices_8 = key("$.orders[*].items[*].price", Some(8)).await;
    let key_prices_64 = key("$.orders[*].items[*].price", Some(64)).await;
    let sel_status = json_selector(cipher, "$.orders[*].status").await;
    let sel_salary = json_selector(cipher, "$.company.salary").await;
    #[cfg(feature = "derivable")]
    let key_root = key("$", None).await;

    for (id, doc) in &rows {
        let id = *id;
        // Top-level subtrees: walked out (budget 0) and under the default budget.
        for (name, k, field) in [
            ("subtree/company", &key_company, "company"),
            ("subtree/user", &key_user, "user"),
            ("subtree/orders", &key_orders, "orders"),
        ] {
            for (mode, budget) in [("walked", Some(0)), ("default_budget", None)] {
                check_one(pool, cipher, &table, id, k, budget, &doc[field], &format!("{name} ({mode})"), t).await?;
            }
        }
        // Member paths below a top-level field.
        check_one(pool, cipher, &table, id, &key_contact, Some(0), &doc["user"]["contact"], "subtree/user.contact (walked)", t).await?;
        check_one(pool, cipher, &table, id, &key_ec_name, None,
            &doc["user"]["contact"]["emergency_contact"]["name"], "subtree/user.contact.emergency_contact.name", t).await?;
        check_one(pool, cipher, &table, id, &key_company_name, None, &doc["company"]["name"], "subtree/company.name", t).await?;
        // The wildcard read: every order's items, in order.
        let orders = doc["orders"].as_array().unwrap();
        let items: Vec<Value> = orders.iter().map(|o| o["items"].clone()).collect();
        for (mode, budget) in [("walked", Some(0)), ("default_budget", None)] {
            check_many(pool, cipher, &table, id, &key_items, budget, &items, true, &format!("subtree/orders[*].items ({mode})"), t).await?;
        }
        // Wildcards at a bound: in the derivable format a row whose arrays
        // pass the bound refuses (`JsonWildcardTruncated`); the gates format
        // has no bound and always answers.
        let gates = cfg!(feature = "gates");
        let totals: Vec<Value> = orders.iter().map(|o| o["total"].clone()).collect();
        let prices: Vec<Value> = orders
            .iter()
            .flat_map(|o| o["items"].as_array().unwrap().iter().map(|i| i["price"].clone()))
            .collect();
        let widest_items = orders.iter().map(|o| o["items"].as_array().unwrap().len()).max().unwrap_or(0);
        check_many(pool, cipher, &table, id, &key_totals_8, None, &totals, gates || orders.len() <= 8, "wildcard/orders[*].total (N=8)", t).await?;
        check_many(pool, cipher, &table, id, &key_totals_64, None, &totals, true, "wildcard/orders[*].total (N=64)", t).await?;
        check_many(pool, cipher, &table, id, &key_prices_8, Some(0), &prices, gates || (orders.len() <= 8 && widest_items <= 8), "wildcard/orders[*].items[*].price (N=8)", t).await?;
        check_many(pool, cipher, &table, id, &key_prices_64, Some(0), &prices, true, "wildcard/orders[*].items[*].price (N=64)", t).await?;
        // `$` under the sealed root: the root entry, which opens to the whole
        // document. The gates format refuses the key (`JsonSealedRoot`).
        #[cfg(feature = "derivable")]
        check_one(pool, cipher, &table, id, &key_root, None, doc, "subtree/$ (sealed root)", t).await?;
        // Children and child count of `$.orders`, against the array.
        let (children_from, mut cargs) = key_orders.children();
        cargs.push(Arg::Int(id));
        let children = entries(
            query_with(
                &format!("SELECT e::jsonb::text FROM {table}, {children_from} WHERE id = ${}", cargs.len()),
                &cargs,
            )
            .fetch_all(pool)
            .await?,
        );
        let (count_expr, mut nargs) = key_orders.child_count();
        nargs.push(Arg::Int(id));
        let count: Option<i32> = query_with(
            &format!("SELECT {count_expr} FROM {table} WHERE id = ${}", nargs.len()),
            &nargs,
        )
        .fetch_one(pool)
        .await?
        .get(0);
        t.record(
            "children/orders (count, d = 1)",
            children.len() == orders.len() && children.iter().all(|e| e.d == Some(1)),
            || format!("row {id}: {} children, {} orders", children.len(), orders.len()),
        );
        t.record("child_count/orders", count == Some(orders.len() as i32), || {
            format!("row {id}: count {count:?}, {} orders", orders.len())
        });
        // Each child, by its position, and a key below a position.
        let mut all = true;
        let mut detail = String::new();
        for (k, order) in orders.iter().enumerate() {
            for (path, want) in [
                (format!("$.orders[{k}]"), order),
                (format!("$.orders[{k}].status"), &order["status"]),
            ] {
                let path: &'static str = Box::leak(path.into_boxed_str());
                let rk = key(path, None).await;
                let got = read(pool, &table, id, &rk, Some(0)).await?;
                let v = open_one(cipher, &rk, got).await;
                if !matches!(&v, Ok(v) if v == want) {
                    all = false;
                    detail = format!("row {id}: {path} got {v:?}");
                }
            }
        }
        t.record("subtree/orders[k] and orders[k].status (each child)", all, || detail);
        // The wildcard path query: every order's status, in order.
        let statuses: Vec<Value> = orders.iter().map(|o| o["status"].clone()).collect();
        let found = entries(
            sqlx::query(&format!(
                "SELECT e::jsonb::text FROM {table}, eql_v4.json_tree_path_query(value::jsonb, $1::text) e WHERE id = $2"
            ))
            .bind(sel_status.to_string())
            .bind(id)
            .fetch_all(pool)
            .await?,
        );
        let mut opened = Vec::new();
        for e in found {
            let e = JsonTreeExtracted::from_selector(&sel_status, e);
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
            Some(e) => {
                let e = JsonTreeExtracted::from_selector(&sel_salary, serde_json::from_str::<JsonTreeEntry>(&e)?);
                Some(cipher.decrypt_as::<Value, _>(e, column()).await)
            }
            None => None,
        };
        t.record("extract/nested", matches!(&v, Some(Ok(v)) if *v == doc["company"]["salary"]), || {
            format!("row {id}: got {v:?}")
        });
    }
    Ok(())
}

async fn table_exists(pool: &PgPool, table: &str) -> Result<bool> {
    Ok(sqlx::query("SELECT to_regclass($1) IS NOT NULL")
        .bind(table)
        .fetch_one(pool)
        .await?
        .get(0))
}

/// The open-root nested table (a read of `$`) and the wide table (children,
/// child count and full reads of a wide object and a wide array), each if it
/// exists.
async fn check_extra(pool: &PgPool, cipher: &Cipher, tier: &str, wide_rows: &str, n: i64, t: &mut Tally) -> Result<()> {
    let open = format!("json_tree_large_open_encrypted_v4_{tier}");
    if table_exists(pool, &open).await? {
        let plain = format!("json_tree_large_open_plain_v4_{tier}");
        let options = Corpus::NestedOpen.options();
        let key_root = ReadKey::derive(cipher, "$", &options, None).await;
        let key_ec = ReadKey::derive(cipher, "$.user.contact.emergency_contact.name", &options, None).await;
        for (id, doc) in sample(pool, &plain, n, "open").await? {
            check_one(pool, cipher, &open, id, &key_root, None, &doc, "open/$ (root open)", t).await?;
            check_one(pool, cipher, &open, id, &key_ec, None,
                &doc["user"]["contact"]["emergency_contact"]["name"], "open/user.contact.emergency_contact.name", t).await?;
        }
    } else {
        eprintln!("no {open}: open-root checks skipped");
    }
    let wide = format!("json_tree_wide_encrypted_v4_{wide_rows}");
    if table_exists(pool, &wide).await? {
        let plain = format!("json_tree_wide_plain_v4_{wide_rows}");
        let options = Corpus::Wide.options();
        for (id, doc) in sample(pool, &plain, n, "wide").await? {
            for field in ["attrs", "list"] {
                let path: &'static str = if field == "attrs" { "$.attrs" } else { "$.list" };
                let k = ReadKey::derive(cipher, path, &options, None).await;
                check_one(pool, cipher, &wide, id, &k, Some(0), &doc[field], &format!("wide/{field} (walked)"), t).await?;
                let want = match &doc[field] {
                    Value::Object(m) => m.len(),
                    Value::Array(a) => a.len(),
                    _ => unreachable!(),
                };
                let (from, mut args) = k.children();
                args.push(Arg::Int(id));
                let n_children = query_with(
                    &format!("SELECT e::jsonb::text FROM {wide}, {from} WHERE id = ${}", args.len()),
                    &args,
                )
                .fetch_all(pool)
                .await?
                .len();
                let (expr, mut args) = k.child_count();
                args.push(Arg::Int(id));
                let count: Option<i32> = query_with(&format!("SELECT {expr} FROM {wide} WHERE id = ${}", args.len()), &args)
                    .fetch_one(pool)
                    .await?
                    .get(0);
                t.record(&format!("wide/children/{field}"), n_children == want, || {
                    format!("row {id}: {n_children} children, want {want}")
                });
                t.record(&format!("wide/child_count/{field}"), count == Some(want as i32), || {
                    format!("row {id}: count {count:?}, want {want}")
                });
            }
        }
    } else {
        eprintln!("no {wide}: wide checks skipped");
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
    let wide_rows = std::env::var("WIDE_ROWS").unwrap_or_else(|_| "1000".into());
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
    check_extra(&pool, &cipher, &tier, &wide_rows, n_ids.min(10), &mut t).await?;

    let (pass, fail) = t.totals();
    println!("json_tree v4 correctness ({FORMAT}), {tier} rows: {pass} passed, {fail} failed");
    for (check, (p, f)) in &t.by_check {
        println!("  {check:<45} {p:>4} pass {f:>4} fail");
    }
    std::fs::create_dir_all(&results_dir)?;
    let summary = json!({
        "target_rows": tier,
        "format": FORMAT,
        "walk_rows_checked": n_ids,
        "needle_rows_per_predicate": n_needles,
        "passed": pass,
        "failed": fail,
        "checks": t.by_check.iter().map(|(k, (p, f))| (k.clone(), json!({"pass": p, "fail": f}))).collect::<serde_json::Map<_, _>>(),
        "failures": t.failures,
    });
    let suffix = if FORMAT == "gates" { "_gates" } else { "" };
    let path = format!("{results_dir}/json_tree_correctness_{tier}{suffix}.json");
    std::fs::write(&path, serde_json::to_string_pretty(&summary)?)?;
    eprintln!("written to {path}");
    if fail > 0 {
        std::process::exit(1);
    }
    Ok(())
}
