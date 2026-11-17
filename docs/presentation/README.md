# Atomic batch: a complete local journey

`atomic-batch-session.json` is the output of the existing synthetic outbox-state
example, run on November 17, 2026 against the source recorded below. It checks
queued state, applies three operations in one batch, opens new CLI processes to
read the result, compacts, then reopens and compares the exact live state. It
sends no external message and uses disposable local data.

```sh
cargo build --release --locked
python3 scripts/batch_demo.py target/release/durablestore
```

The captured build used the existing Rust 1.89.0 toolchain in offline mode; the
binary was built from this default-branch source. No runtime dependency was
installed. The JSON is executed software evidence, not a new filesystem,
power-loss, concurrency or performance claim.

The [batch implementation](../../src/lib.rs), [record format](../format.md),
[CLI](../../src/main.rs) and [existing example](../../scripts/batch_demo.py)
let a reader follow the behavior into the code. The full
[persistence contract](../../README.md#persistence-contract) remains authoritative.

## Source identity

Repository: `nazeeh111/DurableStore`

Default-branch head: `465feb2227f66d1bfb213419e1b5fd9b2b781578`

The presentation changes only documentation and its assets. This record does
not assert that an unpublished engineering successor is merged into the default.
