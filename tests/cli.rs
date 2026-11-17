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

#[test]
fn batch_file_is_atomic_and_invalid_input_does_not_open_store() {
    let d = Dir::new();
    assert!(run(&d, &["init"]).status.success());
    assert!(run(&d, &["put", "61", "31"]).status.success());
    let input = d.0.join("changes.tsv");
    fs::write(&input, "put\t62\t32\ndelete\t61\nput\t\t\n").unwrap();
    let result = run(&d, &["batch", input.to_str().unwrap()]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "{\"ok\":true,\"durable\":true,\"operations\":3}\n"
    );
    assert_eq!(run(&d, &["get", "61"]).status.code(), Some(3));
    assert!(run(&d, &["get", "62"]).status.success());
    assert!(run(&d, &["get", ""]).status.success());
    for invalid in [
        "put\t63\t33\ndelete\tz0\n",
        "put 63 33\n",
        "",
        "\n",
        "put\t63\t33\textra\n",
    ] {
        fs::write(&input, invalid).unwrap();
        let before = fs::read(d.0.join("data.wal")).unwrap();
        let result = run(&d, &["batch", input.to_str().unwrap()]);
        assert_eq!(result.status.code(), Some(2));
        assert_eq!(fs::read(d.0.join("data.wal")).unwrap(), before);
        assert!(result.stdout.is_empty());
    }
    let fresh = Dir::new();
    assert_eq!(
        run(&fresh, &["batch", input.to_str().unwrap()])
            .status
            .code(),
        Some(2)
    );
    assert!(!fresh.0.join("LOCK").exists());
}

#[test]
fn batch_stdin_handles_crlf_and_ordered_duplicate_keys() {
    use std::io::Write;
    use std::process::Stdio;
    let d = Dir::new();
    assert!(run(&d, &["init"]).status.success());
    let mut child = Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&d.0)
        .args(["batch", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"put\t61\t31\r\ndelete\t61\r\nput\t61\t32\r\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        run(&d, &["get", "61"]).stdout,
        b"{\"found\":true,\"value_hex\":\"32\"}\n"
    );
}

#[cfg(feature = "fault-injection")]
#[test]
fn batch_process_crash_never_exposes_partial_outbox_transition() {
    for point in ["append_header", "append_payload", "append_sync"] {
        let d = Dir::new();
        assert!(run(&d, &["init"]).status.success());
        assert!(run(&d, &["put", "61", "31"]).status.success());
        assert!(run(&d, &["put", "62", "32"]).status.success());
        let input = d.0.join("changes.tsv");
        fs::write(&input, "delete\t61\nput\t62\t33\nput\t63\t34\n").unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_durablestore"))
            .arg(&d.0)
            .args(["batch", input.to_str().unwrap()])
            .env("DURABLESTORE_FAIL_AT", point)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(86), "{point}");
        let reopened = run(&d, &["list"]);
        assert!(reopened.status.success());
        let expected: &[u8] = if point == "append_header" {
            b"{\"entries\":[{\"key_hex\":\"61\",\"value_hex\":\"31\"},{\"key_hex\":\"62\",\"value_hex\":\"32\"}]}\n"
        } else {
            b"{\"entries\":[{\"key_hex\":\"62\",\"value_hex\":\"33\"},{\"key_hex\":\"63\",\"value_hex\":\"34\"}]}\n"
        };
        assert_eq!(reopened.stdout, expected, "{point}");
        assert!(run(&d, &["put", "64", "35"]).status.success());
        assert!(run(&d, &["compact"]).status.success());
        assert!(run(&d, &["get", "64"]).status.success());
    }
}

#[test]
fn batch_limits_and_invalid_utf8_preserve_an_unrepaired_tail() {
    use std::io::Write;
    let d = Dir::new();
    assert!(run(&d, &["init"]).status.success());
    fs::OpenOptions::new()
        .append(true)
        .open(d.0.join("data.wal"))
        .unwrap()
        .write_all(b"tail")
        .unwrap();
    let before = fs::read(d.0.join("data.wal")).unwrap();
    let input = d.0.join("changes.tsv");
    for contents in [
        b"put\t61\t31\n\xff".to_vec(),
        "put\t61\t31\n".repeat(1025).into_bytes(),
    ] {
        fs::write(&input, contents).unwrap();
        assert_eq!(
            run(&d, &["batch", input.to_str().unwrap()]).status.code(),
            Some(2)
        );
        assert_eq!(fs::read(d.0.join("data.wal")).unwrap(), before);
    }
}

