//! Seal and open with a fresh key per message, as the Go SDK's guest does
//! (every value has its own data key), for AES-256-GCM and ChaCha20-Poly1305.
//! Prints one JSON line per (cipher, op, size): nanoseconds per message.

use std::hint::black_box;
use std::time::Instant;

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::Aes256Gcm;
use chacha20poly1305::{ChaCha20Poly1305, XChaCha20Poly1305};

const POOL: usize = 64;
const SIZES: [usize; 4] = [16, 64, 256, 2048];

/// Deterministic filler, so the build needs no randomness source.
fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Run `f` until about `budget_ms` has passed and return ns per call.
fn time(budget_ms: u128, mut f: impl FnMut(usize)) -> f64 {
    for i in 0..POOL {
        f(i);
    }
    let t0 = Instant::now();
    let mut n = 0usize;
    while t0.elapsed().as_millis() < budget_ms {
        for _ in 0..POOL {
            f(n % POOL);
            n += 1;
        }
    }
    t0.elapsed().as_nanos() as f64 / n as f64
}

macro_rules! bench {
    ($name:expr, $Cipher:ty, $nonce_len:expr, $label:expr, $budget:expr) => {{
        for &size in &SIZES {
            // A pool of messages, each under its own key and nonce.
            let keys: Vec<Vec<u8>> = (0..POOL).map(|i| bytes(i as u64 + 1, 32)).collect();
            let nonces: Vec<Vec<u8>> = (0..POOL).map(|i| bytes(i as u64 + 1000, $nonce_len)).collect();
            let plain = bytes(7, size);
            let sealed: Vec<(Vec<u8>, Vec<u8>)> = (0..POOL)
                .map(|i| {
                    let c = <$Cipher>::new_from_slice(&keys[i]).unwrap();
                    let mut buf = plain.clone();
                    let tag = c
                        .encrypt_in_place_detached(nonces[i].as_slice().into(), b"aad", &mut buf)
                        .unwrap();
                    (buf, tag.to_vec())
                })
                .collect();

            let key_ns = time($budget, |i| {
                black_box(<$Cipher>::new_from_slice(black_box(&keys[i])).unwrap());
            });
            let seal_ns = time($budget, |i| {
                let c = <$Cipher>::new_from_slice(&keys[i]).unwrap();
                let mut buf = plain.clone();
                black_box(
                    c.encrypt_in_place_detached(nonces[i].as_slice().into(), b"aad", &mut buf)
                        .unwrap(),
                );
                black_box(buf);
            });
            let open_ns = time($budget, |i| {
                let c = <$Cipher>::new_from_slice(&keys[i]).unwrap();
                let (ct, tag) = &sealed[i];
                let mut buf = ct.clone();
                c.decrypt_in_place_detached(
                    nonces[i].as_slice().into(),
                    b"aad",
                    &mut buf,
                    tag.as_slice().into(),
                )
                .unwrap();
                black_box(buf);
            });
            for (op, ns) in [("key", key_ns), ("seal", seal_ns), ("open", open_ns)] {
                println!(
                    "{{\"build\":\"{}\",\"cipher\":\"{}\",\"op\":\"{}\",\"size\":{},\"ns\":{:.0}}}",
                    $label, $name, op, size, ns
                );
            }
        }
    }};
}

fn main() {
    let label = std::env::args().nth(1).unwrap_or_else(|| "native".into());
    let budget: u128 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(300);
    bench!("aes-256-gcm", Aes256Gcm, 12, label, budget);
    bench!("chacha20-poly1305", ChaCha20Poly1305, 12, label, budget);
    bench!("xchacha20-poly1305", XChaCha20Poly1305, 24, label, budget);
}
