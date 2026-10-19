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

/// Stack for [`with_stack`]: room for recursion as deep as Python's own
/// recursion limit (about 1000 frames) many times over. Reserved, not used:
/// only the pages a deep input reaches are ever touched.
pub const DEEP_STACK: usize = 64 << 20;

/// Stack for [`with_stack`] around code that walks a value as deep as
/// Python's json decodes (52000 levels on the default stack) recursively,
/// e.g. serde's own serializer: a few hundred bytes a level, many times
/// over. Reserved, not used, like [`DEEP_STACK`].
pub const JSON_STACK: usize = 512 << 20;

/// `f()` on a thread with a `size`-byte stack, for code that recurses as
/// deep as its input (an XML tree, a plural expression, a value as nested as
/// Python allows). The caller's own stack is whatever its thread was given --
/// `ulimit -s` on the main thread, 2 MiB on a plain Rust thread -- and an
/// overflow there aborts the process. The thread takes the caller's name, so
/// code that asks whether it is on the main thread gets the same answer.
/// Runs here when no thread can be started.
pub fn with_stack<R: Send>(size: usize, f: impl FnOnce() -> R + Send) -> R {
    let mut job = Some(f);
    let name = std::thread::current().name().map(str::to_owned);
    let out = std::thread::scope(|s| {
        let job = &mut job;
        let mut b = std::thread::Builder::new().stack_size(size);
        if let Some(n) = name {
            b = b.name(n);
        }
        match b.spawn_scoped(s, move || job.take().map(|f| f())) {
            Ok(h) => Some(h.join().unwrap_or_else(|p| std::panic::resume_unwind(p))),
            Err(_) => None,
        }
    });
    match (out, job.take()) {
        (Some(Some(r)), _) => r,
        (_, Some(f)) => f(),
        // The job ran on the thread and returned: `out` held its result.
        (_, None) => unreachable!("a job that ran returned its result"),
    }
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

/// Walking a value as nested as Python's json allows (52000 levels on the
/// default stack) without recursion: a derived `Clone`, `PartialEq` or a
/// recursive conversion goes one Rust frame per level and overflows the
/// stack long before that. The value types that parse such documents
/// implement those by hand on top of these.
pub mod tree {
    /// What a node is, for the walks below.
    pub enum Shape<'a, T, K> {
        /// No children.
        Leaf,
        /// Children in order.
        List(&'a [T]),
        /// Keyed children in order.
        Dict(&'a [(K, T)]),
    }

    enum Work<'a, T> {
        Visit(&'a T),
        Build(&'a T, usize, bool),
    }

    /// Post-order fold: `leaf` for a node without children; `list` and
    /// `dict` get the node and its children's results, in order.
    pub fn fold<'a, T, K: 'a, R>(
        root: &'a T,
        shape: impl Fn(&'a T) -> Shape<'a, T, K>,
        mut leaf: impl FnMut(&'a T) -> R,
        mut list: impl FnMut(&'a T, Vec<R>) -> R,
        mut dict: impl FnMut(&'a T, Vec<R>) -> R,
    ) -> R {
        let mut out: Vec<R> = Vec::new();
        let mut work = vec![Work::Visit(root)];
        while let Some(w) = work.pop() {
            match w {
                Work::Visit(n) => match shape(n) {
                    Shape::Leaf => out.push(leaf(n)),
                    Shape::List(xs) => {
                        work.push(Work::Build(n, xs.len(), false));
                        work.extend(xs.iter().rev().map(Work::Visit));
                    }
                    Shape::Dict(es) => {
                        work.push(Work::Build(n, es.len(), true));
                        work.extend(es.iter().rev().map(|(_, v)| Work::Visit(v)));
                    }
                },
                Work::Build(n, k, is_dict) => {
                    let kids = out.split_off(out.len() - k);
                    out.push(if is_dict { dict(n, kids) } else { list(n, kids) });
                }
            }
        }
        match out.pop() {
            Some(r) => r,
            None => unreachable!("the root is always built"),
        }
    }

    /// [`fold`] over an arena, where a node's children are indices: `kids`
    /// adds a node's children (in order) to the vector and says whether it
    /// is a container (`Some(is_dict)`) or a leaf (`None`).
    pub fn fold_ids<R>(
        root: usize,
        kids: impl Fn(usize, &mut Vec<usize>) -> Option<bool>,
        mut leaf: impl FnMut(usize) -> R,
        mut list: impl FnMut(usize, Vec<R>) -> R,
        mut dict: impl FnMut(usize, Vec<R>) -> R,
    ) -> R {
        enum Job {
            Visit(usize),
            Build(usize, usize, bool),
        }
        let mut out: Vec<R> = Vec::new();
        let mut work = vec![Job::Visit(root)];
        let mut buf = Vec::new();
        while let Some(w) = work.pop() {
            match w {
                Job::Visit(n) => {
                    buf.clear();
                    match kids(n, &mut buf) {
                        None => out.push(leaf(n)),
                        Some(is_dict) => {
                            work.push(Job::Build(n, buf.len(), is_dict));
                            work.extend(buf.iter().rev().map(|&c| Job::Visit(c)));
                        }
                    }
                }
                Job::Build(n, k, is_dict) => {
                    let got = out.split_off(out.len() - k);
                    out.push(if is_dict { dict(n, got) } else { list(n, got) });
                }
            }
        }
        match out.pop() {
            Some(r) => r,
            None => unreachable!("the root is always built"),
        }
    }

    /// Drop a serde value without recursion (serde_json's own drop recurses
    /// once per level).
    pub fn drop_value(v: serde_json::Value) {
        use serde_json::Value;
        let mut todo = vec![v];
        while let Some(mut v) = todo.pop() {
            match &mut v {
                Value::Array(a) => todo.append(a),
                Value::Object(m) => todo.extend(std::mem::take(m).into_iter().map(|(_, x)| x)),
                _ => {}
            }
        }
    }

    /// `v.clone()` for a serde value, without recursion.
    pub fn clone_value(v: &serde_json::Value) -> serde_json::Value {
        use serde_json::Value;
        enum Job<'a> {
            Visit(&'a Value),
            Build(&'a Value, usize),
        }
        let mut out: Vec<Value> = Vec::new();
        let mut work = vec![Job::Visit(v)];
        while let Some(w) = work.pop() {
            match w {
                Job::Visit(n @ Value::Array(a)) => {
                    work.push(Job::Build(n, a.len()));
                    work.extend(a.iter().rev().map(Job::Visit));
                }
                Job::Visit(n @ Value::Object(m)) => {
                    work.push(Job::Build(n, m.len()));
                    work.extend(m.values().rev().map(Job::Visit));
                }
                Job::Visit(leaf) => out.push(leaf.clone()),
                Job::Build(n, k) => {
                    let kids = out.split_off(out.len() - k);
                    out.push(match n {
                        Value::Object(m) => Value::Object(m.keys().cloned().zip(kids).collect()),
                        _ => Value::Array(kids),
                    });
                }
            }
        }
        out.pop().unwrap_or(Value::Null)
    }

    /// `a == b` for serde values (objects compare as maps), without
    /// recursion.
    pub fn eq_value(a: &serde_json::Value, b: &serde_json::Value) -> bool {
        use serde_json::Value;
        let mut todo = vec![(a, b)];
        while let Some(pair) = todo.pop() {
            match pair {
                (Value::Array(x), Value::Array(y)) => {
                    if x.len() != y.len() {
                        return false;
                    }
                    todo.extend(x.iter().zip(y));
                }
                (Value::Object(x), Value::Object(y)) => {
                    if x.len() != y.len() {
                        return false;
                    }
                    for (k, va) in x {
                        match y.get(k) {
                            Some(vb) => todo.push((va, vb)),
                            None => return false,
                        }
                    }
                }
                (Value::Array(_) | Value::Object(_), _) | (_, Value::Array(_) | Value::Object(_)) => return false,
                (x, y) => {
                    if x != y {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// How many arrays/objects deep a serde value goes.
    pub fn value_depth(v: &serde_json::Value) -> usize {
        use serde_json::Value;
        let mut deepest = 0;
        let mut todo = vec![(v, 0usize)];
        while let Some((v, d)) = todo.pop() {
            match v {
                Value::Array(a) => {
                    deepest = deepest.max(d + 1);
                    todo.extend(a.iter().map(|x| (x, d + 1)));
                }
                Value::Object(m) => {
                    deepest = deepest.max(d + 1);
                    todo.extend(m.values().map(|x| (x, d + 1)));
                }
                _ => {}
            }
        }
        deepest
    }

    /// Structural equality, as a derived `PartialEq` decides it: the same
    /// variant, the same keys in the same order, equal leaves (`leaf_eq`,
    /// called only for two leaves of one variant).
    pub fn eq<'a, T, K: PartialEq + 'a>(
        a: &'a T,
        b: &'a T,
        shape: impl Fn(&'a T) -> Shape<'a, T, K>,
        leaf_eq: impl Fn(&'a T, &'a T) -> bool,
    ) -> bool {
        let mut todo = vec![(a, b)];
        while let Some((a, b)) = todo.pop() {
            if std::mem::discriminant(a) != std::mem::discriminant(b) {
                return false;
            }
            match (shape(a), shape(b)) {
                (Shape::List(x), Shape::List(y)) => {
                    if x.len() != y.len() {
                        return false;
                    }
                    todo.extend(x.iter().zip(y));
                }
                (Shape::Dict(x), Shape::Dict(y)) => {
                    if x.len() != y.len() {
                        return false;
                    }
                    for ((ka, va), (kb, vb)) in x.iter().zip(y) {
                        if ka != kb {
                            return false;
                        }
                        todo.push((va, vb));
                    }
                }
                (Shape::Leaf, Shape::Leaf) => {
                    if !leaf_eq(a, b) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        true
    }

    /// How many containers deep the value goes (0 for a leaf).
    pub fn depth<'a, T, K: 'a>(root: &'a T, shape: impl Fn(&'a T) -> Shape<'a, T, K>) -> usize {
        let mut deepest = 0;
        let mut todo = vec![(root, 0usize)];
        while let Some((n, d)) = todo.pop() {
            match shape(n) {
                Shape::Leaf => deepest = deepest.max(d),
                Shape::List(xs) => {
                    deepest = deepest.max(d + 1);
                    todo.extend(xs.iter().map(|x| (x, d + 1)));
                }
                Shape::Dict(es) => {
                    deepest = deepest.max(d + 1);
                    todo.extend(es.iter().map(|(_, x)| (x, d + 1)));
                }
            }
        }
        deepest
    }
}

/// `Clone` and `PartialEq` without recursion (see [`tree`]) for a Python
/// value enum of the usual shape: unit variants, one-field leaf variants, a
/// list variant holding `Vec<Self>` and a dict variant holding
/// `Vec<(key, Self)>` (optionally inside a tuple struct, `dict: D(Wrapper)`).
/// The type keeps `#[derive(Debug)]` and its own iterative `Drop`.
macro_rules! tree_clone_eq {
    ($T:ident, key: $K:ty, units: [$($u:ident),*], leaves: [$($l:ident),*], list: $L:ident, dict: $D:ident $(($W:ident))?) => {
        impl $T {
            fn tree_shape(&self) -> $crate::util::tree::Shape<'_, $T, $K> {
                match self {
                    $T::$L(xs) => $crate::util::tree::Shape::List(xs),
                    #[allow(unused_parens)]
                    $T::$D($($W)?(es)) => $crate::util::tree::Shape::Dict(es),
                    _ => $crate::util::tree::Shape::Leaf,
                }
            }
        }

        impl Clone for $T {
            fn clone(&self) -> Self {
                $crate::util::tree::fold(
                    self,
                    $T::tree_shape,
                    |n| match n {
                        $($T::$u => $T::$u,)*
                        $($T::$l(x) => $T::$l(x.clone()),)*
                        _ => unreachable!("containers are built from their children"),
                    },
                    |_, kids| $T::$L(kids),
                    |n, kids| match n {
                        #[allow(unused_parens)]
                        $T::$D($($W)?(es)) => $T::$D($($W)?(es.iter().map(|(k, _)| k.clone()).zip(kids).collect())),
                        _ => unreachable!("only a dict is built as one"),
                    },
                )
            }
        }

        impl PartialEq for $T {
            fn eq(&self, other: &Self) -> bool {
                // Called for two leaves of one variant only.
                $crate::util::tree::eq(self, other, $T::tree_shape, |a, b| match (a, b) {
                    $(($T::$l(x), $T::$l(y)) => x == y,)*
                    _ => true,
                })
            }
        }
    };
}
pub(crate) use tree_clone_eq;

/// Tests that set environment variables (XDG_STATE_HOME and the like) take
/// this first: the environment is the process's, and tests in different
/// modules run at once.
#[cfg(test)]
pub(crate) static TEST_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// Run `f` on a thread with a deliberately small stack: anything that
    /// recursed once per level of a deep value would overflow it.
    fn on_small_stack<R: Send>(f: impl FnOnce() -> R + Send) -> R {
        std::thread::scope(|s| std::thread::Builder::new().stack_size(128 << 10).spawn_scoped(s, f).unwrap().join().unwrap())
    }

    #[derive(Debug)]
    enum V {
        N,
        I(i64),
        L(Vec<V>),
        D(Vec<(String, V)>),
    }
    tree_clone_eq!(V, key: String, units: [N], leaves: [I], list: L, dict: D);
    impl Drop for V {
        fn drop(&mut self) {
            let mut todo = Vec::new();
            match self {
                V::L(a) => todo.append(a),
                V::D(d) => todo.extend(d.drain(..).map(|(_, x)| x)),
                _ => {}
            }
            while let Some(mut v) = todo.pop() {
                match &mut v {
                    V::L(a) => todo.append(a),
                    V::D(d) => todo.extend(d.drain(..).map(|(_, x)| x)),
                    _ => {}
                }
            }
        }
    }

    fn deep_v(n: usize) -> V {
        let mut v = V::I(7);
        for i in 0..n {
            v = if i % 2 == 0 { V::L(vec![V::N, v]) } else { V::D(vec![("k".into(), v)]) };
        }
        v
    }

    #[test]
    fn deep_values_clone_compare_and_drop_without_recursion() {
        on_small_stack(|| {
            let a = deep_v(200_000);
            let b = a.clone();
            assert!(a == b);
            assert_eq!(tree::depth(&a, V::tree_shape), 200_000);
            let mut c = deep_v(200_000);
            if let V::D(d) = &mut c {
                d[0].0 = "other".into();
            }
            assert!(a != c);
            assert!(V::L(vec![V::I(1)]) != V::L(vec![V::I(2)]));
            assert!(V::D(vec![("a".into(), V::N)]) != V::D(vec![("b".into(), V::N)]));
            assert!(V::L(vec![]) != V::D(vec![]));
        });
    }

    #[test]
    fn deep_serde_values_fold_and_drop_without_recursion() {
        use serde_json::Value;
        on_small_stack(|| {
            // An arena: node i holds node i - 1, node 0 is a leaf.
            let n = 200_000;
            let v = tree::fold_ids(
                n,
                |i, out| (i > 0).then(|| out.push(i - 1)).map(|()| i % 2 == 0),
                |_| Value::from(1),
                |_, kids| Value::Array(kids),
                |_, kids| Value::Object(kids.into_iter().map(|k| ("k".to_string(), k)).collect()),
            );
            let mut depth = 0;
            let mut at = &v;
            loop {
                at = match at {
                    Value::Array(a) => &a[0],
                    Value::Object(m) => &m["k"],
                    _ => break,
                };
                depth += 1;
            }
            assert_eq!(depth, n);
            tree::drop_value(v);
        });
    }

    #[test]
    fn with_stack_gives_deep_recursion_room_and_keeps_the_thread_name() {
        fn down(n: u32) -> u32 {
            if n == 0 { 0 } else { 1 + std::hint::black_box(down(n - 1)) }
        }
        let got = std::thread::scope(|s| {
            std::thread::Builder::new()
                .name("main".into())
                .stack_size(128 << 10)
                .spawn_scoped(s, || with_stack(DEEP_STACK, || (down(100_000), std::thread::current().name().map(String::from))))
                .unwrap()
                .join()
                .unwrap()
        });
        assert_eq!(got, (100_000, Some("main".to_string())));
    }
}
