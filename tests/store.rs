use durablestore::{MAX_BATCH_OPERATIONS, MAX_KEY, MAX_VALUE, Operation, Store};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(std::path::PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "durablestore-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn roundtrip_bytes_delete_and_reopen() {
    let d = Dir::new();
    let mut s = Store::open(&d.0).unwrap();
    s.put(b"", b"").unwrap();
    s.put(&[0, 255], &[255, 0, 10]).unwrap();
    s.put(b"key", b"old").unwrap();
    s.put(b"key", b"new").unwrap();
    assert_eq!(s.get(b"key"), Some(b"new".as_slice()));
    s.delete(b"key").unwrap();
    drop(s);
    let s = Store::open(&d.0).unwrap();
    assert_eq!(s.get(b"key"), None);
    assert_eq!(s.get(b""), Some(b"".as_slice()));
    assert_eq!(s.get(&[0, 255]), Some([255, 0, 10].as_slice()));
}

#[test]
fn batch_applies_mixed_operations_in_order_after_reopen() {
    let d = Dir::new();
    let mut s = Store::open(&d.0).unwrap();
    s.put(b"keep", b"old").unwrap();
    let before = s.stats().records;
    s.write_batch(&[
        Operation::Put(b"keep", b"new"),
        Operation::Put(b"", b""),
        Operation::Put(b"same", b"first"),
        Operation::Delete(b"keep"),
        Operation::Put(b"same", b"last"),
        Operation::Delete(b"missing"),
    ])
    .unwrap();
    assert_eq!(s.stats().records, before + 1);
    drop(s);
    let s = Store::open(&d.0).unwrap();
    assert_eq!(s.get(b"keep"), None);
    assert_eq!(s.get(b""), Some(b"".as_slice()));
    assert_eq!(s.get(b"same"), Some(b"last".as_slice()));
    assert_eq!(s.get(b"missing"), None);
}

#[test]
fn incomplete_batch_never_replays_a_prefix() {
    let base = Dir::new();
    let mut s = Store::open(&base.0).unwrap();
    s.put(b"safe", b"before").unwrap();
    let prefix = s.stats().log_bytes as usize;
    s.write_batch(&[
        Operation::Put(b"safe", b"after"),
        Operation::Put(b"new", b"value"),
        Operation::Delete(b"absent"),
    ])
    .unwrap();
    drop(s);
    let bytes = fs::read(base.0.join("data.wal")).unwrap();
    for cut in prefix..bytes.len() {
        let d = Dir::new();
        fs::write(d.0.join("data.wal"), &bytes[..cut]).unwrap();
        let s = Store::open(&d.0).unwrap();
        assert_eq!(s.get(b"safe"), Some(b"before".as_slice()), "cut {cut}");
        assert_eq!(s.get(b"new"), None, "cut {cut}");
        assert_eq!(s.stats().tail_bytes, (cut - prefix) as u64);
        assert_eq!(
            fs::metadata(d.0.join("data.wal")).unwrap().len(),
            prefix as u64
        );
    }
    let d = Dir::new();
    fs::write(d.0.join("data.wal"), bytes).unwrap();
    let s = Store::open(&d.0).unwrap();
    assert_eq!(s.get(b"safe"), Some(b"after".as_slice()));
    assert_eq!(s.get(b"new"), Some(b"value".as_slice()));
}

#[test]
fn invalid_batch_is_rejected_before_writing_without_poisoning() {
    let d = Dir::new();
    let mut s = Store::open(&d.0).unwrap();
    let before = s.stats();
    s.write_batch(&[]).unwrap();
    assert_eq!(s.stats(), before);
    assert!(
        s.write_batch(&[Operation::Put(&vec![0; MAX_KEY + 1], b"v")])
            .is_err()
    );
    assert!(
        s.write_batch(&[Operation::Put(b"k", &vec![0; MAX_VALUE + 1])])
            .is_err()
    );
    let too_many = vec![Operation::Delete(b"k"); MAX_BATCH_OPERATIONS + 1];
    assert!(s.write_batch(&too_many).is_err());
    let large_value = vec![0; MAX_VALUE];
    assert!(
        s.write_batch(&[
            Operation::Put(b"one", &large_value),
            Operation::Put(b"two", &large_value),
        ])
        .is_err()
    );
    assert_eq!(s.stats(), before);
    s.put(b"still", b"usable").unwrap();
    assert_eq!(s.get(b"still"), Some(b"usable".as_slice()));
}

