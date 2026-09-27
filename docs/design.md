# DurableStore design

Original single-writer embedded byte key/value store, MIT, copyright nazeeh111.
The user delegated design decisions and implementation. This scope fills the storage-systems portfolio gap with inspectable persistence behavior.

## Contract

Local Linux/macOS filesystem, Rust 1.89+. `put`/`delete` return success only after `sync_all` succeeds. Reopening recovers all acknowledged operations provided the filesystem honors synchronization. Process crashes are tested; sudden power loss and controller caches are not experimentally certified. Unacknowledged complete records may survive. One process owns an exclusive OS lock for all commands, including reads and inspection. No network filesystems or uncooperative external file mutation.

Versioned file header followed by length-delimited CRC32 records. Header CRC protects lengths before allocating. Each operation carries a monotonic sequence. Bounds: 1 MiB keys, 16 MiB values. Empty keys and values are valid; tombstones explicitly distinguish deletion. CRC detects accidental corruption, not malicious modification.

An incomplete terminal record is discarded only when its header is incomplete, or its complete validated header describes payload beyond EOF. Full records with bad CRC, invalid fields, or sequence violations are errors without modifying the log. A truncated payload cannot be distinguished from some external corruption; this is stated explicitly. Open returns recovered-tail bytes. In-memory values use a sorted BTreeMap. Memory scales with live data, startup with log size.

Compaction writes sorted live data into a temporary log, synchronizes it, atomically renames it, and synchronizes the directory. Lock file is separate and never renamed/unlinked. Temporary files are disposable on next open. Failure after rename poisons the handle so no appends can target the obsolete inode. New store creation synchronizes the file and its directory. Existing directory required, avoiding claims about recursive directory creation persistence.

## Interfaces

Library `Store::open`, `get`, `put`, `delete`, `write_batch`, `entries`, `compact`, `stats`.
CLI `durablestore <directory> init|put|get|delete|batch|list|inspect|compact`; byte arguments encoded as hex, consistent JSON results and errors. CLI get missing returns code 3. Inspector scans without changing files (including truncated tails) under lock.

## Atomic grouped writes

A batch holds up to 1,024 ordered mutations in one checksummed record with a 32 MiB payload limit. Encoding validates the entire input before appending; replay validates the entire payload before changing the index. One synchronization acknowledges the group. This avoids commit markers and a pending-transaction recovery state, at the cost of a bounded in-memory batch. Empty batches do not append. An error after persistence begins poisons the handle, including errors after the record may have reached disk.

The new record tag preserves old logs without an in-place format migration. Old readers reject batch records. Compaction emits the live state as ordinary puts. Interactive transactions, compare-and-swap and reader snapshots are separate contracts and are not implemented.

## Alternatives considered

An LSM tree offers disk-scale reads but adds sorted table/version lifetimes before this durability contract is established. SQLite bindings provide useful applications but do not show original persistence logic. A BTreeMap index plus append-only WAL keeps the first implementation inspectable and testable; no SQL/MVCC claims.
