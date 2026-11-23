# The client half of ZeroKMS key derivation in the Go SDK's wasm guest

**Result: deriving data keys takes about 30% of the CPU of a Go SDK decrypt, and recipher's `reencrypt` is almost all of it.** A rewrite of `reencrypt` applies each permutation in one pass and gives byte-identical output. On a quiet machine, it cuts a decrypt's CPU by about 15%. SHA-256 is about 4%. Setting up the cipher for each value costs nothing.

recipher is now published as `zerokms-keyset`. The figures here are for recipher 0.3.1, the version the guest links.

Measured on 23 November 2026, on one Apple silicon laptop. Other work loaded the machine at times. The tables say which figures come from a quiet machine. Compare figures within a table, not across tables.

## What it measures

For every value it decrypts, the guest derives a data key from the key material ZeroKMS returns. Every value has its own key, so a page of 100 rows of 14 columns needs 1,400 derivations. The stack-kms function `DataKey::from_key_material` runs recipher's `ProxyCipher::reencrypt::<16>` over 33 blocks, then SHA-256. `ProxyCipher::new` only borrows the keyset.

`src/main.rs` times both parts on their own. It also times `reencrypt_single_pass`: the same algorithm, with each permutation applied as `out[i] = in[perm[i]]` through a copy. recipher's `permute_slice_mut` permutes in place by following cycles. That needs a data-dependent loop and a bounds check for every element. The program checks that both give the same bytes before it times anything. `reencrypt` runs 100 permutations per value.

The wasm builds run under wazero 1.12 through `go/cmd/wasmrun`, with `WithCloseOnContextDone` off (cipherstash/stack#1759) and on (rc.6).

## Results

### In the guest: where a decrypt's CPU goes

**`reencrypt` takes most of the 30% that derivation costs: one decrypt of 100 rows of 14 columns, quiet machine**

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
- **Ablation.** Guest builds that repeat part of the derivation for every value, with nothing listening. `derive-ablation.patch` is the change to stack-kms. With 4 extra `reencrypt` calls per value, a decrypt cost 37.9 ms more, so 6.8 µs per `reencrypt`. With 16 extra SHA-256 hashes per value, it cost 27.6 ms more, so 1.23 µs per hash. The 6 runs of each were within 2 ms of each other.

A loaded machine made everything about 2.5 times slower: a decrypt took about 100 ms of CPU, not 40. The shares stayed about the same. The earlier profile in the Go SDK write-up ran on the loaded machine. Its shares hold. Its times are about 2.5 times too high.

### In the guest: the single-pass rewrite

A throwaway build of cipherstash/stack#1765 used the single-pass permutations in `DataKey::from_key_material`, cached per client key. It decrypted rows that unmodified stack-encrypt had written, so it derived the right keys. Runs alternated between the two builds.

**Single-pass `reencrypt` saves about 6 ms, or 15%: ms of CPU per decrypt of 100 rows of 14 columns, quiet machine**

| Round | #1765 | #1765 with single-pass |
|--:|--:|--:|
| 1 | 39.0 | 33.2 |
| 2 | 40.0 | 34.6 |
| 3 | 39.6 | 34.0 |
| 4 | 41.4 | 35.3 |
| 5 | 41.9 | 35.3 |
| 6 | 41.6 | 35.1 |
| 7 | 41.5 | 34.9 |

Rounds where load spiked during either run are left out. `in-guest-attribution.jsonl` has every round.

The isolated figures below predict a saving of 5.6 ms. On a loaded machine, an earlier A/B found 5 to 15%. The saving depended on which cores ran it.

### Isolated, per value

**In wasm, the single-pass `reencrypt` is 3.1 times faster than recipher's: µs per value, fastest of two runs**

In wasm, recipher's `reencrypt` is 3.6 times slower than native.

| Case | Native | wasm | wasm, `+simd128` | wasm, option on (rc.6) |
|---|--:|--:|--:|--:|
| `from_key_material` (reencrypt and SHA-256) | 2.70 | 7.21 | 6.59 | 72.7 |
| `reencrypt` | 1.66 | 5.92 | 5.45 | 70.0 |
| SHA-256 of 528 bytes | 1.00 | 1.18 | 1.18 | 1.37 |
| `reencrypt`, single-pass | 0.67 | 1.89 | 1.33 | 4.17 |
| `from_key_material`, single-pass | 1.68 | 3.10 | 2.52 | 5.55 |

Under rc.6's option, `reencrypt` was 12 times slower still: its permutations are loops inside loops.

`+simd128` here only lets the compiler vectorise loops it already has. A version written with wasm's 16-byte shuffle instruction (`i8x16.swizzle`) has not been tried. Each 16-byte permutation would become one instruction.

## Before adopting it

- It changes recipher (`zerokms-keyset`), a cryptographic library. Both versions read the permutation by index, and the permutation is key material. The single-pass version has fixed control flow. recipher's loop count depends on the permutation, so it leaks timing that the single-pass version does not. A constant-time review should decide whether either is acceptable.
- Every value keeps its own key. That is a fixed requirement of ZeroKMS, so the number of derivations stays at one per value. Savings come from making each derivation cheaper.
- The remaining 70% of the CPU is elsewhere in the guest and the Go host.

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

For the in-guest ablation, apply `derive-ablation.patch` to a stack checkout and build the guest with EQL types with `DERIVE_EXTRA_REENCRYPT` or `DERIVE_EXTRA_SHA` set. The counts are fixed at build time. Then build `go/cmd/guestprof` against it with `scripts/go-variant.sh local`, and run the builds in alternation. Check `uptime` first: the figures only compare between runs on a machine whose load did not change.
