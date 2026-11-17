use durablestore::{
    HealthStatus, MAX_BATCH_BYTES, MAX_BATCH_OPERATIONS, MAX_KEY, MAX_VALUE, Operation, Stats,
    Store,
};
use std::{fmt::Write, io::Read, path::Path};
const USAGE: &str = "durablestore <existing-directory> init | put <key-hex> <value-hex> | get <key-hex> | delete <key-hex> | batch <file|-> [if <key-hex> <value-hex|absent>] | list | scan <prefix-hex> <limit:1..1000> [after-key-hex] | inspect | health | compact";
// Bound hex data per scan response in addition to the row limit.
const SCAN_HEX_BYTES: usize = 64 * 1024 * 1024;
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => {
                write!(out, "\\u{:04x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}
fn decode(s: &str, max: usize) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) || s.len() / 2 > max {
        return Err("hex must contain even digit count within the key/value limit".into());
    }
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            let digit = |c: u8| match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            };
            Ok(digit(p[0]).ok_or("invalid hex digit")? * 16
                + digit(p[1]).ok_or("invalid hex digit")?)
        })
        .collect()
}
fn decode_argument(s: &str, max: usize) -> Result<Vec<u8>, String> {
    let Some(path) = s.strip_prefix('@') else {
        return decode(s, max);
    };
    let input: Box<dyn Read> = if path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(std::fs::File::open(path).map_err(|e| format!("hex file: {e}"))?)
    };
    // Hex doubles the decoded limit; permit one final LF or CRLF, plus a
    // sentinel byte to detect oversized input without reading an entire file.
    let limit = 2 * max + 2;
    let mut bytes = Vec::new();
    input
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("hex input: {e}"))?;
    if bytes.len() > limit {
        return Err("hex input exceeds text size limit".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "hex input must be UTF-8")?;
    let text = text
        .strip_suffix('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .unwrap_or(text);
    decode(text, max)
}
type BatchEntry = (Vec<u8>, Option<Vec<u8>>);

fn read_batch(path: &str) -> Result<Vec<BatchEntry>, String> {
    // Hex expands bytes twofold; allow command names, tabs and CRLF per member.
    let limit = 2 * MAX_BATCH_BYTES + 16 * MAX_BATCH_OPERATIONS;
    let input: Box<dyn Read> = if path == "-" {
        Box::new(std::io::stdin())
    } else {
        Box::new(std::fs::File::open(path).map_err(|e| format!("batch file: {e}"))?)
    };
    let mut bytes = Vec::new();
    input
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("batch input: {e}"))?;
    if bytes.len() > limit {
        return Err("batch input exceeds text size limit".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "batch input must be UTF-8")?;
    let mut entries = Vec::new();
    let mut payload_bytes = 4usize;
    for (line_number, line) in text.lines().enumerate() {
        if entries.len() == MAX_BATCH_OPERATIONS {
            return Err("batch exceeds 1024 operations".into());
        }
        let mut fields = line.split('\t');
        let command = fields.next().unwrap_or_default();
        let key = fields.next();
        let value = fields.next();
        let fail = |message: &str| format!("batch line {}: {message}", line_number + 1);
        let entry = match (command, key, value, fields.next()) {
            ("put", Some(key), Some(value), None) => (
                decode(key, MAX_KEY).map_err(|e| fail(&e))?,
                Some(decode(value, MAX_VALUE).map_err(|e| fail(&e))?),
            ),
            ("delete", Some(key), None, None) => {
                (decode(key, MAX_KEY).map_err(|e| fail(&e))?, None)
            }
            _ => return Err(fail("expected tab-separated put/key/value or delete/key")),
        };
        payload_bytes += 9 + entry.0.len() + entry.1.as_ref().map_or(0, Vec::len);
        if payload_bytes > MAX_BATCH_BYTES {
            return Err("batch payload exceeds 32 MiB".into());
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err("batch input contains no operations".into());
    }
    Ok(entries)
}

fn stats(s: &Stats) -> String {
    format!(
        "{{\"live_keys\":{},\"live_bytes\":{},\"records\":{},\"log_bytes\":{},\"tail_bytes\":{}}}",
        s.live_keys, s.live_bytes, s.records, s.log_bytes, s.tail_bytes
    )
}
fn run(args: &[String]) -> Result<(String, i32), String> {
    if args == ["--help"] || args == ["-h"] {
        return Ok((
            format!(
                "{{\"usage\":{},\"bytes\":\"Keys and values are hex; use empty strings for empty bytes. Hex arguments also accept @FILE or @- for bounded file or stdin input, with at most one stdin argument.\",\"health\":\"Read-only log scan: exit 0 good, 3 incomplete or corrupt, 2 error.\",\"inspect\":\"Read-only live-state scan; ordinary opens can repair an incomplete tail.\"}}",
                quote(USAGE)
            ),
            0,
        ));
    }
    if args == ["--version"] {
        return Ok((
            format!("{{\"version\":{}}}", quote(env!("CARGO_PKG_VERSION"))),
            0,
        ));
    }
    if args.len() < 2 {
        return Err(USAGE.into());
    }
    if args[1] != "batch" && args[2..].iter().filter(|s| s.as_str() == "@-").count() > 1 {
        return Err("at most one hex argument may read stdin".into());
    }
    let scan = if args[1] == "scan" {
        if !(4..=5).contains(&args.len()) {
            return Err(USAGE.into());
        }
        if args[3].is_empty() || !args[3].bytes().all(|b| b.is_ascii_digit()) {
            return Err("scan limit must be an integer from 1 to 1000".into());
        }
        let limit: usize = args[3]
            .parse()
            .ok()
            .filter(|n| (1..=1000).contains(n))
            .ok_or("scan limit must be an integer from 1 to 1000")?;
        let prefix = decode_argument(&args[2], MAX_KEY)?;
        let after = args
            .get(4)
            .map(|s| decode_argument(s, MAX_KEY))
            .transpose()?;
        if after.as_ref().is_some_and(|key| !key.starts_with(&prefix)) {
            return Err("scan cursor must start with the requested prefix".into());
        }
        Some((prefix, limit, after))
    } else {
        None
    };
    let expected = match args[1].as_str() {
        "put" => 4,
        "scan" => args.len(),
        "batch" if args.len() == 6 && args[3] == "if" => 6,
        "get" | "delete" | "batch" => 3,
        "init" | "list" | "inspect" | "health" | "compact" => 2,
        _ => return Err(USAGE.into()),
    };
    if args.len() != expected {
        return Err(USAGE.into());
    }
    let condition = if args[1] == "batch" && args.len() == 6 {
        let stdin_readers = usize::from(args[2] == "-")
            + args[4..].iter().filter(|arg| arg.as_str() == "@-").count();
        if stdin_readers > 1 {
            return Err("batch input and condition may use stdin only once".into());
        }
        let key = decode_argument(&args[4], MAX_KEY)?;
        let expected = match args[5].as_str() {
            "absent" => None,
            value => Some(decode_argument(value, MAX_VALUE)?),
        };
        Some((key, expected))
    } else {
        None
    };
    let batch = if args[1] == "batch" {
        Some(read_batch(&args[2])?)
    } else {
        None
    };
    let key = if matches!(args[1].as_str(), "put" | "get" | "delete") {
        decode_argument(&args[2], MAX_KEY)?
    } else {
        vec![]
    };
    let value = if args[1] == "put" {
        decode_argument(&args[3], MAX_VALUE)?
    } else {
        vec![]
    };
    let dir = Path::new(&args[0]);
    if args[1] == "health" {
        let report = durablestore::log_health(dir).map_err(|e| e.to_string())?;
        let status = match report.status {
            HealthStatus::Good => "good",
            HealthStatus::Truncated => "truncated",
            HealthStatus::Corrupt => "corrupt",
            HealthStatus::SequenceError => "sequence_error",
        };
        let offset = report
            .error_offset
            .map_or("null".to_string(), |v| v.to_string());
        let reason = report.reason.map_or("null".to_string(), quote);
        let code = if report.status == HealthStatus::Good {
            0
        } else {
            3
        };
        return Ok((
            format!(
                "{{\"status\":\"{status}\",\"records\":{},\"valid_bytes\":{},\"log_bytes\":{},\"remaining_bytes\":{},\"error_offset\":{offset},\"reason\":{reason}}}",
                report.records, report.valid_bytes, report.log_bytes, report.remaining_bytes
            ),
            code,
        ));
    }
    if args[1] == "inspect" {
        return durablestore::inspect(dir)
            .map(|s| (stats(&s), 0))
            .map_err(|e| e.to_string());
    }
    if args[1] != "init" && !dir.join("data.wal").is_file() {
        return Err("store does not exist; create the directory and run init first".into());
    }
    let mut store = Store::open(dir).map_err(|e| e.to_string())?;
    match args[1].as_str() {
        "scan" => {
            let (prefix, limit, after) = scan.as_ref().unwrap();
            let mut entries = String::new();
            let mut hex_bytes = 0;
            let mut last_key = None;
            let mut more = false;
            for (count, (key, value)) in store.scan_prefix(prefix, after.as_deref()).enumerate() {
                let size = 2 * (key.len() + value.len());
                if count == *limit || hex_bytes + size > SCAN_HEX_BYTES {
                    more = true;
                    break;
                }
                if count > 0 {
                    entries.push(',');
                }
                write!(
                    entries,
                    "{{\"key_hex\":\"{}\",\"value_hex\":\"{}\"}}",
                    hex(key),
                    hex(value)
                )
                .unwrap();
                hex_bytes += size;
                last_key = Some(key);
            }
            // Every legal single entry fits the byte budget, so a nonterminal
            // page always advances, including when its last key is empty.
            let next = if more {
                quote(&hex(last_key.unwrap()))
            } else {
                "null".into()
            };
            Ok((
                format!("{{\"entries\":[{entries}],\"next_after_hex\":{next}}}"),
                0,
            ))
        }
        "init" => Ok((stats(&store.stats()), 0)),
        "put" => {
            store.put(&key, &value).map_err(|e| e.to_string())?;
            Ok(("{\"ok\":true,\"durable\":true}".into(), 0))
        }
        "delete" => {
            store.delete(&key).map_err(|e| e.to_string())?;
            Ok(("{\"ok\":true,\"durable\":true}".into(), 0))
        }
        "batch" => {
            // This handle owns the writer lock through both comparison and append.
            if let Some((key, expected)) = condition
                && store.get(&key) != expected.as_deref()
            {
                return Ok((r#"{"ok":false,"reason":"condition_failed"}"#.into(), 3));
            }
            let entries = batch.as_ref().unwrap();
            let operations: Vec<_> = entries
                .iter()
                .map(|(key, value)| match value {
                    Some(value) => Operation::Put(key, value),
                    None => Operation::Delete(key),
                })
                .collect();
            store.write_batch(&operations).map_err(|e| e.to_string())?;
            Ok((
                format!(
                    "{{\"ok\":true,\"durable\":true,\"operations\":{}}}",
                    operations.len()
                ),
                0,
            ))
        }
        "get" => match store.get(&key) {
            Some(v) => Ok((
                format!("{{\"found\":true,\"value_hex\":\"{}\"}}", hex(v)),
                0,
            )),
            None => Ok(("{\"found\":false}".into(), 3)),
        },
        "list" => {
            let list = store
                .entries()
                .map(|(k, v)| {
                    format!(
                        "{{\"key_hex\":\"{}\",\"value_hex\":\"{}\"}}",
                        hex(k),
                        hex(v)
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            Ok((format!("{{\"entries\":[{list}]}}"), 0))
        }
        "compact" => {
            let before = store.stats();
            store.compact().map_err(|e| e.to_string())?;
            Ok((
                format!(
                    "{{\"before\":{},\"after\":{}}}",
                    stats(&before),
                    stats(&store.stats())
                ),
                0,
            ))
        }
        _ => unreachable!(),
    }
}
fn main() {
    let args: Result<Vec<_>, _> = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "arguments must be valid UTF-8".to_owned())
        })
        .collect();
    match args.and_then(|args| run(&args)) {
        Ok((json, code)) => {
            println!("{json}");
            std::process::exit(code)
        }
        Err(e) => {
            eprintln!("{{\"error\":{}}}", quote(&e));
            std::process::exit(2)
        }
    }
}
