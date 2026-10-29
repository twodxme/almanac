//! After a system update: did everything go through?
//!
//! The system update runs in a terminal, where it can ask for a password.
//! The terminal's own exit says nothing about it -- the script ends waiting
//! for a key, and a single-instance terminal (ghostty, kitty) returns before
//! the update has even started -- so the script writes the command's exit
//! status to a file of ours, and everything else is read back from the
//! machine: pacman's log since the update began, what is still waiting,
//! services that failed since, AUR packages left linked against libraries
//! that are gone, a kernel whose modules were replaced under it.
//!
//! Everything here is plain logic over what the probes answered, so the
//! judging can be tested on real pacman.log text without touching the
//! machine. The probes themselves are the small `probe_*` functions.

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use regex::Regex;

use crate::history::{ActionLog, Change, CheckResult, CommandRun};
use crate::tasks::{self, LogFn, Step};
use crate::verify::Outcome;

pub const PACMAN_LOG: &str = crate::snapshots::PACMAN_LOG;
pub const DB_LOCK: &str = "/var/lib/pacman/db.lck";
/// By path, as every package tool here: PATH puts user-writable folders first.
const CHECKREBUILD: &str = "/usr/bin/checkrebuild";
pub const PACCACHE: &str = "/usr/bin/paccache";

/// A terminal that hands its window to a server returns within this.
pub const QUICK_RETURN: Duration = Duration::from_secs(5);
/// A terminal that returned at once and has not started the script by now
/// never will: the window was closed, or the terminal could not run it.
pub const NO_START: Duration = Duration::from_secs(120);
/// Longer than any update; past it the run is given up on.
pub const GIVE_UP: Duration = Duration::from_secs(6 * 3600);

const GIB: u64 = 1024 * 1024 * 1024;
/// Under this much free on / an update is likely to fail part-way: pacman
/// downloads every package first, then unpacks them beside the old files.
pub const BLOCK_FREE: u64 = 2 * GIB;
pub const WARN_FREE: u64 = 5 * GIB;
pub const LOW_BATTERY: u8 = 20;

// ---- the run's files -----------------------------------------------------------

/// Where the terminal script reports back: per run, created by Almanac, in
/// a directory only this user can enter.
#[derive(Debug, Clone, PartialEq)]
pub struct RunFiles {
    /// "start <epoch>" and "exit <status> <epoch>" lines
    pub status: PathBuf,
    /// what the terminal showed, when `script` is there to record it
    pub output: PathBuf,
}

/// `$XDG_RUNTIME_DIR/almanac/update-runs`: a per-user tmpfs, so nothing
/// outlives a reboot. State home when there is no runtime directory.
pub fn run_dir() -> Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d),
        _ => crate::xdg::state_home(),
    };
    let dir = base.join("almanac").join("update-runs");
    private_dir(&dir)?;
    Ok(dir)
}

/// Create `dir` (0700) or check the one there: ours, a real directory, and
/// closed to everyone else. A file another user can write is a file that
/// can claim the update went fine.
pub fn private_dir(dir: &Path) -> Result<(), String> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(format!("{} belongs to another user", dir.display()));
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    Ok(())
}

/// A fresh pair of files in `dir`, unique names, mode 0600, created here
/// before the terminal starts. Files from runs over two days old go.
pub fn new_run_files_in(dir: &Path) -> Result<RunFiles, String> {
    prune(dir, Duration::from_secs(2 * 86400));
    let (_f, status) = tempfile::Builder::new()
        .prefix("update-")
        .suffix(".status")
        .tempfile_in(dir)
        .and_then(|t| t.keep().map_err(|e| e.error))
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let output = status.with_extension("log");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)
        .map_err(|e| format!("{}: {e}", output.display()))?;
    Ok(RunFiles { status, output })
}

fn prune(dir: &Path, older: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let old = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|age| age > older));
        if old && e.file_name().to_string_lossy().starts_with("update-") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

impl RunFiles {
    /// Done with: the report has what it needed of them.
    pub fn remove(&self) {
        let _ = std::fs::remove_file(&self.status);
        let _ = std::fs::remove_file(&self.output);
    }
}

/// The terminal half's script. The command runs in a bash of its own, so
/// an `exit` in it ends that and not the script; `script` (util-linux)
/// records what the terminal shows without taking the terminal away from
/// it -- pacman still sees a tty, so prompts and progress bars work. The
/// status line is written before the key prompt: a terminal left open
/// still reports back.
pub fn terminal_script(command: &str, files: Option<&RunFiles>, recorder: Option<&Path>) -> String {
    let q = |s: &str| shell_words::quote(s).into_owned();
    const KEY: &str = "echo; read -n1 -r -p 'Done — press any key to close'";
    let Some(files) = files else {
        return format!("/usr/bin/bash -c {}; {KEY}", q(command));
    };
    let run = match recorder {
        Some(script) => format!(
            "SHELL=/usr/bin/bash {} -qefc {} {}",
            q(&script.to_string_lossy()),
            q(command),
            q(&files.output.to_string_lossy())
        ),
        None => format!("/usr/bin/bash -c {}", q(command)),
    };
    let st = q(&files.status.to_string_lossy());
    format!(
        "printf 'start %s\\n' \"$(date +%s)\" >> {st}; {run}; rc=$?; \
         printf 'exit %s %s\\n' \"$rc\" \"$(date +%s)\" >> {st}; {KEY}"
    )
}

/// What the status file says so far.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunStatus {
    pub started: Option<i64>,
    pub exit: Option<i32>,
    pub ended: Option<i64>,
}

impl RunStatus {
    pub fn finished(&self) -> bool {
        self.ended.is_some()
    }

    pub fn secs(&self) -> Option<f64> {
        Some((self.ended? - self.started?) as f64)
    }
}

pub fn parse_status(text: &str) -> RunStatus {
    let mut st = RunStatus::default();
    for line in text.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["start", t] => st.started = t.parse().ok(),
            ["exit", rc, t] => {
                st.exit = rc.parse().ok();
                st.ended = t.parse().ok();
            }
            _ => {}
        }
    }
    st
}

pub fn read_status(path: &Path) -> RunStatus {
    parse_status(&std::fs::read_to_string(path).unwrap_or_default())
}

/// What to do while waiting for the terminal.
#[derive(Debug, PartialEq)]
pub enum Wait {
    Keep,
    /// it finished, or the window closed before it said how
    Done,
    GiveUp(&'static str),
}

/// One look at the run: `exited_at` is when the terminal's process ended
/// (since launch), `watching` whether there is a status file to read.
///
/// Read `exited_at` before the status file: the script writes its status
/// before the terminal can close, so a terminal seen gone has left its
/// status for the read that follows.
pub fn wait_step(elapsed: Duration, exited_at: Option<Duration>, status: &RunStatus, watching: bool) -> Wait {
    if status.finished() {
        return Wait::Done;
    }
    match exited_at {
        // It ran in the terminal's own process, which is gone: closed before
        // the end, so the exit status is unknown.
        Some(at) if at >= QUICK_RETURN => Wait::Done,
        Some(_) if !watching => Wait::GiveUp("Rescan when the update finishes"),
        Some(_) if status.started.is_none() && elapsed > NO_START => {
            Wait::GiveUp("The update did not start in the terminal")
        }
        _ if elapsed > GIVE_UP => Wait::GiveUp("Stopped waiting for the update — rescan when it is done"),
        _ => Wait::Keep,
    }
}

// ---- pacman.log and the terminal's output ---------------------------------------

/// pacman.log's size now: where this update's lines will start.
pub fn log_offset(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// The log from `offset` on. A log shorter than that was replaced: all of it.
pub fn read_log_since(path: &Path, offset: u64) -> Option<String> {
    let mut fh = std::fs::File::open(path).ok()?;
    let len = fh.metadata().ok()?.len();
    fh.seek(SeekFrom::Start(if len < offset { 0 } else { offset })).ok()?;
    let mut buf = Vec::new();
    fh.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// What the update left in the log, sorted into what can be acted on.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogScan {
    /// upgraded, installed, removed, downgraded, reinstalled
    pub changes: Vec<Change>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    /// new default configs left beside the user's (`.pacnew`)
    pub pacnew: Vec<String>,
    /// the user's configs moved aside (`.pacsave`)
    pub pacsave: Vec<String>,
    /// install scripts and hooks that failed: "package foo", "hook 'x.hook'"
    pub scriptlets: Vec<String>,
    pub initramfs: Vec<String>,
    pub dkms: Vec<String>,
    /// signature and keyring errors
    pub keyring: Vec<String>,
    /// AUR packages that did not build
    pub builds: Vec<String>,
    /// pacman found its database locked
    pub locked: bool,
}

static LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(?P<ts>[^\]]+)\] \[(?P<who>[^\]]+)\] (?P<msg>.*)$").unwrap());
static CHANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<op>upgraded|installed|removed|downgraded|reinstalled) (?P<name>\S+) \((?P<v>.*)\)$").unwrap()
});
static PACNEW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^warning: .+ installed as (?P<p>.+\.pacnew)$").unwrap());
static PACSAVE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^warning: .+ saved as (?P<p>.+\.pacsave)$").unwrap());
static HOOK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^running '(?P<h>[^']+)'\.\.\.$").unwrap());
static KEYRING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)signature|keyring|\bpgp\b|unknown trust|marginal trust|public key|could not be looked up remotely|invalid or corrupted package",
    )
    .unwrap()
});
static LOCKED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(unable to|could not|failed to) lock database|db\.lck").unwrap());
static BUILD_FAILED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"error making: (?P<p>\S+)").unwrap());
static ANSI: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]").unwrap()
});

/// The update's section of pacman.log, sorted.
///
/// pacman logs a package's install script before the line saying the
/// package was upgraded, so a script that failed is put to the next package
/// named; a hook that failed comes after "running 'x.hook'".
pub fn scan_log(text: &str) -> LogScan {
    let mut scan = LogScan::default();
    let mut hook: Option<String> = None;
    let mut failed_script = false;
    for line in text.lines() {
        let Some(m) = LINE.captures(line) else { continue };
        let (who, msg) = (&m["who"], m["msg"].trim_end());
        if who == "ALPM-SCRIPTLET" {
            scriptlet_line(msg, &mut scan);
            continue;
        }
        if msg == "transaction started" {
            hook = None;
            continue;
        }
        if let Some(h) = HOOK.captures(msg) {
            hook = Some(h["h"].to_string());
            continue;
        }
        if let Some(c) = CHANGE.captures(msg) {
            if who == "ALPM" {
                let name = c["name"].to_string();
                if failed_script {
                    scan.scriptlets.push(format!("package {name}"));
                    failed_script = false;
                }
                scan.changes.push(change(&c["op"], name, &c["v"]));
            }
            continue;
        }
        if msg.contains("command failed to execute correctly") {
            match &hook {
                Some(h) => scan.scriptlets.push(format!("hook '{h}'")),
                None => failed_script = true,
            }
            continue;
        }
        message(msg, &mut scan, true);
    }
    if failed_script {
        scan.scriptlets.push("an install script".to_string());
    }
    scan.tidy();
    scan
}