#[test]
fn help_and_version_identify_current_source() {
    let binary = env!("CARGO_BIN_EXE_durablestore");
    let help = Command::new(binary).arg("--help").output().unwrap();
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(text.contains("\"usage\":"));
    assert!(text.contains("Read-only log scan"));
    assert!(text.contains("ordinary opens can repair an incomplete tail"));
    let version = Command::new(binary).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert_eq!(
        String::from_utf8(version.stdout).unwrap().trim(),
        format!("{{\"version\":\"{}\"}}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn scan_pages_have_exclusive_cursors_and_exact_terminal_detection() {
    let d = Dir::new();
    assert!(run(&d, &["init"]).status.success());
    for key in ["", "00", "0000", "00ff", "01", "ff", "ffff"] {
        assert!(run(&d, &["put", key, "ab"]).status.success());
    }
    for (args, expected) in [
        (
            vec!["scan", "00", "2"],
            r#"{"entries":[{"key_hex":"00","value_hex":"ab"},{"key_hex":"0000","value_hex":"ab"}],"next_after_hex":"0000"}"#,
        ),
        (
            vec!["scan", "00", "1", "0000"],
            r#"{"entries":[{"key_hex":"00ff","value_hex":"ab"}],"next_after_hex":null}"#,
        ),
        (
            vec!["scan", "00", "1", "0001"],
            r#"{"entries":[{"key_hex":"00ff","value_hex":"ab"}],"next_after_hex":null}"#,
        ),
        (
            vec!["scan", "", "1"],
            r#"{"entries":[{"key_hex":"","value_hex":"ab"}],"next_after_hex":""}"#,
        ),
        (
            vec!["scan", "", "1", ""],
            r#"{"entries":[{"key_hex":"00","value_hex":"ab"}],"next_after_hex":"00"}"#,
        ),
        (
            vec!["scan", "ff", "2"],
            r#"{"entries":[{"key_hex":"ff","value_hex":"ab"},{"key_hex":"ffff","value_hex":"ab"}],"next_after_hex":null}"#,
        ),
        (
            vec!["scan", "ff", "1", "ffffff"],
            r#"{"entries":[],"next_after_hex":null}"#,
        ),
        (
            vec!["scan", "02", "1000"],
            r#"{"entries":[],"next_after_hex":null}"#,
        ),
    ] {
        let out = run(&d, &args);
        assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), expected);
    }
    assert!(run(&d, &["delete", "0000"]).status.success());
    assert!(run(&d, &["compact"]).status.success());
    let out = run(&d, &["scan", "00", "1", "0000"]);
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        r#"{"entries":[{"key_hex":"00ff","value_hex":"ab"}],"next_after_hex":null}"#
    );
}

