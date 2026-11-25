//! Shared pieces of the EQL v4 benches: the records, the data generator,
//! the cipher, CPU timing and the result lines.
//!
//! The records match `../go/model/model.go` column for column and context
//! for context (scope `individuals` / `documents`, then the column name),
//! so a row this crate stores opens in the Go SDK and the reverse.

use std::io::Write;
use std::time::{Duration, Instant};

use eql_bindings::v4::date::Date as DateCol;
use eql_bindings::v4::integer::IntegerOrd;
use eql_bindings::v4::json::Json;
use eql_bindings::v4::plaintext::{Date, JsonPlaintext};
use eql_bindings::v4::text::{Text, TextEq, TextMatch};
use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use serde::Serialize;
use stack_encrypt::target::{DecryptSource, EncryptTarget};
use zeroize::Zeroize;

pub use stack_encrypt::StackCipher;

// --- records ----------------------------------------------------------------

/// A 14-column person: every column encrypted.
#[derive(Zeroize, Default)]
pub struct Individual {
    pub first_name: String,
    pub last_name: String,
    pub email: String,
    pub phone: String,
    pub date_of_birth: Date,
    pub nationality: String,
    pub tax_id: String,
    pub address_line1: String,
    pub address_line2: String,
    pub city: String,
    pub postcode: String,
    pub country: String,
    pub occupation: String,
    pub risk_score: i32,
}

#[derive(EncryptTarget, DecryptSource, Serialize, serde::Deserialize)]
#[stash(struct = Individual, scope = "individuals")]
pub struct EncryptedIndividual {
    pub first_name: TextEq,
    pub last_name: TextMatch,
    pub email: TextEq,
    pub phone: Text,
    pub date_of_birth: DateCol,
    pub nationality: Text,
    pub tax_id: TextEq,
    pub address_line1: Text,
    pub address_line2: Text,
    pub city: Text,
    pub postcode: Text,
    pub country: Text,
    pub occupation: Text,
    pub risk_score: IntegerOrd,
}

/// The column names, in `EncryptedIndividual`'s field order.
pub const COLUMNS: [&str; 14] = [
    "first_name",
    "last_name",
    "email",
    "phone",
    "date_of_birth",
    "nationality",
    "tax_id",
    "address_line1",
    "address_line2",
    "city",
    "postcode",
    "country",
    "occupation",
    "risk_score",
];

impl EncryptedIndividual {
    /// Each column's EQL value as the JSON text the column stores.
    pub fn column_json(&self) -> anyhow::Result<[String; 14]> {
        use serde_json::to_string as j;
        Ok([
            j(&self.first_name)?,
            j(&self.last_name)?,
            j(&self.email)?,
            j(&self.phone)?,
            j(&self.date_of_birth)?,
            j(&self.nationality)?,
            j(&self.tax_id)?,
            j(&self.address_line1)?,
            j(&self.address_line2)?,
            j(&self.city)?,
            j(&self.postcode)?,
            j(&self.country)?,
            j(&self.occupation)?,
            j(&self.risk_score)?,
        ])
    }

    /// The inverse of [`column_json`](Self::column_json): one row read back
    /// as 14 JSON values.
    pub fn from_columns(cols: Vec<serde_json::Value>) -> anyhow::Result<Self> {
        let obj: serde_json::Map<String, serde_json::Value> =
            COLUMNS.iter().map(|c| c.to_string()).zip(cols).collect();
        Ok(serde_json::from_value(serde_json::Value::Object(obj))?)
    }
}

/// One short text column: `first_name` on its own.
#[derive(Zeroize, Default)]
pub struct ShortText {
    pub first_name: String,
}

#[derive(EncryptTarget, DecryptSource)]
#[stash(struct = ShortText, scope = "individuals")]
pub struct EncryptedShortText {
    pub first_name: TextEq,
}

/// One ~2 KB text column.
#[derive(Zeroize, Default)]
pub struct LongText {
    pub bio: String,
}

#[derive(EncryptTarget, DecryptSource)]
#[stash(struct = LongText, scope = "documents")]
pub struct EncryptedLongText {
    pub bio: Text,
}

/// One ~2 KB JSON document, sealed whole.
#[derive(Zeroize)]
pub struct JsonDoc {
    pub profile: JsonPlaintext,
}

#[derive(EncryptTarget, DecryptSource)]
#[stash(struct = JsonDoc, scope = "documents")]
pub struct EncryptedJsonDoc {
    pub profile: Json,
}

// --- data -------------------------------------------------------------------

const FIRST: &[&str] = &[
    "Ada", "Grace", "Edsger", "Barbara", "Tony", "Margaret", "Donald", "Frances", "Ken", "Radia",
    "Alan", "Hedy", "Linus", "Shafi", "Niklaus", "Leslie",
];
/// The same list as the Go model's `LastNames`: a match probe for any of
/// them finds rows whichever SDK wrote the table.
pub const LAST: &[&str] = &[
    "Lovelace", "Hopper", "Dijkstra", "Liskov", "Hoare", "Hamilton", "Knuth", "Allen", "Thompson",
    "Perlman", "Turing", "Lamarr", "Torvalds", "Goldwasser", "Wirth", "Lamport",
];
const COUNTRIES: &[&str] = &[
    "Australia", "New Zealand", "United Kingdom", "United States", "Canada", "Singapore", "Germany",
    "France",
];
const CITIES: &[&str] = &[
    "Sydney", "Melbourne", "Auckland", "London", "New York", "Toronto", "Singapore", "Berlin", "Paris",
];
const JOBS: &[&str] = &["Engineer", "Accountant", "Teacher", "Nurse", "Lawyer", "Architect", "Chef", "Pilot"];
const WORDS: &[&str] = &[
    "lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipiscing", "elit", "sed", "do",
    "eiusmod", "tempor", "incididunt", "ut", "labore", "et", "dolore", "magna", "aliqua", "enim",
];

