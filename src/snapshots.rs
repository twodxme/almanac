//! Port of `snapshots.py`.
//!
//! Whether the last update can be undone, and undoing it.
//!
//! A system that snapshots before every upgrade can roll any of them back -- but
//! only if the snapshot was actually taken. Full upgrades through a wrapper like
//! `omarchy update` or a snap-pac hook take one; a one-off `yay -S` usually does
//! not, and nothing tells you which kind your last update was until the day you
//! need to undo it.
//!
//! Snapshot listings are root-only, so this never reads them. Whether a snapshot
//! preceded a transaction is read from what any user can see:
//!
//! * pacman.log, where snap-pac and similar hooks announce themselves inside the
//!   transaction they protect, and
//! * the journal, where `sudo`/`pkexec` record a `snapper ... create` command a
//!   minute before an update that ran it by hand.
//!
//! Rolling back is not something a running system can do to itself. Snapshots
//! are booted from the boot menu; once booted into one, it is made permanent.
//! That second step is the one-click part, and it is offered only where the tool
//! for it exists (limine-snapper-restore), otherwise the steps are spelled out.
//!
//! Everything here is optional. Without Snapper there is nothing to report
//! beyond saying so.

use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::util;

/// `Path("/var/log/pacman.log")`.
pub const PACMAN_LOG: &str = "/var/log/pacman.log";
/// How long before a transaction a manually created snapshot still counts as
/// "taken for it". omarchy update snapshots, then syncs and downloads.
pub const LEAD: i64 = 30 * 60;
static _LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(?P<ts>[^\]]+)\] \[(?P<who>[^\]]+)\] (?P<msg>.*)$").unwrap());
// Python's re.I also equates 'i' with U+0130 and U+0131; Rust's (?i) uses
// simple case folding, which does not, so they are spelled out.
static _SNAPPER_HOOK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)snap-pac|snapper|t[i\x{130}\x{131}]meshift|btrfs-autosnap").unwrap());
// `\S` is spelled out: Python's whitespace (str.isspace) includes \x1c-\x1f.
static _CMDLINE_SNAPSHOT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"subvol=[^\s\x1c-\x1f]*?/(\d+)/snapshot").unwrap());
static _CHANGE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(upgraded|installed|removed|downgraded) ").unwrap());
// Python's `COMMAND=\S*snapper\b.*\bcreate\b`, where `\w` is
// [\p{L}\p{N}_] (str.isalnum() or '_') and `\s` includes \x1c-\x1f. The
// regex crate has no settable `\b`, so the boundaries are spelled out: after
// "snapper" comes a non-word character (create cannot follow straight on),
// and the character before "create" is one too, never the newline `.*` stops at.
static _CREATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"COMMAND=[^\s\x1c-\x1f]*snapper[^\p{L}\p{N}_\n](?:.*[^\p{L}\p{N}_\n])?create(?:[^\p{L}\p{N}_]|\z)",
    )
    .unwrap()
});

/// The log function a task step is handed: one line at a time.
pub type Log<'a> = &'a dyn Fn(&str);