#[test]
fn invalid_scan_arguments_do_not_create_store_files() {
    let d = Dir::new();
    for args in [
        vec!["scan"],
        vec!["scan", "00", "0"],
        vec!["scan", "00", "1001"],
        vec!["scan", "00", "-1"],
        vec!["scan", "00", "+1"],
        vec!["scan", "00", "1.0"],
        vec!["scan", "00", "9999999999999999999999999"],
        vec!["scan", "00", ""],
        vec!["scan", "z0", "1"],
        vec!["scan", "0", "1"],
        vec!["scan", "00", "1", "ff"],
        vec!["scan", "00", "1", "z0"],
        vec!["scan", "00", "1", "00", "extra"],
    ] {
        let out = run(&d, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
        assert!(!d.0.join("LOCK").exists());
        assert!(!d.0.join("data.wal").exists());
    }
    let out = run(&d, &["scan", "", "1"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(!d.0.join("LOCK").exists());
}

#[test]
fn maximum_keys_roundtrip_and_resume_from_the_returned_cursor_file() {
    let d = Dir::new();
    let inputs = Dir::new();
    let prefix = "00".repeat(durablestore::MAX_KEY - 1);
    let first = format!("{prefix}01");
    let second = format!("{prefix}02");
    let prefix_file = inputs.0.join("prefix.hex");
    let first_file = inputs.0.join("first.hex");
    let second_file = inputs.0.join("second.hex");
    let cursor_file = inputs.0.join("cursor.hex");
    fs::write(&prefix_file, format!("{prefix}\n")).unwrap();
    fs::write(&first_file, format!("{first}\r\n")).unwrap();
    fs::write(&second_file, &second).unwrap();
    let prefix_arg = format!("@{}", prefix_file.display());
    let first_arg = format!("@{}", first_file.display());
    let second_arg = format!("@{}", second_file.display());
    let cursor_arg = format!("@{}", cursor_file.display());
    assert!(run(&d, &["init"]).status.success());
    for (key, value) in [(&first_arg, "a1"), (&second_arg, "b2")] {
        assert!(run(&d, &["put", key, value]).status.success());
    }
    let page = run(&d, &["scan", &prefix_arg, "1"]);
    assert!(page.status.success());
    let page = String::from_utf8(page.stdout).unwrap();
    let cursor = page
        .split("\"next_after_hex\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap();
    assert!(
        cursor == first,
        "the response must expose the actual first key"
    );
    fs::write(&cursor_file, format!("{cursor}\n")).unwrap();
    assert!(run(&d, &["compact"]).status.success());
    let next = run(&d, &["scan", &prefix_arg, "1", &cursor_arg]);
    assert!(next.status.success());
    assert!(
        next.stdout == format!("{{\"entries\":[{{\"key_hex\":\"{second}\",\"value_hex\":\"b2\"}}],\"next_after_hex\":null}}\n").as_bytes(),
        "a fresh process must continue to the second maximum-size key"
    );
    let get = run(&d, &["get", &second_arg]);
    assert!(get.status.success());
    assert_eq!(get.stdout, b"{\"found\":true,\"value_hex\":\"b2\"}\n");
    assert!(run(&d, &["delete", &second_arg]).status.success());
    assert_eq!(run(&d, &["get", &second_arg]).status.code(), Some(3));
}

#[test]
fn hex_stdin_and_empty_files_preserve_binary_and_empty_key_semantics() {
    use std::io::Write;
    use std::process::Stdio;
    let d = Dir::new();
    let inputs = Dir::new();
    let empty = inputs.0.join("empty.hex");
    fs::write(&empty, b"").unwrap();
    let empty_arg = format!("@{}", empty.display());
    assert!(run(&d, &["init"]).status.success());
    let mut child = Command::new(env!("CARGO_BIN_EXE_durablestore"))
        .arg(&d.0)
        .args(["put", &empty_arg, "@-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"Ff00\r\n").unwrap();
    assert!(child.wait_with_output().unwrap().status.success());
    assert_eq!(
        run(&d, &["get", &empty_arg]).stdout,
        b"{\"found\":true,\"value_hex\":\"ff00\"}\n"
    );
    assert!(run(&d, &["put", "00", "11"]).status.success());
    assert_eq!(
        run(&d, &["scan", "", "1"]).stdout,
        b"{\"entries\":[{\"key_hex\":\"\",\"value_hex\":\"ff00\"}],\"next_after_hex\":\"\"}\n"
    );
    assert_eq!(
        run(&d, &["scan", "", "1", &empty_arg]).stdout,
        b"{\"entries\":[{\"key_hex\":\"00\",\"value_hex\":\"11\"}],\"next_after_hex\":null}\n"
    );
}

#[test]
fn invalid_hex_files_refuse_before_store_creation_or_mutation() {
    let d = Dir::new();
    let inputs = Dir::new();
    for (name, bytes) in [
        ("non-utf8", vec![0xff]),
        ("odd", b"f".to_vec()),
        ("embedded-line", b"00\n00\n".to_vec()),
        ("carriage-only", b"00\r".to_vec()),
        ("too-much-text", vec![b'0'; 2 * durablestore::MAX_KEY + 3]),
        (
            "decoded-too-long",
            vec![b'0'; 2 * durablestore::MAX_KEY + 2],
        ),
    ] {
        let path = inputs.0.join(name);
        fs::write(&path, bytes).unwrap();
        let arg = format!("@{}", path.display());
        for args in [
            vec!["get", &arg],
            vec!["delete", &arg],
            vec!["put", &arg, "00"],
            vec!["scan", &arg, "1"],
            vec!["scan", "", "1", &arg],
        ] {
            let out = run(&d, &args);
            assert_eq!(out.status.code(), Some(2));
            assert!(out.stdout.is_empty());
            assert!(String::from_utf8(out.stderr).unwrap().contains("hex"));
            assert!(!d.0.join("LOCK").exists());
            assert!(!d.0.join("data.wal").exists());
        }
    }
    let missing = format!("@{}", inputs.0.join("missing").display());
    let out = run(&d, &["get", &missing]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8(out.stderr).unwrap().contains("hex file"));
    assert!(!d.0.join("LOCK").exists());
    let out = run(&d, &["put", "@-", "@-"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("at most one")
    );
    assert!(!d.0.join("data.wal").exists());

    assert!(run(&d, &["init"]).status.success());
    assert!(run(&d, &["put", "aa", "bb"]).status.success());
    let wal = fs::read(d.0.join("data.wal")).unwrap();
    let bad_value = format!("@{}", inputs.0.join("non-utf8").display());
    let out = run(&d, &["put", "aa", &bad_value]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8(out.stderr).unwrap().contains("hex input"));
    assert_eq!(fs::read(d.0.join("data.wal")).unwrap(), wal);
}
