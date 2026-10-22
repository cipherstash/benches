//! EQL v4 `json_tree` sibling of the root crate's `benches/json_v3.rs`:
//! stack-encrypt's JSON index queried against
//! `json_tree_small_encrypted_v4_<N>` (`public.eql_v4_json_tree`).
//!
//! Every scenario goes through the EQL v4 surface a real caller emits —
//! the typed operators and `eql_v4.*` functions — with operands the client
//! derives (`cipher.query(..).on(&plan)`), never a hand-built jsonb. Where a
//! scenario has a v3 twin the id's tail matches so a report can join them:
//!
//!   contains/functional   WHERE eql_v4.json_tree_contains(value, $1::…query_json_tree)
//!     v3 twin: eql_v3.jsonb_contains(value, $1). Needle = a sampled row's
//!     whole document (`op::Contains`). GIN over
//!     `eql_v4.json_tree_query(value)::jsonb` (jsonb_path_ops) serves it:
//!     the function is not STRICT so it inlines to native `jsonb @>`.
//!   contains/operator     WHERE value @> $1::…query_json_tree
//!     The operator spelling of the same predicate (same plan expected).
//!   field_eq/extractor    WHERE eql_v4.json_tree_contains(value, $1)  — single-path needle
//!     v3 twin: field_eq/extractor. `op::AtPath(op::Eq)` on `$.first_name`
//!     (the first `hm` entry v3 picks): equality at a path IS containment
//!     of the one-field needle under v4; there is no entry `=` (blocked).
//!   field_eq/unique       the same, on `$.email` (v4-only: a selective needle)
//!   field_gt/functional   WHERE (value -> '<sel>'::text) > $1::…query_json_tree_leaf
//!     v3 twin: field_gt/functional. `op::AtPath(op::Gt)` on `$.age` (the
//!     `oc` entry v3 picks). `>` on (entry, leaf) inlines to
//!     `eql_v4.ord_term(a) > eql_v4.ord_term(b)`; a functional btree on
//!     `eql_v4.ord_term(value -> '<sel>'::text)`, built at startup like v3's
//!     field_order_idx, can serve it. CLLW OPE terms order as native bytea.
//!   field_between/functional   >= lo AND <= hi on the same leaf (v4-only;
//!     `op::AtPath(op::Between)` gives the two bounds, there is no SQL between)
//!   field_order/functional ORDER BY eql_v4.ord_term(value -> '<sel>'::text) LIMIT 10
//!     v3 twin: field_order/functional, on the same functional btree.
//!   extract/by_id         SELECT (value -> '<sel>'::text)::jsonb::text … WHERE id = $1
//!     v4-only: the cost of lifting one entry (no decrypt) by primary key.
//!   path_query/field      eql_v4.json_tree_path_query(value::jsonb, sel) for `$.email`, by id
//!
//! Each scenario has a `plain/*` twin over the plaintext twin table (same
//! rows, same sampled row, same plaintext needles): native `@>` under a
//! jsonb_path_ops GIN, `(plain ->> 'age')::int` comparisons and ordering
//! under a btree on that expression (built at startup), `->` and
//! `jsonb_path_query` by id. Ids are `JSON_TREE/<json_tree|plain>/<scenario>/<N>`;
//! the SteVec side of the comparison is the root crate's
//! `benches/json_compare_v3.rs`.
//!
//! Not exercised on this flat corpus: the token-chain walks
//! (`json_tree_subtree`, `json_tree_children`, `json_tree_child_count`)
//! need a container below the root, and the root is sealed. A nested corpus
//! is a follow-up.
//!
//! The selector is interpolated as a `'<hex>'::text` literal (not bound),
//! exactly as the index expression spells it, or the planner cannot match
//! the functional index — the v3 bench does the same. Containment operands
//! are bound as text and cast `::text::jsonb::eql_v4.query_json_tree`: a
//! direct bind-cast to the domain makes sqlx resolve the domain as the
//! parameter type and fail (see the v3 bench's note on `::jsonb::` casts).

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use eql_bindings::v4::json_tree::{JsonTreeLeafQuery, JsonTreeQuery};
use json_tree_bench::{
    bench_assert, cipher, column, extract_indexes_used, plan, write_metadata_file_in, Cipher,
    JsonPlan, ScenarioMetadata,
};
use serde_json::Value as JsonValue;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::Row;
use stack_encrypt::json::JsonPath;
use stack_encrypt::op;
use tokio::runtime::Runtime;

