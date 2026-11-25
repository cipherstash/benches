# Go SDK rc.7 against rc.6

Dan Draper's benches, 25 November 2026. Measured on one Apple silicon laptop against ZeroKMS in ap-southeast-2, with a spinning process holding the CPU clock steady.

## Summary

Go SDK v0.1.0-rc.7 delivers the fixes that [V4_GO_DECRYPT.md](V4_GO_DECRYPT.md) proposed, and does better than its changelog says.

Decrypting 100 rows of 14 columns, against rc.6:

- **CPU falls from 390 ms to 29 ms**, 13 times less. Native Rust takes 10 ms, so the Go SDK now uses about 3 times native, down from about 40.
- **Wall time falls from 624 ms to 114 ms**, 5.5 times less. Native Rust takes 97 ms.
- **Encrypting the same rows** takes 33 ms of CPU, against 444 ms. Its wall time is 99 ms, against 618 ms, and matches native Rust.

The rc.7 changelog expects about 39 ms of CPU and 134 ms of wall time to decrypt, and 115 ms of wall time to encrypt. All three figures here are lower.

## What rc.7 changed

Four changes in rc.7 affect these figures:

- The host no longer sets `WithCloseOnContextDone(true)`, and the guest is built at `opt-level = 3` (cipherstash/stack#1759). These two changes give most of the CPU saving.
- The guest yields to the host every 64 data keys (cipherstash/stack#1765), so a garbage collector pause no longer waits for a whole decrypt.
- The guest sends up to 3 ZeroKMS requests at the same time (cipherstash/stack#1773). That change gives most of the wall-time saving.
- The guest re-encrypts key material with wasm SIMD (instructions that work on several values at once). The guest now uses `zerokms-keyset` 0.3.4 in place of `recipher` 0.3.1, and enables its `wasm-simd` feature (CIP-5528). The changelog does not mention it.

The SIMD change is the likely reason the CPU is lower than the changelog says. We inferred this, and did not test it alone in rc.7.

Without ZeroKMS, rc.7 decrypts 100 rows of 14 columns in 19 ms of CPU. The fix without SIMD took 18 ms in the same test (`next-fix` in V4_GO_DECRYPT.md), so the guest's other work costs the same. Against ZeroKMS, rc.7 takes 29 ms, against 38 ms for the fix without SIMD. That is 24% less, close to the 23% that `experiments/recipher-wasm/` measured for SIMD alone.

## Results

Every figure is the median of 54 calls: 6 rounds of 9 calls for each release. The rounds alternated between rc.6 and rc.7, and alternated which went first. The CPU medians of single rounds were within 8% of each other. Native Rust ran once, 6 calls per case.

**Decrypt CPU falls by 10 to 14 times: CPU per call, ms**

| Case | Values | Native | rc.6 | rc.7 |
|---|--:|--:|--:|--:|
| 100 rows of 14 columns, no database | 1,400 | 10.1 | 390 | 29.1 |
| Page of 100 by id, 1M rows | 1,400 | 10.1 | 400 | 29.1 |
| Name match, 1M rows | 1,400 | 10.1 | 399 | 29.0 |
| Risk score range, 1M rows | 1,400 | 10.0 | 397 | 29.2 |
| One row by email, 1M rows | 14 | 0.17 | 10.2 | 1.37 |
| 100 rows, short text | 100 | 0.79 | 29.6 | 2.59 |
| 100 rows, 2 KB text | 100 | 0.89 | 90.0 | 6.88 |
| 100 rows, 2 KB JSON | 100 | 1.22 | 104 | 7.98 |

**Decrypt wall time is now close to native: wall time per call, ms**

| Case | Native | rc.6 | rc.7 |
|---|--:|--:|--:|
| 100 rows of 14 columns, no database | 97 | 624 | 114 |
| Page of 100 by id, 1M rows | 112 | 641 | 116 |
| Name match, 1M rows | 111 | 632 | 117 |
| Risk score range, 1M rows | 112 | 630 | 113 |
| 100 rows, 2 KB JSON | 36 | 140 | 41 |

One ZeroKMS round trip from this laptop takes about 25 to 30 ms. That is most of the wall time for a small batch.

**Encrypt matches native wall time: 100 rows of 14 columns, ms**

| Measure | Native | rc.6 | rc.7 |
|---|--:|--:|--:|
| CPU | 12.3 | 444 | 33.4 |
| Wall time | 97 | 618 | 99 |

**Without ZeroKMS, rc.7 needs 6 to 7 times less CPU: offline keys, CPU per call, ms**

| Case | rc.6 | rc.7 |
|---|--:|--:|
| Decrypt 100 rows of 14 columns | 129 | 19.3 |
| Encrypt 100 rows of 14 columns | 176 | 25.1 |
| Decrypt 100 rows, 2 KB JSON | 87 | 7.4 |

## What is left

- **CPU is about 3 times native.** WebAssembly has no AES instructions, so the guest runs AES-GCM in software. `experiments/aead-wasm/` found that is 1 to 12% of a decrypt's CPU.
- **One `Client` runs one guest call at a time.** We have not measured several pages decrypted at once (`gobench -mode db -clients N`).
- **We did not measure garbage collector pauses with rc.7.** The yield every 64 keys is in rc.7, and V4_GO_DECRYPT.md measured it on `next`.

## How to reproduce

rc.6 and rc.7 are the `rc.6` and `rc.7` variants of `scripts/go-variant.sh`. Each is built from its release tag, with the guests the tag contains. The raw results are in `results/v4-rc7/`.

```bash
perl -e 'while(1){}' &    # hold the CPU clock; stop it afterwards
export V4_RESULTS=results/v4-rc7 GO_FLAGS="-rounds 1 -calls 9"
for mode in micro db; do
  for r in 1 2 3 4 5 6; do   # swap which release goes first each round
    if [ $((r % 2)) -eq 1 ]; then order="rc.6 rc.7"; else order="rc.7 rc.6"; fi
    TIER=1000000 mise run bench:v4:go $mode $order
  done
done
mise run bench:v4:go:offline rc.6 rc.7
mise run bench:v4:crypto && mise run bench:v4:query 1000000
python3 report_v4.py --dir results/v4-rc7 --pool [--op encrypt] [--metric wall]
```

The runs here used a script that ran the same commands, with prebuilt `gobench` binaries.
