use durablestore::{Error, HealthStatus, Operation, Store, inspect, log_health};
use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "durablestore-health-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn health_is_read_only_matches_recovery_and_rejects_active_owner() {
    let dir = Temp::new();
    let mut store = Store::open(&dir.0).unwrap();
    store.put(b"a", b"value").unwrap();
    store
        .write_batch(&[Operation::Put(b"b", b"two"), Operation::Delete(b"a")])
        .unwrap();
    assert!(matches!(log_health(&dir.0), Err(Error::Locked)));
    let cmd = Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&dir.0)
        .arg("health")
        .output()
        .unwrap();
    assert_eq!(cmd.status.code(), Some(2));
    drop(store);
    let path = dir.0.join("data.wal");
    let original = fs::read(&path).unwrap();
    let good = log_health(&dir.0).unwrap();
    assert_eq!(good.status, HealthStatus::Good);
    assert_eq!(good.records, 2);
    assert_eq!(good.remaining_bytes, 0);
    let stats = inspect(&dir.0).unwrap();
    assert_eq!(stats.records, good.records);
    assert_eq!(stats.live_keys, 1);
    assert_eq!(stats.live_bytes, 4);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"DSR")
        .unwrap();
    let before = fs::read(&path).unwrap();
    let tail = log_health(&dir.0).unwrap();
    assert_eq!(tail.status, HealthStatus::Truncated);
    assert_eq!(tail.records, 2);
    assert_eq!(tail.error_offset, Some(original.len() as u64));
    assert_eq!(tail.remaining_bytes, 3);
    assert_eq!(fs::read(&path).unwrap(), before);
    let stats = inspect(&dir.0).unwrap();
    assert_eq!(stats.tail_bytes, 3);
    assert_eq!(fs::read(&path).unwrap(), before);
    let recovered = Store::open(&dir.0).unwrap();
    assert_eq!(recovered.stats().tail_bytes, 3);
    drop(recovered);
    assert_eq!(fs::read(&path).unwrap(), original);
}

// Fixed records independently encoded with Python struct + zlib CRC32, not the
// store encoder. Exercise the same bytes through health, inspect and recovery.
fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn independent_records_and_each_tail_boundary_agree_with_recovery() {
    let dir = Temp::new();
    drop(Store::open(&dir.0).unwrap());
    let p = dir.0.join("data.wal");
    let log = bytes(
        "4453544f523030310100000036bbdf8744535231000000000100000000000000010000000500000013f1a408d20ed3166b76616c7565",
    );
    fs::write(&p, &log).unwrap();
    let report = log_health(&dir.0).unwrap();
    assert_eq!(report.status, HealthStatus::Good);
    assert_eq!(report.records, 1);
    assert_eq!(inspect(&dir.0).unwrap().live_bytes, 6);
    for cut in 17..log.len() {
        fs::write(&p, &log[..cut]).unwrap();
        let before = fs::read(&p).unwrap();
        let report = log_health(&dir.0).unwrap();
        assert_eq!(report.status, HealthStatus::Truncated, "cut {cut}");
        assert_eq!(report.error_offset, Some(16));
        assert_eq!(inspect(&dir.0).unwrap().tail_bytes, (cut - 16) as u64);
        assert_eq!(fs::read(&p).unwrap(), before);
    }
    let mut corrupt = log.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    fs::write(&p, &corrupt).unwrap();
    let report = log_health(&dir.0).unwrap();
    assert_eq!(report.status, HealthStatus::Corrupt);
    assert_eq!(report.error_offset, Some(16));
    assert!(matches!(inspect(&dir.0), Err(Error::Corruption { .. })));
    assert_eq!(fs::read(&p).unwrap(), corrupt);
    let mut wrong_seq = log[..16].to_vec();
    wrong_seq.extend(bytes(
        "44535231000000000200000000000000010000000500000013f1a40818437ab96b76616c7565",
    ));
    fs::write(&p, &wrong_seq).unwrap();
    let report = log_health(&dir.0).unwrap();
    assert_eq!(report.status, HealthStatus::SequenceError);
    assert_eq!(report.error_offset, Some(16));
    assert!(matches!(Store::open(&dir.0), Err(Error::Corruption { .. })));
    assert_eq!(fs::read(&p).unwrap(), wrong_seq);
}
