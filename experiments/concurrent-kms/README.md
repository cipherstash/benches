# Concurrent ZeroKMS requests in the Go SDK's wasm guest

**Result: sending a batch's ZeroKMS requests at the same time halves the wall time of a Go SDK decrypt or encrypt, and costs no CPU.** A decrypt of 100 rows of 14 columns falls from 297 ms to 138 ms, against 98 ms for native Rust. The guest still yields every 64 keys, so the bound on step length from cipherstash/stack#1765 holds.

Measured on 23 November 2026, on one Apple silicon laptop against ZeroKMS in ap-southeast-2. A spinning process held the CPU clock steady (see `../recipher-wasm/README.md`).

## What changed

A throwaway build of cipherstash/stack#1765 (`71e849f61`), in `concurrent-kms.patch`:

- **The guest allows 3 requests in flight.** `cipher_init` in `languages/golang/encrypt/guest/src/abi.rs` sets `max_concurrent_reqs` to 3, not 1. A page of 1,400 values is 3 requests of up to 500 keys.
- **stack-kms sends first, then derives.** `send_chunked` sends every chunk's request, up to `max_concurrent_reqs` at once. It derives the keys only once the responses are in, yielding every 64 keys as before. Before, each chunk derived its keys inside its own request's future. With several chunks in flight, `buffered` would derive for every ready chunk before passing a yield on. A step would then grow with the number of requests.
- **The Go host performs a step's requests at the same time.** `performStep` in `languages/golang/encrypt/step.go` runs each request in its own goroutine, and returns the responses in the requests' order.

The guest never runs while a request is in flight. So concurrency here means one step with several requests. The host sends them together.

## Results

**Wall time halves, and CPU does not change: median of 6 alternating rounds of 9 calls, ms**

| Case | #1765 CPU | Concurrent CPU | #1765 wall | Concurrent wall | Native wall |
|---|--:|--:|--:|--:|--:|
| Decrypt 100 rows of 14 columns, no database | 37.6 | 36.7 | 297 | 138 | 98 |
| Decrypt a page of 100 rows from a 1M-row table | 37.8 | 36.9 | 289 | 138 | 129 |
| Encrypt 100 rows of 14 columns, no database (3 rounds) | 43.2 | 41.8 | 232 | 114 | not measured |

The native figures come from the same day's rerun (`report/V4_GO_DECRYPT.md`).

**Steps stay short: guest steps per decrypt of 100 rows of 14 columns, median of 3 runs of 10 calls**

| Build | Steps per decrypt | Longest step (ms) |
|---|--:|--:|
| #1765 | 228 | 8.8 |
| #1765 with 3 concurrent requests | 223 | 9.4 |

The concurrent build has 2 fewer steps that wait on ZeroKMS, and about the same number of yields. If each step had derived keys for all 3 chunks at once, the number of yields would have fallen by about two-thirds. The longest step is not a derivation step: 64 derivations take about 0.5 ms. It is probably decoding the responses or building the batch. It grew by about 0.5 ms.

## Tests

- `cargo test -p stack-kms --lib`: 125 passed.
- `go test ./encrypt/ -run 'Step|Transport|Yield|Perform|Request'`: 25 passed in 45 s.
- The same Go tests with `-race` did not finish within Go's 10-minute limit. The cause was not established. #1765's tests were not run under `-race` for comparison.
- The concurrent build decrypted rows that other builds wrote, so it derived the right keys.

## Not tested

- A batch of more than 1,500 keys. It needs more chunks than requests in flight. The send phase should then send 3 requests per step until every chunk is sent.
- More than one Go caller at once, each sending 3 requests.
- Load on ZeroKMS from many clients each sending 3 requests at once. Native Rust already sends its requests together.
- Holding all of a batch's key material until derivation starts. That is up to 1,500 keys, about 0.8 MB. Before, it was one chunk at a time.

## Reproduce

Apply `concurrent-kms.patch` to a checkout of cipherstash/stack#1765, build the EQL guest (`--features eql`, with `CARGO_PROFILE_RELEASE_STRIP=false` for guestprof), and build `go/cmd/gobench` and `go/cmd/guestprof` against it with `scripts/go-variant.sh local`. Alternate the builds round by round, with a spinner (`perl -e 'while(1){}' &`) running.