#[test]
fn version_one_log_accepts_new_batch_without_migration() {
    // Fixed version-one header and a put of legacy -> yes, created independently.
    const FIXTURE: &str = "4453544f523030310100000036bbdf874453523100000000010000000000000006000000030000006568ef0b915536926c6567616379796573";
    let bytes: Vec<u8> = FIXTURE
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let d = Dir::new();
    fs::write(d.0.join("data.wal"), bytes).unwrap();
    let mut s = Store::open(&d.0).unwrap();
    assert_eq!(s.get(b"legacy"), Some(b"yes".as_slice()));
    s.write_batch(&[Operation::Delete(b"legacy"), Operation::Put(b"new", b"ok")])
        .unwrap();
    drop(s);
    let s = Store::open(&d.0).unwrap();
    assert_eq!(s.get(b"legacy"), None);
    assert_eq!(s.get(b"new"), Some(b"ok".as_slice()));
}

fn test_crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn repair_record_checksums(record: &mut [u8]) {
    let payload_crc = test_crc32(&record[32..]);
    record[24..28].copy_from_slice(&payload_crc.to_le_bytes());
    let header_crc = test_crc32(&record[..28]);
    record[28..32].copy_from_slice(&header_crc.to_le_bytes());
}

#[test]
fn complete_malformed_batch_fails_closed_without_repair() {
    let base = Dir::new();
    let mut s = Store::open(&base.0).unwrap();
    s.put(b"safe", b"before").unwrap();
    let prefix = s.stats().log_bytes as usize;
    s.write_batch(&[
        Operation::Put(b"safe", b"after"),
        Operation::Put(b"new", b"value"),
    ])
    .unwrap();
    drop(s);
    let bytes = fs::read(base.0.join("data.wal")).unwrap();
    for variant in 0..3 {
        let d = Dir::new();
        let mut damaged = bytes.clone();
        let record = &mut damaged[prefix..];
        match variant {
            0 => record[32 + 4] = 7, // Unknown inner operation with valid outer CRCs.
            1 => record[32..36].copy_from_slice(&1u32.to_le_bytes()), // Trailing member.
            _ => record[32 + 5..32 + 9].copy_from_slice(&u32::MAX.to_le_bytes()),
        }
        repair_record_checksums(record);
        fs::write(d.0.join("data.wal"), &damaged).unwrap();
        assert!(matches!(
            Store::open(&d.0),
            Err(durablestore::Error::Corruption { .. })
        ));
        assert_eq!(fs::read(d.0.join("data.wal")).unwrap(), damaged);
    }
}