/// A task step done in this process rather than a command (Python: a callable
/// taking the log function, which raises to fail). `create_step`'s step
/// fails only where Python's raises: output that is not UTF-8, or a
/// description with a NUL byte (ValueError).
pub type StepFn = Box<dyn Fn(Log<'_>) -> Result<(), String> + Send + Sync>;

/// How a step runs its command: `subprocess.run(argv, capture_output=True,
/// text=True, timeout=120)`, `Err(str(OSError))` when it cannot be started.
/// Stood in for by the tests.
type Runner = Arc<dyn Fn(&[String]) -> Result<RawOutput, RunError> + Send + Sync>;

/// Why a [`Runner`] got no output: what `subprocess.run` raised.
#[derive(Debug, Clone, PartialEq)]
enum RunError {
    /// `str(OSError)`: the command could not be started. The step catches it.
    OsError(String),
    /// `str(ValueError)`: an argument holds a NUL byte. Nothing catches it.
    Raise(String),
}

fn _run(cmd: &[&str], timeout: u64) -> Result<String, String> {
    // ValueError (a NUL byte in an argument) is not caught here.
    let r = run_raw(cmd, timeout)?;
    if r.failed_to_start || r.timed_out {
        return Ok(String::new());
    }
    // text=True decodes strictly; a UnicodeDecodeError is not caught here.
    let (stdout, _stderr) = (py_text(&r.stdout)?, py_text(&r.stderr)?);
    Ok(if r.ok() { stdout } else { String::new() })
}

/// Configured snapper configs. Listing them needs no root.
pub fn snapper_configs() -> Result<Vec<String>, String> {
    snapper_configs_with(util::which("snapper").is_some(), &|cmd| _run(cmd, 20))
}

fn snapper_configs_with(
    has_snapper: bool,
    run: &dyn Fn(&[&str]) -> Result<String, String>,
) -> Result<Vec<String>, String> {
    if !has_snapper {
        return Ok(Vec::new());
    }
    let out = run(&["snapper", "--csvout", "list-configs"])?;
    let rows: Vec<String> = py_splitlines(&out)
        .into_iter()
        .skip(1)
        .filter(|l| !py_strip(l).is_empty())
        .map(|l| l.split(',').next().unwrap_or_default().to_string())
        .collect();
    Ok(rows.into_iter().filter(|r| !r.is_empty()).collect())
}

/// The snapshot number this system was booted from, or ''.
///
/// `cmdline` None reads `/proc/cmdline`; Err where Python raises (a file
/// that is not UTF-8: read_text only catches OSError).
pub fn booted_snapshot(cmdline: Option<&str>) -> Result<String, String> {
    let read;
    let cmdline = match cmdline {
        Some(c) => c,
        None => match std::fs::read("/proc/cmdline") {
            Ok(bytes) => {
                read = py_utf8_decode(&bytes)?;
                &read
            }
            Err(_) => return Ok(String::new()),
        },
    };
    Ok(match _CMDLINE_SNAPSHOT.captures(cmdline) {
        Some(m) => m[1].to_string(),
        None => String::new(),
    })
}

/// A pacman.log timestamp as epoch seconds; 0.0 when it cannot be parsed.
/// (`datetime.fromisoformat`, falling back to `"%Y-%m-%d %H:%M"`.)
/// Also used by `history::package_events`.
pub fn _ts(text: &str) -> f64 {
    if let Some(t) = fromisoformat_ts(text) {
        return t;
    }
    if let Some(t) = strptime_ts(text) {
        return t;
    }
    0.0
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transaction {
    pub started: f64,
    /// "name old -> new"
    pub changes: Vec<String>,
    /// a snapshot hook ran in it
    pub hooks_snapshot: bool,
}

/// The most recent pacman transaction that changed something.
///
/// `log_text` None reads the tail of `PACMAN_LOG`; None when it cannot be read.
pub fn last_transaction(log_text: Option<&str>) -> Option<Transaction> {
    let read;
    let log_text = match log_text {
        Some(t) => t,
        None => {
            // The tail is enough; the log grows for the life of the install.
            let tail = || -> std::io::Result<String> {
                let mut fh = std::fs::File::open(PACMAN_LOG)?;
                let end = fh.seek(SeekFrom::End(0))?;
                fh.seek(SeekFrom::Start(end.saturating_sub(512 * 1024)))?;
                let mut buf = Vec::new();
                fh.read_to_end(&mut buf)?;
                Ok(String::from_utf8_lossy(&buf).into_owned())
            };
            match tail() {
                Ok(t) => {
                    read = t;
                    &read
                }
                Err(_) => return None,
            }
        }
    };
    let mut current: Option<Transaction> = None;
    let mut last: Option<Transaction> = None;
    for line in py_splitlines(log_text) {
        let Some(m) = _LINE.captures(line) else {
            continue;
        };
        let msg = &m["msg"];
        if msg == "transaction started" {
            current = Some(Transaction { started: _ts(&m["ts"]), ..Default::default() });
            continue;
        }
        let Some(cur) = current.as_mut() else {
            continue;
        };
        if &m["who"] == "ALPM" && _CHANGE.is_match(msg) {
            cur.changes.push(msg.to_string());
        } else if msg.starts_with("running '") && _SNAPPER_HOOK.is_match(msg) {
            cur.hooks_snapshot = true;
        } else if msg == "transaction completed" {
            if !cur.changes.is_empty() {
                last = current.take();
            }
            current = None;
        }
    }
    // A hook that runs before the transaction ("pre" hooks) logs inside it,
    // so the loop above sees it. One that logs just before "transaction
    // started" is attributed by the journal check instead.
    last
}

/// Did someone run `snapper create` in the run-up to `started`?
///
/// None when the journal cannot be read, which is different from "no".
/// Err where Python raises: a `started` time.localtime cannot convert, or
/// journal output that is not UTF-8.
pub fn snapshot_before(started: f64, journal_text: Option<&str>) -> Result<Option<bool>, String> {
    snapshot_between(started - LEAD as f64, started + 60.0, journal_text)
}

/// Did someone run `snapper create` between `since` and `until` (epoch
/// seconds)? None when the journal cannot be read; Err as `snapshot_before`.
pub fn snapshot_between(since: f64, until: f64, journal_text: Option<&str>) -> Result<Option<bool>, String> {
    let read;
    let journal_text = match journal_text {
        Some(t) => t,
        None => {
            let since = strftime_local("%Y-%m-%d %H:%M:%S", since)?;
            let until = strftime_local("%Y-%m-%d %H:%M:%S", until)?;
            let r = run_raw(
                &[
                    "journalctl", "--no-pager", "-q", "-o", "short-iso",
                    "--since", &since, "--until", &until,
                    "_COMM=sudo", "+", "_COMM=pkexec",
                ],
                20,
            )?;
            if r.failed_to_start || r.timed_out {
                return Ok(None);
            }
            let (stdout, _stderr) = (py_text(&r.stdout)?, py_text(&r.stderr)?);
            if !r.ok() {
                return Ok(None);
            }
            read = stdout;
            &read
        }
    };
    Ok(Some(_CREATE.is_match(journal_text)))
}

/// `create_step`, with Python's raise as Err: callers that can stop the
/// action, as Python's `_snapshot_first` did, should use this one.
pub fn try_create_step(description: &str) -> Result<Option<(String, StepFn)>, String> {
    let runner: Runner = Arc::new(|argv: &[String]| {
        let a: Vec<&str> = argv.iter().map(String::as_str).collect();
        let r = run_raw(&a, 120).map_err(RunError::Raise)?;
        if r.failed_to_start {
            return Err(RunError::OsError(exec_error(argv)));
        }
        Ok(r)
    });
    Ok(create_step_with(description, snapper_configs()?, runner))
}

fn create_step_with(description: &str, configs: Vec<String>, runner: Runner) -> Option<(String, StepFn)> {
    if configs.is_empty() {
        return None;
    }
    // One config is the overwhelmingly common case; with several, snapshot
    // the root one, which is what an update changes.
    let config = if configs.iter().any(|c| c == "root") { "root".to_string() } else { configs[0].clone() };
    let desc: String = description.chars().take(60).collect();
    let cmd: Vec<String> = ["snapper", "-c", &config, "create", "-c", "number", "-d", &desc]
        .iter()
        .map(|s| s.to_string())
        .collect();

    let step = move |log: Log<'_>| -> Result<(), String> {
        let argv: Vec<String> = if geteuid() == 0 {
            cmd.clone()
        } else {
            [crate::tasks::PKEXEC.to_string(), "/usr/bin/snapper".to_string()]
                .into_iter()
                .chain(cmd[1..].iter().cloned())
                .collect()
        };
        let r = match runner(&argv) {
            Ok(r) => r,
            Err(RunError::OsError(exc)) => {
                log(&format!("no snapshot was taken ({exc}); continuing without one"));
                return Ok(());
            }
            // ValueError is neither OSError nor SubprocessError: it fails the task.
            Err(RunError::Raise(exc)) => return Err(exc),
        };
        if r.timed_out {
            // str(subprocess.TimeoutExpired)
            let exc = format!("Command '{}' timed out after 120 seconds", py_list_repr(&argv));
            log(&format!("no snapshot was taken ({exc}); continuing without one"));
            return Ok(());
        }
        // text=True decodes strictly; UnicodeDecodeError is not caught.
        let (stdout, stderr) = (py_text(&r.stdout)?, py_text(&r.stderr)?);
        if r.ok() {
            log(&format!("snapshot taken in the '{config}' config"));
        } else {
            let text = if stderr.is_empty() { &stdout } else { &stderr };
            let why = py_splitlines(py_strip(text));
            let last = why.last().copied().unwrap_or("cancelled");
            log(&format!("no snapshot was taken ({last}); continuing without one"));
        }
        Ok(())
    };

    Some(("Taking a snapshot first".to_string(), Box::new(step)))
}

/// The Health check row for update snapshots.
///
/// Where Python's raises (see `finding_with`), this panics with the
/// message: `maintenance::run_all` catches a panicking check the way the
/// Python catches an exception, and reports "Check failed: ...".
pub fn finding() -> crate::maintenance::Finding {
    let found = finding_with(
        &|| booted_snapshot(None),
        &|| util::which("limine-snapper-restore").is_some(),
        &snapper_configs,
        &|| last_transaction(None),
        &|started| snapshot_before(started, None),
    );
    match found {
        Ok(f) => f,
        Err(exc) => std::panic::panic_any(exc),
    }
}

fn finding_with(
    booted: &dyn Fn() -> Result<String, String>,
    has_restore: &dyn Fn() -> bool,
    configs: &dyn Fn() -> Result<Vec<String>, String>,
    last: &dyn Fn() -> Option<Transaction>,
    before: &dyn Fn(f64) -> Result<Option<bool>, String>,
) -> Result<crate::maintenance::Finding, String> {
    use crate::maintenance as m;

    let number = booted()?;
    if !number.is_empty() {
        let restore = has_restore();
        return Ok(m::Finding {
            id: "snapshot".into(),
            title: "Running from a snapshot".into(),
            detail: format!(
                "This session was started from snapshot #{number}. \
                 Changes made now are lost at the next normal restart \
                 unless the snapshot is made permanent."
            ),
            severity: m::CRIT.into(),
            fix_label: if restore { "Keep this snapshot".into() } else { String::new() },
            fix: if restore { Some(m::Fix::argv(&["limine-snapper-restore"])) } else { None },
            fix_note: if restore {
                "Makes this snapshot the running system, then asks you to restart.".into()
            } else {
                format!("Make it permanent with `sudo snapper rollback {number}`, then restart.")
            },
            ..Default::default()
        });
    }

    let configs = configs()?;
    if configs.is_empty() {
        return Ok(m::Finding {
            id: "snapshot".into(),
            title: "Update snapshots".into(),
            detail: "No snapshot tool is set up, so an update cannot be \
                     undone from the boot menu. Snapper on a btrfs root \
                     provides that."
                .into(),
            severity: m::INFO.into(),
            ..Default::default()
        });
    }

    let Some(tx) = last() else {
        return Ok(m::Finding {
            id: "snapshot".into(),
            title: "Update snapshots".into(),
            detail: "Snapper is set up. No updates are logged yet.".into(),
            severity: m::OK.into(),
            ..Default::default()
        });
    };

    // time.localtime raises OverflowError/OSError for a time it cannot convert.
    let when = strftime_local("%a %d %b %H:%M", tx.started)?;
    let names: Vec<&str> = tx
        .changes
        .iter()
        .filter_map(|c| {
            let parts = py_split(c);
            if parts.len() > 1 { Some(parts[1]) } else { None }
        })
        .collect();
    let mut what = names.iter().take(4).copied().collect::<Vec<_>>().join(", ");
    if names.len() > 4 {
        what.push_str(&format!(" and {} more", names.len() - 4));
    }
    let items: Vec<String> = tx.changes.iter().take(40).cloned().collect();
    let covered = if tx.hooks_snapshot { Some(true) } else { before(tx.started)? };
    if covered == Some(true) {
        let how = "restart, choose the snapshot from the boot menu, then make it \
                   permanent here";
        return Ok(m::Finding {
            id: "snapshot".into(),
            title: "Update snapshots".into(),
            detail: format!(
                "The last update ({when}: {what}) can be undone — a \
                 snapshot was taken just before it. To undo it, {how}."
            ),
            severity: m::OK.into(),
            items,
            ..Default::default()
        });
    }
    if covered.is_none() {
        return Ok(m::Finding {
            id: "snapshot".into(),
            title: "Update snapshots".into(),
            detail: format!(
                "Snapper is set up. Whether the last update ({when}) was \
                 snapshotted could not be read from the system journal."
            ),
            severity: m::INFO.into(),
            items,
            ..Default::default()
        });
    }
    Ok(m::Finding {
        id: "snapshot".into(),
        title: "Update snapshots".into(),
        detail: format!(
            "The last update ({when}: {what}) ran without a snapshot, so \
             it cannot be rolled back. Updates started from Almanac now \
             take one first."
        ),
        severity: m::WARN.into(),
        items,
        ..Default::default()
    })
}

// --- Python standard library behaviour the code above relies on ---------

/// `os.geteuid()`, read from `/proc/self/status` (no `unsafe`). Not root when
/// it cannot be read.
fn geteuid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|rest| rest.split_whitespace().nth(1).and_then(|e| e.parse().ok()))
        })
        .unwrap_or(u32::MAX)
}

