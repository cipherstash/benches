//! Native stack-encrypt reading pages from individuals_v4_<tier> and
//! decrypting them: the Rust counterpart of `gobench -mode db`.
//!
//!   cargo run --release --bin query_v4 -- --tier 10000 [--rounds 2] [--calls 3] [--out FILE]

use std::time::Instant;

use benches_v4::*;
use eql_bindings::v4::integer::IntegerOrdQuery;
use eql_bindings::v4::text::TextEqQuery;
use eql_bindings::Identifier;
use rand::Rng;
use sqlx::{Connection, Row};
use stack_encrypt::{DecryptFrom, EncryptInto, Protected};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (mut tier, mut rounds, mut calls) = (10_000usize, 2usize, 3usize);
    let (mut out, mut only): (Option<String>, Option<String>) = (None, None);
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().expect("flag needs a value");
        match k.as_str() {
            "--tier" => tier = v.parse()?,
            "--rounds" => rounds = v.parse()?,
            "--calls" => calls = v.parse()?,
            "--out" => out = Some(v),
            "--only" => only = Some(v),
            _ => anyhow::bail!("unknown flag {k}"),
        }
    }
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5400/postgres".into());
    let mut conn = sqlx::PgConnection::connect(&url).await?;
    let table = format!("individuals_v4_{tier}");
    let select = std::iter::once("id".to_string())
        .chain(COLUMNS.iter().map(|c| format!("{c}::jsonb")))
        .collect::<Vec<_>>()
        .join(", ");

    let cipher = StackCipher::with_detected_credentials().await?;
    let keyset = cipher.default_keyset();
    let mut r = rng(7);

    for scenario in ["page_100_by_id", "eq_email_1", "match_last_name_100", "ord_risk_gt_100"] {
        if only.as_deref().is_some_and(|o| o != scenario) {
            continue;
        }
        let mut rec = Record {
            variant: "native".into(),
            mode: "db",
            tier: Some(tier),
            scenario: scenario.into(),
            op: "decrypt",
            ..Default::default()
        };
        for i in 0..=(rounds * calls) {
            let t0 = Instant::now();
            let rows = match scenario {
                "page_100_by_id" => {
                    let start = r.gen_range(0..(tier.saturating_sub(100).max(1))) as i64;
                    sqlx::query(&format!("SELECT {select} FROM {table} WHERE id > $1 ORDER BY id LIMIT 100"))
                        .bind(start)
                        .fetch_all(&mut conn)
                        .await?
                }
                "eq_email_1" => {
                    let id = 1 + r.gen_range(0..tier) as i64;
                    let q: TextEqQuery = email_for(id)
                        .encrypt_into_with_context(&keyset, Identifier::for_column("individuals", "email")?)
                        .await?;
                    sqlx::query(&format!(
                        "SELECT {select} FROM {table} WHERE email = $1::jsonb::eql_v4.query_text_eq"
                    ))
                    .bind(serde_json::to_value(&q)?)
                    .fetch_all(&mut conn)
                    .await?
                }
                "match_last_name_100" => {
                    let q = EncryptedIndividual::LAST_NAME
                        .matches(LAST[r.gen_range(0..LAST.len())])
                        .encrypt(&keyset)
                        .await?;
                    sqlx::query(&format!(
                        "SELECT {select} FROM {table} WHERE last_name @@ $1::jsonb::eql_v4.query_text_match LIMIT 100"
                    ))
                    .bind(serde_json::to_value(q.twin())?)
                    .fetch_all(&mut conn)
                    .await?
                }
                "ord_risk_gt_100" => {
                    let q: IntegerOrdQuery = r
                        .gen_range(0..990i32)
                        .encrypt_into_with_context(&keyset, Identifier::for_column("individuals", "risk_score")?)
                        .await?;
                    sqlx::query(&format!(
                        "SELECT {select} FROM {table} WHERE risk_score > $1::jsonb::eql_v4.query_integer_ord \
                         ORDER BY eql_v4.ord_term(risk_score) LIMIT 100"
                    ))
                    .bind(serde_json::to_value(&q)?)
                    .fetch_all(&mut conn)
                    .await?
                }
                _ => unreachable!(),
            };
            let query_ms = ms(t0.elapsed());
            let page = rows
                .iter()
                .map(|row| {
                    let cols = (1..=14)
                        .map(|j| row.try_get::<serde_json::Value, _>(j))
                        .collect::<Result<Vec<_>, _>>()?;
                    EncryptedIndividual::from_columns(cols)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let n = page.len();
            let (w, c, _opened) = timed(async {
                Ok(Vec::<Protected<Individual>>::decrypt_from(page, &keyset).await?)
            })
            .await?;
            if i > 0 {
                rec.rows = n;
                rec.values = n * 14;
                rec.wall_ms.push(w);
                rec.cpu_ms.push(c);
                rec.query_ms.push(query_ms);
            }
        }
        emit(rec, out.as_deref())?;
    }
    Ok(())
}
