# The client half of ZeroKMS key derivation in the Go SDK's wasm guest

**Result: recipher's `reencrypt` can be made about 3 times faster in wasm, but that saves only about 5 to 15% of a decrypt's CPU.** The rewrite applies each permutation in one pass and gives byte-identical output. Most of the remaining CPU is elsewhere in the SDK.

Measured on 23 November 2026, on one Apple silicon laptop under heavy load from other work (load average 8 to 10 on 10 cores). Compare figures within a table, not across tables.

## What it measures

For every value it decrypts, the guest derives a data key from the key material ZeroKMS returns. The stack-kms function `DataKey::from_key_material` runs recipher's `ProxyCipher::reencrypt::<16>` over 33 blocks, then SHA-256. `src/main.rs` times both parts with recipher 0.3.1, the version the guest links.

It also times `reencrypt_single_pass`: the same algorithm, with each permutation applied as `out[i] = in[perm[i]]` through a copy. recipher's `permute_slice_mut` permutes in place by following cycles. That needs a data-dependent loop and a bounds check for every element. The program checks that both give the same bytes before it times anything. `reencrypt` runs 100 permutations per value.

The wasm builds run under wazero 1.12 through `go/cmd/wasmrun`, with `WithCloseOnContextDone` off (cipherstash/stack#1759) and on (rc.6).

## Results

**Per value, isolated, fastest of two runs (µs)**

| Case | Native | wasm | wasm, `+simd128` | wasm, option on (rc.6) |
|---|--:|--:|--:|--:|
| `from_key_material` (reencrypt and SHA-256) | 2.70 | 7.21 | 6.59 | 72.7 |
| `reencrypt` | 1.66 | 5.92 | 5.45 | 70.0 |
| SHA-256 of 528 bytes | 1.00 | 1.18 | 1.18 | 1.37 |
| `reencrypt`, single-pass | 0.67 | 1.89 | 1.33 | 4.17 |
| `from_key_material`, single-pass | 1.68 | 3.10 | 2.52 | 5.55 |

In wasm, `reencrypt` is 3.6 times slower than native. The single-pass version is 3.1 times faster than recipher's. Under rc.6's option, `reencrypt` was 12 times slower still: its permutations are loops inside loops.

**In the guest: a decrypt of 100 rows of 14 columns against ZeroKMS (ms of CPU, 30 calls each)**

A throwaway build of cipherstash/stack#1765 used the single-pass permutations in `DataKey::from_key_material`, cached per client key. It decrypted rows that unmodified stack-encrypt had written, so it derived the right keys. Runs alternated between the two builds.

| Build | Minimum | Median |
|---|--:|--:|
| #1765 | 82.1 | 95.2 |
| #1765 with single-pass `reencrypt` | 76.8 | 90.3 |
| #1765, efficiency cores only (`taskpolicy -b`) | 97.7 | 182 |
| #1765 with single-pass, efficiency cores only | 87.0 | 152 |

The saving is about 5 to 6% normally, and 10 to 15% on the slower cores. That is less than the isolated figures predict (about 18%), probably because the machine was loaded. A quiet machine would give a firmer number.

## Before adopting it

- It changes recipher, a cryptographic library. Both versions read the permutation by index, and the permutation is key material. The single-pass version has fixed control flow. recipher's loop count depends on the permutation, so it leaks timing that the single-pass version does not. A constant-time review should decide whether either is acceptable.
- It saves at most a few ms per 1,400 values. For larger savings, a decrypt needs fewer derivations per batch. That is a ZeroKMS protocol question.

## Run it

```bash
cargo build --release
cargo build --release --target wasm32-wasip1
CARGO_TARGET_DIR=target-simd RUSTFLAGS='-C target-feature=+simd128' cargo build --release --target wasm32-wasip1
(cd ../../go && go build -o bin/wasmrun ./cmd/wasmrun)
./target/release/recipher-wasm native 500
../../go/bin/wasmrun -wasm target/wasm32-wasip1/release/recipher-wasm.wasm -- wasm 500
../../go/bin/wasmrun -close -wasm target/wasm32-wasip1/release/recipher-wasm.wasm -- wasm-closeondone 500
```