/// `str(OSError)` for an argv `subprocess.run` could not start, the way
/// `_posixsubprocess` reports it: the first exec error other than
/// ENOENT/ENOTDIR along PATH, else the last one, with `argv[0]` as filename.
/// `util::run` only says that it failed, so the cause is read back from the
/// file system the way execve would find it.
fn exec_error(argv: &[String]) -> String {
    use std::os::unix::fs::PermissionsExt;
    const ENOENT: i32 = 2;
    const ENOEXEC: i32 = 8;
    const ENOTDIR: i32 = 20;
    const EACCES: i32 = 13;
    let Some(exe) = argv.first() else {
        return "[Errno 2] No such file or directory".to_string();
    };
    // None: execve would have run it, as far as the file system shows.
    let probe = |path: &std::path::Path| -> Option<i32> {
        let meta = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(e) => return Some(e.raw_os_error().unwrap_or(ENOENT)),
        };
        if meta.is_dir() || meta.permissions().mode() & 0o111 == 0 {
            return Some(EACCES);
        }
        if let Err(e) = rustix::fs::access(path, rustix::fs::Access::EXEC_OK) {
            return Some(e.raw_os_error());
        }
        let mut head = [0u8; 256];
        let n = match std::fs::File::open(path).and_then(|mut f| f.read(&mut head)) {
            Ok(n) => n,
            Err(_) => return None,
        };
        let head = &head[..n];
        if head.starts_with(b"\x7fELF") {
            return None;
        }
        if let Some(rest) = head.strip_prefix(b"#!") {
            // A script whose interpreter is missing fails with ENOENT.
            let rest: Vec<u8> = rest.iter().copied().skip_while(|b| *b == b' ' || *b == b'\t').collect();
            let interp: Vec<u8> = rest.into_iter().take_while(|b| !b" \t\n\r".contains(b)).collect();
            let interp = std::path::Path::new(<std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&interp));
            return if interp.exists() { None } else { Some(ENOENT) };
        }
        Some(ENOEXEC)
    };
    let candidates: Vec<std::path::PathBuf> = if exe.contains('/') {
        vec![exe.into()]
    } else {
        // os.get_exec_path(): $PATH, or os.defpath without one.
        let path = std::env::var_os("PATH").map(|p| p.to_string_lossy().into_owned());
        path.as_deref()
            .unwrap_or("/bin:/usr/bin")
            .split(':')
            .map(|d| std::path::Path::new(d).join(exe))
            .collect()
    };
    let mut saved = None;
    let mut last = None;
    for c in &candidates {
        let errno = probe(c);
        last = errno;
        if let Some(e) = errno {
            if e != ENOENT && e != ENOTDIR && saved.is_none() {
                saved = Some(e);
            }
        }
    }
    let errno = saved.or(last).unwrap_or(ENOENT);
    let text = std::io::Error::from_raw_os_error(errno).to_string();
    let strerror = text.strip_suffix(&format!(" (os error {errno})")).unwrap_or(&text).to_string();
    format!("[Errno {errno}] {strerror}: {}", py_repr(exe))
}

/// What `subprocess.run(..., capture_output=True, timeout=n)` leaves before
/// `text=True` decodes it: the output as bytes. `util::run` decodes with
/// replacement, after which a real U+FFFD in the output (valid UTF-8, which
/// Python reads fine) cannot be told from bytes Python's decoder raises on,
/// so the streams are read here as bytes.
type RawOutput = crate::util::Output<Vec<u8>>;

/// `subprocess.run(argv, capture_output=True, timeout=secs)`, as
/// `util::run` does it but keeping the bytes. Err(str(ValueError)) where
/// Python raises before starting anything: an argument with a NUL byte.
fn run_raw(argv: &[&str], timeout_secs: u64) -> Result<RawOutput, String> {
    if argv.iter().any(|a| a.contains('\0')) {
        return Err("embedded null byte".to_string());
    }
    Ok(crate::util::run_bytes(argv, &crate::util::RunOpts {
        timeout: Some(std::time::Duration::from_secs(timeout_secs)),
        ..Default::default()
    }))
}

/// What `subprocess.run(..., text=True)` makes of a stream: decoded
/// strictly as UTF-8 (str(UnicodeDecodeError) where that fails), with
/// universal newlines.
fn py_text(b: &[u8]) -> Result<String, String> {
    Ok(py_utf8_decode(b)?.replace("\r\n", "\n").replace('\r', "\n"))
}

/// `bytes.decode("utf-8")`, with str(UnicodeDecodeError) as the error.
fn py_utf8_decode(b: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(b) {
        Ok(s) => Ok(s.to_string()),
        Err(e) => {
            let start = e.valid_up_to();
            let (end, reason) = match e.error_len() {
                Some(n) => {
                    let lead = b[start];
                    let invalid_start = (0x80..=0xc1).contains(&lead) || lead >= 0xf5;
                    (start + n, if n == 1 && invalid_start { "invalid start byte" } else { "invalid continuation byte" })
                }
                None => (b.len(), "unexpected end of data"),
            };
            Err(if end == start + 1 {
                format!("'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}", b[start])
            } else {
                format!("'utf-8' codec can't decode bytes in position {start}-{}: {reason}", end - 1)
            })
        }
    }
}

/// `struct tm` from `localtime_r`, with the year in full and the weekday
/// 0 = Sunday.
struct Tm {
    year: i64,
    mon: u32,
    mday: u32,
    hour: u32,
    min: u32,
    sec: u32,
    wday: u32,
}

const EOVERFLOW: &str = "[Errno 75] Value too large for defined data type";

// --- glibc's localtime_r (time/tzset.c, time/tzfile.c), which
// time.localtime runs. chrono's Local reads the zone differently where it
// matters here: an invalid TZ (glibc: UTC; chrono: /etc/localtime), DST
// rules from a TZ string before 1970 (glibc: none), leap seconds in a
// "right/" zone (glibc subtracts them), so glibc's reading is followed. ---

/// How a TZ string rule names its day: `Jn`, `n` or `Mm.n.d`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum RuleKind {
    #[default]
    J0,
    J1,
    M,
}

/// glibc's `tz_rule`: one half of a TZ string (0 = standard time and the
/// start of DST, 1 = DST and its end). Zeroed, it is unnamed UTC.
#[derive(Debug, Clone, Copy, Default)]
struct TzRule {
    kind: RuleKind,
    m: u16,
    n: u16,
    d: u16,
    /// Time of day of the change, local time.
    secs: i64,
    /// Seconds east of UTC.
    offset: i64,
}

/// A TZif local time type.
#[derive(Debug, Clone, Copy, Default)]
struct TType {
    offset: i64,
    isdst: bool,
    isstd: bool,
    isgmt: bool,
}

/// A TZif file as glibc keeps it.
#[derive(Debug, Clone, Default)]
struct TzFile {
    transitions: Vec<i64>,
    type_idxs: Vec<usize>,
    types: Vec<TType>,
    /// (transition, total correction) per leap second.
    leaps: Vec<(i64, i64)>,
    /// The footer's TZ string, parsed: the rule past the last transition.
    tzspec: Option<[TzRule; 2]>,
    rule_stdoff: i64,
    rule_dstoff: i64,
}

#[derive(Debug, Clone)]
enum Zone {
    File(TzFile),
    Rules([TzRule; 2]),
}

/// The zone glibc's tzset reads from TZ (and TZDIR), kept while they stay
/// the same.
fn zone() -> Arc<Zone> {
    use std::os::unix::ffi::OsStringExt;
    type Key = (Option<std::ffi::OsString>, Option<std::ffi::OsString>);
    static CACHE: std::sync::Mutex<Option<(Key, Arc<Zone>)>> = std::sync::Mutex::new(None);
    let key: Key = (std::env::var_os("TZ"), std::env::var_os("TZDIR"));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, z)) = cache.as_ref() {
        if *k == key {
            return z.clone();
        }
    }
    // tzset_internal: an empty TZ means "Universal", a leading ':' is
    // dropped, no TZ at all means /etc/localtime.
    let tz: Vec<u8> = match key.0.clone() {
        None => b"/etc/localtime".to_vec(),
        Some(v) => {
            let v = v.into_vec();
            let v = if v.is_empty() { b"Universal".to_vec() } else { v };
            match v.strip_prefix(b":") {
                Some(rest) => rest.to_vec(),
                None => v,
            }
        }
    };
    let z = Arc::new(match tzfile_read(&tz) {
        Some(f) => Zone::File(f),
        None if tz.is_empty() || tz == b"/etc/localtime" => Zone::Rules([TzRule::default(); 2]),
        None => parse_tz(&tz, true),
    });
    *cache = Some((key, z.clone()));
    z
}

