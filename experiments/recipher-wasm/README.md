# The client half of ZeroKMS key derivation in the Go SDK's wasm guest

**Result: a wasm SIMD version of recipher's `reencrypt` cuts the CPU of a Go SDK decrypt by 23%.** Each 16-byte permutation becomes one shuffle instruction. On its own, `reencrypt` falls from 5.9 µs to 0.1 µs per value. Its output matches recipher's byte for byte. Key derivation falls from 30% of a decrypt's CPU to 9%, and SHA-256 is now most of what is left.

recipher is now published as `zerokms-keyset`. The figures here are for recipher 0.3.1, the version the guest links. Every value keeps its own key: these changes make each derivation cheaper, and do not reduce the number of derivations.

Measured on 23 November 2026, on one Apple silicon laptop. Compare figures within a table, not across tables.

## What it measures

For every value it decrypts, the guest derives a data key from the key material ZeroKMS returns. A page of 100 rows of 14 columns needs 1,400 derivations. The stack-kms function `DataKey::from_key_material` runs recipher's `ProxyCipher::reencrypt::<16>` over 33 blocks, then SHA-256. `ProxyCipher::new` only borrows the keyset, so it costs nothing.

Each value needs 100 permutations: 3 of 16 bytes for each of 33 blocks, and 1 of the 33 blocks. In recipher, `permute_slice_mut` permutes in place by following cycles. That needs a data-dependent loop and a bounds check for every element. `src/main.rs` compares it with two rewrites, and checks that each gives recipher's bytes before it times anything:

- **Single-pass** applies each permutation as `out[i] = in[perm[i]]` through a copy.
- **SIMD** holds each 16-byte block in a wasm `v128` register. Each 16-byte permutation is one `i8x16.swizzle`, and each XOR one `v128.xor`. SIMD is enabled on this one function with `#[target_feature(enable = "simd128")]`, so the rest of the module compiles as before. The wazero runtime enables SIMD by default. The Go SDK does not turn it off.

