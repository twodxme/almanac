//! What the Python standard library gave every module for free, written once
//! so 34 ports do not each grow their own subtly different copy: running a
//! command with a timeout, Python's rounding and title-casing, ISO dates, and
//! the atomic JSON write every state file uses.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub mod human;
pub mod pyjson;

pub use human::human_error;

/// The outcome of `subprocess.run(..., capture_output=True, text=True)`;
/// `Output<Vec<u8>>` ([`run_bytes`]) is the same without `text=True`.
#[derive(Debug, Clone, Default)]
pub struct Output<T = String> {
    /// None when the process could not be started, timed out, or was
    /// killed by a signal.
    pub code: Option<i32>,
    /// The signal that ended it, when one did (Python's negative
    /// returncode); None after a timeout, whose kill is ours.
    pub signal: Option<i32>,
    pub stdout: T,
    pub stderr: T,
    pub timed_out: bool,
    /// The process could not be started at all (Python's OSError).
    pub failed_to_start: bool,
    /// Why it could not be started: the errno, when there is one, so a
    /// caller can print the OSError text Python would (`[Errno 13] ...`).
    pub start_errno: Option<i32>,
    /// The start error's own text, for a failure with no errno.
    pub start_error: String,
}

impl<T> Output<T> {
    /// `returncode == 0`.
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

/// What the child reads as its stdin.
#[derive(Debug, Clone, Copy, Default)]
pub enum Stdin<'a> {
    /// /dev/null.
    #[default]
    Null,
    /// This process's own, as `subprocess.run` leaves it without `stdin=`.
    Inherit,
    /// These bytes, then EOF (`input=`).
    Input(&'a [u8]),
    /// A file of ours: a duplicate of its descriptor, read from where it is.
    File(&'a std::fs::File),
}

/// Options for [`run_with`] / [`run_bytes`]; `Default` is a plain capture
/// with no timeout and /dev/null as stdin.
#[derive(Debug, Clone, Default)]
pub struct RunOpts<'a> {
    /// Covers reading the output as well as the exit, as `communicate`'s does.
    pub timeout: Option<Duration>,
    pub stdin: Stdin<'a>,
    /// Extra environment variables on top of the inherited ones.
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<&'a Path>,
    /// The child's `argv[0]`, when it is not the program as given: a caller
    /// that searches PATH itself runs the full path under the name it was
    /// asked for.
    pub arg0: Option<&'a OsStr>,
    /// stderr to /dev/null instead of captured.
    pub discard_stderr: bool,
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=secs)`,
/// decoded lossily. Never panics: a missing binary or a timeout is reported
/// in the result, the way the Python callers catch OSError and
/// TimeoutExpired.
pub fn run(argv: &[&str], timeout_secs: u64) -> Output {
    run_with(argv, &RunOpts { timeout: Some(Duration::from_secs(timeout_secs)), ..Default::default() })
}

/// [`run`] with options.
pub fn run_with(argv: &[&str], opts: &RunOpts) -> Output {
    let out = run_bytes(argv, opts);
    Output {
        code: out.code,
        signal: out.signal,
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        timed_out: out.timed_out,
        failed_to_start: out.failed_to_start,
        start_errno: out.start_errno,
        start_error: out.start_error,
    }
}

/// `subprocess.run(argv, capture_output=True, timeout=...)` without
/// `text=True`: the raw bytes, for a caller that decodes them itself (as
/// strictly as Python does, or not at all). The arguments are OS strings,
/// so a path that is not UTF-8 reaches the child as its own bytes. The core
/// of [`run_with`]; never panics.
pub fn run_bytes<S: AsRef<OsStr>>(argv: &[S], opts: &RunOpts) -> Output<Vec<u8>> {
    let Some((prog, args)) = argv.split_first() else {
        return Output { failed_to_start: true, ..Default::default() };
    };
    let not_started = |e: std::io::Error| Output {
        failed_to_start: true,
        start_errno: e.raw_os_error(),
        start_error: e.to_string(),
        ..Default::default()
    };
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(if opts.discard_stderr { Stdio::null() } else { Stdio::piped() });
    match opts.stdin {
        Stdin::Null => cmd.stdin(Stdio::null()),
        Stdin::Inherit => cmd.stdin(Stdio::inherit()),
        Stdin::Input(_) => cmd.stdin(Stdio::piped()),
        Stdin::File(f) => match f.try_clone() {
            Ok(f) => cmd.stdin(f),
            Err(e) => return not_started(e),
        },
    };
    if let Some(name) = opts.arg0 {
        use std::os::unix::process::CommandExt;
        cmd.arg0(name);
    }
    for (k, v) in &opts.env {
        cmd.env(k, v);
    }
    if let Some(dir) = opts.cwd {
        cmd.current_dir(dir);
    }
    let mut child = match spawn_group(&mut cmd) {
        Ok(c) => c,
        Err(e) => return not_started(e),
    };
    if let (Stdin::Input(bytes), Some(mut stdin)) = (opts.stdin, child.stdin.take()) {
        let bytes = bytes.to_owned();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    // Drain both pipes on their own threads, or a chatty child fills one and
    // blocks forever while we wait on it.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut s);
            }
            let _ = tx.send(s);
        });
        rx
    };
    let out_rx = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_rx = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let deadline = opts.timeout.map(|t| Instant::now() + t);
    let mut timed_out = false;
    let status = match opts.timeout {
        Some(t) if !exits_within(&child, t) => {
            kill_group(&mut child);
            timed_out = true;
            None
        }
        _ => child.wait().ok(),
    };
    // The deadline covers the output too: something the child left running
    // that still holds a pipe is a timeout, and its group goes with it.
    // Past the deadline the readers get a moment to finish after the kill,
    // then are left behind (a descendant that escaped the group, by setsid,
    // may hold the pipe for as long as it lives).
    let mut recv = |rx: &std::sync::mpsc::Receiver<Vec<u8>>| -> Vec<u8> {
        let left = match deadline {
            None => return rx.recv().unwrap_or_default(),
            Some(d) => d.saturating_duration_since(Instant::now()),
        };
        if let Ok(v) = rx.recv_timeout(left) {
            return v;
        }
        if !timed_out {
            kill_group(&mut child);
            timed_out = true;
        }
        rx.recv_timeout(Duration::from_secs(1)).unwrap_or_default()
    };
    let stdout = recv(&out_rx);
    let stderr = recv(&err_rx);
    let (code, signal) = match status {
        Some(s) if !timed_out => {
            use std::os::unix::process::ExitStatusExt;
            (s.code(), s.signal())
        }
        _ => (None, None),
    };
    Output { code, signal, stdout, stderr, timed_out, ..Default::default() }
}

/// Start `cmd` as the leader of a process group of its own, so that a
/// timeout can stop everything it started ([`kill_group`]), not only the
/// direct child: a grandchild left holding the output pipe would otherwise
/// keep the reader waiting, and itself running, after the child is gone.
pub fn spawn_group(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    // Out of the terminal's foreground group, a prompt for a password would
    // stop the child (SIGTTIN) until the timeout; git can fail at once.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.process_group(0).spawn()
}

/// Kill the whole process group of a child started with [`spawn_group`],
/// then reap the child. Safe on a child that has already exited: the group
/// lives on for as long as anything in it does, and is gone (ESRCH,
/// ignored) once nothing is.
pub fn kill_group(child: &mut std::process::Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Whether `child` exits within `timeout`, without polling: a thread blocks
/// in `waitid` and the caller waits on a channel with `recv_timeout`.
///
/// The thread waits with `WNOWAIT`, so it never reaps: the child stays the
/// caller's to kill and `wait()` for, and its pid cannot be reused while the
/// caller still holds it. Should `waitid` itself fail, the thread reports an
/// exit and the caller's blocking `wait()` decides.
fn exits_within(child: &std::process::Child, timeout: Duration) -> bool {
    use rustix::process::{waitid, Pid, WaitId, WaitIdOptions};
    let pid = Pid::from_child(child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Err(rustix::io::Errno::INTR) = waitid(WaitId::Pid(pid), WaitIdOptions::EXITED | WaitIdOptions::NOWAIT) {}
        let _ = tx.send(());
    });
    rx.recv_timeout(timeout).is_ok()
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

/// The first code point of every run of Unicode decimal digits (category
/// Nd, Unicode 16, as the regex crate's `\p{Nd}` and Python 3.14's
/// `unicodedata.decimal` have it). Each run is a whole 0-9 set, ten
/// consecutive code points from its zero (a Unicode stability guarantee).
const DECIMAL_ZEROS: [u32; 76] = [
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0xFF10, 0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450,
    0x114D0, 0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0, 0x11950, 0x11BF0, 0x11C50,
    0x11D50, 0x11DA0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1CCF0, 0x1D7CE,
    0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

/// The value of a Unicode decimal digit (`\d`, category Nd), as Python's
/// `int()` and `float()` read it (`unicodedata.decimal(c, None)`); None for
/// anything else.
pub fn decimal_value(c: char) -> Option<u32> {
    if c.is_ascii() {
        return c.to_digit(10);
    }
    let cp = c as u32;
    let zero = DECIMAL_ZEROS[DECIMAL_ZEROS.partition_point(|&z| z <= cp) - 1];
    (cp - zero < 10).then_some(cp - zero)
}

/// `float()` of the digits a `\d+` group matched, which may be any script's.
pub fn py_digits_value(digits: &str) -> Option<f64> {
    if digits.bytes().all(|b| b.is_ascii_digit()) {
        return digits.parse().ok();
    }
    let ascii: Option<String> = digits.chars().map(|c| char::from_digit(decimal_value(c)?, 10)).collect();
    ascii?.parse().ok()
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
    let name = std::thread::current().name().map(str::to_owned);
    with_stack_named(size, name.as_deref(), f)
}

/// [`with_stack`] on a thread named `name` (None: unnamed) whatever the
/// caller's, for work that stands in for a Python worker thread wherever it
/// is called from.
pub fn with_stack_named<R: Send>(size: usize, name: Option<&str>, f: impl FnOnce() -> R + Send) -> R {
    let mut job = Some(f);
    let out = std::thread::scope(|s| {
        let job = &mut job;
        let mut b = std::thread::Builder::new().stack_size(size);
        if let Some(n) = name {
            b = b.name(n.to_owned());
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
        // `{:e}` leaves the mantissa as Python does: "1" stays "1" (1e+16),
        // "1.5" stays "1.5" (1.5e+16).
        return format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    s
}

/// Argument helpers for `parity()` hooks: `arg_str(args, "name")`.
pub fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> &'a str {
    args.get(key).and_then(|v| v.as_str()).unwrap_or_default()
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

    /// Python's `repr()` of a serde value as `json.loads` gives it, without
    /// recursion: an `extra` value can be as deep as a cache file holds, and
    /// the details sheet and the actions repr one on the window's thread.
    /// `text` is the module's `repr(str)` (keys and strings), `scalar` its
    /// `str()` of a null, bool or number; lists and dicts are built here.
    pub fn repr_value(
        v: &serde_json::Value,
        text: impl Fn(&str) -> String,
        scalar: impl Fn(&serde_json::Value) -> String,
    ) -> String {
        use serde_json::Value;
        enum Job<'a> {
            Value(&'a Value),
            Key(&'a str),
            Lit(&'static str),
        }
        let mut out = String::new();
        let mut work = vec![Job::Value(v)];
        while let Some(job) = work.pop() {
            match job {
                Job::Lit(s) => out.push_str(s),
                Job::Key(k) => {
                    out.push_str(&text(k));
                    out.push_str(": ");
                }
                Job::Value(Value::String(s)) => out.push_str(&text(s)),
                Job::Value(Value::Array(a)) => {
                    out.push('[');
                    work.push(Job::Lit("]"));
                    for (i, x) in a.iter().enumerate().rev() {
                        work.push(Job::Value(x));
                        if i > 0 {
                            work.push(Job::Lit(", "));
                        }
                    }
                }
                Job::Value(Value::Object(m)) => {
                    out.push('{');
                    work.push(Job::Lit("}"));
                    for (i, (k, x)) in m.iter().enumerate().rev() {
                        work.push(Job::Value(x));
                        work.push(Job::Key(k));
                        if i > 0 {
                            work.push(Job::Lit(", "));
                        }
                    }
                }
                Job::Value(leaf) => out.push_str(&scalar(leaf)),
            }
        }
        out
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
        let slow = run_with(&["sleep", "5"], &RunOpts { timeout: Some(Duration::from_millis(200)), ..Default::default() });
        assert!(slow.timed_out);
        assert!(run(&["/nonexistent-binary"], 1).failed_to_start);
    }

    /// `run_bytes` keeps what `run` cannot: bytes that are not UTF-8, the
    /// errno of a failed start, the signal that ended the child; and its
    /// options reach the child.
    #[test]
    fn run_bytes_keeps_bytes_errno_and_signal() {
        let out = run_bytes(&["printf", "a\\377\\r\\n"], &RunOpts::default());
        assert!(out.ok());
        assert_eq!(out.stdout, b"a\xff\r\n");
        let out = run_bytes(&["sh", "-c", "kill -TERM $$"], &RunOpts::default());
        assert_eq!((out.code, out.signal, out.timed_out), (None, Some(15), false));
        let out = run_bytes(&["/nonexistent-binary"], &RunOpts::default());
        assert!(out.failed_to_start);
        assert_eq!(out.start_errno, Some(2));
        let dir = tempfile::tempdir().unwrap();
        let script = "printf '%s|%s|' \"$0\" \"$ALM_T\"; pwd; cat; echo gone >&2";
        let out = run_bytes(&["sh", "-c", script], &RunOpts {
            stdin: Stdin::Input(b"in"),
            env: vec![("ALM_T".into(), "v".into())],
            cwd: Some(dir.path()),
            arg0: Some(OsStr::new("named")),
            discard_stderr: true,
            ..Default::default()
        });
        let want = format!("named|v|{}\nin", dir.path().canonicalize().unwrap().display());
        assert_eq!(String::from_utf8_lossy(&out.stdout), want);
        assert!(out.stderr.is_empty());
        let mut f = tempfile::tempfile().unwrap();
        f.write_all(b"from a file").unwrap();
        std::io::Seek::rewind(&mut f).unwrap();
        let out = run_bytes(&["cat"], &RunOpts { stdin: Stdin::File(&f), ..Default::default() });
        assert_eq!(out.stdout, b"from a file");
    }

    /// A timeout stops what the child started too: a grandchild holding the
    /// pipe used to keep `run` (and itself) going long past the deadline.
    #[test]
    fn a_timeout_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("survived");
        let script = format!("(sleep 0.6; touch '{}') & sleep 30", mark.display());
        let opts = RunOpts { timeout: Some(Duration::from_millis(200)), ..Default::default() };
        let started = Instant::now();
        let out = run_with(&["sh", "-c", &script], &opts);
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_millis(600), "{:?}", started.elapsed());
        // well past the moment the grandchild would have touched the mark
        std::thread::sleep(Duration::from_millis(900));
        assert!(!mark.exists(), "the grandchild outlived the timeout");

        // The child exits at once; what it left behind holds stdout open.
        let started = Instant::now();
        let out = run_with(&["sh", "-c", "sleep 30 & echo hi"], &opts);
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
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
    fn decimal_digits_of_every_script() {
        // The table is exactly `\p{Nd}` (what the per-module copies it
        // replaced matched with a regex), and each run counts 0-9 from its
        // zero.
        let all: String = (0..=0x10ffffu32).filter_map(char::from_u32).collect();
        let nd: Vec<char> = regex::Regex::new(r"\p{Nd}").unwrap().find_iter(&all).filter_map(|m| m.as_str().chars().next()).collect();
        let table: Vec<char> = (0..=0x10ffffu32).filter_map(char::from_u32).filter(|&c| decimal_value(c).is_some()).collect();
        assert_eq!(nd, table);
        assert_eq!(nd.len(), 760);
        for (c, d) in [('7', 7), ('\u{663}', 3), ('\u{ff19}', 9), ('\u{1d7d9}', 1), ('\u{1e5f1}', 0), ('\u{1fbf9}', 9)] {
            assert_eq!(decimal_value(c), Some(d), "{c:?}");
        }
        for c in ['a', '/', ':', '\u{b2}', '\u{2167}', '\u{1d7cd}'] {
            assert_eq!(decimal_value(c), None, "{c:?}");
        }
        assert_eq!(py_digits_value("2024"), Some(2024.0));
        assert_eq!(py_digits_value("\u{661}\u{662}3"), Some(123.0));
        assert_eq!(py_digits_value("1a"), None);
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