/// What the terminal showed, as `script` recorded it: escapes and
/// carriage-return redraws taken out. Only errors are read from it -- the
/// log has the rest, and `--needed` fills the terminal with harmless
/// "is up to date -- skipping" warnings.
pub fn scan_output(raw: &str, scan: &mut LogScan) {
    for line in clean_output(raw).lines() {
        let line = line.trim();
        if let Some(c) = BUILD_FAILED.captures(line) {
            scan.builds.push(c["p"].trim_end_matches(':').to_string());
        } else if line.starts_with("error:") || (line.starts_with("warning:") && KEYRING.is_match(line)) {
            message(line, scan, false);
        }
    }
    scan.tidy();
}

/// Terminal output as plain lines.
pub fn clean_output(raw: &str) -> String {
    let plain = ANSI.replace_all(raw, "");
    plain
        .split('\n')
        .map(|l| {
            let l = l.trim_end_matches('\r');
            l.rsplit('\r').next().unwrap_or(l)
        })
        .filter(|l| !l.starts_with("Script started on") && !l.starts_with("Script done on"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn change(op: &str, name: String, v: &str) -> Change {
    let (from, to) = match op {
        "upgraded" | "downgraded" => match v.split_once(" -> ") {
            Some((a, b)) => (a.to_string(), b.to_string()),
            None => (String::new(), v.to_string()),
        },
        "installed" => (String::new(), v.to_string()),
        "removed" => (v.to_string(), String::new()),
        _ => (v.to_string(), v.to_string()),
    };
    Change { name, from, to }
}

fn scriptlet_line(msg: &str, scan: &mut LogScan) {
    let lower = msg.to_lowercase();
    if msg.contains("Error!") || (lower.contains("dkms") && (lower.contains("error") || lower.contains("failed"))) {
        scan.dkms.push(msg.trim().to_string());
    } else if msg.contains("==> ERROR")
        || ((lower.contains("initcpio") || lower.contains("image generation") || lower.contains("mkinitcpio"))
            && lower.contains("fail"))
    {
        // "==> WARNING: Possibly missing firmware" is everyday noise; only
        // an error means the boot image was not written.
        scan.initramfs.push(msg.trim().to_string());
    }
}

fn message(msg: &str, scan: &mut LogScan, from_log: bool) {
    if let Some(c) = PACNEW.captures(msg) {
        scan.pacnew.push(c["p"].to_string());
    } else if let Some(c) = PACSAVE.captures(msg) {
        scan.pacsave.push(c["p"].to_string());
    } else if (msg.starts_with("error:") || msg.starts_with("warning:")) && KEYRING.is_match(msg) {
        scan.keyring.push(msg.to_string());
    } else if msg.starts_with("error:") && LOCKED.is_match(msg) {
        scan.locked = true;
        scan.errors.push(msg.to_string());
    } else if msg.starts_with("error:") {
        scan.errors.push(msg.to_string());
    } else if msg.starts_with("warning:") && from_log {
        scan.warnings.push(msg.to_string());
    }
}

impl LogScan {
    /// The log and the terminal say some things twice.
    fn tidy(&mut self) {
        for v in [
            &mut self.errors,
            &mut self.warnings,
            &mut self.pacnew,
            &mut self.pacsave,
            &mut self.scriptlets,
            &mut self.initramfs,
            &mut self.dkms,
            &mut self.keyring,
            &mut self.builds,
        ] {
            let mut seen = BTreeSet::new();
            v.retain(|x| seen.insert(x.clone()));
        }
    }
}

// ---- probes ---------------------------------------------------------------------

/// What was true before the update, for the checks to compare against.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Before {
    pub pending: Option<BTreeSet<String>>,
    pub units: Option<BTreeSet<String>>,
    pub rebuild: Option<BTreeSet<String>>,
}

/// What is true after it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct After {
    pub pending: Option<BTreeSet<String>>,
    pub units: Option<BTreeSet<String>>,
    /// None: checkrebuild could not tell (or is not installed, see `rebuild_tool`)
    pub rebuild: Option<BTreeSet<String>>,
    pub rebuild_tool: bool,
    pub kernel_gone: Option<bool>,
    /// db.lck there with nothing running that holds it
    pub stale_lock: bool,
}

pub fn probe_pending() -> Option<BTreeSet<String>> {
    match (crate::verify::pending_updates())() {
        Some(crate::verify::ProbeValue::Set(s)) => Some(s),
        _ => None,
    }
}

/// Failed units, "system x.service" / "user y.service", as Health lists them.
pub fn probe_units() -> Option<BTreeSet<String>> {
    std::panic::catch_unwind(crate::maintenance::failed_units)
        .ok()
        .map(|f| f.items.into_iter().collect())
}

pub fn have_checkrebuild() -> bool {
    Path::new(CHECKREBUILD).exists()
}

/// AUR packages checkrebuild says need rebuilding; None when it could not
/// run. Packages from the repositories are rebuilt by Arch, not here.
pub fn probe_rebuild() -> Option<BTreeSet<String>> {
    let out = crate::util::run(&[CHECKREBUILD], 300);
    if out.failed_to_start || out.timed_out || out.code != Some(0) {
        return None;
    }
    Some(parse_checkrebuild(&out.stdout))
}

pub fn parse_checkrebuild(out: &str) -> BTreeSet<String> {
    out.lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            match (w.next(), w.next()) {
                (Some("foreign"), Some(pkg)) => Some(pkg.to_string()),
                _ => None,
            }
        })
        .collect()
}

/// After a kernel upgrade the running kernel's modules are gone from disk
/// until a reboot: anything that loads a module (a USB stick, a VPN) fails.
pub fn kernel_gone(release: &str, modules: &Path) -> bool {
    !release.is_empty() && !modules.join(release).is_dir()
}

pub fn probe_kernel() -> Option<bool> {
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    Some(kernel_gone(release.trim(), Path::new("/usr/lib/modules")))
}

pub fn probe_after(pending: bool) -> After {
    // Demo mode never looks at the real machine: nothing to report.
    if crate::demo::active() {
        return demo_after();
    }
    let rebuild_tool = pending && have_checkrebuild();
    std::thread::scope(|s| {
        let rebuild = s.spawn(move || if rebuild_tool { probe_rebuild() } else { None });
        let units = s.spawn(probe_units);
        After {
            pending: if pending { probe_pending() } else { None },
            units: units.join().ok().flatten(),
            rebuild: rebuild.join().ok().flatten(),
            rebuild_tool,
            kernel_gone: probe_kernel(),
            stale_lock: lock_is_stale(),
        }
    })
}

pub fn probe_before(pending: bool) -> Before {
    if crate::demo::active() {
        return Before { pending: pending.then(BTreeSet::new), units: Some(BTreeSet::new()), rebuild: None };
    }
    let rebuild_tool = pending && have_checkrebuild();
    std::thread::scope(|s| {
        let rebuild = s.spawn(move || if rebuild_tool { probe_rebuild() } else { None });
        let units = s.spawn(probe_units);
        Before {
            pending: if pending { probe_pending() } else { None },
            units: units.join().ok().flatten(),
            rebuild: rebuild.join().ok().flatten(),
        }
    })
}

/// Whether a process with this name and these arguments holds, or is
/// about to take, pacman's lock. Queries (`pacman -Q`, `yay -Qua`, a sync
/// search) never lock; Almanac runs those itself. Nor does a pacman pointed
/// at a database of its own (`--dbpath`): checkupdates syncs a private copy
/// with `fakeroot pacman -Sy --dbpath /tmp/checkup-db-UID`, and that lock
/// is in its copy, not in /var/lib/pacman.
pub fn locking(comm: &str, args: &[String]) -> bool {
    match comm {
        "makepkg" => true,
        c if c == "pacman" || HELPERS.contains(&c) => !is_query(args) && !own_dbpath(args),
        _ => false,
    }
}

/// AUR helpers and front ends that run pacman, or take its lock through
/// libalpm, for what they are asked to do.
const HELPERS: &[&str] = &["yay", "paru", "pikaur", "aura", "powerpill", "pacaur", "trizen", "pakku", "aurman"];

/// Services that take the lock through libalpm whenever a client asks
/// them to. They sit idle most of the time, so one running is no reason to
/// hold an update back; but while one runs, a lock file may be its own.
const DAEMONS: &[&str] = &["pamac-daemon", "packagekitd"];

/// The command line points pacman at a database other than the system's.
fn own_dbpath(args: &[String]) -> bool {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let path = match a.as_str() {
            "--dbpath" | "-b" => it.next().map(String::as_str),
            s => s.strip_prefix("--dbpath="),
        };
        if let Some(p) = path {
            return p.trim_end_matches('/') != "/var/lib/pacman";
        }
    }
    false
}

fn is_query(args: &[String]) -> bool {
    let mut shorts = String::new();
    for a in args {
        match a.as_str() {
            "--query" | "--deptest" | "--files" | "--print" | "--getpkgbuild" | "--show" => return true,
            "--search" | "--info" | "--list" | "--groups" => return true,
            s if s.starts_with('-') && !s.starts_with("--") => shorts.push_str(&s[1..]),
            _ => {}
        }
    }
    if shorts.contains(['Q', 'T', 'F', 'G', 'P']) {
        return true;
    }
    shorts.contains('S') && shorts.contains(['s', 'i', 'l', 'g', 'p']) && !shorts.contains(['y', 'u'])
}

/// What the processes in /proc say about pacman's lock.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LockScan {
    /// processes that hold it or are about to take it, as "name (pid)"
    pub running: Vec<String>,
    /// package services running (`DAEMONS`), which may hold it
    pub daemons: Vec<String>,
    /// other users' processes cannot be read (/proc mounted with hidepid):
    /// root's pacman would not show, so nothing here says the lock is free
    pub hidden: bool,
}

impl LockScan {
    /// Positive evidence that nothing uses the lock: every process could be
    /// read, and none of them takes it. Anything less is "may be in use".
    pub fn nothing_uses_it(&self) -> bool {
        !self.hidden && self.running.is_empty() && self.daemons.is_empty()
    }
}

/// The names a process goes by: its own, and for a script run through an
/// interpreter (`python /usr/bin/pikaur`), the script's.
fn program_names(comm: &str, args: &[String]) -> Vec<String> {
    let mut names = vec![comm.to_string()];
    let interpreter = ["python", "perl", "bash", "sh", "ruby", "node"].iter().any(|i| comm.starts_with(i));
    if interpreter {
        if let Some(script) = args.iter().find(|a| !a.starts_with('-')) {
            names.push(script.rsplit('/').next().unwrap_or(script).to_string());
        }
    }
    names
}

