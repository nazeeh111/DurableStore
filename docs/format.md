# Binary format, version 1

All integer fields are unsigned little-endian. File: `data.wal`. Companion: `LOCK`, exclusively OS-locked while a handle lives. `compact.tmp` may remain after interruption and is ignored; later initialization/compaction overwrites it while holding the lock. Paths belong to a trusted directory.

## File header: 16 bytes

| Offset | Bytes | Value |
| --- | --- | --- |
| 0 | 8 | ASCII `DSTOR001` |
| 8 | 4 | Version 1 |
| 12 | 4 | IEEE CRC-32 of bytes 0..12 |

No header migration is currently supported. Any mismatch is a corruption/unsupported-version error. An existing empty file is not implicitly initialized.

## Record header: 32 bytes

| Offset | Bytes | Value |
| --- | --- | --- |
| 0 | 4 | ASCII `DSR1` |
| 4 | 1 | Operation: 0 put, 1 delete |
| 5 | 3 | Reserved, must all be zero |
| 8 | 8 | Sequence, exactly previous + 1; first = 1 |
| 16 | 4 | Key length, at most 1,048,576 |
| 20 | 4 | Value length, at most 16,777,216; delete requires 0 |
| 24 | 4 | IEEE CRC-32 of key followed by value |
| 28 | 4 | IEEE CRC-32 of header bytes 0..28 |
| 32 | key + value length | Raw key followed by raw value |

CRC polynomial: reflected `0xedb88320`, initial register `0xffffffff`, final XOR `0xffffffff`. Test vector ASCII `123456789` gives `0xcbf43926`. CRC is not cryptographic integrity protection.

## Recovery order

1. Check the exact file header.
2. If fewer than 32 bytes remain, report an incomplete tail.
3. Read the fixed record header. Validate magic, operation, reserved fields, CRC, sequence and bounds.
4. If the valid header describes more payload bytes than remain, report an incomplete tail.
5. Read the bounded payload and verify its CRC. Apply put/delete to the index.
6. Repeat until EOF. The inspector only reports; `Store::open` truncates incomplete tail bytes and synchronizes the log before returning.

A corrupt complete record is never skipped. A tail is not recovered past a checksum mismatch. File length changes by another non-cooperating process, filesystem corruption and malicious rewrites are outside the recoverable failed-append model. Checksums cannot detect every mathematically possible collision.

Compaction emits a fresh header and one put per live key, in key order, with sequences 1..N. It synchronizes the temporary file, renames it over `data.wal`, and synchronizes the directory. There is no manifest to select ambiguous generations; the atomic directory entry is the generation switch. The old open file cannot be appended after any failed compaction because the handle is poisoned before I/O starts.
