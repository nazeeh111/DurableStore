use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Dir(std::path::PathBuf);
impl Dir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "durablestore-cli-{}-{}",
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
fn run(d: &Dir, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&d.0)
        .args(args)
        .output()
        .unwrap()
}
#[test]
fn cli_roundtrip_and_validation() {
    let d = Dir::new();
    assert!(run(&d, &["init"]).status.success());
    assert!(run(&d, &["put", "00ff", "ff00"]).status.success());
    let got = run(&d, &["get", "00ff"]);
    assert!(got.status.success());
    assert!(
        String::from_utf8(got.stdout)
            .unwrap()
            .contains("\"value_hex\":\"ff00\"")
    );
    assert!(!run(&d, &["put", "z0", "11"]).status.success());
    assert!(!run(&d, &["put", "0", "11"]).status.success());
    assert!(!run(&d, &["put", "00", "11", "extra"]).status.success());
    assert!(run(&d, &["delete", "00ff"]).status.success());
    assert_eq!(run(&d, &["get", "00ff"]).status.code(), Some(3));
}
#[test]
fn read_does_not_initialize_missing_database() {
    let d = Dir::new();
    assert!(!run(&d, &["get", "00"]).status.success());
    assert!(!d.0.join("data.wal").exists());
}
#[test]
fn non_utf8_arguments_return_json_input_error() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let d = Dir::new();
    // Reject invalid text in every CLI position before opening or mutating a store.
    for position in 0..4 {
        let mut args = vec![
            d.0.as_os_str().to_owned(),
            OsString::from("put"),
            OsString::from("61"),
            OsString::from("31"),
        ];
        args[position] = OsString::from_vec(vec![0xff]);
        let out = Command::new(env!("CARGO_BIN_EXE_durablestore"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "argument {position}");
        assert!(out.stdout.is_empty());
        assert_eq!(
            out.stderr,
            b"{\"error\":\"arguments must be valid UTF-8\"}\n"
        );
        assert!(!d.0.join("data.wal").exists());
        assert!(!d.0.join("LOCK").exists());
    }
}
#[cfg(feature = "fault-injection")]
#[test]
fn acknowledged_records_survive_process_crashes() {
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
        let d = Dir::new();
        assert!(run(&d, &["init"]).status.success());
        assert!(run(&d, &["put", "61", "31"]).status.success());
        assert!(run(&d, &["put", "62", "32"]).status.success());
        assert!(run(&d, &["delete", "62"]).status.success());
        let args = if point.starts_with("append") {
            vec!["put", "63", "33"]
        } else {
            vec!["compact"]
        };
        let o = Command::new(env!("CARGO_BIN_EXE_durablestore"))
            .arg(&d.0)
            .args(args)
            .env("DURABLESTORE_FAIL_AT", point)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(86), "{point}");
        assert_eq!(run(&d, &["get", "61"]).status.code(), Some(0), "{point}");
        assert_eq!(run(&d, &["get", "62"]).status.code(), Some(3), "{point}");
        assert!(run(&d, &["put", "64", "34"]).status.success());
    }
}
#[cfg(feature = "fault-injection")]
#[test]
fn initial_creation_and_recovery_crashes_are_restartable() {
    for point in ["init_header", "init_sync", "init_rename", "init_dirsync"] {
        let d = Dir::new();
        let o = Command::new(env!("CARGO_BIN_EXE_durablestore"))
            .arg(&d.0)
            .arg("init")
            .env("DURABLESTORE_FAIL_AT", point)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(86), "{point}");
        assert!(run(&d, &["init"]).status.success(), "{point}");
        assert!(run(&d, &["put", "61", "31"]).status.success());
    }
    for point in ["recovery_truncate", "recovery_sync"] {
        use std::io::Write;
        let d = Dir::new();
        assert!(run(&d, &["init"]).status.success());
        assert!(run(&d, &["put", "61", "31"]).status.success());
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(d.0.join("data.wal"))
            .unwrap();
        f.write_all(b"incomplete").unwrap();
        f.sync_all().unwrap();
        drop(f);
        let o = Command::new(env!("CARGO_BIN_EXE_durablestore"))
            .arg(&d.0)
            .args(["get", "61"])
            .env("DURABLESTORE_FAIL_AT", point)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(86), "{point}");
        assert!(run(&d, &["get", "61"]).status.success());
    }
}
#[test]
fn cli_respects_existing_process_lock() {
    let d = Dir::new();
    let _s = durablestore::Store::open(&d.0).unwrap();
    let out = run(&d, &["put", "61", "31"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8(out.stderr).unwrap().contains("locked"));
}
#[cfg(not(feature = "fault-injection"))]
#[test]
fn normal_binary_ignores_fault_environment() {
    let d = Dir::new();
    let o = Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&d.0)
        .arg("init")
        .env("DURABLESTORE_FAIL_AT", "init_header")
        .output()
        .unwrap();
    assert!(o.status.success());
}