/// `__tzfile_read`: a TZif file, by path or by name under TZDIR.
fn tzfile_read(file: &[u8]) -> Option<TzFile> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    if file.is_empty() {
        return None;
    }
    let path: Vec<u8> = if file[0] == b'/' {
        file.to_vec()
    } else {
        let dir = std::env::var_os("TZDIR")
            .map(|d| d.into_vec())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| b"/usr/share/zoneinfo".to_vec());
        [dir.as_slice(), b"/", file].concat()
    };
    let data = std::fs::read(std::ffi::OsStr::from_bytes(&path)).ok()?;
    parse_tzif(&data)
}

/// The body of `__tzfile_read`: None where glibc gives up on the file.
fn parse_tzif(data: &[u8]) -> Option<TzFile> {
    let mut pos = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let end = pos.checked_add(n)?;
        let out = data.get(pos..end)?;
        pos = end;
        Some(out)
    };
    let be32 = |b: &[u8]| i64::from(i32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let be64 = |b: &[u8]| i64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
    // magic, version, 15 reserved, then isut, isstd, leap, time, type, char counts.
    let header = |h: &[u8]| -> Option<(u8, [usize; 6])> {
        if &h[..4] != b"TZif" {
            return None;
        }
        let mut c = [0usize; 6];
        for (i, v) in c.iter_mut().enumerate() {
            let o = 20 + 4 * i;
            *v = u32::from_be_bytes([h[o], h[o + 1], h[o + 2], h[o + 3]]) as usize;
        }
        Some((h[4], c))
    };
    let (version, mut counts) = header(take(44)?)?;
    let mut width = 4usize;
    if counts[1] > counts[4] || counts[0] > counts[4] {
        return None;
    }
    if version != 0 {
        // The 64-bit data follows the 32-bit data and a second header.
        let [isgmt, isstd, leaps, times, types, chars] = counts;
        take(times * 5 + types * 6 + chars + leaps * 8 + isstd + isgmt)?;
        counts = header(take(44)?)?.1;
        width = 8;
        if counts[1] > counts[4] || counts[0] > counts[4] {
            return None;
        }
    }
    let [num_isgmt, num_isstd, num_leaps, num_transitions, num_types, chars] = counts;
    if num_types == 0 {
        return None;
    }
    let decode = |b: &[u8]| if width == 8 { be64(b) } else { be32(b) };
    let transitions: Vec<i64> = take(num_transitions * width)?.chunks(width).map(decode).collect();
    let type_idxs: Vec<usize> = take(num_transitions)?.iter().map(|&b| usize::from(b)).collect();
    if type_idxs.iter().any(|&i| i >= num_types) {
        return None;
    }
    let mut types = Vec::with_capacity(num_types);
    for _ in 0..num_types {
        let t = take(6)?;
        if t[4] > 1 || usize::from(t[5]) > chars {
            return None;
        }
        types.push(TType { offset: be32(&t[..4]), isdst: t[4] == 1, isstd: false, isgmt: false });
    }
    take(chars)?;
    let mut leaps = Vec::with_capacity(num_leaps);
    for _ in 0..num_leaps {
        let l = take(width + 4)?;
        leaps.push((decode(&l[..width]), be32(&l[width..])));
    }
    for t in types.iter_mut().take(num_isstd).collect::<Vec<_>>() {
        t.isstd = take(1)?[0] != 0;
    }
    for t in types.iter_mut().take(num_isgmt).collect::<Vec<_>>() {
        t.isgmt = take(1)?[0] != 0;
    }
    let rest = &data[pos..];
    let mut tzspec = None;
    if width == 8 {
        // A newline, the TZ string, a newline (which glibc overwrites).
        if rest.is_empty() {
            return None;
        }
        if rest.len() > 1 && rest[0] == b'\n' {
            let spec = &rest[1..rest.len() - 1];
            let spec = spec.split(|&b| b == 0).next().unwrap_or_default();
            if !spec.is_empty() {
                // A footer always carries its rule; posixrules is not
                // consulted for it.
                if let Zone::Rules(r) = parse_tz(spec, false) {
                    tzspec = Some(r);
                }
            }
        }
    }
    let mut f = TzFile { transitions, type_idxs, types, leaps, tzspec, rule_stdoff: 0, rule_dstoff: 0 };
    // The standard and daylight offsets of the latest transitions of each flavour.
    if f.transitions.is_empty() {
        f.rule_stdoff = f.types[0].offset;
        f.rule_dstoff = f.types[0].offset;
    } else {
        let (mut std_set, mut dst_set) = (false, false);
        for &ti in f.type_idxs.iter().rev() {
            let t = f.types[ti];
            if !std_set && !t.isdst {
                std_set = true;
                f.rule_stdoff = t.offset;
            } else if !dst_set && t.isdst {
                dst_set = true;
                f.rule_dstoff = t.offset;
            }
            if std_set && dst_set {
                break;
            }
        }
        if !dst_set {
            f.rule_dstoff = f.rule_stdoff;
        }
    }
    Some(f)
}

/// `__tzfile_default`: a TZ string with a DST name but no rule takes its
/// transitions from the "posixrules" file, moved to the given offsets.
fn tzfile_default(stdoff: i64, dstoff: i64) -> Option<TzFile> {
    let mut f = tzfile_read(b"posixrules")?;
    if f.types.len() < 2 {
        return None;
    }
    let mut isdst = false;
    for i in 0..f.transitions.len() {
        let t = f.types[f.type_idxs[i]];
        f.type_idxs[i] = usize::from(t.isdst);
        if t.isgmt {
            // In UTC: nothing to correct.
        } else if isdst && !t.isstd {
            f.transitions[i] += dstoff - f.rule_dstoff;
        } else {
            f.transitions[i] += stdoff - f.rule_stdoff;
        }
        isdst = t.isdst;
    }
    f.rule_stdoff = stdoff;
    f.rule_dstoff = dstoff;
    f.types.truncate(2);
    f.types[0].offset = stdoff;
    f.types[0].isdst = false;
    f.types[1].offset = dstoff;
    f.types[1].isdst = true;
    Some(f)
}

/// The byte at `i`, NUL past the end (a C string).
fn c_at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

/// sscanf's `%hu` at `i`: blanks, a sign, digits, read as strtoul does and
/// cut to 16 bits. The value and where it ends, None where nothing matched.
fn scan_hu(s: &[u8], mut i: usize) -> Option<(u16, usize)> {
    while matches!(c_at(s, i), b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
        i += 1;
    }
    let neg = c_at(s, i) == b'-';
    if matches!(c_at(s, i), b'-' | b'+') {
        i += 1;
    }
    let start = i;
    let mut v: u64 = 0;
    let mut overflow = false;
    while c_at(s, i).is_ascii_digit() {
        match v.checked_mul(10).and_then(|x| x.checked_add(u64::from(c_at(s, i) - b'0'))) {
            Some(x) => v = x,
            None => overflow = true,
        }
        i += 1;
    }
    if i == start {
        return None;
    }
    let v = if overflow {
        u64::MAX
    } else if neg {
        v.wrapping_neg()
    } else {
        v
    };
    Some((v as u16, i))
}

/// sscanf(s, "%hu%n:%hu%n:%hu%n"): the fields read, and where the last ends.
fn scan_hms(s: &[u8], start: usize) -> (Vec<u16>, usize) {
    let mut vals = Vec::new();
    let mut end = start;
    let mut i = start;
    for k in 0..3 {
        if k > 0 {
            if c_at(s, i) != b':' {
                break;
            }
            i += 1;
        }
        match scan_hu(s, i) {
            Some((v, e)) => {
                vals.push(v);
                i = e;
                end = e;
            }
            None => break,
        }
    }
    (vals, end)
}

/// `parse_tzname`: three or more letters, or `<...>`.
fn parse_tzname(s: &[u8], p: &mut usize) -> bool {
    let mut i = *p;
    while c_at(s, i).is_ascii_alphabetic() {
        i += 1;
    }
    if i - *p < 3 {
        i = *p;
        if c_at(s, i) != b'<' {
            return false;
        }
        i += 1;
        let start = i;
        while c_at(s, i).is_ascii_alphanumeric() || matches!(c_at(s, i), b'+' | b'-') {
            i += 1;
        }
        let len = i - start;
        if c_at(s, i) != b'>' || len < 3 {
            return false;
        }
        i += 1;
    }
    *p = i;
    true
}

/// `parse_offset`: `[+-]hh[:mm[:ss]]`, west positive, each part capped.
fn parse_offset(s: &[u8], p: &mut usize, rules: &mut [TzRule; 2], which: usize) -> bool {
    let mut i = *p;
    let c = c_at(s, i);
    if which == 0 && (c == 0 || (c != b'+' && c != b'-' && !c.is_ascii_digit())) {
        return false;
    }
    let sign: i64 = if c == b'+' || c == b'-' {
        i += 1;
        if c == b'-' { 1 } else { -1 }
    } else {
        -1
    };
    *p = i;
    let (vals, end) = scan_hms(s, i);
    if let Some(&hh) = vals.first() {
        let mm = vals.get(1).copied().unwrap_or(0);
        let ss = vals.get(2).copied().unwrap_or(0);
        rules[which].offset =
            sign * (i64::from(ss.min(59)) + i64::from(mm.min(59)) * 60 + i64::from(hh.min(24)) * 3600);
    } else if which == 0 {
        // Standard time defaults to offset zero.
        rules[0].offset = 0;
        return false;
    } else {
        // DST defaults to one hour later than standard time.
        rules[1].offset = rules[0].offset + 3600;
    }
    *p = if vals.is_empty() { i } else { end };
    true
}