const CONTAINS_CAST: &str = "::text::jsonb::eql_v4.query_json_tree";
const LEAF_CAST: &str = "::text::jsonb::eql_v4.query_json_tree_leaf";

/// Everything a scenario binds, derived once at startup.
struct Needles {
    /// A sampled row: its id and plaintext (from the plain twin table).
    sample_id: i32,
    sample: JsonValue,
    /// `op::Contains` over the whole sampled document.
    document_query: String,
    /// `op::AtPath(op::Eq)` on `$.first_name` and `$.email` of the sample.
    first_name_query: String,
    email_query: String,
    /// `$.age` selector (hex) and the `op::AtPath(..)` leaf operands.
    age_selector: String,
    age_gt: String,
    age_between: (String, String),
    /// The plaintext ages behind `age_gt` and `age_between`.
    age: i64,
    age_bounds: (i64, i64),
    /// `$.email` selector (hex), for the path query.
    email_selector: String,
}

/// The containment operand for `needle`, as the SQL takes it.
async fn contains_operand(cipher: &Cipher, plan: &JsonPlan, needle: &JsonValue) -> String {
    let q = cipher
        .query(op::Contains, needle)
        .on(plan)
        .await
        .expect("derive containment operand");
    serde_json::to_string(&JsonTreeQuery::try_from(&q).expect("typed operand")).unwrap()
}

/// The operand of equality at `path`: a one-field needle, under containment.
async fn at_path_eq_operand(
    cipher: &Cipher,
    plan: &JsonPlan,
    path: &'static str,
    value: JsonValue,
) -> String {
    let q = cipher
        .query(op::AtPath(op::Eq), &(path, value))
        .on(plan)
        .await
        .expect("derive AtPath(Eq) operand");
    serde_json::to_string(&JsonTreeQuery::try_from(&q).expect("typed operand")).unwrap()
}

async fn sample_needles(
    pool: &sqlx::PgPool,
    cipher: &Cipher,
    plan: &JsonPlan,
    plain_table: &str,
) -> Needles {
    // A row from the middle of the table, so neither a seq scan nor a
    // LIMIT stops at the first page by luck.
    let row = sqlx::query(&format!(
        "SELECT id, plain FROM {plain_table} \
         WHERE id >= (SELECT (MIN(id) + MAX(id)) / 2 FROM {plain_table}) \
         ORDER BY id LIMIT 1"
    ))
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("sampling a row from `{plain_table}` failed: {e}"));
    let sample_id: i32 = row.get("id");
    let sample: Json<JsonValue> = row.get("plain");
    let sample = sample.0;

    let leaf = |q: &stack_encrypt::json::JsonLeafQuery| -> String {
        serde_json::to_string(&JsonTreeLeafQuery::try_from(q).expect("typed leaf operand")).unwrap()
    };

    let age = sample["age"].as_i64().expect("sample has an integer age");
    let age_gt = cipher
        .query(op::AtPath(op::Gt), &("$.age", age))
        .on(plan)
        .await
        .expect("derive AtPath(Gt) operand");
    // A ten-year window around the sampled age, clamped to the corpus
    // range (18..=99).
    let (lo, hi) = ((age - 5).max(18), (age + 5).min(99));
    let age_between = cipher
        .query(op::AtPath(op::Between), &("$.age", (lo, hi)))
        .on(plan)
        .await
        .expect("derive AtPath(Between) operand");
    let age_selector = cipher
        .default_keyset()
        .json_selector(&JsonPath::parse("$.age").unwrap(), column())
        .await
        .expect("derive selector")
        .to_string();
    let email_selector = cipher
        .default_keyset()
        .json_selector(&JsonPath::parse("$.email").unwrap(), column())
        .await
        .expect("derive selector")
        .to_string();

    Needles {
        sample_id,
        document_query: contains_operand(cipher, plan, &sample).await,
        first_name_query: at_path_eq_operand(
            cipher,
            plan,
            "$.first_name",
            sample["first_name"].clone(),
        )
        .await,
        email_query: at_path_eq_operand(cipher, plan, "$.email", sample["email"].clone()).await,
        age_selector,
        age_gt: leaf(&age_gt),
        age_between: (leaf(age_between.lo()), leaf(age_between.hi())),
        age,
        age_bounds: (lo, hi),
        email_selector,
        sample,
    }
}