/// Read /proc for what uses pacman's lock file `lock`; our own process is
/// left out. A process with the file open holds it, whatever it is called,
/// but only our own processes' files can be looked at (root's are closed to
/// us), so names and command lines are read too.
pub fn lock_scan_in(proc_root: &Path, lock: &Path) -> LockScan {
    let me = std::process::id().to_string();
    let mut scan = LockScan::default();
    // The positive control: init is root's, so where it cannot be read,
    // neither can a pacman run as root.
    let init = proc_root.join("1");
    let init_seen = std::fs::read_to_string(init.join("comm")).is_ok_and(|c| !c.trim().is_empty())
        && std::fs::read(init.join("cmdline")).is_ok();
    scan.hidden = !init_seen;
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        scan.hidden = true;
        return scan;
    };
    for e in entries.flatten() {
        let pid = e.file_name().to_string_lossy().into_owned();
        if !pid.bytes().all(|b| b.is_ascii_digit()) || pid == me {
            continue;
        }
        let comm = match std::fs::read_to_string(e.path().join("comm")) {
            Ok(c) => c.trim().to_string(),
            // Gone since the listing: it holds nothing now.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                scan.hidden = true;
                continue;
            }
        };
        let cmdline = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
        let args: Vec<String> = cmdline
            .split(|b| *b == 0)
            .skip(1)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        let has_open = std::fs::read_dir(e.path().join("fd"))
            .map(|fds| fds.flatten().any(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == lock)))
            .unwrap_or(false);
        let names = program_names(&comm, &args);
        let name = names.last().cloned().unwrap_or_default();
        if has_open || names.iter().any(|n| locking(n, &args)) {
            scan.running.push(format!("{name} ({pid})"));
        } else if names.iter().any(|n| DAEMONS.contains(&n.as_str())) {
            scan.daemons.push(format!("{name} ({pid})"));
        }
    }
    scan.running.sort();
    scan.daemons.sort();
    scan
}

/// Processes that hold pacman's lock, as "name (pid)". Our own process is
/// left out.
pub fn lockers_in(proc_root: &Path) -> Vec<String> {
    lock_scan_in(proc_root, Path::new(DB_LOCK)).running
}

/// Whether pacman's lock file is there and nothing uses it.
pub fn lock_is_stale() -> bool {
    Path::new(DB_LOCK).exists() && lock_scan_in(Path::new("/proc"), Path::new(DB_LOCK)).nothing_uses_it()
}

/// Free bytes for an unprivileged user on the filesystem holding `path`.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let st = rustix::fs::statvfs(path).ok()?;
    Some(st.f_bavail.saturating_mul(st.f_frsize))
}

/// Free bytes summed over the filesystems holding `paths`, each counted
/// once. btrfs subvolumes report different devices for one shared pool, so
/// filesystems are told apart by size, as Health's disk check does.
pub fn free_total(paths: &[&Path]) -> Option<u64> {
    let mut seen = Vec::new();
    let mut total = 0u64;
    for p in paths {
        let Ok(st) = rustix::fs::statvfs(*p) else { continue };
        let size = st.f_blocks.saturating_mul(st.f_frsize);
        if seen.contains(&size) {
            continue;
        }
        seen.push(size);
        total += st.f_bavail.saturating_mul(st.f_frsize);
    }
    (!seen.is_empty()).then_some(total)
}

// ---- before the update ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub on_battery: bool,
}

/// The laptop battery under `/sys/class/power_supply`, or None without one.
/// A mouse's battery ("scope" Device) is not the machine's.
pub fn battery_in(root: &Path) -> Option<Battery> {
    let read = |p: PathBuf| std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(root).ok()?.flatten().map(|e| e.path()).collect();
    entries.sort();
    let mut mains_online = false;
    let mut found: Option<(u8, bool)> = None;
    for e in entries {
        match read(e.join("type")).as_str() {
            "Mains" | "USB" => mains_online |= read(e.join("online")) == "1",
            "Battery" if read(e.join("scope")) != "Device" => {
                let Ok(pct) = read(e.join("capacity")).parse::<u8>() else { continue };
                let discharging = read(e.join("status")) == "Discharging";
                // Two batteries (some ThinkPads): the lower one is the worry.
                if found.is_none_or(|(p, _)| pct < p) {
                    found = Some((pct, discharging));
                }
            }
            _ => {}
        }
    }
    found.map(|(percent, discharging)| Battery { percent, on_battery: discharging && !mains_online })
}

/// What is looked at before an update starts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Preflight {
    pub free_root: Option<u64>,
    pub lock: LockScan,
    pub lock_file: bool,
    pub battery: Option<Battery>,
}

pub fn probe_preflight() -> Preflight {
    // Demo mode: the machine is not looked at, and nothing stands in the way.
    if crate::demo::active() {
        return Preflight { free_root: Some(50 * GIB), ..Default::default() };
    }
    Preflight {
        free_root: free_bytes(Path::new("/")),
        lock: lock_scan_in(Path::new("/proc"), Path::new(DB_LOCK)),
        lock_file: Path::new(DB_LOCK).exists(),
        battery: battery_in(Path::new("/sys/class/power_supply")),
    }
}

