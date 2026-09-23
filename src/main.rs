use durablestore::{MAX_KEY, MAX_VALUE, Stats, Store};
use std::{fmt::Write, path::Path};
const USAGE: &str = "durablestore <existing-directory> init | put <key-hex> <value-hex> | get <key-hex> | delete <key-hex> | list | inspect | compact";
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
fn stats(s: &Stats) -> String {
    format!(
        "{{\"live_keys\":{},\"live_bytes\":{},\"records\":{},\"log_bytes\":{},\"tail_bytes\":{}}}",
        s.live_keys, s.live_bytes, s.records, s.log_bytes, s.tail_bytes
    )
}
fn run(args: &[String]) -> Result<(String, i32), String> {
    if args == ["--help"] || args == ["-h"] {
        return Ok((format!("{{\"usage\":{}}}", quote(USAGE)), 0));
    }
    if args.len() < 2 {
        return Err(USAGE.into());
    }
    let expected = match args[1].as_str() {
        "put" => 4,
        "get" | "delete" => 3,
        "init" | "list" | "inspect" | "compact" => 2,
        _ => return Err(USAGE.into()),
    };
    if args.len() != expected {
        return Err(USAGE.into());
    }
    let key = if expected >= 3 {
        decode(&args[2], MAX_KEY)?
    } else {
        vec![]
    };
    let value = if expected == 4 {
        decode(&args[3], MAX_VALUE)?
    } else {
        vec![]
    };
    let dir = Path::new(&args[0]);
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
        "init" => Ok((stats(&store.stats()), 0)),
        "put" => {
            store.put(&key, &value).map_err(|e| e.to_string())?;
            Ok(("{\"ok\":true,\"durable\":true}".into(), 0))
        }
        "delete" => {
            store.delete(&key).map_err(|e| e.to_string())?;
            Ok(("{\"ok\":true,\"durable\":true}".into(), 0))
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
