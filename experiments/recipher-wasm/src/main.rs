//! Time the client half of ZeroKMS data-key derivation, per value, as
//! stack-kms's `DataKey::from_key_material` does it:
//!
//!   ProxyCipher::new(keyset).reencrypt::<16>(iv, material), then SHA-256.
//!
//! Also times `reencrypt_single_pass`: the same algorithm with each
//! permutation applied as `out[i] = in[perm[i]]` through a copy, instead of
//! recipher's in-place cycle following, and checks that it gives the same
//! bytes. Prints one JSON line per case: nanoseconds per value.

use std::hint::black_box;
use std::time::Instant;

use recipher::cipher::ProxyCipher;
use recipher::keyset::{EncryptionKeySet, ProxyKeySet};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const POOL: usize = 64;

/// The keyset's permutations, read back from its CBOR encoding (the
/// fields are crate-private in recipher).
#[derive(Deserialize)]
struct Perm {
    permutation: Vec<u8>,
}

#[derive(Deserialize)]
struct Keyset {
    p1: Perm,
    p2_from: Perm,
    p2_to: Perm,
    p3: Perm,
}

/// `out[i] = in[perm[i]]`: what recipher's `permute_slice_mut` computes.
#[inline(always)]
fn permute<const N: usize, T: Copy>(perm: &[u8; N], input: &mut [T; N]) {
    let src = *input;
    for i in 0..N {
        input[i] = src[perm[i] as usize];
    }
}

fn xor(a: &mut [u8; 16], b: &[u8; 16]) {
    for i in 0..16 {
        a[i] ^= b[i];
    }
}

/// recipher's `reencrypt::<16>`, with the permutations applied in one pass.
/// Fixed at 33 blocks, the size every keyset from `EncryptionKeySet::generate`
/// has.
fn reencrypt_single_pass(k: &Fixed, iv: &[u8; 16], input: &[u8]) -> Vec<u8> {
    let mut data = [[0u8; 16]; 33];
    for (block, chunk) in data.iter_mut().zip(input.chunks_exact(16)) {
        block.copy_from_slice(chunk);
    }
    let mut p2 = *iv;
    for block in data.iter_mut() {
        let tmp = *block;
        permute(&k.p2_from, &mut p2);
        xor(block, &p2);
        p2 = tmp;
        permute(&k.p1, block);
    }
    permute(&k.p3, &mut data);
    p2 = *iv;
    for c in data.iter_mut() {
        permute(&k.p2_to, &mut p2);
        xor(c, &p2);
        p2 = *c;
    }
    data.concat()
}

struct Fixed {
    p1: [u8; 16],
    p2_from: [u8; 16],
    p2_to: [u8; 16],
    p3: [u8; 33],
}

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

fn main() {
    let label = std::env::args().nth(1).unwrap_or_else(|| "native".into());
    let budget: u128 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(500);

    let a = EncryptionKeySet::generate().unwrap();
    let b = EncryptionKeySet::generate().unwrap();
    let keyset = ProxyKeySet::generate(&a, &b);
    let mirror: Keyset = serde_cbor::from_slice(&keyset.to_bytes().unwrap()).unwrap();
    let fixed = Fixed {
        p1: mirror.p1.permutation.try_into().unwrap(),
        p2_from: mirror.p2_from.permutation.try_into().unwrap(),
        p2_to: mirror.p2_to.permutation.try_into().unwrap(),
        p3: mirror.p3.permutation.try_into().unwrap(),
    };

    let len = ProxyCipher::new(&keyset).expected_input_len::<16>();
    assert_eq!(len, 33 * 16, "a generated keyset covers 33 blocks");
    let ivs: Vec<[u8; 16]> = (0..POOL).map(|i| bytes(i as u64 + 1, 16).try_into().unwrap()).collect();
    let materials: Vec<Vec<u8>> = (0..POOL).map(|i| bytes(i as u64 + 500, len)).collect();

    // The single-pass variant must give recipher's bytes exactly.
    for i in 0..POOL {
        let want = ProxyCipher::new(&keyset).reencrypt::<16>(&ivs[i], &materials[i]).unwrap();
        assert_eq!(reencrypt_single_pass(&fixed, &ivs[i], &materials[i]), want, "value {i}");
    }

    let rect: Vec<Vec<u8>> = (0..POOL)
        .map(|i| ProxyCipher::new(&keyset).reencrypt::<16>(&ivs[i], &materials[i]).unwrap())
        .collect();
    let cases: [(&str, f64); 5] = [
        ("from_key_material (reencrypt + sha256)", time(budget, |i| {
            let r = ProxyCipher::new(&keyset).reencrypt::<16>(&ivs[i], &materials[i]).unwrap();
            black_box(Sha256::digest(&r));
        })),
        ("reencrypt", time(budget, |i| {
            black_box(ProxyCipher::new(&keyset).reencrypt::<16>(&ivs[i], &materials[i]).unwrap());
        })),
        ("sha256 of 528 bytes", time(budget, |i| {
            black_box(Sha256::digest(&rect[i]));
        })),
        ("reencrypt, single-pass permutations", time(budget, |i| {
            black_box(reencrypt_single_pass(&fixed, &ivs[i], &materials[i]));
        })),
        ("from_key_material, single-pass", time(budget, |i| {
            let r = reencrypt_single_pass(&fixed, &ivs[i], &materials[i]);
            black_box(Sha256::digest(&r));
        })),
    ];
    for (case, ns) in cases {
        println!("{{\"build\":\"{label}\",\"case\":\"{case}\",\"ns\":{ns:.0}}}");
    }
}