#[test]
fn corrupt_batch_payload_fails_closed_without_repair() {
    let d = Dir::new();
    let mut s = Store::open(&d.0).unwrap();
    s.write_batch(&[Operation::Put(b"one", b"1"), Operation::Put(b"two", b"2")])
        .unwrap();
    drop(s);
    let mut damaged = fs::read(d.0.join("data.wal")).unwrap();
    damaged[16 + 32 + 4 + 9] ^= 0x80;
    fs::write(d.0.join("data.wal"), &damaged).unwrap();
    assert!(matches!(
        Store::open(&d.0),
        Err(durablestore::Error::Corruption { .. })
    ));
    assert_eq!(fs::read(d.0.join("data.wal")).unwrap(), damaged);
}
#[test]
fn lock_is_exclusive_and_released() {
    let d = Dir::new();
    let s = Store::open(&d.0).unwrap();
    assert!(Store::open(&d.0).is_err());
    drop(s);
    assert!(Store::open(&d.0).is_ok());
}
#[test]
fn compact_preserves_live_sorted_entries() {
    let d = Dir::new();
    let mut s = Store::open(&d.0).unwrap();
    for i in 0u8..30 {
        s.put(&[i % 5], &[i]).unwrap();
    }
    s.delete(&[2]).unwrap();
    let before = s
        .entries()
        .map(|(k, v)| (k.to_vec(), v.to_vec()))
        .collect::<Vec<_>>();
    let old = s.stats().log_bytes;
    s.compact().unwrap();
    assert!(s.stats().log_bytes < old);
    drop(s);
    let s = Store::open(&d.0).unwrap();
    assert_eq!(
        before,
        s.entries()
            .map(|(k, v)| (k.to_vec(), v.to_vec()))
            .collect::<Vec<_>>()
    );
}
#[test]
fn model_sequence_matches_across_reopens_and_compactions() {
    use std::collections::BTreeMap;
    for seed in [1u64, 29, 0xdeadbeef, 987654321] {
        let d = Dir::new();
        let mut s = Store::open(&d.0).unwrap();
        let mut model = BTreeMap::new();
        let mut rng = seed;
        for step in 0..160 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let k = (rng % 23).to_le_bytes();
            match (rng >> 32) % 5 {
                0 => {
                    s.delete(&k).unwrap();
                    model.remove(k.as_slice());
                }
                _ => {
                    let v = rng.to_be_bytes();
                    s.put(&k, &v).unwrap();
                    model.insert(k.to_vec(), v.to_vec());
                }
            }
            if step % 37 == 0 {
                s.compact().unwrap();
            }
            if step % 19 == 0 {
                drop(s);
                s = Store::open(&d.0).unwrap();
            }
            assert_eq!(
                s.entries()
                    .map(|(k, v)| (k.to_vec(), v.to_vec()))
                    .collect::<BTreeMap<_, _>>(),
                model,
                "seed {seed} step {step}"
            );
        }
    }
}
#[test]
fn every_truncation_of_last_record_preserves_acknowledged_prefix() {
    let base = Dir::new();
    let mut s = Store::open(&base.0).unwrap();
    s.put(b"safe", b"acknowledged").unwrap();
    let prefix = s.stats().log_bytes as usize;
    s.put(b"next", b"inflight").unwrap();
    drop(s);
    let bytes = fs::read(base.0.join("data.wal")).unwrap();
    for cut in prefix..bytes.len() {
        let d = Dir::new();
        fs::write(d.0.join("data.wal"), &bytes[..cut]).unwrap();
        let s = Store::open(&d.0).unwrap();
        assert_eq!(s.get(b"safe"), Some(b"acknowledged".as_slice()));
        assert_eq!(s.get(b"next"), None);
        assert_eq!(s.stats().tail_bytes, (cut - prefix) as u64);
        assert_eq!(
            fs::metadata(d.0.join("data.wal")).unwrap().len(),
            prefix as u64
        );
    }
}
#[test]
fn corruption_is_not_silently_repaired() {
    let base = Dir::new();
    let mut s = Store::open(&base.0).unwrap();
    s.put(b"a", b"first").unwrap();
    s.put(b"b", b"second").unwrap();
    drop(s);
    let bytes = fs::read(base.0.join("data.wal")).unwrap();
    for offset in 0..bytes.len() {
        let d = Dir::new();
        let mut damaged = bytes.clone();
        damaged[offset] ^= 0x80;
        fs::write(d.0.join("data.wal"), &damaged).unwrap();
        assert!(
            matches!(
                Store::open(&d.0),
                Err(durablestore::Error::Corruption { .. })
            ),
            "byte {offset}"
        );
        assert_eq!(fs::read(d.0.join("data.wal")).unwrap(), damaged);
    }
}
#[test]
fn inspector_does_not_repair_tail() {
    use std::io::Write;
    let d = Dir::new();
    let s = Store::open(&d.0).unwrap();
    drop(s);
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(d.0.join("data.wal"))
        .unwrap();
    f.write_all(b"DSR").unwrap();
    let before = fs::read(d.0.join("data.wal")).unwrap();
    let report = durablestore::inspect(&d.0).unwrap();
    assert_eq!(report.tail_bytes, 3);
    assert_eq!(fs::read(d.0.join("data.wal")).unwrap(), before);
}
#[test]
fn oversized_inputs_do_not_poison_handle() {
    let d = Dir::new();
    let mut s = Store::open(&d.0).unwrap();
    assert!(s.put(&vec![0; durablestore::MAX_KEY + 1], b"").is_err());
    assert!(s.put(b"k", &vec![0; durablestore::MAX_VALUE + 1]).is_err());
    s.put(b"ok", b"yes").unwrap();
}
#[test]
fn empty_compaction_and_stale_temp_are_safe() {
    let d = Dir::new();
    let mut s = Store::open(&d.0).unwrap();
    s.put(b"a", b"b").unwrap();
    s.delete(b"a").unwrap();
    fs::write(d.0.join("compact.tmp"), b"interrupted").unwrap();
    s.compact().unwrap();
    drop(s);
    let s = Store::open(&d.0).unwrap();
    assert_eq!(s.stats().live_keys, 0);
    assert_eq!(s.stats().log_bytes, 16);
}
#[cfg(feature = "fault-injection")]
#[test]
fn injected_io_errors_poison_until_reopen() {
    for point in [
        "append_header",
        "append_payload",
        "append_sync",
        "compact_header",
        "compact_record",
        "compact_sync",
        "compact_rename",
        "compact_dirsync",
    ] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "io_failure_child", "--nocapture"])
            .env("DURABLESTORE_CHILD_POINT", point)
            .env("DURABLESTORE_FAIL_AT", point)
            .env("DURABLESTORE_FAIL_MODE", "error")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{point}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
