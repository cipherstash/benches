//! Correctness of the SteVec (EQL v3) side of the three-way JSON comparison,
//! and of the plaintext bench queries, against the plaintext control.
//!
//! For `CHECK_NEEDLES` (default 10) rows of each corpus at `TARGET_ROWS`,
//! chosen by a fixed pseudo-random order, every predicate scenario of
//! `benches/json_compare_v3.rs` is run WITHOUT its `LIMIT` and its id set
//! compared with the control: the semantic definition over the plaintext
//! twin (`plain @> needle` for containment, `plain #>> path` compared as an
//! integer or text for the field predicates). For `ORDER BY … LIMIT 10` the
//! plaintext keys of the returned rows are compared, which is robust to
//! ties. The plaintext bench's own query forms are checked against the same
//! control where they differ from it (field equality is `@>` in the bench,
//! `#>>` equality in the control).
//!
//! The by-id scenarios on the nested corpus (`->` on a container selector,
//! the wildcard path queries) are checked by decrypting what they return and
//! comparing it with the twin's value at that path, as a multiset.
//!
//! Writes `results/query/compare/stevec_correctness_<N>.json` and prints one
//! line per disagreement; exits zero either way (a disagreement is a
//! finding, not a harness error).

use anyhow::{Context, Result};
use cipherstash_client::encryption::{Plaintext, ScopedCipher};
use cipherstash_client::eql::decrypt_eql;
use cipherstash_client::AutoStrategy;
use dbbenches::twin::{tables, V3Operands};
use dbbenches::v3::v3_row_to_v2_ciphertext;
use serde_json::{json, Value};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

type Cipher = Arc<ScopedCipher<AutoStrategy>>;

#[derive(Default)]
struct Tally {
    by_check: BTreeMap<String, Value>,
    failures: Vec<String>,
}

impl Tally {
    fn ids(&mut self, check: &str, row: i32, got: &BTreeSet<i32>, want: &BTreeSet<i32>) {
        let extra = got.difference(want).count();
        let missing = want.difference(got).count();
        let e = self.by_check.entry(check.to_string()).or_insert_with(|| {
            json!({"needles": 0, "agree": 0, "disagree": 0, "extra_rows": 0, "missing_rows": 0, "control_rows": 0})
        });
        e["needles"] = json!(e["needles"].as_u64().unwrap() + 1);
        e["control_rows"] = json!(e["control_rows"].as_u64().unwrap() + want.len() as u64);
        e["extra_rows"] = json!(e["extra_rows"].as_u64().unwrap() + extra as u64);
        e["missing_rows"] = json!(e["missing_rows"].as_u64().unwrap() + missing as u64);
        if extra == 0 && missing == 0 {
            e["agree"] = json!(e["agree"].as_u64().unwrap() + 1);
        } else {
            e["disagree"] = json!(e["disagree"].as_u64().unwrap() + 1);
            let msg = format!(
                "DISAGREE {check}: needle row {row}: {} ids, control {} ids ({extra} extra, {missing} missing)",
                got.len(),
                want.len()
            );
            eprintln!("{msg}");
            self.failures.push(msg);
        }
    }

    fn same(&mut self, check: &str, row: i32, ok: bool, detail: impl FnOnce() -> String) {
        let e = self
            .by_check
            .entry(check.to_string())
            .or_insert_with(|| json!({"needles": 0, "agree": 0, "disagree": 0}));
        e["needles"] = json!(e["needles"].as_u64().unwrap() + 1);
        if ok {
            e["agree"] = json!(e["agree"].as_u64().unwrap() + 1);
        } else {
            e["disagree"] = json!(e["disagree"].as_u64().unwrap() + 1);
            let msg = format!("DISAGREE {check}: row {row}: {}", detail());
            eprintln!("{msg}");
            self.failures.push(msg);
        }
    }
}

