//! Fill individuals_v4_<tier> with `tier` rows encrypted by stack-encrypt
//! into EQL v4 payloads, and record the throughput.
//!
//!   cargo run --release --bin ingest_v4 -- --tier 10000 [--chunk 500] [--inflight 8] [--out FILE]
//!
//! The table must exist (sql/schema_v4.sql); indexes are built afterwards
//! (sql/indexes/v4_individuals.sql). `mise run prepare:v4 <tier>` does all
//! three.

use std::time::Instant;

use benches_v4::*;
use futures::{StreamExt, TryStreamExt};
use serde::Serialize;
use sqlx::Connection;
use stack_encrypt::EncryptInto;

#[derive(Serialize)]
struct IngestResult {
    sdk: &'static str,
    tier: usize,
    rows: usize,
    values: usize,
    chunk: usize,
    inflight: usize,
    total_s: f64,
    encrypt_cpu_s: f64,
    copy_s: f64,
    rows_per_s: f64,
    at: String,
}

fn csv_field(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        if ch == '"' {
            out.push('"');
        }
        out.push(ch);
    }
    out.push('"');
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut tier = 10_000usize;
    let mut chunk = 500usize;
    let mut inflight = 8usize;
    let mut out: Option<String> = None;
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().expect("flag needs a value");
        match k.as_str() {
            "--tier" => tier = v.parse()?,
            "--chunk" => chunk = v.parse()?,
            "--inflight" => inflight = v.parse()?,
            "--out" => out = Some(v),
            _ => anyhow::bail!("unknown flag {k}"),
        }
    }

    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5400/postgres".into());
    let mut conn = sqlx::PgConnection::connect(&url).await?;
    let table = format!("individuals_v4_{tier}");
    // Resume: each chunk is one COPY, so a load that stopped part way holds
    // whole chunks 1..=max(id). Regenerate those rows (the generator is
    // sequential) and skip them.
    let (existing, max_id): (i64, Option<i64>) =
        sqlx::query_as(&format!("SELECT count(*), max(id) FROM {table}"))
            .fetch_one(&mut conn)
            .await?;
    let resume_after = max_id.unwrap_or(0);
    anyhow::ensure!(
        existing == resume_after && resume_after as usize % chunk == 0,
        "{table} holds {existing} rows up to id {resume_after}, not whole chunks of {chunk}; recreate it"
    );
    if resume_after > 0 {
        eprintln!("resuming after id {resume_after}");
    }

    let cipher = StackCipher::with_detected_credentials().await?;
    let keyset = cipher.default_keyset();

    let started = Instant::now();
    let cpu0 = cpu_now();
    let mut copy_time = std::time::Duration::ZERO;
    let mut r = rng(1);
    // Generate in order (one generator, so row ids and plaintexts are
    // reproducible), encrypt up to `inflight` chunks at once, COPY in order.
    let ids: Vec<(i64, i64)> = (0..tier)
        .step_by(chunk)
        .map(|lo| (lo as i64 + 1, (lo + chunk).min(tier) as i64))
        .collect();
    let mut plain = Vec::with_capacity(ids.len());
    for &(lo, hi) in &ids {
        let rows = (lo..=hi).map(|id| gen_individual(&mut r, id)).collect::<Vec<_>>();
        if hi > resume_after {
            plain.push((lo, rows));
        }
    }
    let keyset_ref = &keyset;
    // Encryption consumes its input, so a retry regenerates the chunk from
    // a generator positioned at its first row.
    let regen = |lo: i64, n: usize| -> Vec<Individual> {
        let mut r = rng(1);
        for id in 1..lo {
            gen_individual(&mut r, id);
        }
        (lo..lo + n as i64).map(|id| gen_individual(&mut r, id)).collect()
    };
    let regen = &regen;
    let mut sealed = futures::stream::iter(plain.into_iter().map(|(lo, people)| async move {
        let n = people.len();
        let mut attempt = 0;
        let mut people = Some(people);
        loop {
            let input = people.take().unwrap_or_else(|| regen(lo, n));
            match input.encrypt_into(keyset_ref).await {
                Ok(enc) => {
                    let enc: Vec<EncryptedIndividual> = enc;
                    return anyhow::Ok((lo, enc));
                }
                Err(e) if attempt < 3 => {
                    attempt += 1;
                    eprintln!("chunk at id {lo}: {e}; retry {attempt}");
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }))
    .buffered(inflight);

    let cols = std::iter::once("id").chain(COLUMNS).collect::<Vec<_>>().join(", ");
    let mut done = 0usize;
    let mut last_report = Instant::now();
    while let Some((lo, enc)) = sealed.try_next().await? {
        let mut buf = String::with_capacity(enc.len() * 14 * 400);
        for (i, row) in enc.iter().enumerate() {
            buf.push_str(&(lo + i as i64).to_string());
            for j in row.column_json()? {
                buf.push(',');
                csv_field(&j, &mut buf);
            }
            buf.push('\n');
        }
        let t = Instant::now();
        let mut copy = conn
            .copy_in_raw(&format!("COPY {table} ({cols}) FROM STDIN WITH (FORMAT csv)"))
            .await?;
        copy.send(buf.as_bytes()).await?;
        copy.finish().await?;
        copy_time += t.elapsed();
        done += enc.len();
        if last_report.elapsed().as_secs() >= 10 {
            let s = started.elapsed().as_secs_f64();
            eprintln!("{done}/{tier} rows  {:.0} rows/s  {:.0}s", done as f64 / s, s);
            last_report = Instant::now();
        }
    }
    let total = started.elapsed().as_secs_f64();
    let res = IngestResult {
        sdk: "rust",
        tier,
        rows: done,
        values: done * 14,
        chunk,
        inflight,
        total_s: total,
        encrypt_cpu_s: (cpu_now() - cpu0).as_secs_f64(),
        copy_s: copy_time.as_secs_f64(),
        rows_per_s: done as f64 / total,
        at: chrono::Utc::now().to_rfc3339(),
    };
    let line = serde_json::to_string(&res)?;
    println!("{line}");
    if let Some(p) = out {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(p)?;
        writeln!(f, "{line}")?;
    }
    Ok(())
}