/// The per-path functional btree for the ordering scenarios (the selector
/// is known only once derived), mirroring the v3 bench's field_order_idx;
/// and the plaintext twin's indexes: the jsonb_path_ops GIN its `@>` uses
/// (also in sql/indexes/json_tree_small_encrypted_v4_<N>_up.sql) and a btree
/// on `(plain ->> 'age')::int`.
async fn create_field_indexes(pool: &sqlx::PgPool, table: &str, plain_table: &str, needles: &Needles) {
    sqlx::query(&format!(
        "CREATE INDEX IF NOT EXISTS {plain_table}_gin_idx ON {plain_table} USING GIN (plain jsonb_path_ops)"
    ))
    .execute(pool)
    .await
    .expect("create plaintext GIN");
    sqlx::query(&format!("DROP INDEX IF EXISTS {plain_table}_age_idx"))
        .execute(pool)
        .await
        .expect("drop stale plaintext age index");
    sqlx::query(&format!(
        "CREATE INDEX {plain_table}_age_idx ON {plain_table} USING btree (((plain ->> 'age')::int))"
    ))
    .execute(pool)
    .await
    .expect("create plaintext age index");
    sqlx::query(&format!("ANALYZE {plain_table}"))
        .execute(pool)
        .await
        .expect("ANALYZE plaintext twin");
    eprintln!("json_tree_v4 bench: building the per-selector functional index...");
    sqlx::query(&format!("DROP INDEX IF EXISTS {table}_field_order_idx"))
        .execute(pool)
        .await
        .expect("drop stale field_order index");
    sqlx::query(&format!(
        "CREATE INDEX {table}_field_order_idx ON {table} \
         USING btree (eql_v4.ord_term(value -> '{}'::text))",
        needles.age_selector
    ))
    .execute(pool)
    .await
    .expect("create field_order functional index");
    sqlx::query(&format!("ANALYZE {table}"))
        .execute(pool)
        .await
        .expect("ANALYZE after index creation");
}

#[derive(Clone)]
enum Bind {
    None,
    Text(String),
    Texts(String, String),
    Id(i32),
    TextId(String, i32),
    Jsonb(JsonValue),
    Int(i32),
    Ints(i32, i32),
}

struct Scenario {
    name: &'static str,
    sql: String,
    bind: Bind,
}