async fn ids_text(pool: &PgPool, sql: &str, binds: &[&str]) -> Result<BTreeSet<i32>> {
    let q = binds.iter().fold(sqlx::query(sql), |q, b| q.bind(*b));
    Ok(q.fetch_all(pool)
        .await
        .with_context(|| sql.to_string())?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

async fn ids_json(pool: &PgPool, sql: &str, v: &Value) -> Result<BTreeSet<i32>> {
    Ok(sqlx::query(sql)
        .bind(Json(v))
        .fetch_all(pool)
        .await
        .with_context(|| sql.to_string())?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

async fn ids_ints(pool: &PgPool, sql: &str, binds: &[i32]) -> Result<BTreeSet<i32>> {
    let q = binds.iter().fold(sqlx::query(sql), |q, b| q.bind(*b));
    Ok(q.fetch_all(pool)
        .await
        .with_context(|| sql.to_string())?
        .iter()
        .map(|r| r.get(0))
        .collect())
}

async fn entry_at(pool: &PgPool, table: &str, sel: &str, id: i32) -> Result<String> {
    let r = sqlx::query(&format!(
        "SELECT eql_v3.jsonb_path_query_first(value, '{sel}')::jsonb::text FROM {table} WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(pool)
    .await?;
    r.get::<Option<String>, _>(0)
        .with_context(|| format!("row {id} has no entry at {sel}"))
}

async fn id_with(pool: &PgPool, twin: &str, path: &str, v: i64) -> Result<i32> {
    Ok(sqlx::query(&format!(
        "SELECT id FROM {twin} WHERE (plain #>> '{path}')::int = $1 ORDER BY id LIMIT 1"
    ))
    .bind(v as i32)
    .fetch_one(pool)
    .await
    .with_context(|| format!("no row with {path} = {v}"))?
    .get(0))
}

fn to_json(pt: &Plaintext) -> Value {
    match pt {
        Plaintext::Json(Some(v)) => v.clone(),
        Plaintext::Text(Some(s)) => json!(s),
        Plaintext::Int(Some(i)) => json!(i),
        Plaintext::BigInt(Some(i)) => json!(i),
        Plaintext::SmallInt(Some(i)) => json!(i),
        Plaintext::Float(Some(f)) => json!(f),
        Plaintext::Boolean(Some(b)) => json!(b),
        other => json!(format!("{other:?}")),
    }
}

/// Decrypt entries (`public.jsonb_entry` rows as jsonb) to their plaintexts.
async fn open_entries(cipher: &Cipher, entries: Vec<Value>) -> Result<Vec<Value>> {
    if entries.is_empty() {
        return Ok(vec![]);
    }
    let cts = entries
        .iter()
        .map(v3_row_to_v2_ciphertext)
        .collect::<Result<Vec<_>>>()?;
    Ok(decrypt_eql(cipher.clone(), cts, &Default::default())
        .await?
        .iter()
        .map(to_json)
        .collect())
}

fn multiset(v: &[Value]) -> Vec<String> {
    let mut s: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    s.sort();
    s
}

async fn sample(pool: &PgPool, twin: &str, n: i64, salt: &str) -> Result<Vec<(i32, Value)>> {
    Ok(sqlx::query(&format!(
        "SELECT id, plain FROM {twin} ORDER BY md5(id::text || $1) LIMIT $2"
    ))
    .bind(salt)
    .bind(n)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| (r.get("id"), r.get::<Json<Value>, _>("plain").0))
    .collect())
}

async fn check_flat(pool: &PgPool, cipher: &Cipher, tier: &str, n: i64, t: &mut Tally) -> Result<()> {
    let (table, twin) = tables("small", &format!("_{tier}"));
    let ops = V3Operands::new(cipher.clone(), &table);
    let sel_age = ops.selector("$.age").await?;
    let sel_email = ops.selector("$.email").await?;
    let fn_sql = format!("SELECT id FROM {table} WHERE eql_v3.jsonb_contains(value, $1::jsonb)");
    let op_sql = format!("SELECT id FROM {table} WHERE value @> $1::jsonb::eql_v3.query_jsonb");
    let pl_contains = format!("SELECT id FROM {twin} WHERE plain @> $1");
    for (id, s) in sample(pool, &twin, n, "flat-v3").await? {
        let want = ids_json(pool, &pl_contains, &s).await?;
        let needle = ops.containment(&s).await?;
        t.ids("flat/stevec/contains/functional", id, &ids_text(pool, &fn_sql, &[&needle]).await?, &want);
        t.ids("flat/stevec/contains/operator", id, &ids_text(pool, &op_sql, &[&needle]).await?, &want);

        for key in ["first_name", "email"] {
            let label = if key == "email" { "unique" } else { "extractor" };
            let want: BTreeSet<i32> = ids_text(
                pool,
                &format!("SELECT id FROM {twin} WHERE plain ->> '{key}' = $1"),
                &[s[key].as_str().unwrap()],
            )
            .await?;
            let needle = json!({key: s[key]});
            let enc = ops.containment(&needle).await?;
            t.ids(&format!("flat/stevec/field_eq/{label}"), id, &ids_text(pool, &fn_sql, &[&enc]).await?, &want);
            t.ids(&format!("flat/plain/field_eq/{label}"), id, &ids_json(pool, &pl_contains, &needle).await?, &want);
        }
        let want_email = ids_text(
            pool,
            &format!("SELECT id FROM {twin} WHERE plain ->> 'email' = $1"),
            &[s["email"].as_str().unwrap()],
        )
        .await?;
        let email_entry = entry_at(pool, &table, &sel_email, id).await?;
        t.ids(
            "flat/stevec/field_eq/eq_term",
            id,
            &ids_text(
                pool,
                &format!(
                    "SELECT id FROM {table} WHERE eql_v3.eq_term(eql_v3.jsonb_path_query_first(value, '{sel_email}')) \
                     = eql_v3.eq_term($1::jsonb::public.jsonb_entry)"
                ),
                &[&email_entry],
            )
            .await?,
            &want_email,
        );

        let age = s["age"].as_i64().unwrap();
        let want = ids_ints(pool, &format!("SELECT id FROM {twin} WHERE (plain #>> '{{age}}')::int > $1"), &[age as i32]).await?;
        let entry = entry_at(pool, &table, &sel_age, id).await?;
        t.ids(
            "flat/stevec/field_gt/functional",
            id,
            &ids_text(
                pool,
                &format!("SELECT id FROM {table} WHERE eql_v3.jsonb_path_query_first(value, '{sel_age}') > $1::jsonb::public.jsonb_entry"),
                &[&entry],
            )
            .await?,
            &want,
        );
        let (lo, hi) = ((age - 5).max(18), (age + 5).min(99));
        let want = ids_ints(
            pool,
            &format!("SELECT id FROM {twin} WHERE (plain #>> '{{age}}')::int BETWEEN $1 AND $2"),
            &[lo as i32, hi as i32],
        )
        .await?;
        let lo_e = entry_at(pool, &table, &sel_age, id_with(pool, &twin, "{age}", lo).await?).await?;
        let hi_e = entry_at(pool, &table, &sel_age, id_with(pool, &twin, "{age}", hi).await?).await?;
        t.ids(
            "flat/stevec/field_between/functional",
            id,
            &ids_text(
                pool,
                &format!(
                    "SELECT id FROM {table} WHERE eql_v3.jsonb_path_query_first(value, '{sel_age}') >= $1::jsonb::public.jsonb_entry \
                     AND eql_v3.jsonb_path_query_first(value, '{sel_age}') <= $2::jsonb::public.jsonb_entry"
                ),
                &[&lo_e, &hi_e],
            )
            .await?,
            &want,
        );
    }
    // ORDER BY … LIMIT 10: the keys, not the ids (ties).
    let keys = |sql: String| async move {
        sqlx::query(&sql)
            .fetch_all(pool)
            .await
            .map(|rows| rows.iter().map(|r| r.get::<i32, _>(0)).collect::<Vec<i32>>())
    };
    let enc = keys(format!(
        "SELECT (p.plain ->> 'age')::int FROM (SELECT id FROM {table} \
         ORDER BY eql_v3.ore_cllw(eql_v3.jsonb_path_query_first(value, '{sel_age}')) LIMIT 10) e \
         JOIN {twin} p USING (id) ORDER BY 1"
    ))
    .await?;
    let pl = keys(format!("SELECT (plain ->> 'age')::int a FROM {twin} ORDER BY a LIMIT 10")).await?;
    t.same("flat/stevec/field_order/functional", 0, enc == pl, || format!("{enc:?} vs {pl:?}"));
    Ok(())
}

async fn check_nested(pool: &PgPool, cipher: &Cipher, tier: &str, n: i64, t: &mut Tally) -> Result<()> {
    let (table, twin) = tables("large", &format!("_{tier}"));
    let ops = V3Operands::new(cipher.clone(), &table);
    let sel_salary = ops.selector("$.company.salary").await?;
    let sel = |p: &'static str| {
        let ops = &ops;
        async move { ops.selector(p).await }
    };
    let (sel_company, sel_user, sel_orders) = (sel("$.company").await?, sel("$.user").await?, sel("$.orders").await?);
    let (sel_items, sel_elems, sel_status) = (
        sel("$.orders[*].items").await?,
        sel("$.orders[*]").await?,
        sel("$.orders[*].status").await?,
    );
    let fn_sql = format!("SELECT id FROM {table} WHERE eql_v3.jsonb_contains(value, $1::jsonb)");
    let op_sql = format!("SELECT id FROM {table} WHERE value @> $1::jsonb::eql_v3.query_jsonb");
    let pl_contains = format!("SELECT id FROM {twin} WHERE plain @> $1");
    let by_id_entries = |sql: String, binds: Vec<String>, id: i32| async move {
        let q = binds.iter().fold(sqlx::query(&sql), |q, b| q.bind(b.clone())).bind(id);
        q.fetch_all(pool)
            .await
            .map(|rows| rows.iter().map(|r| r.get::<Json<Value>, _>(0).0).collect::<Vec<Value>>())
    };
    let path_query = format!(
        "SELECT e::jsonb FROM {table}, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2"
    );

    for (id, s) in sample(pool, &twin, n, "nested-v3").await? {
        let address = &s["addresses"][0];
        let order0 = &s["orders"][0];
        let needles = [
            ("deep_object", json!({"user": {"contact": {"emergency_contact": {
                "relationship": s["user"]["contact"]["emergency_contact"]["relationship"]}}}})),
            ("array_element", json!({"addresses": [{"city": address["city"], "state": address["state"]}]})),
            ("nested_array", json!({"orders": [{"status": order0["status"],
                "items": [{"product": order0["items"][0]["product"]}]}]})),
        ];
        for (label, needle) in &needles {
            let want = ids_json(pool, &pl_contains, needle).await?;
            let enc = ops.containment(needle).await?;
            t.ids(&format!("nested/stevec/contains/{label}_fn"), id, &ids_text(pool, &fn_sql, &[&enc]).await?, &want);
            t.ids(&format!("nested/stevec/contains/{label}"), id, &ids_text(pool, &op_sql, &[&enc]).await?, &want);
        }
        let email = s["user"]["email"].as_str().unwrap();
        let want = ids_text(pool, &format!("SELECT id FROM {twin} WHERE plain #>> '{{user,email}}' = $1"), &[email]).await?;
        let needle = json!({"user": {"email": email}});
        let enc = ops.containment(&needle).await?;
        t.ids("nested/stevec/field_eq/nested", id, &ids_text(pool, &fn_sql, &[&enc]).await?, &want);
        t.ids("nested/plain/field_eq/nested", id, &ids_json(pool, &pl_contains, &needle).await?, &want);

        let salary = s["company"]["salary"].as_i64().unwrap();
        let entry = entry_at(pool, &table, &sel_salary, id).await?;
        let want = ids_ints(
            pool,
            &format!("SELECT id FROM {twin} WHERE (plain #>> '{{company,salary}}')::int > $1"),
            &[salary as i32],
        )
        .await?;
        t.ids(
            "nested/stevec/field_gt/nested",
            id,
            &ids_text(
                pool,
                &format!("SELECT id FROM {table} WHERE eql_v3.jsonb_path_query_first(value, '{sel_salary}') > $1::jsonb::public.jsonb_entry"),
                &[&entry],
            )
            .await?,
            &want,
        );
        let (hi,): (i32,) = sqlx::query_as(&format!(
            "SELECT MIN((plain #>> '{{company,salary}}')::int) FROM {twin} WHERE (plain #>> '{{company,salary}}')::int >= $1"
        ))
        .bind(salary as i32 + 10_000)
        .fetch_one(pool)
        .await?;
        let want = ids_ints(
            pool,
            &format!("SELECT id FROM {twin} WHERE (plain #>> '{{company,salary}}')::int BETWEEN $1 AND $2"),
            &[salary as i32, hi],
        )
        .await?;
        let hi_e = entry_at(pool, &table, &sel_salary, id_with(pool, &twin, "{company,salary}", i64::from(hi)).await?).await?;
        t.ids(
            "nested/stevec/field_between/nested",
            id,
            &ids_text(
                pool,
                &format!(
                    "SELECT id FROM {table} WHERE eql_v3.jsonb_path_query_first(value, '{sel_salary}') >= $1::jsonb::public.jsonb_entry \
                     AND eql_v3.jsonb_path_query_first(value, '{sel_salary}') <= $2::jsonb::public.jsonb_entry"
                ),
                &[&entry, &hi_e],
            )
            .await?,
            &want,
        );

        // By-id scenarios: decrypt what they return, compare with the twin.
        for (label, sel, key) in [("company", &sel_company, "company"), ("user", &sel_user, "user"), ("orders", &sel_orders, "orders")] {
            let got = open_entries(
                cipher,
                by_id_entries(format!("SELECT (value -> $1::text)::jsonb FROM {table} WHERE id = $2"), vec![sel.clone()], id).await?,
            )
            .await?;
            t.same(&format!("nested/stevec/subtree/{label}"), id, got == vec![s[key].clone()], || {
                format!("decrypted {} value(s), first {:?}", got.len(), got.first().map(|v| v.to_string().chars().take(80).collect::<String>()))
            });
        }
        let orders = s["orders"].as_array().unwrap();
        let items: Vec<Value> = orders.iter().map(|o| o["items"].clone()).collect();
        let statuses: Vec<Value> = orders.iter().map(|o| o["status"].clone()).collect();
        for (label, sel, want) in [
            ("subtree_at/items", &sel_items, &items),
            ("children/orders", &sel_elems, orders),
            ("path_query/wildcard", &sel_status, &statuses),
        ] {
            let got = open_entries(cipher, by_id_entries(path_query.clone(), vec![sel.clone()], id).await?).await?;
            t.same(&format!("nested/stevec/{label}"), id, multiset(&got) == multiset(want), || {
                format!("{} decrypted, twin has {}", got.len(), want.len())
            });
        }
        let (count,): (i64,) = sqlx::query_as(&format!(
            "SELECT count(*) FROM {table}, eql_v3.jsonb_path_query(value, $1::text) e WHERE id = $2"
        ))
        .bind(&sel_elems)
        .bind(id)
        .fetch_one(pool)
        .await?;
        t.same("nested/stevec/child_count/orders", id, count as usize == orders.len(), || {
            format!("{count} vs {}", orders.len())
        });
        let got = open_entries(
            cipher,
            by_id_entries(format!("SELECT (value -> $1::text)::jsonb FROM {table} WHERE id = $2"), vec![sel_salary.clone()], id).await?,
        )
        .await?;
        t.same("nested/stevec/extract/nested", id, got == vec![s["company"]["salary"].clone()], || format!("{got:?}"));
    }
    let enc: Vec<i32> = sqlx::query(&format!(
        "SELECT (p.plain #>> '{{company,salary}}')::int FROM (SELECT id FROM {table} \
         ORDER BY eql_v3.ore_cllw(eql_v3.jsonb_path_query_first(value, '{sel_salary}')) LIMIT 10) e \
         JOIN {twin} p USING (id) ORDER BY 1"
    ))
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect();
    let pl: Vec<i32> = sqlx::query(&format!(
        "SELECT (plain #>> '{{company,salary}}')::int s FROM {twin} ORDER BY s LIMIT 10"
    ))
    .fetch_all(pool)
    .await?
    .iter()
    .map(|r| r.get(0))
    .collect();
    t.same("nested/stevec/field_order/nested", 0, enc == pl, || format!("{enc:?} vs {pl:?}"));
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    dbbenches::init_tracing();
    let tier = std::env::var("TARGET_ROWS").unwrap_or_else(|_| "10000".into());
    let n: i64 = std::env::var("CHECK_NEEDLES").unwrap_or_else(|_| "10".into()).parse()?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("DATABASE_URL")?)
        .await?;
    let cipher = dbbenches::init_scoped_cipher().await?;
    let mut t = Tally::default();
    check_flat(&pool, &cipher, &tier, n, &mut t).await?;
    check_nested(&pool, &cipher, &tier, n, &mut t).await?;
    let summary = json!({
        "target_rows": tier,
        "needles_per_predicate": n,
        "checks": t.by_check,
        "disagreements": t.failures,
    });
    std::fs::create_dir_all("results/query/compare")?;
    let path = format!("results/query/compare/stevec_correctness_{tier}.json");
    std::fs::write(&path, serde_json::to_string_pretty(&summary)?)?;
    println!("{}", serde_json::to_string_pretty(&summary["checks"])?);
    println!("{} disagreement(s); written to {path}", t.failures.len());
    Ok(())
}