/// `parse_rule`: `,date[/time]`, written into the rule as far as it reads.
fn parse_rule(s: &[u8], p: &mut usize, r: &mut TzRule, first: bool) -> bool {
    let mut i = *p;
    // Ignore comma to support string following the incorrect
    // specification in early POSIX.1 printings.
    if c_at(s, i) == b',' {
        i += 1;
    }
    let c = c_at(s, i);
    if c == b'J' || c.is_ascii_digit() {
        r.kind = if c == b'J' { RuleKind::J1 } else { RuleKind::J0 };
        if r.kind == RuleKind::J1 {
            i += 1;
            if !c_at(s, i).is_ascii_digit() {
                return false;
            }
        }
        let start = i;
        let mut d: u64 = 0;
        while c_at(s, i).is_ascii_digit() {
            d = d.saturating_mul(10).saturating_add(u64::from(c_at(s, i) - b'0'));
            i += 1;
        }
        if i == start || d > 365 {
            return false;
        }
        if r.kind == RuleKind::J1 && d == 0 {
            return false;
        }
        r.d = d as u16;
    } else if c == b'M' {
        r.kind = RuleKind::M;
        // sscanf(tz, "M%hu.%hu.%hu%n", &m, &n, &d, &consumed) != 3
        let mut j = i + 1;
        let mut got = 0;
        for k in 0..3 {
            if k > 0 {
                if c_at(s, j) != b'.' {
                    break;
                }
                j += 1;
            }
            let Some((v, e)) = scan_hu(s, j) else { break };
            match k {
                0 => r.m = v,
                1 => r.n = v,
                _ => r.d = v,
            }
            j = e;
            got += 1;
        }
        if got != 3 || r.m < 1 || r.m > 12 || r.n < 1 || r.n > 5 || r.d > 6 {
            return false;
        }
        i = j;
    } else if c == 0 {
        // Daylight time rules in the U.S. are defined in the U.S. Code,
        // Title 15, Chapter 6, Subchapter IX - Standard Time. These dates
        // were established by Congress in the Energy Policy Act of 2005.
        // Below is the equivalent of "M3.2.0,M11.1.0".
        r.kind = RuleKind::M;
        (r.m, r.n, r.d) = if first { (3, 2, 0) } else { (11, 1, 0) };
    } else {
        return false;
    }
    let c = c_at(s, i);
    if c != 0 && c != b'/' && c != b',' {
        return false;
    } else if c == b'/' {
        // Get the time of day of the change.
        i += 1;
        if c_at(s, i) == 0 {
            return false;
        }
        let negative = c_at(s, i) == b'-';
        if negative {
            i += 1;
        }
        // Default to 2:00 AM.
        let (vals, end) = scan_hms(s, i);
        let hh = vals.first().copied().unwrap_or(2);
        let mm = vals.get(1).copied().unwrap_or(0);
        let ss = vals.get(2).copied().unwrap_or(0);
        if !vals.is_empty() {
            i = end;
        }
        r.secs = (if negative { -1 } else { 1 }) * (i64::from(hh) * 3600 + i64::from(mm) * 60 + i64::from(ss));
    } else {
        // Default to 2:00 AM.
        r.secs = 2 * 3600;
    }
    *p = i;
    true
}

/// `__tzset_parse_tz`. `posixrules`: whether a DST name with no rule may
/// take its transitions from that file.
fn parse_tz(s: &[u8], posixrules: bool) -> Zone {
    // Clear out old state and reset to unnamed UTC.
    let mut rules = [TzRule::default(); 2];
    let mut p = 0usize;
    if parse_tzname(s, &mut p) && parse_offset(s, &mut p, &mut rules, 0) {
        if c_at(s, p) != 0 {
            if parse_tzname(s, &mut p) {
                parse_offset(s, &mut p, &mut rules, 1);
                if posixrules && (c_at(s, p) == 0 || (c_at(s, p) == b',' && c_at(s, p + 1) == 0)) {
                    // There is no rule. See if there is a default rule file.
                    if let Some(f) = tzfile_default(rules[0].offset, rules[1].offset) {
                        return Zone::File(f);
                    }
                }
            }
            // Figure out the standard <-> DST rules.
            let [r0, r1] = &mut rules;
            if parse_rule(s, &mut p, r0, true) {
                parse_rule(s, &mut p, r1, false);
            }
        } else {
            // There is no DST.
            rules[1].offset = rules[0].offset;
        }
    }
    Zone::Rules(rules)
}

/// `compute_change`: when the rule changes in `year`. Years up to 1970 all
/// count from 1970-01-01, so a rule never puts a time before 1970 in DST
/// in the northern hemisphere (and always does in the southern).
fn compute_change(r: &TzRule, year: i32) -> i64 {
    const MON_YDAY: [[u16; 13]; 2] = [
        [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365],
        [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335, 366],
    ];
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    // January 1st, 0:00:00 UTC in YEAR, the day count in C int arithmetic.
    let mut t: i64 = if year > 1970 {
        let days = (year - 1970)
            .wrapping_mul(365)
            .wrapping_add((year - 1) / 4 - 1970 / 4)
            .wrapping_sub((year - 1) / 100 - 1970 / 100)
            .wrapping_add((year - 1) / 400 - 1970 / 400);
        i64::from(days) * 86_400
    } else {
        0
    };
    match r.kind {
        RuleKind::J1 => {
            // Jn: 1 == January 1, 60 == March 1 even in leap years.
            t += (i64::from(r.d) - 1) * 86_400;
            if r.d >= 60 && leap {
                t += 86_400;
            }
        }
        // n: day of year, from 0.
        RuleKind::J0 => t += i64::from(r.d) * 86_400,
        RuleKind::M => {
            // `__mon_yday[leap][m]`, read as the flat table it is in memory.
            let flat = |k: i64| -> i64 {
                let k = i64::from(leap) * 13 + k;
                if (0..26).contains(&k) { i64::from(MON_YDAY[(k / 13) as usize][(k % 13) as usize]) } else { 0 }
            };
            let m = i64::from(r.m);
            t += flat(m - 1) * 86_400;
            // Zeller's Congruence: the weekday of the first of the month.
            let m1 = (i32::from(r.m) + 9) % 12 + 1;
            let yy0 = if r.m <= 2 { year.wrapping_sub(1) } else { year };
            let (yy1, yy2) = (yy0 / 100, yy0 % 100);
            let mut dow = ((26 * m1 - 2) / 10 + 1 + yy2 + yy2 / 4 + yy1 / 4 - 2 * yy1) % 7;
            if dow < 0 {
                dow += 7;
            }
            let mut d = i32::from(r.d) - dow;
            if d < 0 {
                d += 7;
            }
            let month_len = (flat(m) - flat(m - 1)) as i32;
            for _ in 1..r.n {
                if d + 7 >= month_len {
                    break;
                }
                d += 7;
            }
            t += i64::from(d) * 86_400;
        }
    }
    t - r.offset + r.secs
}

/// `__tz_compute`: the offset a TZ string gives at `timer`, in UTC `year`.
fn tz_compute(rules: &[TzRule; 2], timer: i64, year: i32) -> i64 {
    let start = compute_change(&rules[0], year);
    let end = compute_change(&rules[1], year);
    // The southern hemisphere's DST ends in the next year.
    let isdst = if start > end { timer < end || timer >= start } else { timer >= start && timer < end };
    rules[usize::from(isdst)].offset
}

/// The UTC year of `timer` as glibc's tm has it (`1900 + tm_year`, in int),
/// None where `__offtime` fails.
fn utc_year(timer: i64) -> Option<i32> {
    let year = civil_from_days(timer.div_euclid(86_400)).0;
    let tm_year = i32::try_from(year - 1900).ok()?;
    Some(tm_year.wrapping_add(1900))
}

