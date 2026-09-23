# Verification record

Local verification: September 23, 2026, macOS aarch64, Rust 1.98.1. These results are bounded observations on one machine. Linux and Rust 1.89 checks are configured in CI but were not executed locally for this record.

| Check | Observed result |
| --- | --- |
| `cargo fmt --check` | Passed |
| `cargo clippy --locked --all-targets --all-features -- -D warnings` | Passed |
| `cargo test --locked` | 19 unit/integration entries and 1 README compilation test passed |
| `cargo test --locked --all-features` | 22 unit/integration entries and 1 README compilation test passed |
| `cargo build --release --locked` | Passed |
| `python3 scripts/crash_demo.py target/debug/durablestore` after feature build | All 8 public demonstration cases passed |
| Release benchmark: 1,000 keys, 256-byte values | 2,000 durable puts, 10,000 memory gets, 250 deletes, compaction and full reopened-state validation passed |

The test entry counts include subprocess helper entry points; they are not counts of independent persistence scenarios. More useful coverage detail:

- Four deterministic seeds, 160 mutations each, checked after every mutation against a separate `BTreeMap` model, with repeated reopening and compaction.
- Every possible cut of the final nonempty record, preserving the preceding complete record.
- Every individual byte in a two-record file flipped once, including both file and record headers. Every damaged file was rejected without changing its bytes.
- Oversized declared lengths with a correctly recomputed header checksum, proving bounds are checked independently of checksum rejection.
- Four initialization termination boundaries, two recovery termination boundaries, three append termination boundaries and five compaction termination boundaries. Termination is a subprocess `exit(86)` that bypasses Rust destructors, not a physical power cut.
- Eight injected returned-I/O-error boundaries. Each poisons the handle and requires reopening; acknowledged data remains recoverable.
- Same-process and cross-process exclusive lock rejection, released-lock reopening, default-build fault-variable immunity, invalid CLI inputs and absent-key exits.
- Non-UTF-8 command-line arguments return a JSON error and exit code 2 before any store files are created.
- An initially failing subprocess test exposed compaction following a caller's changed working directory. Canonicalizing the directory when opening fixed it; the regression test now passes.

The public [crash demonstration JSON](crash-demo.json) records its eight tested boundaries. A compacted log may retain a fully written operation that never returned success; this is intentional and distinct from losing an acknowledged operation.

## Measured workload

The exact machine-readable output is [benchmark.json](benchmark.json). The final measured run reports:

- 2,000 synchronized puts in 9.211 seconds; p50 4.121 ms, p95 6.933 ms, p99 8.121 ms.
- Warm in-memory gets: p50 84 ns, p95 125 ns, p99 458 ns. These timings include timer overhead and are not disk reads.
- Log bytes: 602,016 before compaction; 222,016 after compaction.
- Compaction: 15.003 ms. Reopen: 8.072 ms.
- OS: Darwin 25.6.0, aarch64; runtime available parallelism: 10. CPU model query was unavailable in the execution sandbox and remains explicitly marked `unavailable`.

No isolated-disk baseline, throughput comparison, sustained-load study, or power-loss hardware experiment was run. The bitwise CRC implementation favors inspectability over optimized checksum throughput. A separate early run exposed a local toolchain's `rust-objcopy` library lookup warning; the final release build used the already-installed toolchain's library directory and completed without that warning. No project code workaround or additional dependency was added.

## Remaining limits

Only local filesystem process-crash semantics were exercised. Disk-full errors, controller/cache behavior under power loss, filesystem fault injection, network filesystems and production deployment were not validated. Record CRCs are not authenticated. External destructive edits cannot be recovered. Startup and live data use memory proportional to the current dataset. The directory and lock pathname must remain in place while a store is open, and other programs must respect the lock.