#[cfg(feature = "fault-injection")]
#[test]
fn io_failure_child() {
    let Ok(point) = std::env::var("DURABLESTORE_CHILD_POINT") else {
        return;
    };
    let d = Dir::new();
    // Generate a valid seed log through another CLI process without the failure environment.
    let seed = std::process::Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&d.0)
        .arg("init")
        .env_remove("DURABLESTORE_FAIL_AT")
        .output()
        .unwrap();
    assert!(seed.status.success());
    let seed = std::process::Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&d.0)
        .args(["put", "61", "31"])
        .env_remove("DURABLESTORE_FAIL_AT")
        .output()
        .unwrap();
    assert!(seed.status.success());
    let mut s = Store::open(&d.0).unwrap();
    let result = if point.starts_with("append") {
        s.put(b"b", b"2")
    } else {
        s.compact()
    };
    assert!(result.is_err());
    assert!(matches!(
        s.put(b"c", b"3"),
        Err(durablestore::Error::Poisoned)
    ));
    drop(s);
    let s = Store::open(&d.0).unwrap();
    assert_eq!(s.get(b"a"), Some(b"1".as_slice()));
}

#[cfg(feature = "fault-injection")]
#[test]
fn batch_io_errors_poison_until_reopen_and_replay_all_or_none() {
    for point in ["append_header", "append_payload", "append_sync"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "batch_io_failure_child", "--nocapture"])
            .env("DURABLESTORE_BATCH_CHILD_POINT", point)
            .env("DURABLESTORE_FAIL_AT", point)
            .env("DURABLESTORE_FAIL_MODE", "error")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{point}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[cfg(feature = "fault-injection")]
#[test]
fn batch_io_failure_child() {
    let Ok(point) = std::env::var("DURABLESTORE_BATCH_CHILD_POINT") else {
        return;
    };
    let d = Dir::new();
    let seed = std::process::Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&d.0)
        .arg("init")
        .env_remove("DURABLESTORE_FAIL_AT")
        .output()
        .unwrap();
    assert!(seed.status.success());
    let seed = std::process::Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&d.0)
        .args(["put", "73616665", "6265666f7265"])
        .env_remove("DURABLESTORE_FAIL_AT")
        .output()
        .unwrap();
    assert!(seed.status.success());
    let mut s = Store::open(&d.0).unwrap();
    assert!(
        s.write_batch(&[
            Operation::Put(b"safe", b"after"),
            Operation::Put(b"new", b"value"),
        ])
        .is_err()
    );
    assert!(matches!(
        s.write_batch(&[]),
        Err(durablestore::Error::Poisoned)
    ));
    assert_eq!(s.get(b"safe"), Some(b"before".as_slice()));
    assert_eq!(s.get(b"new"), None);
    drop(s);
    let s = Store::open(&d.0).unwrap();
    let both_survived =
        s.get(b"safe") == Some(b"after".as_slice()) && s.get(b"new") == Some(b"value".as_slice());
    let neither_survived = s.get(b"safe") == Some(b"before".as_slice()) && s.get(b"new").is_none();
    assert!(both_survived || neither_survived, "{point}");
    if point == "append_header" {
        assert!(neither_survived);
    }
    if point == "append_sync" {
        assert!(both_survived);
    }
}
#[test]
fn unfinished_initial_file_is_rejected_and_preserved() {
    let d = Dir::new();
    fs::write(d.0.join("data.wal"), b"DSTOR").unwrap();
    assert!(Store::open(&d.0).is_err());
    assert_eq!(fs::read(d.0.join("data.wal")).unwrap(), b"DSTOR");
}
#[test]
fn relative_directory_is_stable_after_cwd_changes() {
    let d = Dir::new();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "cwd_change_child", "--nocapture"])
        .current_dir(&d.0)
        .env("DURABLESTORE_CWD_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
#[test]
fn cwd_change_child() {
    if std::env::var("DURABLESTORE_CWD_CHILD").as_deref() != Ok("1") {
        return;
    }
    let mut s = Store::open(".").unwrap();
    s.put(b"a", b"original").unwrap();
    fs::create_dir("elsewhere").unwrap();
    std::env::set_current_dir("elsewhere").unwrap();
    s.compact().unwrap();
    assert!(
        !std::path::Path::new("data.wal").exists(),
        "compaction followed process cwd into another directory"
    );
    s.put(b"b", b"after").unwrap();
    drop(s);
    let s = Store::open("..").unwrap();
    assert_eq!(s.get(b"a"), Some(b"original".as_slice()));
    assert_eq!(s.get(b"b"), Some(b"after".as_slice()));
}