/// `__tzfile_compute`: the offset, the leap second correction, and how
/// many leap seconds `timer` is the moment of.
fn tzfile_compute(f: &TzFile, timer: i64) -> (i64, i64, i64) {
    let n = f.transitions.len();
    let offset = if n == 0 || timer < f.transitions[0] {
        // Before any transition: the first non-DST type (or the first).
        let i = f.types.iter().position(|t| !t.isdst).unwrap_or(0);
        f.types[i].offset
    } else if timer >= f.transitions[n - 1] {
        // Past the last transition: the footer's TZ string, if any and if
        // the year converts, else the last transition's type.
        match (&f.tzspec, utc_year(timer)) {
            (Some(rules), Some(year)) => tz_compute(rules, timer, year),
            _ => f.types[f.type_idxs[n - 1]].offset,
        }
    } else {
        let i = f.transitions.partition_point(|&x| x <= timer);
        f.types[f.type_idxs[i - 1]].offset
    };
    // The latest leap second at or before TIMER, and whether TIMER is it.
    let Some(i) = f.leaps.iter().rposition(|&(at, _)| timer >= at) else {
        return (offset, 0, 0);
    };
    let correct = f.leaps[i].1;
    let mut hit = 0;
    if timer == f.leaps[i].0 && f.leaps[i].1 > if i == 0 { 0 } else { f.leaps[i - 1].1 } {
        hit = 1;
        let mut i = i;
        while i > 0 && f.leaps[i].0 == f.leaps[i - 1].0 + 1 && f.leaps[i].1 == f.leaps[i - 1].1 + 1 {
            hit += 1;
            i -= 1;
        }
    }
    (offset, correct, hit)
}

/// `__tz_convert`'s zone part: (offset, leap correction, leap seconds hit),
/// None where it fails before converting (a year past int).
fn zone_at(t: i64) -> Option<(i64, i64, i64)> {
    match &*zone() {
        Zone::File(f) => Some(tzfile_compute(f, t)),
        Zone::Rules(r) => Some((tz_compute(r, t, utc_year(t)?), 0, 0)),
    }
}

/// Days since 1970-01-01 to (year, month, day), for any year.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// `localtime_r(t)`; Err(str(OSError)) where glibc fails (a year past int).
fn localtime_i(t: i64) -> Result<Tm, String> {
    let (gmtoff, leap_correction, leap_extra_secs) = zone_at(t).ok_or_else(|| EOVERFLOW.to_string())?;
    // __offtime(timer, tm_gmtoff - leap_correction), then tm_sec += the leap
    // seconds hit (a 60th second).
    let local = i128::from(t) + i128::from(gmtoff) - i128::from(leap_correction);
    let days = local.div_euclid(86_400);
    let secs = local.rem_euclid(86_400) as u32;
    let days = i64::try_from(days).map_err(|_| EOVERFLOW.to_string())?;
    let (year, mon, mday) = civil_from_days(days);
    if i32::try_from(year - 1900).is_err() {
        return Err(EOVERFLOW.to_string());
    }
    Ok(Tm {
        year,
        mon,
        mday,
        hour: secs / 3600,
        min: secs / 60 % 60,
        sec: secs % 60 + leap_extra_secs as u32,
        wday: (days + 4).rem_euclid(7) as u32,
    })
}

/// `time.localtime(t)`: Err(str(exception)) where it raises.
fn localtime(t: f64) -> Result<Tm, String> {
    if t.is_nan() {
        return Err("Invalid value NaN (not a number)".to_string());
    }
    let whole = t.floor();
    // time_t is 64 bits: [-2**63, 2**63).
    if !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&whole) {
        return Err("timestamp out of range for platform time_t".to_string());
    }
    localtime_i(whole as i64)
}

/// An LC_TIME name, as strftime finds it in the process locale (which
/// `gtk::init` set from the environment, as Python's i18n did with
/// setlocale): glib reads the same nl_langinfo table. English (the C
/// locale's names) when glib cannot say.
fn lc_time_name(fmt: &str, month: u32, day: u32, fallback: &str) -> String {
    gtk::glib::DateTime::from_utc(2023, month as i32, day as i32, 12, 0, 0.0)
        .ok()
        .and_then(|d| d.format(fmt).ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

/// `time.strftime(fmt, time.localtime(t))`, for the directives used here.
/// glibc prints the year unpadded, however long or negative.
fn strftime_local(fmt: &str, t: f64) -> Result<String, String> {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let tm = localtime(t)?;
    let mut out = String::new();
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&tm.year.to_string()),
            Some('m') => out.push_str(&format!("{:02}", tm.mon)),
            Some('d') => out.push_str(&format!("{:02}", tm.mday)),
            Some('H') => out.push_str(&format!("{:02}", tm.hour)),
            Some('M') => out.push_str(&format!("{:02}", tm.min)),
            Some('S') => out.push_str(&format!("{:02}", tm.sec)),
            // 2023-01-01 was a Sunday.
            Some('a') => out.push_str(&lc_time_name("%a", 1, 1 + tm.wday, DAYS[tm.wday as usize])),
            Some('b') => out.push_str(&lc_time_name("%b", tm.mon, 1, MONTHS[tm.mon as usize - 1])),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    Ok(out)
}

/// Python's `str.isspace()` for one character.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\x1c'..='\x1f')
}

/// Python's `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python's `str.split()` with no separator.
fn py_split(s: &str) -> Vec<&str> {
    s.split(py_isspace).filter(|p| !p.is_empty()).collect()
}

/// Python's `str.splitlines()`: every line boundary it knows, no trailing
/// empty line.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let end = match c {
            '\r' => {
                if let Some(&(_, '\n')) = it.peek() {
                    it.next();
                    i + 2
                } else {
                    i + 1
                }
            }
            '\n' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                i + c.len_utf8()
            }
            _ => continue,
        };
        out.push(&s[start..i]);
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `str.isprintable()` for one character: false for the categories `repr()`
/// escapes (Cc, Cf, Cs, Co, Cn, Zl, Zp, and Zs other than the space).
fn py_printable(c: char) -> bool {
    static NONPRINT: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\p{Cc}\p{Cf}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]$").unwrap());
    c == ' ' || !NONPRINT.is_match(c.encode_utf8(&mut [0u8; 4]))
}

/// Python's `repr(str)`.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if !py_printable(c) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr(list_of_str)`.
fn py_list_repr(items: &[String]) -> String {
    format!("[{}]", items.iter().map(|s| py_repr(s)).collect::<Vec<_>>().join(", "))
}

// --- datetime: _datetimemodule.c, which CPython runs (not _pydatetime) ---

/// `date(1970, 1, 1).toordinal()` in seconds.
const EPOCH_SECONDS: i64 = 719_163 * 24 * 60 * 60;

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    const DIM: [i64; 13] = [0, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if month == 2 && is_leap(year) { 29 } else { DIM[month as usize] }
}