The wasm builds run under wazero 1.12 through `go/cmd/wasmrun`, with `WithCloseOnContextDone` off (cipherstash/stack#1759) and on (rc.6).

## Results

### In the guest

A throwaway build of cipherstash/stack#1765 used each rewrite in `DataKey::from_key_material`, with the permutations cached per client key. `derive-simd-single-pass.patch` is the change to stack-kms. Each build decrypted rows that unmodified stack-encrypt had written, so each derived the right keys. Runs alternated between the builds, round by round.

**The SIMD build uses 23% less CPU: ms of CPU per decrypt of 100 rows of 14 columns, 8 rounds**

| Build | Median | Range | Saving, paired median |
|---|--:|--:|--:|
| #1765 | 39.1 | 38.5 to 40.8 | not applicable |
| #1765 with single-pass `reencrypt` | 32.4 | 32.0 to 33.3 | 6.7 ms (17%) |
| #1765 with SIMD `reencrypt` | 30.3 | 29.5 to 31.3 | 8.8 ms (23%) |

**Derivation falls from 30% of the CPU to 9%: ms per decrypt, from wazero's function listener, 3 runs**

| Part | #1765 | #1765 with SIMD |
|---|--:|--:|
| Whole decrypt (process CPU) | 39.3 | 30.0 |
| All guest code | 34.0 | 25.0 |
| `DataKey::from_key_material` | 11.9 | 2.7 |
| SHA-256 in `from_key_material` | 1.8 | 1.8 |

In the SIMD build, the 2.7 ms of derivation is about 1.8 ms of SHA-256, 0.1 ms of `reencrypt`, and 0.8 ms of other work. The other work includes looking up the cached permutations and wiping buffers.

### Where the time goes, before the change

**`reencrypt` takes most of the 30% that derivation costs: one decrypt of 100 rows of 14 columns**

Each row after the first is part of the row above it.

| Part | µs per value | ms per decrypt | Share of process CPU |
|---|--:|--:|--:|
| Whole decrypt (process CPU) | 28.3 | 39.6 | 100% |
| All guest code | 24.4 | 34.2 | 86% |
| `DataKey::from_key_material` | 8.4 | 11.7 | 30% |
| `reencrypt` | 6.7 | 9.4 | 24% |
| SHA-256 of 528 bytes | 1.2 | 1.7 | 4% |

Two methods give these figures, and they agree:

- **Function listener.** `go/cmd/guestprof` times named guest functions through a wazero listener. The listener adds little time to each call. SHA-256's block function runs 2,800 times per decrypt. The listener times it at 1.3 µs per value, against 1.2 µs in isolation.
- **Ablation.** Guest builds that repeat part of the derivation for every value, with nothing listening. `derive-ablation.patch` is the change to stack-kms. With 4 extra `reencrypt` calls per value, a decrypt cost 37.9 ms more, so 6.8 µs per `reencrypt`. With 16 extra SHA-256 hashes per value, it cost 27.6 ms more, so 1.23 µs per hash.

### Isolated, per value

**In wasm, SIMD `reencrypt` is 60 times faster than recipher's: µs per value, fastest of three runs**

| Case | Native | wasm | wasm, `+simd128` for the whole module | wasm, option on (rc.6) |
|---|--:|--:|--:|--:|
| `from_key_material` (reencrypt and SHA-256) | 2.70 | 7.10 | 6.59 | 72.7 |
| `reencrypt` | 1.66 | 5.87 | 5.45 | 70.0 |
| SHA-256 of 528 bytes | 1.00 | 1.17 | 1.18 | 1.37 |
| `reencrypt`, single-pass | 0.67 | 1.77 | 1.33 | 4.17 |
| `from_key_material`, single-pass | 1.68 | 3.03 | 2.52 | 5.55 |
| `reencrypt`, SIMD | not measured | 0.097 | not measured | not measured |
| `from_key_material`, SIMD | not measured | 1.27 | not measured | not measured |

The SIMD rows were measured later than the others, on the same machine. The column with `+simd128` for the whole module only lets the compiler vectorise the loops it already has.

## How to measure on this laptop

The decrypt does a few ms of work between ZeroKMS round trips. Between those bursts, macOS lets the performance cores clock down, so the guest runs about 2.5 times slower. A decrypt then takes about 100 ms of CPU, not 40. Load from other programs does not cause this: it happened with a load average of 2. A tight loop, such as the isolated benchmark, keeps the clock up and does not see it.

The slow state also makes comparisons smaller than they are. With SIMD, derivation took less time, so the cores spent more of each decrypt clocked down. In that state SHA-256, unchanged code, ran 25% slower in the SIMD build than in the base build.

To hold the clock up, run a spinning process alongside the benchmark, such as `perl -e 'while(1){}' &`. The benchmark reports its own process's CPU, so the spinner adds nothing to the figures. Every in-guest figure above was measured this way, or on a machine in the same state. `in-guest-attribution.jsonl` also has the runs taken with the cores clocked down.

## Before adopting it

- **Where it goes.** The change belongs in `zerokms-keyset`, with recipher's `reencrypt` as the native path. A native version could use NEON on arm64 (`vqtbl1q_u8`) or SSSE3 on x86 (`pshufb`) in the same way. That has not been tried.
- **Constant time.** The permutations are key material, and each version applies them differently. In recipher, the loop count depends on the permutation, so its timing can leak it. The single-pass version reads memory at key-dependent addresses. The SIMD version keeps the 16-byte permutations in registers. The wazero compiler turns `i8x16.swizzle` into one `TBL` instruction on arm64. On amd64 it uses `PSHUFB` with a fixed mask. Neither instruction's timing depends on the index values. The permutation of the 33 blocks still reads memory at key-dependent addresses, as recipher does. That read could be made constant-time by selecting each block with masks, at some cost. A constant-time review should decide.
- **Unsafe code.** The SIMD version needs `unsafe` to load and store `v128` values. stack-kms forbids `unsafe`, so the throwaway build allowed it for the one module. The real change belongs in `zerokms-keyset`. That crate allows `unsafe`, but uses none today.
- **The next cost.** After this change, SHA-256 is about two-thirds of derivation, and about 6% of a decrypt. Changing the hash would change every derived key, so that is a question for ZeroKMS v2. The other 91% of a decrypt's CPU is elsewhere in the guest and the Go host.

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

For the in-guest builds, apply a patch to a stack checkout and build the guest with EQL types (`--features eql`):

- `derive-simd-single-pass.patch` builds single-pass by default, and SIMD with `DERIVE_SIMD=1` set.
- `derive-ablation.patch` repeats parts of the derivation. Set `DERIVE_EXTRA_REENCRYPT` or `DERIVE_EXTRA_SHA` to the count.

These settings take effect when the guest is built. Then build `go/cmd/guestprof` against each guest with `scripts/go-variant.sh local`, and run the builds in alternation with a spinner running.