/// What the post-checks of a demo run find: everything went through.
fn demo_after() -> After {
    After {
        pending: Some(BTreeSet::new()),
        units: Some(BTreeSet::new()),
        rebuild: None,
        rebuild_tool: false,
        kernel_gone: Some(false),
        stale_lock: false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Warn,
    /// the update would fail: no "Continue anyway"
    Block,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Issue {
    pub level: Level,
    pub title: String,
    pub detail: String,
    pub fix: Option<Repair>,
}

/// What stands in the way of an update. `system`: a pacman update is part
/// of it (the lock only matters then).
pub fn preflight_issues(p: &Preflight, system: bool) -> Vec<Issue> {
    let mut out = Vec::new();
    if let Some(free) = p.free_root {
        let size = crate::maintenance::fmt_size(free as f64 / 1048576.0);
        if free < BLOCK_FREE {
            out.push(Issue {
                level: Level::Block,
                title: "Not enough free space".into(),
                detail: format!(
                    "Only {size} is free on /. An update downloads every package before \
                     installing any, and would stop part-way. Free some space first — \
                     Health can trim the package cache."
                ),
                fix: None,
            });
        } else if free < WARN_FREE {
            out.push(Issue {
                level: Level::Warn,
                title: "Free space is low".into(),
                detail: format!("{size} is free on /. A large update may not fit."),
                fix: None,
            });
        }
    }
    // A service that is running only matters when a lock file is there:
    // then it may be the service's.
    let mut holders = p.lock.running.clone();
    if p.lock_file {
        holders.extend(p.lock.daemons.iter().cloned());
    }
    if system && !holders.is_empty() {
        out.push(Issue {
            level: Level::Block,
            title: "Another package manager is running".into(),
            detail: format!(
                "{} is running. Only one can change packages at a time; wait for it to \
                 finish, then update.",
                holders.join(", ")
            ),
            fix: None,
        });
    } else if system && p.lock_file && p.lock.hidden {
        // Without every process in view, "nothing uses it" cannot be said,
        // and removing a live lock breaks the update that holds it.
        out.push(Issue {
            level: Level::Warn,
            title: "pacman’s lock file is there".into(),
            detail: format!(
                "{DB_LOCK} is there, and this system hides other users’ programs, so Almanac \
                 cannot tell whether a package manager is using it. If one is, wait for it to \
                 finish; pacman will not start while the file is there. If none is, an earlier \
                 update was interrupted, and the file has to be removed by hand."
            ),
            fix: None,
        });
    } else if system && p.lock_file {
        out.push(Issue {
            level: Level::Warn,
            title: "pacman’s lock file was left behind".into(),
            detail: format!(
                "{DB_LOCK} is there, but nothing that uses it is running — an earlier \
                 update was interrupted. The update would refuse to start until it is removed."
            ),
            fix: Some(Repair::RemoveLock),
        });
    }
    if let Some(b) = p.battery {
        if b.on_battery && b.percent < LOW_BATTERY {
            out.push(Issue {
                level: Level::Warn,
                title: format!("Battery at {}%", b.percent),
                detail: "Plug in first: an update cut short by a flat battery can leave \
                         the system unable to start."
                    .into(),
                fix: None,
            });
        }
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.level));
    out
}

// ---- the report -----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pass,
    Fail,
    /// the check could not tell
    Unknown,
    /// worth knowing, not a failure
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckId {
    Exit,
    Pending,
    Log,
    Keyring,
    Scriptlets,
    Initramfs,
    Dkms,
    Builds,
    Lock,
    Units,
    Rebuild,
    Kernel,
    Pacnew,
    Warnings,
    Item,
    Held,
}

/// An automatic repair for a failed check.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Repair {
    RemoveLock,
    Keyring,
    Retry(Vec<String>),
    Rebuild(Vec<String>),
    Restart { system: Vec<String>, user: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub id: CheckId,
    pub title: String,
    pub status: Status,
    pub detail: String,
    pub items: Vec<String>,
    pub repair: Option<Repair>,
}

impl Check {
    fn new(id: CheckId, status: Status, title: impl Into<String>, detail: impl Into<String>) -> Check {
        Check { id, title: title.into(), status, detail: detail.into(), items: Vec::new(), repair: None }
    }

    fn items(mut self, items: impl IntoIterator<Item = String>) -> Check {
        self.items = items.into_iter().collect();
        self
    }

    fn repair(mut self, r: Repair) -> Check {
        self.repair = Some(r);
        self
    }

    pub fn needs_attention(&self) -> bool {
        matches!(self.status, Status::Fail | Status::Unknown)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    pub checks: Vec<Check>,
    /// what pacman changed, from its log
    pub changes: Vec<Change>,
    /// the log section and the terminal's output, for "Show log"
    pub log: String,
}

/// Everything the checks of a system update look at.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inputs {
    /// a pacman/AUR update was part of it
    pub system: bool,
    pub exit: Option<i32>,
    /// None: pacman.log could not be read
    pub log: Option<LogScan>,
    pub before: Before,
    pub after: After,
}

fn listing(names: &[String], n: usize) -> String {
    let mut s = names.iter().take(n).cloned().collect::<Vec<_>>().join(", ");
    if names.len() > n {
        s.push('…');
    }
    s
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Judge a system update from what the probes found.
pub fn build_report(i: &Inputs) -> Report {
    let mut checks = Vec::new();
    let scan = i.log.clone().unwrap_or_default();

    checks.push(match i.exit {
        Some(0) => Check::new(CheckId::Exit, Status::Pass, "Update command", "Finished without an error."),
        Some(n) => Check::new(
            CheckId::Exit,
            Status::Fail,
            "Update command",
            format!("It stopped with an error (exit status {n})."),
        ),
        None => Check::new(
            CheckId::Exit,
            Status::Unknown,
            "Update command",
            "Could not tell how it ended — the terminal closed before it reported back.",
        ),
    });

    if i.system {
        checks.push(pending_check(&i.before.pending, &i.after.pending));
    }

    match &i.log {
        None => checks.push(Check::new(
            CheckId::Log,
            Status::Unknown,
            "pacman’s log",
            format!("Could not read {PACMAN_LOG}."),
        )),
        Some(scan) => checks.extend(log_checks(scan)),
    }

    if i.after.stale_lock {
        checks.push(
            Check::new(
                CheckId::Lock,
                Status::Fail,
                "pacman’s lock file was left behind",
                "Nothing is running that uses it, but pacman will refuse to run until it is removed.",
            )
            .repair(Repair::RemoveLock),
        );
    }

    checks.push(units_check(&i.before.units, &i.after.units));

    if i.system && i.after.rebuild_tool {
        checks.push(rebuild_check(&i.before.rebuild, &i.after.rebuild));
    }

    if i.after.kernel_gone == Some(true) {
        checks.push(Check::new(
            CheckId::Kernel,
            Status::Info,
            "Restart to finish",
            "The kernel was updated. Until a restart, the running kernel cannot load \
             modules — a USB drive or a VPN may not work.",
        ));
    }

    let configs: Vec<String> = scan.pacnew.iter().chain(&scan.pacsave).cloned().collect();
    if !configs.is_empty() {
        checks.push(
            Check::new(
                CheckId::Pacnew,
                Status::Info,
                format!("{} to merge", plural(configs.len(), "new config file", "new config files")),
                "Not merged automatically: merging needs your decisions, file by file. \
                 Health lists them under Unmerged config files.",
            )
            .items(configs),
        );
    }
    if !scan.warnings.is_empty() {
        checks.push(
            Check::new(CheckId::Warnings, Status::Info, "Warnings from pacman", "Shown for reference.")
                .items(scan.warnings.clone()),
        );
    }

    // A keyring failure, a lock or packages left behind is also why the
    // command failed: repairing those is repairing it.
    let cause: Vec<Repair> = checks
        .iter()
        .filter(|c| c.status == Status::Fail)
        .filter_map(|c| c.repair.clone())
        .filter(|r| matches!(r, Repair::Keyring | Repair::RemoveLock | Repair::Retry(_)))
        .collect();
    if let Some(exit) = checks.iter_mut().find(|c| c.id == CheckId::Exit && c.status == Status::Fail) {
        if let Some(r) = cause.into_iter().next() {
            exit.repair = Some(r);
        }
    }

    Report { checks, changes: scan.changes, log: String::new() }
}

fn pending_check(before: &Option<BTreeSet<String>>, after: &Option<BTreeSet<String>>) -> Check {
    let (Some(before), Some(after)) = (before, after) else {
        return Check::new(
            CheckId::Pending,
            Status::Unknown,
            "Updates installed",
            "Could not ask what is still waiting (checkupdates or the AUR did not answer).",
        );
    };
    let left: Vec<String> = before.intersection(after).cloned().collect();
    if left.is_empty() {
        let detail = if before.is_empty() {
            "Nothing was waiting.".to_string()
        } else {
            format!("All {} installed.", plural(before.len(), "update", "updates"))
        };
        return Check::new(CheckId::Pending, Status::Pass, "Updates installed", detail);
    }
    Check::new(
        CheckId::Pending,
        Status::Fail,
        "Updates installed",
        format!(
            "{} still waiting: {}.",
            plural(left.len(), "update is", "updates are"),
            listing(&left, 6)
        ),
    )
    .items(left.clone())
    .repair(Repair::Retry(left))
}

fn log_checks(scan: &LogScan) -> Vec<Check> {
    let mut out = Vec::new();
    if !scan.keyring.is_empty() {
        out.push(
            Check::new(
                CheckId::Keyring,
                Status::Fail,
                "Package signatures",
                "Packages could not be verified: the keyring that checks them is out of date \
                 or damaged. Nothing unverified was installed.",
            )
            .items(scan.keyring.clone())
            .repair(Repair::Keyring),
        );
    }
    if !scan.initramfs.is_empty() {
        out.push(
            Check::new(
                CheckId::Initramfs,
                Status::Fail,
                "Boot image",
                "The boot image (initramfs) was not rebuilt. Do not restart until this is \
                 sorted out — see the log.",
            )
            .items(scan.initramfs.clone()),
        );
    }
    if !scan.dkms.is_empty() {
        out.push(
            Check::new(
                CheckId::Dkms,
                Status::Fail,
                "Kernel modules (DKMS)",
                "A kernel module did not build for the new kernel; the hardware it drives may \
                 not work after a restart.",
            )
            .items(scan.dkms.clone()),
        );
    }
    if !scan.scriptlets.is_empty() {
        out.push(
            Check::new(
                CheckId::Scriptlets,
                Status::Fail,
                "Install scripts and hooks",
                format!("{} failed.", listing(&scan.scriptlets, 4)),
            )
            .items(scan.scriptlets.clone()),
        );
    }
    if !scan.builds.is_empty() {
        out.push(
            Check::new(
                CheckId::Builds,
                Status::Fail,
                "AUR builds",
                format!("{} could not be built.", listing(&scan.builds, 4)),
            )
            .items(scan.builds.clone()),
        );
    }
    let others: Vec<String> = scan.errors.iter().filter(|e| !(scan.locked && LOCKED.is_match(e))).cloned().collect();
    if !others.is_empty() {
        out.push(
            Check::new(CheckId::Log, Status::Fail, "pacman’s log", format!("{}.", plural(others.len(), "error", "errors")))
                .items(others),
        );
    }
    if out.is_empty() {
        out.push(Check::new(CheckId::Log, Status::Pass, "pacman’s log", "No errors."));
    }
    out
}

fn units_check(before: &Option<BTreeSet<String>>, after: &Option<BTreeSet<String>>) -> Check {
    let Some(after) = after else {
        return Check::new(CheckId::Units, Status::Unknown, "Services", "Could not ask systemd which services failed.");
    };
    let new: Vec<String> = match before {
        Some(b) => after.difference(b).cloned().collect(),
        None => after.iter().cloned().collect(),
    };
    if new.is_empty() {
        return Check::new(CheckId::Units, Status::Pass, "Services", "No service failed since the update began.");
    }
    let unit = |scope: &str| -> Vec<String> {
        new.iter().filter_map(|u| u.strip_prefix(scope).map(str::to_string)).collect()
    };
    let (system, user) = (unit("system "), unit("user "));
    let mut c = Check::new(
        CheckId::Units,
        if before.is_some() { Status::Fail } else { Status::Unknown },
        "Services",
        if before.is_some() {
            format!("{} since the update began.", plural(new.len(), "service failed", "services failed"))
        } else {
            format!("{} failed now; it is not known whether before.", plural(new.len(), "service has", "services have"))
        },
    )
    .items(new.clone());
    if !system.is_empty() || !user.is_empty() {
        c = c.repair(Repair::Restart { system, user });
    }
    c
}

fn rebuild_check(before: &Option<BTreeSet<String>>, after: &Option<BTreeSet<String>>) -> Check {
    let Some(after) = after else {
        return Check::new(CheckId::Rebuild, Status::Unknown, "AUR packages", "checkrebuild did not answer.");
    };
    // Flagged before the update is not this update's doing (and a -bin
    // package checkrebuild always flags would otherwise fail every run).
    let new: Vec<String> = after.difference(&before.clone().unwrap_or_default()).cloned().collect();
    if new.is_empty() {
        return Check::new(CheckId::Rebuild, Status::Pass, "AUR packages", "None needs rebuilding.");
    }
    Check::new(
        CheckId::Rebuild,
        Status::Fail,
        "AUR packages",
        format!(
            "{} built against libraries this update replaced: {}.",
            plural(new.len(), "package was", "packages were"),
            listing(&new, 6)
        ),
    )
    .items(new.clone())
    .repair(Repair::Rebuild(new))
}

impl Report {
    pub fn attention(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| c.needs_attention()).collect()
    }

    pub fn passed(&self) -> bool {
        self.attention().is_empty()
    }

    pub fn notes(&self) -> Vec<&Check> {
        self.checks.iter().filter(|c| c.status == Status::Info).collect()
    }

    /// The repairs for what failed, each once, in the order they should
    /// run: the lock first (nothing else can run while it is there), the
    /// keyring before anything that installs.
    pub fn repairs(&self) -> Vec<Repair> {
        let mut out: Vec<Repair> = self
            .checks
            .iter()
            .filter(|c| c.needs_attention())
            .filter_map(|c| c.repair.clone())
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Fold in the items updated outside the terminal, each judged by its
    /// own probe, and what was held back.
    pub fn add_items(&mut self, outcomes: &[(String, Outcome)], held: &[String]) {
        for (name, out) in outcomes {
            let status = match out.changed {
                Some(true) => Status::Pass,
                Some(false) => Status::Fail,
                None => Status::Unknown,
            };
            self.checks.push(Check::new(CheckId::Item, status, name.clone(), out.message.clone()));
        }
        if !held.is_empty() {
            self.checks.push(
                Check::new(CheckId::Held, Status::Info, "Held back", "Each keeps its own Update button.")
                    .items(held.to_vec()),
            );
        }
    }

    /// The one line a toast has room for, when everything passed.
    pub fn summary(&self) -> String {
        let n = self.changes.iter().filter(|c| !c.to.is_empty()).count()
            + self.checks.iter().filter(|c| c.id == CheckId::Item && c.status == Status::Pass).count();
        let mut s = if n == 0 {
            "Everything is up to date — every check passed".to_string()
        } else {
            format!("{} installed — every check passed", plural(n, "update", "updates"))
        };
        for c in self.notes() {
            match c.id {
                CheckId::Kernel => s.push_str(" · restart to finish"),
                CheckId::Pacnew => s.push_str(&format!(" · {} (see Health)", c.title.to_lowercase())),
                _ => {}
            }
        }
        s
    }

    /// Re-judge after the repairs: each check that had a repair is asked
    /// again; what had none (a failed install script, say) stands as it was.
    pub fn recheck(&self, before: &Before, after: &After) -> Report {
        let mut checks: Vec<Check> = Vec::new();
        for c in &self.checks {
            if c.repair.is_none() || !c.needs_attention() {
                checks.push(c.clone());
                continue;
            }
            let again = match c.id {
                CheckId::Pending => pending_check(&before.pending, &after.pending),
                CheckId::Units => {
                    // The units this run found failed, asked again.
                    let now = after.units.clone().map(|u| {
                        u.into_iter().filter(|x| c.items.contains(x)).collect::<BTreeSet<_>>()
                    });
                    units_check(&Some(BTreeSet::new()), &now)
                }
                CheckId::Rebuild => rebuild_check(&before.rebuild, &after.rebuild),
                CheckId::Lock => {
                    if after.stale_lock {
                        c.clone()
                    } else {
                        Check::new(CheckId::Lock, Status::Pass, c.title.clone(), "Removed.")
                    }
                }
                // Signatures, and a command that failed for a cause the
                // repairs went after: fixed when what was waiting is now in.
                CheckId::Keyring | CheckId::Exit => {
                    let pending = pending_check(&before.pending, &after.pending);
                    if pending.status == Status::Pass && !after.stale_lock {
                        Check::new(c.id, Status::Pass, c.title.clone(), "Repaired: nothing is left waiting.")
                    } else {
                        c.clone()
                    }
                }
                _ => c.clone(),
            };
            checks.push(again);
        }
        Report { checks, changes: self.changes.clone(), log: self.log.clone() }
    }
}

// ---- repairs and the tidy-up ----------------------------------------------------

/// The steps that carry out `repairs`, in order. Root goes through pkexec
/// by absolute path; yay builds as this user.
pub fn repair_steps(repairs: &[Repair], have_yay: bool) -> Vec<Step> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let mut out = Vec::new();
    for r in repairs {
        match r {
            Repair::RemoveLock => {
                out.push(Step::call("Removing pacman’s stale lock file", Arc::new(remove_stale_lock)));
            }
            Repair::Keyring => {
                out.push(Step::argv(
                    "Reloading the Arch keys",
                    s(&[tasks::PKEXEC, "/usr/bin/pacman-key", "--populate", "archlinux"]),
                ));
                out.push(Step::argv(
                    "Updating the keyring",
                    s(&[tasks::PKEXEC, "/usr/bin/pacman", "-Sy", "--needed", "--noconfirm", "archlinux-keyring"]),
                ));
            }
            Repair::Retry(names) => {
                let argv = if have_yay {
                    tasks::yay_install(names)
                } else {
                    let mut a = s(&[tasks::PKEXEC, "/usr/bin/pacman", "-S", "--needed", "--noconfirm"]);
                    a.extend(names.iter().cloned());
                    a
                };
                out.push(Step::argv("Installing what is still waiting", argv));
            }
            Repair::Rebuild(names) if have_yay => {
                out.push(Step::argv("Rebuilding AUR packages", rebuild_argv(names)));
            }
            Repair::Rebuild(_) => {}
            Repair::Restart { system, user } => {
                if !user.is_empty() {
                    let mut a = s(&["systemctl", "--user", "restart"]);
                    a.extend(user.iter().cloned());
                    out.push(Step::argv("Restarting your services", a));
                }
                if !system.is_empty() {
                    let mut a = s(&[tasks::PKEXEC, "/usr/bin/systemctl", "restart"]);
                    a.extend(system.iter().cloned());
                    out.push(Step::argv("Restarting system services", a));
                }
            }
        }
    }
    out
}

/// What a repair does, for the dialog's "Fix will" list; None for one
/// that cannot run here (a rebuild without yay).
pub fn describe(r: &Repair, have_yay: bool) -> Option<String> {
    Some(match r {
        Repair::RemoveLock => "Remove pacman’s stale lock file (nothing is using it)".to_string(),
        Repair::Keyring => "Reload the Arch signing keys and update archlinux-keyring".to_string(),
        Repair::Retry(names) => format!("Install what is still waiting: {}", listing(names, 6)),
        Repair::Rebuild(names) if have_yay => format!("Rebuild with yay: {}", listing(names, 6)),
        Repair::Rebuild(_) => return None,
        Repair::Restart { system, user } => {
            let all: Vec<String> = system.iter().chain(user).cloned().collect();
            format!("Restart {}", listing(&all, 6))
        }
    })
}

/// Package updates that were waiting before and are not now: installed.
pub fn installed_since(before: &Before, after: &After) -> Vec<String> {
    match (&before.pending, &after.pending) {
        (Some(b), Some(a)) => b.difference(a).cloned().collect(),
        _ => Vec::new(),
    }
}

/// "Show log": the update's section of pacman.log, then the terminal's
/// output as `script` recorded it.
pub fn log_text(section: Option<&str>, output: &str) -> String {
    let mut t = match section {
        Some(s) if !s.trim().is_empty() => format!("{PACMAN_LOG}\n\n{}", s.trim_end()),
        Some(_) => format!("Nothing was written to {PACMAN_LOG}."),
        None => format!("{PACMAN_LOG} could not be read."),
    };
    let out = clean_output(output);
    if !out.trim().is_empty() {
        t += &format!("\n\nIn the terminal\n\n{}", out.trim());
    }
    t
}

/// `yay -S --rebuild`: build the targets even though they are installed
/// and current. No `--needed`, which would skip exactly those.
pub fn rebuild_argv(names: &[String]) -> Vec<String> {
    let mut argv: Vec<String> = [crate::inventory::YAY, "-S", "--rebuild", "--noconfirm", "--sudo", tasks::PKEXEC, "--pacman", "/usr/bin/pacman"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(names.iter().cloned());
    argv
}

/// The lock is removed only while nothing that takes it is running, asked
/// again right before: a pacman started since must keep its lock.
fn remove_stale_lock(log: LogFn<'_>) -> Result<(), String> {
    if crate::demo::active() {
        return Err("not removed: demo mode does not touch the machine".to_string());
    }
    if !Path::new(DB_LOCK).exists() {
        log("already gone");
        return Ok(());
    }
    let scan = lock_scan_in(Path::new("/proc"), Path::new(DB_LOCK));
    let running: Vec<String> = scan.running.iter().chain(&scan.daemons).cloned().collect();
    if !running.is_empty() {
        return Err(format!("not removed: {} is running", running.join(", ")));
    }
    if !scan.nothing_uses_it() {
        return Err("not removed: other users’ programs are hidden here, so it may be in use".to_string());
    }
    let out = crate::util::run(&[tasks::PKEXEC, "/usr/bin/rm", "-f", DB_LOCK], 300);
    if Path::new(DB_LOCK).exists() {
        return Err(if out.code == Some(126) || out.code == Some(127) {
            "not removed: the password was not given".to_string()
        } else {
            format!("not removed (exit status {})", out.code.map_or("?".into(), |c| c.to_string()))
        });
    }
    log(&format!("removed {DB_LOCK}"));
    Ok(())
}

/// After a successful update: the package cache trimmed to two versions
/// (Health's own fix), and the build cache of each AUR package just
/// updated. Empty when there is nothing to do it with.
pub fn cleanup_steps(have_paccache: bool, updated: &[String]) -> Vec<Step> {
    let mut out = Vec::new();
    if have_paccache {
        out.extend(tasks::fix_steps("Trimming the package cache", Some(crate::maintenance::paccache_trim_fix())));
    }
    if let Some(fix) = crate::maintenance::yay_cache_fix_for(updated) {
        out.extend(tasks::fix_steps("Clearing the AUR build cache of what was updated", Some(fix)));
    }
    out
}

/// The filesystems a tidy-up frees space on.
pub fn cache_paths() -> Vec<PathBuf> {
    vec![PathBuf::from(crate::maintenance::PACMAN_CACHE), crate::maintenance::YAY_CACHE.clone()]
}

/// The snapshot this run took, when snapper says so: the newest one dated
/// after `started` (epoch seconds). Listing needs read access to the
/// snapshots, so without it the journal can still say one was taken, any
/// time from the click to the end of the run (`ended`): the snapshot comes
/// when pacman starts, which is after the password, however long that took.
pub fn snapshot_since(started: f64, ended: f64) -> Option<String> {
    let out = crate::util::run(&["/usr/bin/snapper", "--csvout", "-c", "root", "list", "--columns", "number,date"], 20);
    if out.code == Some(0) {
        if let Some(n) = newest_since(&out.stdout, started) {
            return Some(n);
        }
    }
    let (since, until) = snapshot_window(started, ended);
    match crate::snapshots::snapshot_between(since, until, None) {
        Ok(Some(true)) => Some("taken (its number needs root to read)".to_string()),
        _ => None,
    }
}

/// The journal window `snapshot_since` reads: from just before the click
/// (the clocks of journal and Almanac are read a moment apart) to a minute
/// past the end.
pub fn snapshot_window(started: f64, ended: f64) -> (f64, f64) {
    (started - 5.0, ended.max(started) + 60.0)
}

pub fn newest_since(csv: &str, started: f64) -> Option<String> {
    use chrono::TimeZone;
    csv.lines()
        .skip(1)
        .filter_map(|l| {
            let (n, date) = l.split_once(',')?;
            let n: u64 = n.trim().parse().ok()?;
            let dt = chrono::NaiveDateTime::parse_from_str(date.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
            let ts = chrono::Local.from_local_datetime(&dt).earliest()?.timestamp() as f64;
            (ts + 1.0 >= started).then_some(n)
        })
        .max()
        .map(|n| n.to_string())
}

// ---- the history entry ----------------------------------------------------------

/// What one Update all run did, gathered as it goes, for its history entry.
#[derive(Debug, Clone, Default)]
pub struct RunRecord {
    pub commands: Vec<CommandRun>,
    pub changes: Vec<Change>,
    pub report: Option<Report>,
    pub recheck: Option<Report>,
    pub freed: Option<i64>,
    pub snapshot: Option<String>,
}

fn check_result(c: &Check) -> CheckResult {
    let ok = match c.status {
        Status::Pass | Status::Info => Some(true),
        Status::Fail => Some(false),
        Status::Unknown => None,
    };
    let mut detail = c.detail.clone();
    if !c.items.is_empty() {
        detail = format!("{detail} [{}]", c.items.join(", "));
    }
    CheckResult { name: c.title.clone(), ok, detail }
}

impl RunRecord {
    /// The report the run ended with.
    pub fn last(&self) -> Option<&Report> {
        self.recheck.as_ref().or(self.report.as_ref())
    }

    pub fn failed(&self) -> bool {
        self.last().is_some_and(|r| !r.passed())
    }

    pub fn action(&self, secs: f64) -> ActionLog {
        let mut changes = self.changes.clone();
        if let Some(r) = &self.report {
            changes.extend(r.changes.iter().cloned());
        }
        ActionLog {
            commands: self.commands.clone(),
            changes,
            checks: self.report.iter().flat_map(|r| r.checks.iter().map(check_result)).collect(),
            // Each repaired check as the recheck found it.
            repairs: match (&self.report, &self.recheck) {
                (Some(first), Some(again)) => first
                    .checks
                    .iter()
                    .zip(&again.checks)
                    .filter(|(c, _)| c.repair.is_some() && c.needs_attention())
                    .map(|(_, c)| check_result(c))
                    .collect(),
                _ => Vec::new(),
            },
            freed_bytes: self.freed,
            snapshot: self.snapshot.clone(),
            duration_secs: Some(secs),
        }
    }

    /// The history line, folded.
    pub fn detail(&self) -> String {
        let mut parts = Vec::new();
        let n = self.changes.len() + self.report.as_ref().map_or(0, |r| r.changes.len());
        if n > 0 {
            parts.push(plural(n, "change", "changes"));
        }
        match self.last() {
            Some(r) if r.passed() => parts.push("every check passed".to_string()),
            Some(r) => parts.push(format!("{} not passed", plural(r.attention().len(), "check", "checks"))),
            None => {}
        }
        if self.recheck.is_some() {
            parts.push("repaired".to_string());
        }
        if let Some(b) = self.freed.filter(|b| *b > 0) {
            parts.push(format!("{} freed", crate::maintenance::fmt_size(b as f64 / 1048576.0)));
        }
        parts.join(" · ")
    }

    /// "warn" when something failed in the end; a run with no report is
    /// judged by its commands.
    pub fn severity(&self) -> &'static str {
        let bad_exit = self.commands.iter().any(|c| c.exit != Some(0));
        if self.failed() || (self.last().is_none() && bad_exit) {
            "warn"
        } else {
            ""
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A clean upgrade, as this machine's own log has them.
    const CLEAN: &str = "\
[2026-10-20T13:43:02-0700] [PACMAN] Running 'pacman -Sy --needed --noconfirm archlinux-keyring'
[2026-10-20T13:43:02-0700] [PACMAN] synchronizing package lists
[2026-10-20T13:43:03-0700] [PACMAN] Running 'pacman -Syu --noconfirm --overwrite /usr/share/omarchy/*'
[2026-10-20T13:43:03-0700] [PACMAN] starting full system upgrade
[2026-10-20T13:43:31-0700] [ALPM] running '00-omarchy-update-guard.hook'...
[2026-10-20T13:43:31-0700] [ALPM] running '10-limine-snapper-lock.hook'...
[2026-10-20T13:43:31-0700] [ALPM] transaction started
[2026-10-20T13:43:32-0700] [ALPM] upgraded 1password (8.12.40-3 -> 8.12.42-1)
[2026-10-20T13:43:33-0700] [ALPM] upgraded mise-bin (2026.10.13-1 -> 2026.10.15-1)
[2026-10-20T13:43:33-0700] [ALPM-SCRIPTLET] Applying Omarchy etc-overrides...
[2026-10-20T13:43:33-0700] [ALPM] upgraded omarchy (4.0.5-1 -> 4.0.6-1)
[2026-10-20T13:43:33-0700] [ALPM] transaction completed
[2026-10-20T13:43:34-0700] [ALPM] running '35-systemd-update.hook'...
[2026-10-20T13:43:34-0700] [ALPM] running 'rebuild-detector.hook'...
[2026-10-19T14:33:24-0700] [ALPM-SCRIPTLET] ==> WARNING: Possibly missing firmware for module: 'qat_6xxx'
[2026-10-19T14:33:25-0700] [ALPM-SCRIPTLET] ==> Initcpio image generation successful
";

    #[test]
    fn a_clean_upgrade_lists_its_changes_and_nothing_else() {
        let s = scan_log(CLEAN);
        assert_eq!(s.changes.len(), 3);
        assert_eq!(s.changes[0], Change { name: "1password".into(), from: "8.12.40-3".into(), to: "8.12.42-1".into() });
        assert!(s.errors.is_empty() && s.warnings.is_empty() && s.scriptlets.is_empty());
        assert!(s.initramfs.is_empty(), "a missing-firmware warning is not a failed boot image");
        assert!(!s.locked && s.keyring.is_empty());
    }

    #[test]
    fn installs_removals_and_reinstalls_are_changes_too() {
        let s = scan_log(
            "[2026-10-24T12:23:33-0700] [ALPM] installed almanac (2.1.6-1)\n\
             [2026-10-19T14:36:01-0700] [ALPM] removed gst-plugin-gtk (1.28.8-1)\n\
             [2026-10-19T14:36:01-0700] [ALPM] reinstalled foo (1-1)\n\
             [2026-10-19T14:36:01-0700] [ALPM] downgraded bar (2-1 -> 1-1)\n",
        );
        let pairs: Vec<(&str, &str, &str)> =
            s.changes.iter().map(|c| (c.name.as_str(), c.from.as_str(), c.to.as_str())).collect();
        assert_eq!(
            pairs,
            vec![("almanac", "", "2.1.6-1"), ("gst-plugin-gtk", "1.28.8-1", ""), ("foo", "1-1", "1-1"), ("bar", "2-1", "1-1")]
        );
    }

    #[test]
    fn pacnew_and_pacsave_files_are_named() {
        let s = scan_log(
            "[2026-10-20T13:43:33-0700] [ALPM] warning: /etc/pacman.conf installed as /etc/pacman.conf.pacnew\n\
             [2026-10-20T13:43:33-0700] [ALPM] warning: /etc/foo.conf saved as /etc/foo.conf.pacsave\n\
             [2026-10-20T13:43:33-0700] [ALPM] warning: directory permissions differ on /boot/, filesystem: 700  package: 755\n",
        );
        assert_eq!(s.pacnew, strs(&["/etc/pacman.conf.pacnew"]));
        assert_eq!(s.pacsave, strs(&["/etc/foo.conf.pacsave"]));
        assert_eq!(s.warnings.len(), 1, "other warnings are kept for reference");
    }

    /// pacman logs a package's install script before the package's
    /// "upgraded" line, and a hook's failure after "running 'x.hook'".
    #[test]
    fn a_failed_install_script_is_put_to_its_package_and_a_hook_to_itself() {
        let s = scan_log(
            "[2026-10-20T13:43:31-0700] [ALPM] transaction started\n\
             [2026-10-20T13:43:32-0700] [ALPM] upgraded good (1-1 -> 2-1)\n\
             [2026-10-20T13:43:33-0700] [ALPM-SCRIPTLET] something went wrong\n\
             [2026-10-20T13:43:33-0700] [ALPM] error: command failed to execute correctly\n\
             [2026-10-20T13:43:33-0700] [ALPM] upgraded bad (1-1 -> 2-1)\n\
             [2026-10-20T13:43:33-0700] [ALPM] transaction completed\n\
             [2026-10-20T13:43:34-0700] [ALPM] running '90-mkinitcpio-install.hook'...\n\
             [2026-10-20T13:43:34-0700] [ALPM] error: command failed to execute correctly\n",
        );
        assert_eq!(s.scriptlets, strs(&["package bad", "hook '90-mkinitcpio-install.hook'"]));
        assert!(s.errors.is_empty(), "counted once, as the script that failed");
    }

    #[test]
    fn a_boot_image_or_dkms_failure_is_told_apart_from_noise() {
        let s = scan_log(
            "[2026-10-19T14:33:24-0700] [ALPM-SCRIPTLET] ==> WARNING: Possibly missing firmware for module: 'qat_6xxx'\n\
             [2026-10-19T14:33:25-0700] [ALPM-SCRIPTLET] ==> ERROR: Hook 'btrfs-overlayfs' cannot be found\n\
             [2026-10-19T14:33:25-0700] [ALPM-SCRIPTLET] ==> Image generation FAILED: 'default' generation failed\n\
             [2026-10-19T14:33:26-0700] [ALPM-SCRIPTLET] Error! Bad return status for module build on kernel: 6.17.1 (x86_64)\n\
             [2026-10-19T14:33:26-0700] [ALPM-SCRIPTLET] ==> dkms install --no-depmod nvidia/580.95 -k 6.17.1 failed\n",
        );
        assert_eq!(s.initramfs.len(), 2);
        assert_eq!(s.dkms.len(), 2);
    }

    #[test]
    fn signature_and_lock_errors_are_sorted_out_for_their_repairs() {
        let mut s = scan_log("");
        scan_output(
            "Script started on 2026-10-26 10:00:00-07:00 [COMMAND=\"omarchy update\"]\n\
             \x1b[1;31merror:\x1b[0m linux: signature from \"Jan Alexander Steffens (heftig) <heftig@archlinux.org>\" is unknown trust\r\n\
             :: File /var/cache/pacman/pkg/linux-6.17.2.pkg.tar.zst is corrupted (invalid or corrupted package (PGP signature)).\r\n\
             error: failed to commit transaction (invalid or corrupted package (PGP signature))\r\n\
             error: failed to init transaction (unable to lock database)\r\n\
             error: could not lock database: File exists\r\n\
             warning: zsh-5.9-5 is up to date -- skipping\r\n\
              -> error making: hyprshade-git\r\n\
             (12/40) checking package integrity  50%\r(12/40) checking package integrity 100%\r\n",
            &mut s,
        );
        assert_eq!(s.keyring.len(), 2, "{:?}", s.keyring);
        assert!(s.locked);
        assert_eq!(s.builds, strs(&["hyprshade-git"]));
        assert!(s.warnings.is_empty(), "--needed's skipping is not worth a word");
        let r = build_report(&Inputs { exit: Some(1), log: Some(s), ..Default::default() });
        assert!(r.repairs().contains(&Repair::Keyring));
        // The lock line is not counted again as a generic error.
        assert!(r.checks.iter().all(|c| !c.items.iter().any(|i| i.contains("could not lock"))));
    }

    #[test]
    fn terminal_output_loses_its_escapes_and_redraws() {
        assert_eq!(clean_output("\x1b[1m::\x1b[0m Proceed?\r\n50%\r100%\n"), ":: Proceed?\n100%\n");
    }

    #[test]
    fn the_log_after_the_offset_is_this_update_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacman.log");
        std::fs::write(&p, "old line\n").unwrap();
        let at = log_offset(&p).unwrap();
        std::fs::write(&p, "old line\nnew line\n").unwrap();
        assert_eq!(read_log_since(&p, at).as_deref(), Some("new line\n"));
        // A log replaced by a shorter one is read whole.
        std::fs::write(&p, "x\n").unwrap();
        assert_eq!(read_log_since(&p, at).as_deref(), Some("x\n"));
        assert_eq!(read_log_since(&dir.path().join("none"), 0), None);
    }

    #[test]
    fn the_status_file_says_when_it_started_and_how_it_ended() {
        assert_eq!(parse_status(""), RunStatus::default());
        let st = parse_status("start 100\n");
        assert!(!st.finished());
        let st = parse_status("start 100\nexit 1 160\n");
        assert_eq!(st, RunStatus { started: Some(100), exit: Some(1), ended: Some(160) });
        assert_eq!(st.secs(), Some(60.0));
        assert_eq!(parse_status("garbage\nexit x 5\n").exit, None);
    }

    #[test]
    fn the_script_reports_its_status_before_waiting_for_a_key() {
        let files = RunFiles { status: "/run/user/1000/almanac/update-runs/update-a b.status".into(), output: "/r/o.log".into() };
        let s = terminal_script("omarchy update", Some(&files), Some(Path::new("/usr/bin/script")));
        assert!(s.starts_with("printf 'start %s\\n' \"$(date +%s)\" >> '/run/user/1000/almanac/update-runs/update-a b.status';"), "{s}");
        assert!(s.contains("SHELL=/usr/bin/bash /usr/bin/script -qefc 'omarchy update' /r/o.log; rc=$?;"), "{s}");
        let exit_at = s.find("printf 'exit").unwrap();
        assert!(exit_at < s.find("read -n1").unwrap(), "a terminal left open still reports back");
        // Without `script`, the command still runs in a shell of its own.
        let s = terminal_script("a; exit 3", Some(&files), None);
        assert!(s.contains("/usr/bin/bash -c 'a; exit 3'; rc=$?;"), "{s}");
        // Without a status file, as before.
        assert_eq!(
            terminal_script("omarchy update", None, None),
            "/usr/bin/bash -c 'omarchy update'; echo; read -n1 -r -p 'Done — press any key to close'"
        );
    }

    #[test]
    fn run_files_are_private_and_unique() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("almanac/update-runs");
        private_dir(&runs).unwrap();
        assert_eq!(std::fs::metadata(&runs).unwrap().mode() & 0o777, 0o700);
        let a = new_run_files_in(&runs).unwrap();
        let b = new_run_files_in(&runs).unwrap();
        assert_ne!(a.status, b.status);
        for f in [&a.status, &a.output] {
            assert_eq!(std::fs::metadata(f).unwrap().mode() & 0o777, 0o600);
        }
        // A directory opened up is closed again.
        std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&runs).unwrap();
        assert_eq!(std::fs::metadata(&runs).unwrap().mode() & 0o777, 0o700);
        a.remove();
        assert!(!a.status.exists() && !a.output.exists());
    }

    /// The bug this replaces: a terminal that returns at once used to end
    /// with "rescan when the update finishes", and nothing reloaded.
    #[test]
    fn a_terminal_that_returns_at_once_is_watched_until_the_status_comes() {
        let quick = Some(Duration::from_secs(1));
        let running = RunStatus { started: Some(1), ..Default::default() };
        assert_eq!(wait_step(Duration::from_secs(600), quick, &running, true), Wait::Keep);
        let done = parse_status("start 1\nexit 0 9\n");
        assert_eq!(wait_step(Duration::from_secs(600), quick, &done, true), Wait::Done);
        // Never started: given up on, not waited for all night.
        assert!(matches!(wait_step(NO_START + Duration::from_secs(1), quick, &RunStatus::default(), true), Wait::GiveUp(_)));
        assert!(matches!(wait_step(GIVE_UP + Duration::from_secs(1), quick, &running, true), Wait::GiveUp(_)));
        // No status file to watch: nothing to wait for.
        assert!(matches!(wait_step(Duration::from_secs(2), quick, &RunStatus::default(), false), Wait::GiveUp(_)));
    }

    #[test]
    fn a_terminal_that_ran_the_update_itself_is_done_when_it_closes() {
        let slow = Some(Duration::from_secs(90));
        assert_eq!(wait_step(Duration::from_secs(90), slow, &RunStatus::default(), true), Wait::Done);
        assert_eq!(wait_step(Duration::from_secs(30), None, &RunStatus::default(), true), Wait::Keep);
    }

    fn inputs() -> Inputs {
        Inputs {
            system: true,
            exit: Some(0),
            log: Some(scan_log(CLEAN)),
            before: Before { pending: Some(set(&["1password", "mise-bin", "omarchy"])), units: Some(set(&[])), rebuild: Some(set(&[])) },
            after: After { pending: Some(set(&[])), units: Some(set(&[])), rebuild: Some(set(&[])), rebuild_tool: true, kernel_gone: Some(false), stale_lock: false },
        }
    }

    #[test]
    fn a_clean_run_passes_every_check() {
        let r = build_report(&inputs());
        assert!(r.passed(), "{:#?}", r.attention());
        assert!(r.repairs().is_empty());
        assert_eq!(r.summary(), "3 updates installed — every check passed");
    }

    #[test]
    fn what_is_still_waiting_fails_and_is_retried() {
        let mut i = inputs();
        i.exit = Some(1);
        i.after.pending = Some(set(&["omarchy", "newly-released"]));
        let r = build_report(&i);
        let pending = r.checks.iter().find(|c| c.id == CheckId::Pending).unwrap();
        assert_eq!(pending.status, Status::Fail);
        assert_eq!(pending.items, strs(&["omarchy"]), "a release that appeared since is not a failure");
        assert_eq!(r.repairs(), vec![Repair::Retry(strs(&["omarchy"]))], "one repair, for the command and the list");
    }

    #[test]
    fn a_closed_terminal_or_an_unanswered_probe_is_could_not_tell() {
        let mut i = inputs();
        i.exit = None;
        i.after.pending = None;
        i.log = None;
        let r = build_report(&i);
        let unknown: Vec<CheckId> = r.checks.iter().filter(|c| c.status == Status::Unknown).map(|c| c.id).collect();
        assert_eq!(unknown, vec![CheckId::Exit, CheckId::Pending, CheckId::Log]);
        assert!(!r.passed());
    }

    #[test]
    fn only_services_that_failed_since_count_and_each_is_restarted_where_it_runs() {
        let mut i = inputs();
        i.before.units = Some(set(&["system old.service"]));
        i.after.units = Some(set(&["system old.service", "system cups.service", "user waybar.service"]));
        let r = build_report(&i);
        let units = r.checks.iter().find(|c| c.id == CheckId::Units).unwrap();
        assert_eq!(units.status, Status::Fail);
        assert_eq!(units.items, strs(&["system cups.service", "user waybar.service"]));
        let steps = repair_steps(&r.repairs(), true);
        let argvs: Vec<String> = steps
            .iter()
            .filter_map(|s| match &s.action {
                tasks::StepAction::Argv(a) => Some(a.join(" ")),
                _ => None,
            })
            .collect();
        assert_eq!(
            argvs,
            vec!["systemctl --user restart waybar.service", "/usr/bin/pkexec /usr/bin/systemctl restart cups.service"]
        );
    }

    #[test]
    fn only_aur_packages_newly_needing_a_rebuild_are_rebuilt() {
        assert_eq!(parse_checkrebuild("foreign\tzoom\ncore\tlinux\nforeign\tsome-bin\n"), set(&["some-bin", "zoom"]));
        let mut i = inputs();
        i.before.rebuild = Some(set(&["always-flagged-bin"]));
        i.after.rebuild = Some(set(&["always-flagged-bin", "python-thing"]));
        let r = build_report(&i);
        assert_eq!(r.repairs(), vec![Repair::Rebuild(strs(&["python-thing"]))]);
        let steps = repair_steps(&r.repairs(), true);
        let tasks::StepAction::Argv(a) = &steps[0].action else { panic!("an argv") };
        assert_eq!(a.join(" "), "/usr/bin/yay -S --rebuild --noconfirm --sudo /usr/bin/pkexec --pacman /usr/bin/pacman python-thing");
        assert!(!a.contains(&"--needed".to_string()), "--needed would skip the rebuild");
        // Without yay there is nothing to rebuild with.
        assert!(repair_steps(&r.repairs(), false).is_empty());
    }

    #[test]
    fn a_replaced_kernel_and_new_configs_are_notes_not_failures() {
        let mut i = inputs();
        i.after.kernel_gone = Some(true);
        let mut scan = scan_log(CLEAN);
        scan.pacnew.push("/etc/pacman.conf.pacnew".into());
        i.log = Some(scan);
        let r = build_report(&i);
        assert!(r.passed());
        assert_eq!(r.notes().len(), 2);
        assert!(r.summary().ends_with(" · restart to finish · 1 new config file to merge (see Health)"), "{}", r.summary());
        assert!(r.repairs().is_empty(), "configs are never merged automatically");
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("6.17.2-arch1-1")).unwrap();
        assert!(!kernel_gone("6.17.2-arch1-1", dir.path()));
        assert!(kernel_gone("6.17.1-arch1-1", dir.path()));
    }

    #[test]
    fn keyring_repair_refreshes_keys_through_pkexec_by_path() {
        let steps = repair_steps(&[Repair::Keyring, Repair::Retry(strs(&["linux"]))], false);
        let argvs: Vec<String> = steps
            .iter()
            .filter_map(|s| match &s.action {
                tasks::StepAction::Argv(a) => Some(a.join(" ")),
                _ => None,
            })
            .collect();
        assert_eq!(
            argvs,
            vec![
                "/usr/bin/pkexec /usr/bin/pacman-key --populate archlinux",
                "/usr/bin/pkexec /usr/bin/pacman -Sy --needed --noconfirm archlinux-keyring",
                "/usr/bin/pkexec /usr/bin/pacman -S --needed --noconfirm linux",
            ]
        );
        for a in &argvs {
            assert_eq!(tasks::elevate(&a.split(' ').map(String::from).collect::<Vec<_>>()).join(" "), *a, "not wrapped twice");
        }
    }

    #[test]
    fn each_repair_is_described_before_it_runs() {
        assert_eq!(describe(&Repair::Retry(strs(&["a", "b"])), true).unwrap(), "Install what is still waiting: a, b");
        assert_eq!(describe(&Repair::Rebuild(strs(&["x"])), false), None, "not offered without yay");
        let r = Repair::Restart { system: strs(&["cups.service"]), user: strs(&["waybar.service"]) };
        assert_eq!(describe(&r, true).unwrap(), "Restart cups.service, waybar.service");
    }

    #[test]
    fn the_log_shown_is_the_section_then_the_terminal() {
        let t = log_text(Some("[x] [ALPM] upgraded a (1 -> 2)\n"), "\x1b[1mdone\x1b[0m\r\n");
        assert_eq!(t, "/var/log/pacman.log\n\n[x] [ALPM] upgraded a (1 -> 2)\n\nIn the terminal\n\ndone");
        assert_eq!(log_text(None, ""), "/var/log/pacman.log could not be read.");
        let b = Before { pending: Some(set(&["a", "b"])), ..Default::default() };
        let a = After { pending: Some(set(&["b", "c"])), ..Default::default() };
        assert_eq!(installed_since(&b, &a), strs(&["a"]));
        assert!(installed_since(&b, &After::default()).is_empty(), "not known: nothing is marked");
    }

    #[test]
    fn repairs_run_lock_first_and_keyring_before_installs() {
        let r = Report {
            checks: vec![
                Check::new(CheckId::Pending, Status::Fail, "", "").repair(Repair::Retry(strs(&["a"]))),
                Check::new(CheckId::Keyring, Status::Fail, "", "").repair(Repair::Keyring),
                Check::new(CheckId::Lock, Status::Fail, "", "").repair(Repair::RemoveLock),
                Check::new(CheckId::Exit, Status::Fail, "", "").repair(Repair::Keyring),
                Check::new(CheckId::Units, Status::Pass, "", "").repair(Repair::Keyring),
            ],
            ..Default::default()
        };
        assert_eq!(r.repairs(), vec![Repair::RemoveLock, Repair::Keyring, Repair::Retry(strs(&["a"]))]);
    }

    #[test]
    fn the_recheck_asks_again_only_what_was_repaired() {
        let mut i = inputs();
        i.exit = Some(1);
        i.log = Some(LogScan { keyring: strs(&["error: signature is unknown trust"]), scriptlets: strs(&["package x"]), ..Default::default() });
        i.after.pending = Some(set(&["omarchy"]));
        let r = build_report(&i);
        assert!(r.repairs().contains(&Repair::Keyring));
        let mut later = i.after.clone();
        later.pending = Some(set(&[]));
        let again = r.recheck(&i.before, &later);
        let status = |id| again.checks.iter().find(|c| c.id == id).unwrap().status;
        assert_eq!(status(CheckId::Pending), Status::Pass);
        assert_eq!(status(CheckId::Keyring), Status::Pass);
        assert_eq!(status(CheckId::Exit), Status::Pass);
        assert_eq!(status(CheckId::Scriptlets), Status::Fail, "no repair for it: it stands");
        // Still waiting: still failed.
        let again = r.recheck(&i.before, &i.after);
        assert_eq!(again.checks.iter().find(|c| c.id == CheckId::Keyring).unwrap().status, Status::Fail);
    }

    #[test]
    fn batch_items_fold_into_the_same_report() {
        let mut r = build_report(&inputs());
        r.add_items(
            &[
                ("Ghostty".into(), Outcome::new(Some(true), "Ghostty 1 → 2")),
                ("claude".into(), Outcome::new(Some(false), "claude did not change")),
                ("plugin".into(), Outcome::new(None, "plugin: could not be checked")),
            ],
            &["git-thing — has local edits".into()],
        );
        let attention: Vec<&str> = r.attention().iter().map(|c| c.title.as_str()).collect();
        assert_eq!(attention, vec!["claude", "plugin"]);
        assert_eq!(r.notes().last().unwrap().items, strs(&["git-thing — has local edits"]));
    }

    #[test]
    fn queries_never_count_as_holding_the_lock() {
        let q = |c: &str, a: &[&str]| locking(c, &strs(a));
        assert!(!q("pacman", &["-Qq"]));
        assert!(!q("yay", &["-Qua"]));
        assert!(!q("pacman", &["-Ss", "foo"]));
        assert!(!q("pacman", &["-Si", "foo"]));
        assert!(!q("pacman", &["--query", "--info"]));
        assert!(q("pacman", &["-Syu"]));
        assert!(q("pacman", &["-S", "foo"]));
        assert!(q("pacman", &["-U", "/x.pkg.tar.zst"]));
        assert!(q("yay", &[]), "bare yay is -Syu");
        assert!(q("makepkg", &["-si"]));
        assert!(!q("bash", &["-Syu"]));
    }

    #[test]
    fn processes_are_read_from_proc() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |pid: &str, comm: &str, args: &[&str]| {
            let p = dir.path().join(pid);
            std::fs::create_dir(&p).unwrap();
            std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
            std::fs::write(p.join("cmdline"), args.iter().map(|a| format!("{a}\0")).collect::<String>()).unwrap();
        };
        mk("10", "pacman", &["pacman", "-Syu"]);
        mk("11", "pacman", &["pacman", "-Qq"]);
        mk("12", "bash", &["bash"]);
        mk(&std::process::id().to_string(), "yay", &["yay"]);
        std::fs::create_dir(dir.path().join("self")).unwrap();
        assert_eq!(lockers_in(dir.path()), strs(&["pacman (10)"]));
    }

    /// checkupdates syncs its own copy of the databases with
    /// `fakeroot pacman -Sy --dbpath /tmp/checkup-db-UID`: that pacman's
    /// lock is in the copy, and Almanac's own scan must not block Update all.
    #[test]
    fn a_pacman_on_a_database_of_its_own_does_not_hold_the_lock() {
        let q = |c: &str, a: &[&str]| locking(c, &strs(a));
        assert!(!q("pacman", &["-Sy", "--dbpath", "/tmp/checkup-db-1000", "--logfile", "/dev/null"]));
        assert!(!q("pacman", &["-Sy", "--dbpath=/tmp/checkup-db-1000"]));
        assert!(!q("pacman", &["-b", "/tmp/x", "-Syu"]));
        assert!(q("pacman", &["-Syu", "--dbpath", "/var/lib/pacman/"]), "the system's own, spelled out");
        assert!(q("pikaur", &["-Syu"]));
        assert!(q("paru", &[]));
    }

    fn fake_proc(dir: &Path, pid: &str, comm: &str, args: &[&str]) -> PathBuf {
        let p = dir.join(pid);
        std::fs::create_dir(&p).unwrap();
        std::fs::write(p.join("comm"), format!("{comm}\n")).unwrap();
        std::fs::write(p.join("cmdline"), args.iter().map(|a| format!("{a}\0")).collect::<String>()).unwrap();
        p
    }

    /// pamac-daemon, packagekitd and AUR helpers other than yay take the
    /// lock too; a lock is stale only when every process could be read and
    /// none of them is running.
    #[test]
    fn a_lock_is_stale_only_with_every_process_in_view_and_none_using_it() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("db.lck");
        fake_proc(dir.path(), "1", "systemd", &["/sbin/init"]);
        assert!(lock_scan_in(dir.path(), &lock).nothing_uses_it(), "init alone: free");
        fake_proc(dir.path(), "20", "python3", &["/usr/bin/python3", "/usr/bin/pikaur", "-Syu"]);
        fake_proc(dir.path(), "21", "pamac-daemon", &["/usr/bin/pamac-daemon"]);
        fake_proc(dir.path(), "22", "pacman", &["pacman", "-Sy", "--dbpath", "/tmp/checkup-db-1000"]);
        let scan = lock_scan_in(dir.path(), &lock);
        assert_eq!(scan.running, strs(&["pikaur (20)"]));
        assert_eq!(scan.daemons, strs(&["pamac-daemon (21)"]));
        assert!(!scan.nothing_uses_it());

        // Whatever it is called, a process with the file open holds it.
        let held = tempfile::tempdir().unwrap();
        fake_proc(held.path(), "1", "systemd", &["/sbin/init"]);
        let p = fake_proc(held.path(), "30", "alpm-thing", &["alpm-thing"]);
        std::fs::create_dir(p.join("fd")).unwrap();
        std::os::unix::fs::symlink(&lock, p.join("fd").join("3")).unwrap();
        assert_eq!(lock_scan_in(held.path(), &lock).running, strs(&["alpm-thing (30)"]));

        // /proc mounted hidepid: init (root's) cannot be seen, so neither
        // could root's pacman. Never stale.
        let hidden = tempfile::tempdir().unwrap();
        fake_proc(hidden.path(), "40", "bash", &["bash"]);
        let scan = lock_scan_in(hidden.path(), &lock);
        assert!(scan.hidden && scan.running.is_empty());
        assert!(!scan.nothing_uses_it());
    }

    /// ALMANAC_DEMO: the probes before and after an update do not look at
    /// the machine, and a repair does not touch it.
    #[test]
    fn demo_mode_neither_reads_nor_changes_the_machine() {
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        crate::demo::TEST_DEMO.store(1, std::sync::atomic::Ordering::SeqCst);
        let r = std::panic::catch_unwind(|| {
            let log = |_: &str| {};
            (probe_preflight(), probe_after(true), probe_before(true), remove_stale_lock(&log))
        });
        crate::demo::TEST_DEMO.store(0, std::sync::atomic::Ordering::SeqCst);
        let (pre, after, before, removed) = r.unwrap();
        assert_eq!(pre, Preflight { free_root: Some(50 * GIB), ..Default::default() });
        assert_eq!(after, demo_after());
        assert_eq!(before.pending, Some(BTreeSet::new()));
        assert!(removed.is_err());
    }

    #[test]
    fn a_lock_that_may_be_in_use_is_never_offered_for_removal() {
        let hidden = Preflight {
            free_root: Some(50 * GIB),
            lock: LockScan { hidden: true, ..Default::default() },
            lock_file: true,
            battery: None,
        };
        let issues = preflight_issues(&hidden, true);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].level, Level::Warn);
        assert_eq!(issues[0].fix, None);
        // A package service running with the lock file there: it may be its.
        let service = LockScan { daemons: strs(&["pamac-daemon (21)"]), ..Default::default() };
        let p = Preflight { lock: service.clone(), ..hidden.clone() };
        let issues = preflight_issues(&p, true);
        assert_eq!(issues[0].level, Level::Block);
        assert!(issues[0].detail.contains("pamac-daemon"));
        // Idle, with no lock file: no reason to wait.
        let p = Preflight { lock: service, lock_file: false, ..hidden };
        assert!(preflight_issues(&p, true).is_empty());
    }

    #[test]
    fn the_battery_is_low_only_when_running_on_it() {
        let dir = tempfile::tempdir().unwrap();
        let w = |d: &str, f: &str, v: &str| {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
            std::fs::write(dir.path().join(d).join(f), format!("{v}\n")).unwrap();
        };
        assert_eq!(battery_in(dir.path()), None);
        w("BAT0", "type", "Battery");
        w("BAT0", "capacity", "15");
        w("BAT0", "status", "Discharging");
        w("hidpp_battery_0", "type", "Battery");
        w("hidpp_battery_0", "scope", "Device");
        w("hidpp_battery_0", "capacity", "5");
        w("AC", "type", "Mains");
        w("AC", "online", "0");
        assert_eq!(battery_in(dir.path()), Some(Battery { percent: 15, on_battery: true }), "the mouse is not the laptop");
        w("AC", "online", "1");
        assert_eq!(battery_in(dir.path()), Some(Battery { percent: 15, on_battery: false }));
    }

    #[test]
    fn preflight_blocks_what_would_fail_and_warns_about_the_rest() {
        let ok = Preflight { free_root: Some(50 * GIB), ..Default::default() };
        assert!(preflight_issues(&ok, true).is_empty());
        let p = Preflight {
            free_root: Some(GIB),
            lock: LockScan::default(),
            lock_file: true,
            battery: Some(Battery { percent: 10, on_battery: true }),
        };
        let issues = preflight_issues(&p, true);
        assert_eq!(issues[0].level, Level::Block, "the blocking one first");
        assert_eq!(issues.iter().filter(|i| i.level == Level::Warn).count(), 2);
        assert_eq!(issues.iter().find(|i| i.fix.is_some()).unwrap().fix, Some(Repair::RemoveLock));
        let p = Preflight {
            free_root: Some(3 * GIB),
            lock: LockScan { running: strs(&["pacman (10)"]), ..Default::default() },
            lock_file: true,
            battery: None,
        };
        let issues = preflight_issues(&p, true);
        assert_eq!(issues.len(), 2);
        assert!(issues[0].detail.contains("pacman (10)"));
        assert!(issues.iter().all(|i| i.fix.is_none()), "never remove a lock something holds");
        // Updating only AppImages and plugins: pacman's lock is no concern.
        assert_eq!(preflight_issues(&p, false).len(), 1);
    }

    #[test]
    fn the_newest_snapshot_since_the_start_is_this_runs() {
        use chrono::TimeZone;
        let start = chrono::Local.with_ymd_and_hms(2026, 10, 20, 13, 43, 0).unwrap().timestamp() as f64;
        let csv = "number,date\n0,\n40,2026-10-19 10:00:00\n41,2026-10-20 13:43:05\n42,2026-10-20 13:43:40\n";
        assert_eq!(newest_since(csv, start).as_deref(), Some("42"));
        assert_eq!(newest_since("number,date\n40,2026-10-19 10:00:00\n", start), None);
    }

    /// The journal is asked about the whole run, not its first minute: a
    /// password typed slowly put the snapshot past the old 60 s window, and
    /// History lost it.
    #[test]
    fn the_journal_is_asked_about_the_whole_run() {
        let (since, until) = snapshot_window(1000.0, 1000.0 + 3600.0);
        assert!(since <= 1000.0 && since > 1000.0 - 60.0, "not a snapshot from before the click");
        assert!(until >= 4600.0);
    }

    #[test]
    fn the_history_entry_keeps_what_the_run_did() {
        let mut i = inputs();
        i.after.pending = Some(set(&["omarchy"]));
        let report = build_report(&i);
        let rec = RunRecord {
            commands: vec![CommandRun { argv: "omarchy update".into(), exit: Some(0), secs: Some(60.0) }],
            report: Some(report.clone()),
            recheck: Some(report.recheck(&i.before, &After { pending: Some(set(&[])), ..i.after.clone() })),
            freed: Some(3 * 1048576),
            snapshot: Some("42".into()),
            ..Default::default()
        };
        let a = rec.action(75.0);
        assert_eq!(a.changes.len(), 3);
        assert!(a.checks.iter().any(|c| c.name == "Updates installed" && c.ok == Some(false)));
        assert!(a.repairs.iter().any(|c| c.name == "Updates installed" && c.ok == Some(true)));
        assert_eq!(a.freed_bytes, Some(3 * 1048576));
        assert_eq!(a.duration_secs, Some(75.0));
        assert_eq!(rec.severity(), "");
        assert_eq!(rec.detail(), "3 changes · every check passed · repaired · 3 MB freed");
        let failed = RunRecord { report: Some(report), ..Default::default() };
        assert_eq!(failed.severity(), "warn");
    }
}