/// `utc_to_seconds`: None where it raises (a year outside 1..9999).
fn utc_to_seconds(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Option<i64> {
    if !(1..=9999).contains(&year) {
        return None;
    }
    let ord = i64::from(chrono::Datelike::num_days_from_ce(&chrono::NaiveDate::from_ymd_opt(year as i32, month, day)?));
    Some(((ord * 24 + i64::from(hour)) * 60 + i64::from(minute)) * 60 + i64::from(second))
}

/// `local(u)`: the local wall clock at `u`, in the same seconds.
fn local(u: i64) -> Option<i64> {
    let tm = localtime_i(u - EPOCH_SECONDS).ok()?;
    utc_to_seconds(tm.year, tm.mon, tm.mday, tm.hour, tm.min, tm.sec)
}

/// `local_to_seconds(..., fold=0)`: a naive local time as seconds.
fn local_to_seconds(year: i64, month: u32, day: u32, hour: u32, minute: u32, second: u32) -> Option<i64> {
    const MAX_FOLD_SECONDS: i64 = 24 * 3600;
    let t = utc_to_seconds(year, month, day, hour, minute, second)?;
    let lt = local(t)?;
    let a = lt - t;
    let u1 = t - a;
    let t1 = local(u1)?;
    let b;
    if t1 == t {
        // One solution; fold=0 wants the earlier one, if there is one.
        let u2 = u1 - MAX_FOLD_SECONDS;
        let lt = local(u2)?;
        b = lt - u2;
        if a == b {
            return Some(u1);
        }
    } else {
        b = t1 - u1;
    }
    let u2 = t - b;
    let t2 = local(u2)?;
    if t2 == t {
        return Some(u2);
    }
    if t1 == t {
        return Some(u1);
    }
    // Neither t - a nor t - b is a solution: t is in the gap.
    Some(u1.max(u2))
}

/// `int / int` in Python: the correctly rounded quotient.
fn py_true_div(a: i64, b: i64) -> f64 {
    const LIMIT: i64 = 1 << 53;
    if a.abs() <= LIMIT && b.abs() <= LIMIT {
        return a as f64 / b as f64;
    }
    // Scale the quotient to 53 significant bits and round half to even.
    let neg = (a < 0) != (b < 0);
    let (a, b) = (i128::from(a).abs(), i128::from(b).abs());
    let bits = |x: i128| 128 - x.leading_zeros() as i32;
    let mut shift = 53 - (bits(a) - bits(b));
    let quotient = |shift: i32| if shift >= 0 { (a << shift) / b } else { 0 };
    while shift > 0 && quotient(shift) >= 1 << 53 {
        shift -= 1;
    }
    while shift < 64 && quotient(shift) < 1 << 52 {
        shift += 1;
    }
    if shift < 0 {
        return if neg { -(a as f64 / b as f64) } else { a as f64 / b as f64 };
    }
    let num = a << shift;
    let (mut q, r) = (num / b, num % b);
    if 2 * r > b || (2 * r == b && q % 2 == 1) {
        q += 1;
    }
    let v = q as f64 * 2f64.powi(-shift);
    if neg { -v } else { v }
}

/// `parse_digits`: `n` ASCII digits from `p` onwards, accumulating into `var`.
fn parse_digits(at: &dyn Fn(usize) -> u8, p: &mut usize, var: &mut i64, n: usize) -> bool {
    for _ in 0..n {
        let d = at(*p).wrapping_sub(b'0');
        *p += 1;
        if d > 9 {
            return false;
        }
        *var = *var * 10 + i64::from(d);
    }
    true
}

/// `_find_isoformat_datetime_separator`; None for its -1.
fn iso_separator(at: &dyn Fn(usize) -> u8, len: usize) -> Option<usize> {
    if len == 7 {
        return Some(7);
    }
    if at(4) == b'-' {
        if at(5) == b'W' {
            if len < 8 {
                return None;
            }
            if len > 8 && at(8) == b'-' {
                if len == 9 {
                    return None;
                }
                if len > 10 && at(10).is_ascii_digit() {
                    return Some(8);
                }
                return Some(10);
            }
            return Some(8);
        }
        return Some(10);
    }
    if at(4) == b'W' {
        let mut idx = 7;
        while idx < len && at(idx).is_ascii_digit() {
            idx += 1;
        }
        if idx < 9 {
            return Some(idx);
        }
        return Some(if idx % 2 == 0 { 7 } else { 8 });
    }
    Some(8)
}

/// `parse_isoformat_date`: (year, month, day), not yet range-checked.
fn iso_date(at: &dyn Fn(usize) -> u8, len: usize) -> Option<(i64, i64, i64)> {
    let mut p = 0;
    let (mut year, mut month, mut day) = (0, 0, 0);
    if !parse_digits(at, &mut p, &mut year, 4) {
        return None;
    }
    let uses_separator = at(p) == b'-';
    if uses_separator {
        p += 1;
    }
    if at(p) == b'W' {
        // An isocalendar-style date.
        p += 1;
        let (mut week, mut wday) = (0, 0);
        if !parse_digits(at, &mut p, &mut week, 2) {
            return None;
        }
        if p < len {
            if uses_separator {
                let c = at(p);
                p += 1;
                if c != b'-' {
                    return None;
                }
            }
            if !parse_digits(at, &mut p, &mut wday, 1) {
                return None;
            }
        } else {
            wday = 1;
        }
        // iso_to_ymd
        if !(1..=9999).contains(&year) || !(1..=7).contains(&wday) {
            return None;
        }
        let weekday = chrono::Weekday::try_from((wday - 1) as u8).ok()?;
        let d = chrono::NaiveDate::from_isoywd_opt(year as i32, u32::try_from(week).ok()?, weekday)?;
        use chrono::Datelike;
        return Some((i64::from(d.year()), i64::from(d.month()), i64::from(d.day())));
    }
    if !parse_digits(at, &mut p, &mut month, 2) {
        return None;
    }
    if uses_separator {
        let c = at(p);
        p += 1;
        if c != b'-' {
            return None;
        }
    }
    if !parse_digits(at, &mut p, &mut day, 2) {
        return None;
    }
    Some((year, month, day))
}

/// `parse_hh_mm_ss_ff` over `[start, end)`: its return code (0, or 1 when
/// something follows) and `[hour, minute, second, microsecond]`; None for
/// its negative codes.
fn iso_hms(at: &dyn Fn(usize) -> u8, start: usize, end: usize) -> Option<(bool, [i64; 4])> {
    let mut vals = [0i64; 4];
    let mut p = start;
    let mut has_separator = true;
    let mut i = 0;
    while i < 3 {
        if !parse_digits(at, &mut p, &mut vals[i], 2) {
            return None;
        }
        let c = at(p);
        p += 1;
        if i == 0 {
            has_separator = c == b':';
        }
        if c == b'.' || c == b',' {
            if i < 2 || p >= end {
                return None;
            }
            break;
        } else if p >= end {
            return Some((c != 0, vals));
        } else if has_separator && c == b':' {
            if i == 2 {
                return None;
            }
            i += 1;
            continue;
        } else if !has_separator {
            p -= 1;
        } else {
            return None;
        }
        i += 1;
    }
    // The fraction: up to six digits count, more are skipped.
    let to_parse = end.saturating_sub(p).min(6);
    if to_parse == 0 {
        return None;
    }
    let mut us = 0;
    if !parse_digits(at, &mut p, &mut us, to_parse) {
        return None;
    }
    if to_parse < 6 {
        us *= 10i64.pow((6 - to_parse) as u32);
    }
    vals[3] = us;
    while at(p).is_ascii_digit() {
        p += 1;
    }
    Some((at(p) != 0, vals))
}

/// `parse_isoformat_time` over `[start, end)`: the time and, when there is
/// one, the UTC offset as (seconds, microseconds).
type IsoTime = ([i64; 4], Option<(i64, i64)>);

fn iso_time(at: &dyn Fn(usize) -> u8, start: usize, end: usize) -> Option<IsoTime> {
    let mut tz = start;
    loop {
        if matches!(at(tz), b'Z' | b'+' | b'-') {
            break;
        }
        tz += 1;
        if tz >= end {
            break;
        }
    }
    let (more, time) = iso_hms(at, start, tz)?;
    if tz == end {
        // No time zone, so anything left over is an error.
        return if more { None } else { Some((time, None)) };
    }
    if at(tz) == b'Z' {
        return if at(tz + 1) != 0 { None } else { Some((time, Some((0, 0)))) };
    }
    let sign = if at(tz) == b'-' { -1 } else { 1 };
    let (more, off) = iso_hms(at, tz + 1, end)?;
    if more {
        return None;
    }
    Some((time, Some((sign * (off[0] * 3600 + off[1] * 60 + off[2]), sign * off[3]))))
}

/// `datetime.fromisoformat(text).timestamp()`, None on ValueError.
fn fromisoformat_ts(text: &str) -> Option<f64> {
    // `_sanitize_isoformat_str`: fewer than 7 characters is never valid.
    if text.chars().count() < 7 {
        return None;
    }
    // The C parser walks the UTF-8 bytes, stopping at the terminating NUL.
    let b = text.as_bytes();
    let len = b.len();
    let at = |i: usize| -> u8 { b.get(i).copied().unwrap_or(0) };
    let sep = iso_separator(&at, len)?;
    let (mut year, mut month, mut day) = iso_date(&at, sep)?;
    let mut time = [0i64; 4];
    let mut offset = None;
    if len > sep {
        // Skip the separator, which may be any one character.
        let lead = at(sep);
        let width = if lead & 0x80 == 0 {
            1
        } else {
            match lead & 0xf0 {
                0xe0 => 3,
                0xf0 => 4,
                _ => 2,
            }
        };
        let start = sep + width;
        if start > len {
            return None;
        }
        (time, offset) = iso_time(&at, start, len)?;
    }
    let [mut hour, minute, second, us] = time;
    // tzinfo_from_isoformat_results: a zero offset is UTC; others must be
    // strictly within a day.
    let offset_us = match offset {
        Some((s, u)) => {
            let total = s * 1_000_000 + u;
            if (s != 0 || u != 0)
                && !(-86_400_000_000 + 1..86_400_000_000).contains(&total) {
                    return None;
                }
            Some(total)
        }
        None => None,
    };
    if hour == 24 && (1..=12).contains(&month) {
        let dim = days_in_month(year, month);
        if day <= dim {
            if minute == 0 && second == 0 && us == 0 {
                // Midnight of the next day.
                hour = 0;
                day += 1;
                if day > dim {
                    day = 1;
                    month += 1;
                    if month > 12 {
                        month = 1;
                        year += 1;
                    }
                }
            } else {
                return None;
            }
        }
    }
    // new_datetime's range checks.
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..60).contains(&second)
    {
        return None;
    }
    let (mo, d, h, mi, s) = (month as u32, day as u32, hour as u32, minute as u32, second as u32);
    match offset_us {
        // (self - epoch).total_seconds()
        Some(off) => {
            let secs = utc_to_seconds(year, mo, d, h, mi, s)? - EPOCH_SECONDS;
            Some(py_true_div(secs * 1_000_000 + us - off, 1_000_000))
        }
        None => {
            let secs = local_to_seconds(year, mo, d, h, mi, s)?;
            Some((secs - EPOCH_SECONDS) as f64 + us as f64 / 1e6)
        }
    }
}

