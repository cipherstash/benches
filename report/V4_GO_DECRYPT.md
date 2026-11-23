# Go SDK decrypt CPU: replication and fix

Dan Draper's benches, 23 November 2026. Measured on one Apple silicon laptop against ZeroKMS in ap-southeast-2.

## Summary

We reproduced the customer's report. The Go SDK, v0.1.0-rc.6, spends about 10 times the CPU that the same Rust engine spends natively to decrypt the same rows. Two settings cause most of it, as the customer found:

1. The host builds its wazero runtime with `WithCloseOnContextDone(true)`. That option makes wazero call back into Go at the top of every loop the guest runs, and the guest's cryptography is almost all loops.
2. The guest is built with `opt-level = "s"`. That setting optimises for size, not speed.

With both changed, decrypting 100 rows of 14 columns takes about 100 ms of CPU instead of 405. Its wall time falls from about 650 ms to 350. The `next` branch costs the same as rc.6. The fix on branch `fix/golang-guest-decrypt-perf` in `cipherstash/stack` brings `next` down to about 100 ms of CPU too.

The Go SDK still uses about 2.5 times the CPU of native Rust after the fix. Its wall time is about 2.5 times native too, mostly because it sends ZeroKMS one request per 500 values, one after another. See [What is left](#what-is-left).

## What we measured

Every figure is the median of 6 to 9 calls. CPU is the process's user plus system time (`getrusage`), so it counts every goroutine, including the HTTP client and the garbage collector. Wall time includes the ZeroKMS round trip. From this laptop, one round trip takes about 28 ms.

The builds compared:

| Name | What it is |
|---|---|
| native | stack-encrypt `next` (`5949db0`) in Rust, with no WebAssembly |
| shipped | Go SDK rc.6 as released |
| rebuilt-s | rc.6 with its guest rebuilt locally, unchanged: a check that our builds match the release |
| noclose | rc.6 without `WithCloseOnContextDone(true)` |
| o3 | rc.6 with its guest built at `opt-level = 3` |
| o3-noclose | rc.6 with both changes |
| next | Go SDK on `next` (`5949db0`), as it stands |
| next-fix | `next` with the fix |

The data is a person record with 14 encrypted EQL v4 columns. Eleven are text (3 `TextEq`, 1 `TextMatch`, and 7 `Text`). The others are a `Date` and an `IntegerOrd`. Rust on `next` wrote it. The Go SDK, rc.6 and `next`, read it back without trouble.

## Results

**Decrypting a page costs the same at every table size: 100 rows of 14 columns, CPU in ms**

| Case | Values | native | shipped | noclose | o3-noclose | next | next-fix |
|---|--:|--:|--:|--:|--:|--:|--:|
| 100 rows, no database | 1,400 | 40 | 403 | 117 | 97 | 409 | 104 |
| Page by id, 100k rows | 1,400 | 36 | 405 | 102 | 94 | 406 | 92 |
| Page by id, 1M rows | 1,400 | 39 | 400 | 121 | 93 | 408 | 106 |
| Name match, 1M rows | 1,400 | 15 | 397 | 83 | 83 | 405 | 90 |
| Risk score range, 1M rows | 1,400 | 32 | 396 | 100 | 100 | 406 | 101 |
| One row by email, 1M rows | 14 | 1.3 | 11 | 5.7 | 7.1 | 11 | 7.8 |

**Wall time falls by about half: the same cases, wall time in ms**

| Case | native | shipped | noclose | o3-noclose | next | next-fix |
|---|--:|--:|--:|--:|--:|--:|
| 100 rows, no database | 134 | 654 | 352 | 358 | 667 | 342 |
| Page by id, 1M rows | 157 | 637 | 440 | 385 | 659 | 462 |
| Name match, 1M rows | 110 | 685 | 324 | 330 | 646 | 364 |
| Risk score range, 1M rows | 131 | 647 | 354 | 331 | 663 | 344 |

**The single-column cases the customer used: 100 rows, CPU in ms**

| Case | native | shipped | rebuilt-s | noclose | o3 | o3-noclose | next | next-fix |
|---|--:|--:|--:|--:|--:|--:|--:|--:|
| Short text | 4.4 | 30 | 29 | 19 | 25 | 14 | 31 | 18 |
| 2 KB text | 5.0 | 88 | 87 | 22 | 30 | 21 | 91 | 25 |
| 2 KB JSON | 5.5 | 102 | 99 | 22 | 44 | 21 | 103 | 23 |

Each query used its index: hash for email, GIN for the name match, and btree for the risk score. At 1M rows, they took between 1 and 44 ms to run.

### Without ZeroKMS

`encrypttest`'s offline key source runs the same host and guest with no network. It skips the ZeroKMS key derivation each value needs, so its figures show the host and the AES-GCM work alone. Removing the option cuts that work by 5 to 8 times, and both changes by 6 to 12 times.

| Case | shipped | noclose | o3 | o3-noclose | next | next-fix |
|---|--:|--:|--:|--:|--:|--:|
| 100 rows, 14 columns | 117 | 23 | 52 | 17 | 124 | 18 |
| 100 rows, 2 KB text | 68 | 8.0 | 19 | 5.7 | 68 | 5.6 |
| 100 rows, 2 KB JSON | 81 | 9.5 | 33 | 6.7 | 81 | 7.0 |

### Compared with the customer's figures

Our shipped figures are about half of theirs: 403 ms against their 853 to 866 ms for 100 rows of 14 columns. The ratios match. Removing the option cut CPU by about 3.5 times for them and 3.4 times for us. They used different hardware and a different dataset.

## The fix

On branch `fix/golang-guest-decrypt-perf` of `cipherstash/stack`, from `next`:

- `encrypt/guest.go` no longer sets `WithCloseOnContextDone(true)`.
- `encrypt/guest/Cargo.toml` builds the guest at `opt-level = 3`. The EQL build of the guest grows from about 5 MB to 6.9 MB.
- The client now handles a stuck guest call without that option. Without the option, wazero cannot interrupt a guest call that never finishes. Such a call would be a bug. When a call runs 30 seconds past its caller's deadline, the client is marked closed at once. Callers queued behind it, `Close`, and the abort of a paused operation return `ErrState` instead of waiting. If the stuck call ever returns, the client wipes and releases the guest's memory.
- New tests cover a stuck call, the lock's new `AcquireUnless`, and that a call under an ended context still runs. The full Go test suite passes, and the new tests pass with the race detector.

The trade-off: before, wazero stopped a stuck guest call. Now it keeps running, holding one goroutine and the guest's memory, until it returns. A stuck call has never been seen; it would be a bug in the guest.

## What removing the option costs

GC pause latency. wazero's native code cannot be preempted by the Go scheduler, and the option's calls into Go were the guest's only safe points. Without them, a stop-the-world GC pause waits for the running guest step, and every goroutine in the process waits with it.

**The longest GC pause grows from 0.3 ms to as much as 25 ms while the guest decrypts**

| Build | One decrypt (ms) | GC pause p99 (ms) | GC pause max (ms) | 1 ms ticker, max lateness (ms) |
|---|--:|--:|--:|--:|
| rc.6 as shipped | 120 | 0.08 | 0.3 | 2.6 |
| rc.6, option removed | 23 | 8.4 | 14.7 | 12.4 |
| rc.6, both changes | 17 | 7.3 | 8.4 | 7.8 |
| Go on `next`, with the fix | 21 | 10.5 | 25 | 22.7 |

One goroutine decrypted 100 rows of 14 columns in a loop for 5 seconds, with offline keys. The test forced a GC every 5 ms. See `go/offline/latency_test.go`. Making the guest yield to the host every few hundred values would bound the wait without the per-loop cost.

## What is left

- **ZeroKMS requests run one at a time.** The guest sets `max_concurrent_reqs` to 1, so 1,400 values go to ZeroKMS as 3 requests, one after another. Native Rust sends them together. Running them at the same time would close most of the remaining wall-time gap. The guest's step interface can already hand the host several requests.
- **CPU is still about 2.5 times native.** In WebAssembly, AES-GCM runs in software, without the hardware AES that native code uses.
- **One `Client` runs one guest call at a time.** A service decrypting several pages at once queues them. The customer found that splitting a page across 4 clients cut its latency by about 3 times. `gobench -mode db -clients N` measures this, but we have not run it yet.

## How to reproduce

See "EQL v4, stack-encrypt and the Go SDK" in the [README](../README.md). The raw results are in `results/v4/`, and `mise run report:v4` prints the tables. The 1M load stopped once, at 399,455 rows, when ZeroKMS answered 403. It resumed from there, and the retry worked the first time.