pub fn rng(seed: u64) -> SmallRng {
    SmallRng::seed_from_u64(seed)
}

fn pick<'a>(r: &mut SmallRng, xs: &[&'a str]) -> &'a str {
    xs[r.gen_range(0..xs.len())]
}

/// Row `id`'s email. Matches the Go model's `EmailFor`.
pub fn email_for(id: i64) -> String {
    format!("person{id}@example.com")
}

pub fn gen_individual(r: &mut SmallRng, id: i64) -> Individual {
    let dob = jiff::civil::date(
        1940 + r.gen_range(0..65),
        1 + r.gen_range(0..12),
        1 + r.gen_range(0..28),
    );
    Individual {
        first_name: pick(r, FIRST).into(),
        last_name: pick(r, LAST).into(),
        email: email_for(id),
        phone: format!(
            "+61 4{:02} {:03} {:03}",
            r.gen_range(0..100),
            r.gen_range(0..1000),
            r.gen_range(0..1000)
        ),
        date_of_birth: Date::from(dob),
        nationality: pick(r, COUNTRIES).into(),
        tax_id: format!(
            "{:03}-{:03}-{:03}",
            r.gen_range(0..1000),
            r.gen_range(0..1000),
            r.gen_range(0..1000)
        ),
        address_line1: format!("{} {} Street", 1 + r.gen_range(0..400), pick(r, LAST)),
        address_line2: format!("Unit {}", 1 + r.gen_range(0..60)),
        city: pick(r, CITIES).into(),
        postcode: format!("{:04}", r.gen_range(0..10000)),
        country: pick(r, COUNTRIES).into(),
        occupation: pick(r, JOBS).into(),
        risk_score: r.gen_range(0..1000),
    }
}

pub fn gen_text(r: &mut SmallRng, n: usize) -> String {
    let mut s = String::with_capacity(n + 16);
    while s.len() < n {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(pick(r, WORDS));
    }
    s.truncate(n);
    s
}

pub fn gen_json(r: &mut SmallRng, id: i64, n: usize) -> String {
    let mut doc = serde_json::json!({
        "id": id,
        "name": format!("{} {}", pick(r, FIRST), pick(r, LAST)),
        "address": {"city": pick(r, CITIES), "country": pick(r, COUNTRIES), "postcode": format!("{:04}", r.gen_range(0..10000))},
        "tags": [pick(r, JOBS), pick(r, JOBS)],
        "score": r.gen_range(0..1000),
        "active": r.gen_bool(0.5),
    });
    let mut notes = Vec::new();
    loop {
        let s = doc.to_string();
        if s.len() >= n - 80 {
            return s;
        }
        notes.push(gen_text(r, 64));
        doc["notes"] = serde_json::json!(notes);
    }
}

// --- cipher -----------------------------------------------------------------

// Every bin builds its cipher with `StackCipher::with_detected_credentials()`:
// `CS_*` from the environment, or the profile under `CS_CONFIG_PATH`
// (default `~/.cipherstash`).

// --- timing -----------------------------------------------------------------

/// Process CPU time (user + system) so far.
pub fn cpu_now() -> Duration {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage writes into the struct we pass and nothing else.
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    assert_eq!(rc, 0, "getrusage failed");
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, (t.tv_usec as u32) * 1000);
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

pub fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// Wall and CPU time of one call.
pub async fn timed<F, T>(f: F) -> anyhow::Result<(f64, f64, T)>
where
    F: std::future::Future<Output = anyhow::Result<T>>,
{
    let (c0, t0) = (cpu_now(), Instant::now());
    let v = f.await?;
    Ok((ms(t0.elapsed()), ms(cpu_now() - c0), v))
}

/// One case: one JSON line, the same shape the Go bench writes.
#[derive(Serialize, Default)]
pub struct Record {
    pub sdk: &'static str,
    pub variant: String,
    pub mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<usize>,
    pub scenario: String,
    pub op: &'static str,
    pub rows: usize,
    pub values: usize,
    pub wall_ms: Vec<f64>,
    pub cpu_ms: Vec<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub query_ms: Vec<f64>,
    pub at: String,
}

pub fn median(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut s = xs.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s[s.len() / 2]
}

/// Print a summary line and append the JSON line to `out`, if given.
pub fn emit(mut r: Record, out: Option<&str>) -> anyhow::Result<()> {
    r.sdk = "rust";
    r.at = chrono::Utc::now().to_rfc3339();
    let lo = r.cpu_ms.iter().cloned().fold(f64::INFINITY, f64::min);
    let hi = r.cpu_ms.iter().cloned().fold(0.0, f64::max);
    let q = if r.query_ms.is_empty() {
        String::new()
    } else {
        format!("  query {:7.2} ms", median(&r.query_ms))
    };
    println!(
        "{:<8} {:<22} {:<8} rows={:<4} vals={:<5} cpu {:8.2} ms ({:.2}–{:.2})  wall {:8.2} ms  {:6.1} µs/value{}",
        r.variant,
        r.scenario,
        r.op,
        r.rows,
        r.values,
        median(&r.cpu_ms),
        lo,
        hi,
        median(&r.wall_ms),
        median(&r.cpu_ms) / r.values.max(1) as f64 * 1000.0,
        q
    );
    if let Some(path) = out {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{}", serde_json::to_string(&r)?)?;
    }
    Ok(())
}
