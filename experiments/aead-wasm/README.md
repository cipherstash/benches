# AES-256-GCM or ChaCha20-Poly1305 in the Go SDK's wasm guest

**Result: switching to ChaCha20-Poly1305 is not worth it.** ChaCha is 4.5 to 7 times faster than the guest's software AES-256-GCM per message. Against ZeroKMS, though, the AEAD is only about 1 to 3% of a decrypt's CPU per short value. For a 2 KB value it is about 12%. The rest is the ZeroKMS key path and the host.

Switching would save at most about 10%, and less for typical short columns.

Measured on 23 November 2026, on one Apple silicon laptop.

## What it measures

`src/main.rs` seals and opens messages with a fresh key each time, as the guest does. Every value has its own data key, so every value pays the key setup. It compares:

- AES-256-GCM: RustCrypto `aes-gcm` 0.10.3, configured as the guest links it through vitaminc-encrypt 0.10. In wasm this is software AES, because WebAssembly has no AES instructions.
- ChaCha20-Poly1305 and XChaCha20-Poly1305: RustCrypto `chacha20poly1305` 0.10.

The wasm builds run under wazero 1.12 through `go/cmd/wasmrun`, with `WithCloseOnContextDone` off (the fix, cipherstash/stack#1759) and on (rc.6). The `native` rows are RustCrypto, not the aws-lc the native SDK uses.

## Results

**Open, µs per message, fresh key each time**

| Build | Size (bytes) | AES-256-GCM | ChaCha20-Poly1305 | ChaCha speed-up |
|---|--:|--:|--:|--:|
| wasm | 16 | 2.05 | 0.28 | 7.2x |
| wasm | 256 | 4.91 | 1.03 | 4.8x |
| wasm | 2,048 | 30.1 | 6.76 | 4.5x |
| wasm, `+simd128` | 2,048 | 23.8 | 3.92 | 6.1x |
| wasm, option on (rc.6) | 2,048 | 74.4 | 29.7 | 2.5x |

Sealing costs about the same. XChaCha20-Poly1305 costs about 0.1 µs more per message than ChaCha20-Poly1305. The AES key schedule is about 0.7 µs of each AES message; ChaCha has none. `summarise.py results.jsonl` prints every row.

**The AEAD's share of the guest's decrypt CPU, with the fix**

| Case | Per value, against ZeroKMS (µs) | AES share | Per value, offline keys (µs) | AES share |
|---|--:|--:|--:|--:|
| 100 rows of 14 columns | 74 | 3% | 13 | 16% |
| 100 rows, short text | 176 | 1% | 14 | 15% |
| 100 rows, 2 KB text | 246 | 12% | 56 | 54% |

The per-value figures are from `gobench` (`results/v4/`). The AES share uses the open times above, so it is an estimate. With offline keys, AES is a large share because nothing else is expensive. Against ZeroKMS, the per-value key path costs far more: recipher's key derivation, decoding the response, and the host's HTTP and JSON work.

## Why we stop here

Besides the small gain, a switch would change the ciphertext format every SDK reads. AES-GCM is also FIPS-approved. ChaCha20-Poly1305 is not. The ZeroKMS key path is where the remaining CPU is.

## Run it

```bash
cargo build --release
cargo build --release --target wasm32-wasip1
CARGO_TARGET_DIR=target-simd RUSTFLAGS='-C target-feature=+simd128' cargo build --release --target wasm32-wasip1
(cd ../../go && go build -o bin/wasmrun ./cmd/wasmrun)
./target/release/aead-wasm native 300 > results.jsonl
../../go/bin/wasmrun -wasm target/wasm32-wasip1/release/aead-wasm.wasm -- wasm 300 >> results.jsonl
../../go/bin/wasmrun -close -wasm target/wasm32-wasip1/release/aead-wasm.wasm -- wasm-closeondone 300 >> results.jsonl
../../go/bin/wasmrun -wasm target-simd/wasm32-wasip1/release/aead-wasm.wasm -- wasm-simd128 300 >> results.jsonl
python3 summarise.py results.jsonl
```
