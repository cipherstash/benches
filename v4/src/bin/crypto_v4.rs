//! Native stack-encrypt: the five cases BNDRY reported against the Go SDK,
//! run through the same Rust engine natively (aws-lc AES-GCM, no wasm).
//! This is the floor the Go SDK's guest is measured against.
//!
//!   cargo run --release --bin crypto_v4 -- [--rounds 2] [--calls 3] [--out FILE]

use benches_v4::*;
use stack_encrypt::{DecryptFrom, EncryptInto, Protected};

struct Args {
    rounds: usize,
    calls: usize,
    out: Option<String>,
    only: Option<String>,
}

fn args() -> Args {
    let mut a = Args { rounds: 2, calls: 3, out: None, only: None };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let v = it.next().expect("flag needs a value");
        match k.as_str() {
            "--rounds" => a.rounds = v.parse().unwrap(),
            "--calls" => a.calls = v.parse().unwrap(),
            "--out" => a.out = Some(v),
            "--only" => a.only = Some(v),
            _ => panic!("unknown flag {k}"),
        }
    }
    a
}

macro_rules! case {
    ($a:expr, $keyset:expr, $name:expr, $values_per_row:expr, $gen:expr, $Enc:ty, $Plain:ty) => {{
        if $a.only.as_deref().map_or(true, |o| o == $name) {
            let gen = $gen;
            let rows = gen().len();
            // decrypt: seal once, open rounds x calls times
            let sealed: Vec<$Enc> = gen().encrypt_into(&$keyset).await?;
            let mut store = Some(sealed);
            let mut rec = Record {
                variant: "native".into(),
                mode: "micro",
                scenario: $name.into(),
                op: "decrypt",
                rows,
                values: rows * $values_per_row,
                ..Default::default()
            };
            // warm-up + timed calls. Decryption consumes the sealed values,
            // so each call re-seals outside the timed region.
            for i in 0..=($a.rounds * $a.calls) {
                let sealed = match store.take() {
                    Some(s) => s,
                    None => gen().encrypt_into(&$keyset).await?,
                };
                let (w, c, _opened) = timed(async {
                    Ok(Vec::<Protected<$Plain>>::decrypt_from(sealed, &$keyset).await?)
                })
                .await?;
                if i > 0 {
                    rec.wall_ms.push(w);
                    rec.cpu_ms.push(c);
                }
            }
            emit(rec, $a.out.as_deref())?;

            let mut rec = Record {
                variant: "native".into(),
                mode: "micro",
                scenario: $name.into(),
                op: "encrypt",
                rows,
                values: rows * $values_per_row,
                ..Default::default()
            };
            for i in 0..=($a.rounds * $a.calls) {
                let input = gen();
                let (w, c, _s) = timed(async {
                    let s: Vec<$Enc> = input.encrypt_into(&$keyset).await?;
                    Ok(s)
                })
                .await?;
                if i > 0 {
                    rec.wall_ms.push(w);
                    rec.cpu_ms.push(c);
                }
            }
            emit(rec, $a.out.as_deref())?;
        }
    }};
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = args();
    let cipher = StackCipher::with_detected_credentials().await?;
    let keyset = cipher.default_keyset();

    let people = |n: usize| {
        let mut r = rng(42);
        (1..=n as i64).map(|id| gen_individual(&mut r, id)).collect::<Vec<_>>()
    };
    let short = |n: usize| {
        people(n)
            .into_iter()
            .map(|p| ShortText { first_name: p.first_name.clone() })
            .collect::<Vec<_>>()
    };

    case!(a, keyset, "1_row_short_text", 1, || short(1), EncryptedShortText, ShortText);
    case!(a, keyset, "100_rows_short_text", 1, || short(100), EncryptedShortText, ShortText);
    case!(
        a,
        keyset,
        "100_rows_2kb_text",
        1,
        || {
            let mut r = rng(43);
            (0..100).map(|_| LongText { bio: gen_text(&mut r, 2048) }).collect::<Vec<_>>()
        },
        EncryptedLongText,
        LongText
    );
    case!(
        a,
        keyset,
        "100_rows_2kb_json",
        1,
        || {
            let mut r = rng(44);
            (1..=100)
                .map(|id| JsonDoc {
                    profile: eql_bindings::v4::plaintext::JsonPlaintext::parse(&gen_json(&mut r, id, 2048))
                        .expect("generated JSON parses"),
                })
                .collect::<Vec<_>>()
        },
        EncryptedJsonDoc,
        JsonDoc
    );
    case!(a, keyset, "100_rows_14_cols", 14, || people(100), EncryptedIndividual, Individual);
    Ok(())
}
