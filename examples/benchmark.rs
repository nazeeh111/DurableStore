//! Reproducible local workload. No throughput comparison with other engines is implied.
use durablestore::Store;
use std::{fmt::Write, fs, hint::black_box, path::PathBuf, process::Command, time::Instant};
fn quantile(ns: &mut [u128], pct: usize) -> u128 {
    ns.sort_unstable();
    ns[(ns.len() - 1) * pct / 100]
}
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c < ' ' => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn command(args: &[&str]) -> String {
    Command::new(args[0])
        .args(&args[1..])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unavailable".into())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() || args.len() > 2 {
        return Err("usage: benchmark <new-empty-directory> [keys: 1..100000]".into());
    }
    let count: usize = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(1000);
    if !(1..=100_000).contains(&count) {
        return Err("key count must be 1..100000".into());
    }
    let path = PathBuf::from(&args[0]);
    fs::create_dir(&path)?;
    let mut s = Store::open(&path)?;
    let value = vec![0x5a; 256];
    let mut writes = Vec::with_capacity(2 * count);
    let start = Instant::now();
    for round in 0..2 {
        for i in 0..count {
            let key = (i as u64).to_le_bytes();
            let mut payload = value.clone();
            payload[0] = round;
            let t = Instant::now();
            s.put(&key, &payload)?;
            writes.push(t.elapsed().as_nanos());
        }
    }
    let write_elapsed = start.elapsed().as_secs_f64();
    let mut reads = Vec::with_capacity(count * 10);
    for i in 0..count * 10 {
        let key = ((i % count) as u64).to_le_bytes();
        let t = Instant::now();
        let got = s.get(black_box(&key)).unwrap();
        black_box(got);
        reads.push(t.elapsed().as_nanos());
        assert_eq!(got[0], 1);
    }
    for i in 0..count / 4 {
        s.delete(&(i as u64).to_le_bytes())?;
    }
    let before = s.stats();
    let t = Instant::now();
    s.compact()?;
    let compact_ns = t.elapsed().as_nanos();
    let after = s.stats();
    drop(s);
    let t = Instant::now();
    let s = Store::open(&path)?;
    let reopen_ns = t.elapsed().as_nanos();
    assert_eq!(s.stats().live_keys, count - count / 4);
    for i in 0..count {
        let got = s.get(&(i as u64).to_le_bytes());
        if i < count / 4 {
            assert!(got.is_none());
        } else {
            assert_eq!(got.unwrap()[0], 1);
        }
    }
    println!(
        "{{\"schema_version\":1,\"package_version\":{},\"profile\":{},\"os\":{},\"arch\":{},\"kernel\":{},\"cpu\":{},\"rustc\":{},\"available_parallelism\":{},\"keys\":{},\"value_bytes\":256,\"put_operations\":{},\"read_operations\":{},\"delete_operations\":{},\"write_seconds\":{},\"durable_put_ns\":{{\"p50\":{},\"p95\":{},\"p99\":{}}},\"in_memory_get_ns\":{{\"p50\":{},\"p95\":{},\"p99\":{}}},\"compact_ns\":{},\"reopen_ns\":{},\"log_bytes_before\":{},\"log_bytes_after\":{},\"validated\":true}}",
        quote(env!("CARGO_PKG_VERSION")),
        quote(if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }),
        quote(std::env::consts::OS),
        quote(std::env::consts::ARCH),
        quote(&command(&["uname", "-sr"])),
        quote(&if cfg!(target_os = "macos") {
            command(&["sysctl", "-n", "machdep.cpu.brand_string"])
        } else {
            command(&["uname", "-m"])
        }),
        quote(&command(&["rustc", "--version"])),
        std::thread::available_parallelism()?.get(),
        count,
        count * 2,
        count * 10,
        count / 4,
        write_elapsed,
        quantile(&mut writes, 50),
        quantile(&mut writes, 95),
        quantile(&mut writes, 99),
        quantile(&mut reads, 50),
        quantile(&mut reads, 95),
        quantile(&mut reads, 99),
        compact_ns,
        reopen_ns,
        before.log_bytes,
        after.log_bytes
    );
    Ok(())
}