fn criterion_benchmark(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let target_rows = std::env::var("TARGET_ROWS").unwrap_or_else(|_| "unknown".to_string());
    let table_suffix = match target_rows.as_str() {
        "10000" | "100000" | "1000000" | "10000000" => format!("_{target_rows}"),
        _ => String::new(),
    };
    let table = format!("json_tree_small_encrypted_v4{table_suffix}");
    let plain_table = format!("json_tree_small_plain_v4{table_suffix}");
    let results_dir =
        std::env::var("RESULTS_DIR").unwrap_or_else(|_| "../results/query/v4".to_string());

    let (pool, needles) = rt.block_on(async {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL environment variable must be set");
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .expect("Failed to connect to database");
        let cipher = cipher().await;
        let plan = plan();
        let needles = sample_needles(&pool, &cipher, &plan, &plain_table).await;
        eprintln!(
            "json_tree_v4 bench picked row {} from `{}` — age selector {}",
            needles.sample_id, table, needles.age_selector
        );
        create_field_indexes(&pool, &table, &plain_table, &needles).await;
        (pool, needles)
    });

    let sel = &needles.age_selector;
    let scenarios = vec![
        Scenario {
            name: "json_tree/contains/functional",
            sql: format!(
                "SELECT id FROM {table} \
                 WHERE eql_v4.json_tree_contains(value, $1{CONTAINS_CAST}) LIMIT 10"
            ),
            bind: Bind::Text(needles.document_query.clone()),
        },
        Scenario {
            name: "json_tree/contains/operator",
            sql: format!("SELECT id FROM {table} WHERE value @> $1{CONTAINS_CAST} LIMIT 10"),
            bind: Bind::Text(needles.document_query.clone()),
        },
        Scenario {
            name: "json_tree/field_eq/extractor",
            sql: format!(
                "SELECT id FROM {table} \
                 WHERE eql_v4.json_tree_contains(value, $1{CONTAINS_CAST}) LIMIT 10"
            ),
            bind: Bind::Text(needles.first_name_query.clone()),
        },
        Scenario {
            name: "json_tree/field_eq/unique",
            sql: format!(
                "SELECT id FROM {table} \
                 WHERE eql_v4.json_tree_contains(value, $1{CONTAINS_CAST}) LIMIT 10"
            ),
            bind: Bind::Text(needles.email_query.clone()),
        },
        Scenario {
            name: "json_tree/field_gt/functional",
            sql: format!(
                "SELECT id FROM {table} \
                 WHERE (value -> '{sel}'::text) > $1{LEAF_CAST} LIMIT 10"
            ),
            bind: Bind::Text(needles.age_gt.clone()),
        },
        Scenario {
            name: "json_tree/field_between/functional",
            sql: format!(
                "SELECT id FROM {table} \
                 WHERE (value -> '{sel}'::text) >= $1{LEAF_CAST} \
                   AND (value -> '{sel}'::text) <= $2{LEAF_CAST} LIMIT 10"
            ),
            bind: Bind::Texts(needles.age_between.0.clone(), needles.age_between.1.clone()),
        },
        Scenario {
            name: "json_tree/field_order/functional",
            sql: format!(
                "SELECT id FROM {table} \
                 ORDER BY eql_v4.ord_term(value -> '{sel}'::text) LIMIT 10"
            ),
            bind: Bind::None,
        },
        Scenario {
            name: "json_tree/extract/by_id",
            sql: format!("SELECT (value -> '{sel}'::text)::jsonb::text FROM {table} WHERE id = $1"),
            bind: Bind::Id(needles.sample_id),
        },
        Scenario {
            name: "json_tree/path_query/field",
            sql: format!(
                "SELECT e::jsonb::text FROM {table}, \
                 eql_v4.json_tree_path_query(value::jsonb, $1::text) e WHERE id = $2"
            ),
            bind: Bind::TextId(needles.email_selector.clone(), needles.sample_id),
        },
        // ---- plaintext twins (same rows, same needles) ----
        Scenario {
            name: "plain/contains/operator",
            sql: format!("SELECT id FROM {plain_table} WHERE plain @> $1 LIMIT 10"),
            bind: Bind::Jsonb(needles.sample.clone()),
        },
        Scenario {
            name: "plain/field_eq/extractor",
            sql: format!("SELECT id FROM {plain_table} WHERE plain @> $1 LIMIT 10"),
            bind: Bind::Jsonb(serde_json::json!({"first_name": needles.sample["first_name"]})),
        },
        Scenario {
            name: "plain/field_eq/unique",
            sql: format!("SELECT id FROM {plain_table} WHERE plain @> $1 LIMIT 10"),
            bind: Bind::Jsonb(serde_json::json!({"email": needles.sample["email"]})),
        },
        Scenario {
            name: "plain/field_gt/functional",
            sql: format!("SELECT id FROM {plain_table} WHERE (plain ->> 'age')::int > $1 LIMIT 10"),
            bind: Bind::Int(needles.age as i32),
        },
        Scenario {
            name: "plain/field_between/functional",
            sql: format!(
                "SELECT id FROM {plain_table} \
                 WHERE (plain ->> 'age')::int BETWEEN $1 AND $2 LIMIT 10"
            ),
            bind: Bind::Ints(needles.age_bounds.0 as i32, needles.age_bounds.1 as i32),
        },
        Scenario {
            name: "plain/field_order/functional",
            sql: format!("SELECT id FROM {plain_table} ORDER BY (plain ->> 'age')::int LIMIT 10"),
            bind: Bind::None,
        },
        Scenario {
            name: "plain/extract/by_id",
            sql: format!("SELECT (plain -> 'age')::text FROM {plain_table} WHERE id = $1"),
            bind: Bind::Id(needles.sample_id),
        },
        Scenario {
            name: "plain/path_query/field",
            sql: format!(
                "SELECT e::text FROM {plain_table}, jsonb_path_query(plain, '$.email') e WHERE id = $1"
            ),
            bind: Bind::Id(needles.sample_id),
        },
    ];

    // --- Metadata sidecar: EXPLAIN + one execution per scenario ---

    fn query_with<'q>(
        sql: &'q str,
        bind: &'q Bind,
    ) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        let q = sqlx::query(sql);
        match bind {
            Bind::None => q,
            Bind::Text(a) => q.bind(a),
            Bind::Texts(a, b) => q.bind(a).bind(b),
            Bind::Id(id) => q.bind(*id),
            Bind::TextId(a, id) => q.bind(a).bind(*id),
            Bind::Jsonb(v) => q.bind(Json(v)),
            Bind::Int(i) => q.bind(*i),
            Bind::Ints(a, b) => q.bind(*a).bind(*b),
        }
    }

    let metadata = rt.block_on(async {
        let mut out = Vec::with_capacity(scenarios.len());
        for s in &scenarios {
            let id = format!("JSON_TREE/{}/{}", s.name, target_rows);
            let explain_sql = format!("EXPLAIN (FORMAT JSON) {}", s.sql);
            let explain: Json<JsonValue> = query_with(&explain_sql, &s.bind)
                .fetch_one(&pool)
                .await
                .unwrap_or_else(|e| panic!("EXPLAIN failed for {id}: {e}"))
                .get(0);
            let rows = query_with(&s.sql, &s.bind)
                .fetch_all(&pool)
                .await
                .unwrap_or_else(|e| panic!("row-count execute failed for {id}: {e}"));
            let parameters = match &s.bind {
                Bind::None => vec![],
                Bind::Text(a) => vec![JsonValue::String(a.clone())],
                Bind::Texts(a, b) => {
                    vec![JsonValue::String(a.clone()), JsonValue::String(b.clone())]
                }
                Bind::Id(id) => vec![JsonValue::from(*id)],
                Bind::TextId(a, id) => vec![JsonValue::String(a.clone()), JsonValue::from(*id)],
                Bind::Jsonb(v) => vec![v.clone()],
                Bind::Int(i) => vec![JsonValue::from(*i)],
                Bind::Ints(a, b) => vec![JsonValue::from(*a), JsonValue::from(*b)],
            };
            out.push(ScenarioMetadata {
                id,
                query: s.sql.clone(),
                parameters,
                indexes_used: extract_indexes_used(&explain.0),
                explain: explain.0,
                rows_returned: rows.len() as u64,
            });
        }

        // Sanity: the encrypted whole-document containment returns the
        // sampled row, and exactly the rows plaintext containment returns.
        let encrypted: Vec<i32> = query_with(
            &format!(
                "SELECT id FROM {table} \
                 WHERE eql_v4.json_tree_contains(value, $1{CONTAINS_CAST}) ORDER BY id"
            ),
            &Bind::Text(needles.document_query.clone()),
        )
        .fetch_all(&pool)
        .await
        .expect("containment oracle (encrypted)")
        .iter()
        .map(|r| r.get(0))
        .collect();
        let plain: Vec<i32> = sqlx::query(&format!(
            "SELECT id FROM {plain_table} WHERE plain @> $1::jsonb ORDER BY id"
        ))
        .bind(Json(needles.sample.clone()))
        .fetch_all(&pool)
        .await
        .expect("containment oracle (plain)")
        .iter()
        .map(|r| r.get(0))
        .collect();
        assert_eq!(
            encrypted, plain,
            "encrypted @> must agree with plaintext @>"
        );
        assert!(encrypted.contains(&needles.sample_id));
        eprintln!(
            "json_tree_v4 bench: containment oracle agrees ({} row(s))",
            encrypted.len()
        );
        out
    });
    write_metadata_file_in(&results_dir, "json_tree", &target_rows, metadata)
        .expect("failed to write bench metadata sidecar");

    // --- Bench loop ---

    let mut group = c.benchmark_group("JSON_TREE");
    group.sample_size(10);
    for s in &scenarios {
        let id = format!("JSON_TREE/{}/{}", s.name, target_rows);
        let sql = s.sql.clone();
        let bind = s.bind.clone();
        group.bench_function(format!("{}/{}", s.name, target_rows), |b| {
            b.to_async(&rt).iter(|| async {
                let rows = bench_assert(query_with(&sql, &bind).fetch_all(&pool).await, &id);
                black_box(rows.len())
            })
        });
    }
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
