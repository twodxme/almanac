//! What the Python standard library gave every module for free, written once
//! so 34 ports do not each grow their own subtly different copy: running a
//! command with a timeout, Python's rounding and title-casing, ISO dates, and
//! the atomic JSON write every state file uses.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The outcome of `subprocess.run(..., capture_output=True, text=True)`.
#[derive(Debug, Clone, Default)]
pub struct Output {
    /// None when the process could not be started, or timed out.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    /// The process could not be started at all (Python's OSError).
    pub failed_to_start: bool,
}

impl Output {
    /// `returncode == 0`.
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// Options for [`run_with`]; `Default` is a plain capture with no timeout.
#[derive(Debug, Clone, Default)]
pub struct RunOpts<'a> {
    pub timeout: Option<Duration>,
    pub input: Option<&'a str>,
    /// Extra environment variables on top of the inherited ones.
    pub env: Vec<(String, String)>,
    pub cwd: Option<&'a Path>,
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=secs)`.
/// Never panics: a missing binary or a timeout is reported in the result,
/// the way the Python callers catch OSError and TimeoutExpired.
pub fn run(argv: &[&str], timeout_secs: u64) -> Output {
    run_with(argv, &RunOpts { timeout: Some(Duration::from_secs(timeout_secs)), ..Default::default() })
}

pub fn run_with(argv: &[&str], opts: &RunOpts) -> Output {
    let Some((prog, args)) = argv.split_first() else {
        return Output { failed_to_start: true, ..Default::default() };
    };
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdin(if opts.input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    if let Some(dir) = opts.cwd {
        cmd.current_dir(dir);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return Output { failed_to_start: true, ..Default::default() },
    };
    if let (Some(text), Some(mut stdin)) = (opts.input, child.stdin.take()) {
        let text = text.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    // Drain both pipes on their own threads, or a chatty child fills one and
    // blocks forever while we wait on it.
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.read_to_end(&mut s);
        }
        s
    });
    let err_t = std::thread::spawn(move || {
        let mut s = Vec::new();
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.read_to_end(&mut s);
        }
        s
    });
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if let Some(t) = opts.timeout {
                    if start.elapsed() >= t {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        break None;
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break None,
        }
    };
    let stdout = String::from_utf8_lossy(&out_t.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    Output { code: status.and_then(|s| s.code()), stdout, stderr, timed_out, failed_to_start: false }
}

/// `shutil.which(name)`.
pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let p = PathBuf::from(name);
        return is_executable(&p).then_some(p);
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// `Path.home()`.
pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Python's `round(x)`: halves go to the even neighbour (`round(2.5) == 2`),
/// not away from zero as Rust's `f64::round` does. Every colour mix and size
/// in the Python goes through this, so parity depends on it.
pub fn py_round(x: f64) -> f64 {
    x.round_ties_even()
}

/// Python's `round(x, ndigits)`.
pub fn py_round_to(x: f64, ndigits: i32) -> f64 {
    // Python rounds the exact decimal value correctly; going through the
    // shortest-repr string and back matches it for every value we display.
    format!("{:.*}", ndigits.max(0) as usize, x).parse().unwrap_or(x)
}

/// Python's `str.title()`: upper-case the first cased character after any
/// uncased one, lower-case the rest. `"apple-dark".title() == "Apple-Dark"`.
pub fn py_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for ch in s.chars() {
        if prev_cased {
            out.extend(ch.to_lowercase());
        } else {
            out.extend(ch.to_uppercase());
        }
        prev_cased = ch.is_lowercase() || ch.is_uppercase();
    }
    out
}

/// Python's `str.islower()`: at least one cased character, none upper-case.
pub fn py_islower(s: &str) -> bool {
    let mut cased = false;
    for ch in s.chars() {
        if ch.is_uppercase() {
            return false;
        }
        if ch.is_lowercase() {
            cased = true;
        }
    }
    cased
}

/// `datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()`, or 0.0
/// when it does not parse -- the `_ts` helper several modules carry. A naive
/// timestamp (no offset) is taken as local time, as Python does.
pub fn iso_ts(value: &str) -> f64 {
    let v = value.trim();
    if v.is_empty() {
        return 0.0;
    }
    let v = v.replace('Z', "+00:00");
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&v) {
        return dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_micros()) / 1e6;
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%dT%H:%M%:z"] {
        if let Ok(dt) = chrono::DateTime::parse_from_str(&v, fmt) {
            return dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_micros()) / 1e6;
        }
    }
    use chrono::TimeZone;
    for fmt in ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(&v, fmt) {
            if let Some(dt) = chrono::Local.from_local_datetime(&naive).earliest() {
                return dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_micros()) / 1e6;
            }
        }
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(&v, "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(0, 0, 0).and_then(|n| chrono::Local.from_local_datetime(&n).earliest()) {
            return dt.timestamp() as f64;
        }
    }
    0.0
}

/// `time.time()`.
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Read a JSON file, or `None` when it is missing or broken -- never an error,
/// the way every Python state reader falls back to its defaults.
pub fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write JSON atomically: to `<name>.tmp` beside it, then rename -- the
/// `tmp.write_text(...); tmp.replace(p)` pattern of the Python. Creates the
/// parent directory. `indent` mirrors `json.dumps(indent=...)`; `sort_keys`
/// mirrors `sort_keys=True`.
pub fn write_json(path: &Path, value: &serde_json::Value, indent: Option<usize>, sort_keys: bool) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = dumps(value, indent, sort_keys);
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// `json.dumps(value, indent=indent, sort_keys=sort_keys)` with Python's
/// default separators, so files written by either implementation are
/// byte-identical.
pub fn dumps(value: &serde_json::Value, indent: Option<usize>, sort_keys: bool) -> String {
    let mut out = String::new();
    dump_into(&mut out, value, indent, sort_keys, 0);
    out
}

fn dump_into(out: &mut String, v: &serde_json::Value, indent: Option<usize>, sort: bool, level: usize) {
    use serde_json::Value;
    let nl = |out: &mut String, lvl: usize| {
        if let Some(n) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(n * lvl));
        }
    };
    let (item_sep, key_sep) = if indent.is_some() { (",", ": ") } else { (", ", ": ") };
    match v {
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                nl(out, level + 1);
                dump_into(out, x, indent, sort, level + 1);
            }
            nl(out, level);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = map.keys().collect();
            if sort {
                keys.sort();
            }
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push_str(item_sep);
                }
                nl(out, level + 1);
                out.push_str(&py_json_string(k));
                out.push_str(key_sep);
                dump_into(out, &map[*k], indent, sort, level + 1);
            }
            nl(out, level);
            out.push('}');
        }
        Value::String(s) => out.push_str(&py_json_string(s)),
        Value::Number(n) => {
            if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
                out.push_str(&py_float_repr(f));
            } else {
                out.push_str(&n.to_string());
            }
        }
        other => out.push_str(&other.to_string()),
    }
}

/// A JSON string the way Python's `json.dumps` writes it (`ensure_ascii=True`).
pub fn py_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Python's `repr(float)`: shortest round-trip, always with a decimal point
/// or exponent (`1.0`, not `1`).
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity".into() } else { "-Infinity".into() };
    }
    let s = format!("{f:?}");
    // Rust's Debug is shortest round-trip too; Python switches to exponent
    // notation from 1e16 and below 1e-4.
    let a = f.abs();
    if a != 0.0 && (a >= 1e16 || a < 1e-4) {
        let e = format!("{f:e}");
        let (m, exp) = e.split_once('e').unwrap_or((&e, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let m = if m.contains('.') { m.to_string() } else { m.to_string() };
        return format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    s
}

/// A `dict[str, str]`-shaped helper for parity args.
pub fn str_map(v: &serde_json::Value) -> HashMap<String, String> {
    v.as_object()
        .map(|m| m.iter().map(|(k, x)| (k.clone(), x.as_str().unwrap_or_default().to_string())).collect())
        .unwrap_or_default()
}

/// Argument helpers for `parity()` hooks: `arg_str(args, "name")`.
pub fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or_default()
}

pub fn arg_f64(args: &serde_json::Value, key: &str) -> f64 {
    args.get(key).and_then(|v| v.as_f64()).unwrap_or_default()
}

pub fn arg_i64(args: &serde_json::Value, key: &str) -> i64 {
    args.get(key).and_then(|v| v.as_i64()).unwrap_or_default()
}

pub fn arg_bool(args: &serde_json::Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or_default()
}

pub fn arg_strs(args: &serde_json::Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().map(|x| x.as_str().unwrap_or_default().to_string()).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_like_python() {
        assert_eq!(py_round(2.5), 2.0);
        assert_eq!(py_round(3.5), 4.0);
        assert_eq!(py_round(-2.5), -2.0);
        assert_eq!(py_round(2.6), 3.0);
    }

    #[test]
    fn title_and_islower_like_python() {
        assert_eq!(py_title("apple-dark"), "Apple-Dark");
        assert_eq!(py_title("a1b c"), "A1B C");
        assert!(py_islower("apple-dark"));
        assert!(!py_islower("Apple"));
        assert!(!py_islower("123"));
    }

    #[test]
    fn json_matches_python_dumps() {
        let v: serde_json::Value = serde_json::from_str(r#"{"b": [1, 2.5, "é"], "a": {}}"#).unwrap();
        assert_eq!(dumps(&v, None, true), r#"{"a": {}, "b": [1, 2.5, "\u00e9"]}"#);
        assert_eq!(dumps(&v, Some(2), true), "{\n  \"a\": {},\n  \"b\": [\n    1,\n    2.5,\n    \"\\u00e9\"\n  ]\n}");
        assert_eq!(py_float_repr(1.0), "1.0");
        assert_eq!(py_float_repr(1e16), "1e+16");
    }

    #[test]
    fn runs_with_timeout() {
        let out = run(&["sh", "-c", "echo hi; echo err >&2"], 5);
        assert!(out.ok());
        assert_eq!(out.stdout, "hi\n");
        assert_eq!(out.stderr, "err\n");
        let slow = run(&["sleep", "5"], 1);
        assert!(slow.timed_out);
        assert!(run(&["/nonexistent-binary"], 1).failed_to_start);
    }

    #[test]
    fn parses_iso_like_python() {
        assert_eq!(iso_ts("2026-10-17T00:00:00Z"), 1792195200.0);
        assert_eq!(iso_ts("2026-10-17T00:00:00+00:00"), 1792195200.0);
        assert_eq!(iso_ts("garbage"), 0.0);
    }
}