// _strptime's regex for "%Y-%m-%d %H:%M" (re.IGNORECASE changes nothing in
// it). `\d` is any Unicode decimal digit in both; `\s` is spelled out, as
// Python's includes \x1c-\x1f.
static _STRPTIME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(\d\d\d\d)-(1[0-2]|0[1-9]|[1-9])-(3[0-1]|[1-2]\d|0[1-9]|[1-9]| [1-9])",
        r"[\s\x1c-\x1f]+(2[0-3]|[0-1]\d|\d| \d):([0-5]\d|\d)\z",
    ))
    .unwrap()
});

/// The value of a Unicode decimal digit (what `int()` takes it for).
/// Decimal digits come in runs that start at a zero, so it is the distance
/// from the start of its run, modulo ten.
fn digit_value(c: char) -> Option<i64> {
    static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").unwrap());
    let is_digit = |c: char| DIGIT.is_match(c.encode_utf8(&mut [0u8; 4]));
    if !is_digit(c) {
        return None;
    }
    let mut n = c as u32;
    let mut back = 0;
    while n > 0 && char::from_u32(n - 1).is_some_and(is_digit) {
        n -= 1;
        back += 1;
    }
    Some(back % 10)
}

/// `int(text)` for what `_STRPTIME` captured: digits, maybe a leading space.
fn py_int(text: &str) -> Option<i64> {
    text.trim_start_matches(' ').chars().try_fold(0i64, |acc, c| Some(acc * 10 + digit_value(c)?))
}

/// `datetime.strptime(text, "%Y-%m-%d %H:%M").timestamp()`, None on ValueError.
fn strptime_ts(text: &str) -> Option<f64> {
    let m = _STRPTIME.captures(text)?;
    let n = |i: usize| py_int(&m[i]);
    let (year, month, day, hour, minute) = (n(1)?, n(2)?, n(3)?, n(4)?, n(5)?);
    if !(1..=9999).contains(&year) || day > days_in_month(year, month) {
        return None;
    }
    let secs = local_to_seconds(year, month as u32, day as u32, hour as u32, minute as u32, 0)?;
    Some((secs - EPOCH_SECONDS) as f64)
}


#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = concat!(
        "[2026-10-16T22:52:33-0700] [PACMAN] starting full system upgrade\n",
        "[2026-10-16T22:52:40-0700] [ALPM] transaction started\n",
        "[2026-10-16T22:52:40-0700] [ALPM] running '05-snap-pac-pre.hook'...\n",
        "[2026-10-16T22:52:41-0700] [ALPM] upgraded a (1-1 -> 2-1)\n",
        "[2026-10-16T22:52:42-0700] [ALPM] transaction completed\n",
        "[2026-10-16T23:02:32-0700] [ALPM] transaction started\n",
        "[2026-10-16T23:02:33-0700] [ALPM] upgraded b (1-1 -> 2-1)\n",
        "[2026-10-16T23:02:34-0700] [ALPM] transaction completed\n",
    );

    #[test]
    fn test_the_last_transaction_is_read_with_its_changes() {
        let tx = last_transaction(Some(LOG)).unwrap();
        assert_eq!(tx.changes, vec!["upgraded b (1-1 -> 2-1)"]);
        assert!(!tx.hooks_snapshot);
    }

    #[test]
    fn test_a_snapshot_hook_inside_the_transaction_counts() {
        let first = LOG.split("[2026-10-16T23:02:32").next().unwrap();
        assert!(last_transaction(Some(first)).unwrap().hooks_snapshot);
    }

    #[test]
    fn test_a_manual_snapper_create_in_the_journal_counts() {
        let j = "Oct 16 23:02:22 host sudo[1]: tim : COMMAND=/usr/bin/snapper \
                 -c root create -c number -d 4.0.4-1\n";
        assert_eq!(snapshot_before(0.0, Some(j)), Ok(Some(true)));
        assert_eq!(snapshot_before(0.0, Some("nothing\n")), Ok(Some(false)));
    }

    #[test]
    fn test_a_snapshot_boot_is_recognised() {
        let line = "root=UUID=x rw rootflags=subvol=/@/.snapshots/142/snapshot quiet";
        assert_eq!(booted_snapshot(Some(line)).unwrap(), "142");
        assert_eq!(booted_snapshot(Some("root=UUID=x rw")).unwrap(), "");
    }

    #[test]
    fn test_no_snapper_means_no_snapshot_step() {
        let runner: Runner = Arc::new(|_: &[String]| Ok(RawOutput::default()));
        assert!(create_step_with("x", vec![], runner).is_none());
    }

    #[test]
    fn test_a_refused_snapshot_does_not_stop_the_update() {
        let runner: Runner = Arc::new(|_: &[String]| {
            Ok(RawOutput {
                code: Some(1),
                stdout: Vec::new(),
                stderr: b"Request dismissed".to_vec(),
                ..Default::default()
            })
        });
        let (_label, step) = create_step_with("x", vec!["root".into()], runner).unwrap();
        let lines = std::cell::RefCell::new(Vec::<String>::new());
        assert!(step(&|l: &str| lines.borrow_mut().push(l.to_string())).is_ok()); // must not raise
        assert!(lines.borrow().last().unwrap().contains("continuing without one"));
    }

    #[test]
    fn pacman_timestamps_parse() {
        assert_eq!(_ts("2026-10-16T22:52:33-0700"), 1792216353.0);
        assert_eq!(_ts("garbage"), 0.0);
    }

    #[test]
    fn utf8_errors_read_as_python_words_them() {
        let cases: [(&[u8], &str); 7] = [
            (b"ab\xff", "'utf-8' codec can't decode byte 0xff in position 2: invalid start byte"),
            (b"ab\xe2\x82", "'utf-8' codec can't decode bytes in position 2-3: unexpected end of data"),
            (b"\xe2\x82A", "'utf-8' codec can't decode bytes in position 0-1: invalid continuation byte"),
            (b"\xe2A", "'utf-8' codec can't decode byte 0xe2 in position 0: invalid continuation byte"),
            (b"\xed\xa0\x80", "'utf-8' codec can't decode byte 0xed in position 0: invalid continuation byte"),
            (b"\xc0\xaf", "'utf-8' codec can't decode byte 0xc0 in position 0: invalid start byte"),
            (b"x\xf0\x9f", "'utf-8' codec can't decode bytes in position 1-2: unexpected end of data"),
        ];
        for (b, want) in cases {
            assert_eq!(py_utf8_decode(b), Err(want.to_string()));
        }
    }

    #[test]
    fn exec_errors_name_the_cause() {
        let missing = vec!["/nonexistent/almanac-x".to_string()];
        assert_eq!(exec_error(&missing), "[Errno 2] No such file or directory: '/nonexistent/almanac-x'");
        let dir = vec!["/".to_string()];
        assert_eq!(exec_error(&dir), "[Errno 13] Permission denied: '/'");
    }

    #[test]
    fn a_real_replacement_character_is_not_a_decode_error() {
        assert_eq!(py_text("warning: \u{FFFD}\r\n".as_bytes()), Ok("warning: \u{FFFD}\n".to_string()));
        assert!(py_text(b"bad \xff").is_err());
    }

    #[test]
    fn a_nul_byte_in_the_description_fails_the_step() {
        assert_eq!(run_raw(&["echo", "a\0b"], 5).map(|r| r.ok()), Err("embedded null byte".to_string()));
        let runner: Runner = Arc::new(|_: &[String]| Err(RunError::Raise("embedded null byte".into())));
        let (_label, step) = create_step_with("a\0b", vec!["root".into()], runner).unwrap();
        assert_eq!(step(&|_: &str| {}), Err("embedded null byte".to_string()));
    }

    #[test]
    fn tz_strings_read_as_glibc_reads_them() {
        let rules = |tz: &str| match parse_tz(tz.as_bytes(), false) {
            Zone::Rules(r) => r,
            Zone::File(_) => panic!("rules expected"),
        };
        // Not a TZ string: unnamed UTC.
        assert_eq!(tz_compute(&rules("Foo"), 1_792_216_353, 2026), 0);
        // Offsets are capped, not rejected.
        assert_eq!(tz_compute(&rules("XXX-25"), 1_792_216_353, 2026), 86_400);
        let cet = rules("CET-1CEST,M3.5.0,M10.5.0/3");
        // Summer 2026 is DST; summer 1965 is not (no rule before 1970).
        assert_eq!(tz_compute(&cet, 1_782_216_353, 2026), 7200);
        assert_eq!(tz_compute(&cet, -142_092_000, 1965), 3600);
        // A southern rule keeps every time before 1970 in DST.
        let aus = rules("AEST-10AEDT,M10.1.0,M4.1.0/3");
        assert_eq!(tz_compute(&aus, -142_092_000, 1965), 39_600);
    }
}
