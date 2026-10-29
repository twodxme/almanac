//! What still runs old code after an update.
//!
//! pacman replaces files on disk; it cannot replace what is already loaded.
//! A process that mapped a library before the update keeps the old copy until
//! it restarts, and a kernel stays the running one until the next boot. Until
//! then the update has not fully happened: a security fix to a library is not
//! in effect for anything that was already running.
//!
//! Two questions, answered from what any user can read:
//!
//! * Does the computer need to restart? The running kernel's modules directory
//!   is gone (pacman removed it with the old package), or the kernel package
//!   it came from is installed at a different version. This is the test
//!   arch-update and Omarchy's `omarchy-update-restart` make.
//! * Which processes use files that were replaced? `/proc/PID/maps` lists each
//!   mapped file, with "(deleted)" after one whose path now names a different
//!   file -- what needrestart and `zypper ps` read. Only the user's own
//!   processes can be read without root; system services, running as root or
//!   their own user, are skipped quietly, so what this lists is what the user
//!   runs (the reboot test still covers the system as a whole).
//!
//! Each process is placed in the systemd unit that runs it, read from
//! `/proc/PID/cgroup`. A service can be restarted by name; an app the user
//! started, the session and its compositor cannot, and are named instead.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::LazyLock;
use std::time::Instant;

use crate::maintenance::{Finding, Fix, INFO, OK, WARN};

/// `PF_KTHREAD` in /proc/PID/stat's flags: a kernel thread, which maps nothing.
const PF_KTHREAD: u64 = 0x0020_0000;

/// Where the kernel modules of each installed kernel live.
pub const MODULES: &str = "/usr/lib/modules";

/// Whether a user unit or a system one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Scope {
    System,
    User,
}

/// A systemd unit, by name as `systemctl` takes it (escapes left as they are).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Unit {
    pub scope: Scope,
    pub name: String,
}

/// What can be done about a unit running replaced files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// The session, its compositor or its shell: log out and back in.
    Session,
    /// PID 1, the display manager, D-Bus, logind: only a reboot restarts these
    /// safely.
    Reboot,
    /// A service `systemctl restart` can restart.
    Service,
    /// An app the user started (its own scope): close it and open it again.
    App,
}

/// One process using files that have been replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Stale {
    pub pid: u32,
    pub name: String,
    /// None: no cgroup could be read
    pub unit: Option<Unit>,
    pub files: Vec<String>,
}

/// Everything the check found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    /// Why the computer needs to restart, when it does.
    pub reboot: Option<String>,
    pub stale: Vec<Stale>,
    /// How many processes were read, and how long it took.
    pub scanned: usize,
    pub millis: u128,
}

// ---- the kernel -----------------------------------------------------------

/// A version split where people would read a break: at `.`, `-`, `_`, `+`, and
/// where digits run into letters ("18rc1" is 18 then rc1). The leading numbers
/// are the kernel version proper; the rest is the flavour and release.
fn version_parts(v: &str) -> (Vec<u64>, Vec<String>) {
    // An epoch ("1:6.17.2-1") is pacman's alone; uname never has one.
    let v = v.rsplit_once(':').map_or(v, |(_, rest)| rest);
    let mut tokens: Vec<String> = Vec::new();
    for raw in v.split(['.', '-', '_', '+']).filter(|t| !t.is_empty()) {
        let split = raw
            .char_indices()
            .find(|&(i, c)| i > 0 && c.is_ascii_alphabetic() && raw.as_bytes()[i - 1].is_ascii_digit())
            .map(|(i, _)| i);
        match split {
            Some(i) => {
                tokens.push(raw[..i].to_string());
                tokens.push(raw[i..].to_string());
            }
            None => tokens.push(raw.to_string()),
        }
    }
    let mut nums = Vec::new();
    let mut rest = tokens.into_iter().peekable();
    while let Some(n) = rest.peek().and_then(|t| t.parse::<u64>().ok()) {
        nums.push(n);
        rest.next();
    }
    // "6.18" and "6.18.0" are the same release: Arch's pkgver drops a .0
    // patch level that uname keeps.
    while nums.len() < 3 {
        nums.push(0);
    }
    (nums, rest.collect())
}

/// Whether the kernel `release` (`uname -r`) is the one package version
/// `pkgver` installs: `6.17.2-arch1-1` is `linux 6.17.2.arch1-1`,
/// `6.18.0-arch1-1` is `6.18.arch1-1`, `7.2.8-9-omarchy` is
/// `linux-omarchy 7.2.8-9`, `6.17.2-zen1-1-zen` is `linux-zen 6.17.2.zen1-1`.
/// The release may carry a flavour suffix the package version does not.
pub fn kernel_matches(release: &str, pkgver: &str) -> bool {
    let (rn, rr) = version_parts(release);
    let (pn, pr) = version_parts(pkgver);
    rn == pn && rr.len() >= pr.len() && rr.iter().zip(&pr).all(|(a, b)| a == b)
}

/// Why the computer needs restarting, or None. `modules_present`: the running
/// kernel's modules directory still exists; `installed`: the package the
/// running kernel came from and the version of it installed now, when known.
pub fn reboot_reason(release: &str, modules_present: bool, installed: Option<(&str, &str)>) -> Option<String> {
    if release.is_empty() {
        return None;
    }
    if !modules_present {
        return Some(format!(
            "The running kernel ({release}) was replaced by an update; its modules are gone, \
             so a device plugged in now may not work"
        ));
    }
    match installed {
        Some((pkg, ver)) if !ver.is_empty() && !kernel_matches(release, ver) => {
            Some(format!("{pkg} {ver} is installed, but {release} is still running"))
        }
        _ => None,
    }
}

/// The running kernel, checked against what is installed. Reads `uname`, the
/// modules directory, and `pacman -Q` for the one package the kernel says it
/// came from (its `pkgbase` file). The answer is kept while pacman's local
/// database stands still: Today, the news card and the Health check all ask
/// at launch. In demo mode nothing is read, and nothing needs a restart.
pub fn reboot_needed() -> Option<String> {
    if crate::demo::active() {
        return None;
    }
    let release = rustix::system::uname().release().to_string_lossy().into_owned();
    let key = local_db_stamp().map(|stamp| (release.clone(), stamp));
    kept(&REBOOT, key, || {
        reboot_needed_in(Path::new(MODULES), &release, &|pkg| {
            let out = crate::util::run(&["pacman", "-Q", pkg], 15);
            if !out.ok() {
                return None;
            }
            let line = out.stdout.lines().next()?.to_string();
            let mut parts = line.split_whitespace();
            (parts.next() == Some(pkg)).then(|| parts.next().map(str::to_string)).flatten()
        })
    })
}

/// What `reboot_needed` is kept against: the running kernel's release and
/// when pacman's local database last changed. Installing, upgrading or
/// removing a kernel changes the second (and is the only thing that removes
/// a modules directory).
type RebootKey = (String, std::time::SystemTime);

/// The last `reboot_needed` answer, with what it was read at.
static REBOOT: std::sync::Mutex<Option<(RebootKey, Option<String>)>> = std::sync::Mutex::new(None);

/// When pacman's local database last changed, or None where it cannot be
/// read (and then nothing is kept).
fn local_db_stamp() -> Option<std::time::SystemTime> {
    std::fs::metadata("/var/lib/pacman/local").and_then(|m| m.modified()).ok()
}

/// `compute()`, or what it said last time for the same `key`. No key: asked
/// afresh, and not kept.
fn kept(
    slot: &std::sync::Mutex<Option<(RebootKey, Option<String>)>>,
    key: Option<RebootKey>,
    compute: impl FnOnce() -> Option<String>,
) -> Option<String> {
    let Some(key) = key else { return compute() };
    if let Some((at, answer)) = slot.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *at == key {
            return answer.clone();
        }
    }
    let answer = compute();
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, answer.clone()));
    answer
}

fn reboot_needed_in(modules: &Path, release: &str, installed: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    if release.is_empty() || release.contains('/') {
        return None;
    }
    let dir = modules.join(release);
    if !dir.is_dir() {
        return reboot_reason(release, false, None);
    }
    // Every Arch kernel package writes its own name here. A kernel built by
    // hand has none, and then there is nothing to compare with.
    let pkgbase = std::fs::read_to_string(dir.join("pkgbase")).unwrap_or_default();
    let pkgbase = pkgbase.trim();
    if pkgbase.is_empty() {
        return None;
    }
    let ver = installed(pkgbase)?;
    reboot_reason(release, true, Some((pkgbase, &ver)))
}

// ---- processes ----------------------------------------------------------------

/// The path of each replaced program or library a maps file lists, once each.
/// Only code counts: a replaced cache or font file is read again on its own,
/// and a deleted file outside /usr (a memfd, a temporary file, /dev/shm) was
/// never an installed one.
pub fn deleted_files(maps: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in maps.lines() {
        let Some(head) = line.strip_suffix(" (deleted)") else { continue };
        // address perms offset dev inode, then the path, which may hold spaces.
        let mut rest = head;
        for _ in 0..5 {
            rest = rest.trim_start();
            match rest.find(' ') {
                Some(i) => rest = &rest[i..],
                None => {
                    rest = "";
                    break;
                }
            }
        }
        let path = rest.trim_start();
        if is_code(path) && !out.iter().any(|p| p == path) {
            out.push(path.to_string());
        }
    }
    out
}

fn is_code(path: &str) -> bool {
    const DIRS: [&str; 6] = ["/usr/bin/", "/usr/sbin/", "/usr/lib/", "/usr/lib32/", "/usr/lib64/", "/usr/libexec/"];
    DIRS.iter().any(|d| path.starts_with(d))
        && !path.starts_with("/usr/lib/locale/")
        && !path.ends_with(".cache")
}

/// The unit a process runs in, from its `/proc/PID/cgroup`: the innermost
/// `.service` or `.scope` on the path. Under `user@UID.service` it is one of
/// the user's own units; anywhere else, a system one.
pub fn unit_of(cgroup: &str) -> Option<Unit> {
    // cgroup v2 is the "0::" line; a hybrid setup names systemd's hierarchy.
    let path = cgroup
        .lines()
        .find_map(|l| l.strip_prefix("0::"))
        .or_else(|| cgroup.lines().find_map(|l| l.split_once(":name=systemd:").map(|(_, p)| p)))?;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let leaf = parts.iter().rev().find(|p| p.ends_with(".service") || p.ends_with(".scope"))?;
    let user = parts.iter().any(|p| p.starts_with("user@") && p.ends_with(".service"));
    // systemd --user itself sits in user@UID.service/init.scope; the
    // user@UID.service unit is the system's.
    if user && leaf.starts_with("user@") {
        return Some(Unit { scope: Scope::System, name: leaf.to_string() });
    }
    Some(Unit { scope: if user { Scope::User } else { Scope::System }, name: leaf.to_string() })
}

/// The programs that are the desktop session: restarting one ends or breaks
/// the session, so they are never offered. Names as `/proc/PID/comm` has them
/// (15 characters at most).
const SESSION_PROGRAMS: &[&str] = &[
    "Hyprland", "Xwayland", "quickshell", "qs", "waybar", "mako", "swaync", "hypridle", "hyprlock",
    "hyprpaper", "swaybg", "swayosd-server", "walker", "uwsm", "sway", "niri", "gnome-shell",
    "kwin_wayland", "plasmashell",
];

/// System units that restarting would end the session or the machine's
/// ability to log in. Matched by name before ".service" (and before "@").
const NEVER_SYSTEM: &[&str] = &[
    "dbus", "dbus-broker", "systemd-logind", "display-manager", "sddm", "gdm", "lightdm", "greetd",
    "ly", "lxdm", "getty", "serial-getty", "user", "user-runtime-dir", "systemd-journald", "polkit",
];

/// User units that carry the session itself.
const NEVER_USER: &[&str] = &["dbus", "dbus-broker", "wayland-wm", "wayland-wm-env", "wayland-session-bindpid",
    "wayland-session-waitenv", "graphical-session", "xdg-desktop-portal-hyprland"];

fn stem(unit: &str) -> &str {
    let base = unit.strip_suffix(".service").or_else(|| unit.strip_suffix(".scope")).unwrap_or(unit);
    base.split_once('@').map_or(base, |(a, _)| a)
}

/// What can be done about `unit`, which runs a process named `program`.
pub fn kind_of(unit: Option<&Unit>, program: &str) -> Kind {
    if SESSION_PROGRAMS.contains(&program) {
        return Kind::Session;
    }
    let Some(unit) = unit else { return Kind::App };
    let name = unit.name.as_str();
    match unit.scope {
        Scope::System => {
            if name == "init.scope" {
                Kind::Reboot
            } else if name.starts_with("session-") && name.ends_with(".scope") {
                // the login session: what was started from the TTY or by the
                // display manager, the compositor's launcher included
                Kind::Session
            } else if name.ends_with(".scope") {
                Kind::App
            } else if NEVER_SYSTEM.contains(&stem(name)) {
                Kind::Reboot
            } else {
                Kind::Service
            }
        }
        Scope::User => {
            if name == "init.scope" || NEVER_USER.contains(&stem(name)) {
                Kind::Session
            } else if name.ends_with(".scope") || name.starts_with("app-") {
                // An app launched from the desktop: restarting its unit would
                // close it without asking about unsaved work.
                Kind::App
            } else {
                Kind::Service
            }
        }
    }
}

/// Read every process under `proc` (normally /proc). Kernel threads are
/// skipped before their maps are read; a process that cannot be read (another
/// user's, or one that has just exited) is skipped quietly.
pub fn scan(proc: &Path) -> (Vec<Stale>, usize) {
    let mut out = Vec::new();
    let mut scanned = 0;
    let Ok(dir) = std::fs::read_dir(proc) else { return (out, 0) };
    for entry in dir.flatten() {
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let base = proc.join(file_name);
        let Ok(stat) = std::fs::read_to_string(base.join("stat")) else { continue };
        let Some((name, flags)) = parse_stat(&stat) else { continue };
        if flags & PF_KTHREAD != 0 {
            continue;
        }
        scanned += 1;
        let Ok(maps) = std::fs::read(base.join("maps")) else { continue };
        // Most processes have nothing replaced: look for the marker before
        // decoding and splitting the whole file.
        if !DELETED.is_match(&maps) {
            continue;
        }
        let files = deleted_files(&String::from_utf8_lossy(&maps));
        if files.is_empty() {
            continue;
        }
        let unit = std::fs::read_to_string(base.join("cgroup")).ok().and_then(|c| unit_of(&c));
        out.push(Stale { pid, name, unit, files });
    }
    out.sort_by_key(|s| s.pid);
    (out, scanned)
}

static DELETED: LazyLock<regex::bytes::Regex> =
    LazyLock::new(|| regex::bytes::Regex::new(r"(?m) \(deleted\)$").unwrap());

/// (comm, flags) from `/proc/PID/stat`. The name is between the first "(" and
/// the last ")", since it may itself hold either.
fn parse_stat(stat: &str) -> Option<(String, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_string();
    // state ppid pgrp session tty_nr tpgid flags
    let flags = stat.get(close + 1..)?.split_whitespace().nth(6)?.parse().ok()?;
    Some((name, flags))
}

/// The whole check, read now: every process's maps once (about 120 ms), so
/// a Health run, which calls this once, reads them once. In demo mode
/// nothing of the real machine is read.
pub fn status() -> Status {
    if crate::demo::active() {
        return Status::default();
    }
    let started = Instant::now();
    let reboot = reboot_needed();
    let (stale, scanned) = scan(Path::new("/proc"));
    Status { reboot, stale, scanned, millis: started.elapsed().as_millis() }
}

// ---- the finding --------------------------------------------------------------

/// The stale processes grouped by unit, each with its kind and the program
/// names in it (once each, in pid order).
pub fn groups(stale: &[Stale]) -> Vec<(Kind, Option<Unit>, Vec<String>)> {
    let mut by: BTreeMap<(Kind, Option<Unit>), Vec<String>> = BTreeMap::new();
    for s in stale {
        let kind = kind_of(s.unit.as_ref(), &s.name);
        // Apps are named by program, not by their launch scope's
        // generated name.
        let key_unit = if kind == Kind::App || kind == Kind::Session { None } else { s.unit.clone() };
        let names = by.entry((kind, key_unit)).or_default();
        if !names.contains(&s.name) {
            names.push(s.name.clone());
        }
    }
    by.into_iter().map(|((k, u), n)| (k, u, n)).collect()
}

/// The units the fix restarts.
pub fn restartable(stale: &[Stale]) -> Vec<Unit> {
    groups(stale).into_iter().filter(|(k, _, _)| *k == Kind::Service).filter_map(|(_, u, _)| u).collect()
}

/// The commands that restart `units`: the user's in one `systemctl --user`,
/// the system's in one `systemctl` through pkexec, so there is one password
/// prompt at most.
pub fn restart_commands(units: &[Unit]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let names = |scope: Scope| -> Vec<String> {
        units.iter().filter(|u| u.scope == scope).map(|u| u.name.clone()).collect()
    };
    let user = names(Scope::User);
    if !user.is_empty() {
        let mut argv: Vec<String> = ["/usr/bin/systemctl", "--user", "restart"].map(String::from).to_vec();
        argv.extend(user);
        out.push(argv);
    }
    let system = names(Scope::System);
    if !system.is_empty() {
        let mut argv: Vec<String> =
            [crate::tasks::PKEXEC, "/usr/bin/systemctl", "restart"].map(String::from).to_vec();
        argv.extend(system);
        out.push(argv);
    }
    out
}

/// The finding for `status`. Pure.
pub fn finding_from(status: &Status) -> Finding {
    let groups = groups(&status.stale);
    let has = |k: Kind| groups.iter().any(|(g, _, _)| *g == k);
    let mut items: Vec<String> = Vec::new();
    if let Some(why) = &status.reboot {
        items.push(format!("Computer: {why}"));
    }
    for (kind, unit, names) in &groups {
        let names = names.join(", ");
        items.push(match (kind, unit) {
            (Kind::Session, _) => format!("Session: {names} — log out and back in"),
            (Kind::Reboot, Some(u)) => format!("System: {} ({names}) — restart the computer", u.name),
            (Kind::Service, Some(u)) if u.scope == Scope::User => format!("Your service: {} ({names})", u.name),
            (Kind::Service, Some(u)) => format!("System service: {} ({names})", u.name),
            (Kind::App, _) => format!("App: {names} — close it and open it again"),
            (_, None) => names,
        });
    }
    let units = if status.reboot.is_some() { Vec::new() } else { restartable(&status.stale) };

    if items.is_empty() {
        return Finding {
            id: "restart-needed".into(),
            title: "Restart after updates".into(),
            detail: "Nothing is running replaced code; updates are fully in effect.".into(),
            severity: OK.into(),
            ..Default::default()
        };
    }
    let severity = if status.reboot.is_some() || has(Kind::Session) || has(Kind::Reboot) { WARN } else { INFO };
    let detail = if status.reboot.is_some() {
        "Updated software is installed but not all of it is running yet. \
         Restart the computer to finish updating."
            .to_string()
    } else if has(Kind::Reboot) {
        "Parts of the system that cannot be restarted on their own still run the \
         old version. Restart the computer to finish updating."
            .to_string()
    } else if has(Kind::Session) {
        "Your desktop session still runs the old version of updated software. \
         Log out and back in to finish updating."
            .to_string()
    } else {
        format!(
            "{} still run{} the old version of updated software.",
            plural(groups.len(), "program"),
            if groups.len() == 1 { "s" } else { "" }
        )
    };
    let commands = restart_commands(&units);
    let mut note: Vec<String> = Vec::new();
    if !commands.is_empty() {
        note.push(format!(
            "Restarts {}. Each picks up the updated files as it starts again.\n\n{}",
            plural(units.len(), "service"),
            commands.iter().map(|c| format!("$ {}", c.join(" "))).collect::<Vec<_>>().join("\n")
        ));
    }
    if status.reboot.is_some() || has(Kind::Reboot) {
        note.push("Restart to finish updating: save your work, then use Restart now on Today.".into());
    } else if has(Kind::Session) {
        note.push("Log out and back in to restart the desktop session.".into());
    }
    if has(Kind::App) {
        note.push("Apps you started are not closed for you: close and reopen them when it suits you.".into());
    }
    Finding {
        id: "restart-needed".into(),
        title: if status.reboot.is_some() { "Restart needed".into() } else { "Running old versions".into() },
        detail,
        severity: severity.into(),
        count: items.len() as i64,
        items,
        fix_label: if commands.is_empty() { String::new() } else { "Restart services".into() },
        fix: if commands.is_empty() { None } else { Some(restart_fix(units)) },
        fix_note: note.join("\n\n"),
        ..Default::default()
    }
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The health check.
pub fn finding() -> Finding {
    finding_from(&status())
}

/// Restart `units`, then look again: a unit whose processes still map
/// replaced files did not take the update, whatever systemctl said. The
/// whole of it goes to the history log.
fn restart_fix(units: Vec<Unit>) -> Fix {
    Fix::func(move |log| {
        if crate::demo::active() {
            log("Demo mode: nothing is restarted.");
            return Ok(());
        }
        let started = Instant::now();
        let mut action = crate::history::ActionLog::default();
        let mut failed = Vec::new();
        for argv in restart_commands(&units) {
            log(&format!("$ {}", argv.join(" ")));
            let t = Instant::now();
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            let out = crate::util::run(&refs, 180);
            for line in out.stdout.lines().chain(out.stderr.lines()) {
                log(&crate::tasks::output(line));
            }
            action.commands.push(crate::history::CommandRun {
                argv: argv.join(" "),
                exit: out.code,
                secs: Some(t.elapsed().as_secs_f64()),
            });
            if !out.ok() {
                failed.push(argv.join(" "));
            }
        }
        let (after, _) = scan(Path::new("/proc"));
        for u in &units {
            let still: Vec<&Stale> = after.iter().filter(|s| s.unit.as_ref() == Some(u)).collect();
            action.checks.push(crate::history::CheckResult {
                name: u.name.clone(),
                ok: Some(still.is_empty()),
                detail: if still.is_empty() {
                    "running the updated files".into()
                } else {
                    format!("still uses {}", still[0].files.first().cloned().unwrap_or_default())
                },
            });
        }
        action.duration_secs = Some(started.elapsed().as_secs_f64());
        let bad = action.checks.iter().filter(|c| c.ok == Some(false)).count();
        let title = format!("Restarted {}", plural(units.len(), "service"));
        let detail = if bad > 0 { format!("{} still run old files", plural(bad, "service")) } else { String::new() };
        let severity = if bad > 0 || !failed.is_empty() { WARN } else { "" };
        if let Err(e) = crate::history::record_action(crate::history::ALMANAC, &title, &detail, severity, &action) {
            log(&format!("Could not write the history log: {e}"));
        }
        if !failed.is_empty() {
            return Err(format!("Could not restart: {}", failed.join("; ")));
        }
        if bad > 0 {
            return Err(detail);
        }
        Ok(())
    })
}

/// Restart the computer, logged first (nothing can be logged after). Err
/// with what went wrong when `systemctl reboot` refused, and always in demo
/// mode, which never restarts the machine it is shown on.
pub fn reboot_now() -> Result<(), String> {
    reboot_now_with(crate::demo::active(), &|argv| crate::util::run(argv, 60))
}

/// `reboot_now`, told whether this is demo mode and handed what runs
/// `systemctl reboot`: a test must never be able to reach the real one.
fn reboot_now_with(demo: bool, run: &dyn Fn(&[&str]) -> crate::util::Output) -> Result<(), String> {
    if demo {
        return Err("Demo mode does not restart the computer.".into());
    }
    let argv = ["/usr/bin/systemctl", "reboot"];
    let mut action = crate::history::ActionLog::default();
    action.commands.push(crate::history::CommandRun { argv: argv.join(" "), exit: None, secs: None });
    let why = reboot_needed().unwrap_or_default();
    let _ = crate::history::record_action(crate::history::ALMANAC, "Restarted to finish updating", &why, "", &action);
    let out = run(&argv);
    if out.ok() {
        return Ok(());
    }
    let msg = out.stderr.trim().to_string();
    action.commands[0].exit = out.code;
    let _ = crate::history::record_action(crate::history::ALMANAC, "Restart did not start", &msg, WARN, &action);
    Err(if msg.is_empty() { "systemctl reboot failed".into() } else { msg })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(name: &str) -> Unit {
        Unit { scope: Scope::User, name: name.into() }
    }

    fn system(name: &str) -> Unit {
        Unit { scope: Scope::System, name: name.into() }
    }

    fn stale(pid: u32, name: &str, unit: Option<Unit>) -> Stale {
        Stale { pid, name: name.into(), unit, files: vec!["/usr/lib/libc.so.6".into()] }
    }

    const MAPS: &str = "\
5581a000-5581b000 r--p 00000000 103:02 1311 /usr/bin/pipewire (deleted)
7f3a1000-7f3a2000 r-xp 00001000 103:02 2211 /usr/lib/libpipewire-0.3.so.0.1400.0 (deleted)
7f3a2000-7f3a3000 r--p 00002000 103:02 2211 /usr/lib/libpipewire-0.3.so.0.1400.0 (deleted)
7f3a4000-7f3a5000 r--p 00000000 103:02 3311 /usr/lib/libfine.so.1
7f3a6000-7f3a7000 r--s 00000000 103:02 4411 /usr/share/mime/mime.cache (deleted)
7f3a8000-7f3a9000 r--p 00000000 103:02 5511 /usr/lib/locale/locale-archive (deleted)
7f3aa000-7f3ab000 r--p 00000000 103:02 6611 /etc/ld.so.cache (deleted)
7f3ac000-7f3ad000 rw-s 00000000 00:01 7711 /memfd:pipewire-memfd:flags=0x0000000f (deleted)
7f3ae000-7f3af000 r--p 00000000 103:02 8811 /usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache (deleted)
7f3b0000-7f3b1000 r-xp 00000000 103:02 9911                      /usr/lib/odd dir/libspace.so (deleted)
7ffd1000-7ffd2000 rw-p 00000000 00:00 0                          [stack]
";

    #[test]
    fn only_replaced_programs_and_libraries_count_once_each() {
        assert_eq!(
            deleted_files(MAPS),
            vec![
                "/usr/bin/pipewire".to_string(),
                "/usr/lib/libpipewire-0.3.so.0.1400.0".to_string(),
                "/usr/lib/odd dir/libspace.so".to_string(),
            ]
        );
        assert!(deleted_files("").is_empty());
    }

    #[test]
    fn a_process_is_placed_in_the_innermost_unit_of_its_cgroup() {
        let u = unit_of;
        assert_eq!(
            u("0::/user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service\n"),
            Some(user("pipewire.service"))
        );
        assert_eq!(u("0::/system.slice/NetworkManager.service\n"), Some(system("NetworkManager.service")));
        assert_eq!(
            u("0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-graphical.slice/app-Hyprland-xdg\\x2dterminal\\x2dexec-e782e8bb.scope"),
            Some(user("app-Hyprland-xdg\\x2dterminal\\x2dexec-e782e8bb.scope"))
        );
        assert_eq!(u("0::/user.slice/user-1000.slice/session-1.scope"), Some(system("session-1.scope")));
        assert_eq!(u("0::/init.scope"), Some(system("init.scope")));
        assert_eq!(u("0::/user.slice/user-1000.slice/user@1000.service/init.scope"), Some(user("init.scope")));
        // cgroup v1 with systemd's named hierarchy
        assert_eq!(u("12:memory:/\n1:name=systemd:/system.slice/cups.service\n"), Some(system("cups.service")));
        assert_eq!(u("0::/\n"), None);
        assert_eq!(u(""), None);
    }

    #[test]
    fn the_session_and_what_keeps_it_running_are_never_restarted() {
        let k = |u: Unit, p: &str| kind_of(Some(&u), p);
        assert_eq!(k(user("wayland-wm@hyprland.desktop.service"), "Hyprland"), Kind::Session);
        assert_eq!(k(user("wayland-wm@hyprland.desktop.service"), "inotifywait"), Kind::Session);
        assert_eq!(k(user("dbus-broker.service"), "dbus-broker"), Kind::Session);
        assert_eq!(k(user("init.scope"), "systemd"), Kind::Session);
        assert_eq!(k(user("waybar.service"), "waybar"), Kind::Session);
        assert_eq!(k(system("session-1.scope"), "sh"), Kind::Session);
        assert_eq!(k(system("init.scope"), "systemd"), Kind::Reboot);
        for unit in [
            "dbus-broker.service",
            "systemd-logind.service",
            "sddm.service",
            "gdm.service",
            "display-manager.service",
            "getty@tty1.service",
            "user@1000.service",
        ] {
            assert_eq!(k(system(unit), "x"), Kind::Reboot, "{unit}");
        }
    }

    #[test]
    fn services_are_restartable_and_apps_are_left_to_their_user() {
        let k = |u: Unit, p: &str| kind_of(Some(&u), p);
        assert_eq!(k(user("pipewire.service"), "pipewire"), Kind::Service);
        assert_eq!(k(user("gnome-keyring-daemon.service"), "gnome-keyring-d"), Kind::Service);
        assert_eq!(k(system("NetworkManager.service"), "NetworkManager"), Kind::Service);
        assert_eq!(k(user("app-Hyprland-gtk\\x2dlaunch-82a6189c.scope"), "spotify"), Kind::App);
        assert_eq!(k(user("app-Hyprland-org.mozilla.firefox@a1b2.service"), "firefox"), Kind::App);
        assert_eq!(k(system("docker-1234.scope"), "nginx"), Kind::App);
        assert_eq!(kind_of(None, "something"), Kind::App);
    }

    #[test]
    fn kernel_versions_match_across_the_two_spellings() {
        assert!(kernel_matches("6.17.2-arch1-1", "6.17.2.arch1-1"));
        assert!(kernel_matches("6.18.0-arch1-1", "6.18.arch1-1"));
        assert!(kernel_matches("7.2.8-9-omarchy", "7.2.8-9"));
        assert!(kernel_matches("6.12.50-1-lts", "6.12.50-1"));
        assert!(kernel_matches("6.17.2-zen1-1-zen", "6.17.2.zen1-1"));
        assert!(kernel_matches("6.17.2-hardened1-1-hardened", "6.17.2.hardened1-1"));
        assert!(kernel_matches("6.18.0-rc1-1-mainline", "6.18rc1-1"));
        assert!(kernel_matches("6.17.2-arch1-1", "1:6.17.2.arch1-1"));
        assert!(!kernel_matches("6.17.2-arch1-1", "6.17.3.arch1-1"));
        assert!(!kernel_matches("6.17.2-arch1-1", "6.17.2.arch1-2"));
        assert!(!kernel_matches("7.2.8-9-omarchy", "7.2.9-1"));
        assert!(!kernel_matches("6.17.2-arch1-1", "6.17.2.arch2-1"));
    }

    #[test]
    fn a_reboot_is_needed_when_the_running_kernel_is_gone_or_superseded() {
        let d = tempfile::tempdir().unwrap();
        let running = d.path().join("7.2.8-9-omarchy");
        std::fs::create_dir(&running).unwrap();
        std::fs::write(running.join("pkgbase"), "linux-omarchy\n").unwrap();
        let same = |p: &str| (p == "linux-omarchy").then(|| "7.2.8-9".to_string());
        let newer = |p: &str| (p == "linux-omarchy").then(|| "7.2.9-1".to_string());
        assert_eq!(reboot_needed_in(d.path(), "7.2.8-9-omarchy", &same), None);
        let why = reboot_needed_in(d.path(), "7.2.8-9-omarchy", &newer).unwrap();
        assert!(why.contains("linux-omarchy 7.2.9-1") && why.contains("7.2.8-9-omarchy"), "{why}");
        // The modules directory gone: pacman removed the old package.
        let why = reboot_needed_in(d.path(), "7.2.7-1-omarchy", &same).unwrap();
        assert!(why.contains("modules are gone"), "{why}");
        // A kernel no package owns: nothing to compare, so no claim.
        std::fs::create_dir(d.path().join("6.99.0-custom")).unwrap();
        assert_eq!(reboot_needed_in(d.path(), "6.99.0-custom", &newer), None);
        // The package query failing is not a reason to restart either.
        assert_eq!(reboot_needed_in(d.path(), "7.2.8-9-omarchy", &|_| None), None);
    }

    #[test]
    fn the_reboot_answer_is_kept_while_pacmans_database_stands_still() {
        let slot = std::sync::Mutex::new(None);
        let calls = std::cell::Cell::new(0);
        let ask = |answer: Option<&str>| {
            calls.set(calls.get() + 1);
            answer.map(str::to_string)
        };
        let t0 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(100);
        let t1 = t0 + std::time::Duration::from_secs(1);
        let key = |stamp| Some(("7.2.8-9-omarchy".to_string(), stamp));
        assert_eq!(kept(&slot, key(t0), || ask(None)), None);
        assert_eq!(kept(&slot, key(t0), || ask(Some("x"))), None, "the same database: the kept answer");
        assert_eq!(calls.get(), 1);
        // A kernel installed since: asked again.
        assert_eq!(kept(&slot, key(t1), || ask(Some("newer"))).as_deref(), Some("newer"));
        assert_eq!(calls.get(), 2);
        // No stamp to key on: asked every time, and not kept.
        kept(&slot, None, || ask(None));
        kept(&slot, None, || ask(None));
        assert_eq!(calls.get(), 4);
        assert_eq!(kept(&slot, key(t1), || ask(None)).as_deref(), Some("newer"));
    }

    #[test]
    fn demo_mode_reads_nothing_and_restarts_nothing() {
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        crate::demo::TEST_DEMO.store(1, std::sync::atomic::Ordering::SeqCst);
        let (reboot, status) = (reboot_needed(), status());
        let now = reboot_now_with(crate::demo::active(), &|_argv| panic!("systemctl reboot ran in demo mode"));
        // A unit that does not exist, should the guard ever fail.
        let fix = restart_fix(vec![user("almanac-test-no-such-unit.service")]);
        let ran = std::sync::Mutex::new(Vec::new());
        let fixed = match &fix {
            Fix::Func(f) => f(&|l| ran.lock().unwrap().push(l.to_string())),
            _ => Err("not a callable".into()),
        };
        crate::demo::TEST_DEMO.store(0, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(reboot, None);
        assert_eq!(status, Status::default(), "no process was read");
        assert!(now.unwrap_err().contains("Demo mode"));
        assert!(fixed.is_ok() && ran.lock().unwrap().iter().all(|l| !l.starts_with('$')), "no command ran");
    }

    #[test]
    fn a_fake_proc_is_read_and_kernel_threads_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        let add = |pid: &str, comm: &str, flags: u64, maps: &str, cgroup: &str| {
            let p = d.path().join(pid);
            std::fs::create_dir(&p).unwrap();
            std::fs::write(p.join("stat"), format!("{pid} ({comm}) S 1 1 1 0 -1 {flags} 0 0 0")).unwrap();
            std::fs::write(p.join("maps"), maps).unwrap();
            std::fs::write(p.join("cgroup"), cgroup).unwrap();
        };
        let pw = "0::/user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service\n";
        add("10", "pipewire", 0x40_0100, MAPS, pw);
        add("11", "kworker/0:1", PF_KTHREAD, MAPS, "0::/\n");
        add("12", "fine (x) y", 0, "7f3a4000-7f3a5000 r--p 00000000 103:02 3311 /usr/lib/libfine.so.1\n", "0::/system.slice/a.service\n");
        std::fs::create_dir(d.path().join("self")).unwrap();
        let (stale, scanned) = scan(d.path());
        assert_eq!(scanned, 2);
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].pid, 10);
        assert_eq!(stale[0].unit, Some(user("pipewire.service")));
        assert_eq!(parse_stat("12 (fine (x) y) S 1 1 1 0 -1 64 0"), Some(("fine (x) y".into(), 64)));
    }

    #[test]
    fn user_and_system_services_restart_in_one_command_each() {
        let units = vec![user("pipewire.service"), system("cups.service"), user("wireplumber.service")];
        assert_eq!(
            restart_commands(&units),
            vec![
                vec!["/usr/bin/systemctl", "--user", "restart", "pipewire.service", "wireplumber.service"],
                vec![crate::tasks::PKEXEC, "/usr/bin/systemctl", "restart", "cups.service"],
            ]
        );
        assert!(restart_commands(&[]).is_empty());
    }

    #[test]
    fn nothing_stale_is_ok_and_offers_nothing() {
        let f = finding_from(&Status::default());
        assert_eq!(f.severity, OK);
        assert!(f.fix.is_none() && f.items.is_empty());
        assert_eq!(f.id, "restart-needed");
    }

    #[test]
    fn services_alone_are_info_with_a_fix_for_just_those() {
        let app = "app-Hyprland-gtk\\x2dlaunch-82a6189c.scope";
        let st = Status {
            stale: vec![
                stale(1, "pipewire", Some(user("pipewire.service"))),
                stale(2, "pipewire-pulse", Some(user("pipewire-pulse.service"))),
                stale(3, "spotify", Some(user(app))),
                stale(4, "spotify", Some(user(app))),
            ],
            ..Default::default()
        };
        let f = finding_from(&st);
        assert_eq!(f.severity, INFO);
        assert!(f.fix.is_some());
        assert_eq!(restartable(&st.stale), vec![user("pipewire-pulse.service"), user("pipewire.service")]);
        assert!(f.items.iter().any(|i| i == "App: spotify — close it and open it again"), "{:?}", f.items);
        assert!(f.fix_note.contains("systemctl --user restart pipewire-pulse.service pipewire.service"));
    }

    #[test]
    fn the_compositor_running_old_code_is_a_warning_with_no_restart_offered_for_it() {
        let st = Status {
            stale: vec![stale(1, "Hyprland", Some(user("wayland-wm@hyprland.desktop.service")))],
            ..Default::default()
        };
        let f = finding_from(&st);
        assert_eq!(f.severity, WARN);
        assert!(f.fix.is_none());
        assert!(f.fix_note.contains("Log out and back in"));
    }

    #[test]
    fn a_reboot_is_a_warning_and_restarting_services_first_is_not_offered() {
        let st = Status {
            reboot: Some("linux 6.17.3.arch1-1 is installed, but 6.17.2-arch1-1 is still running".into()),
            stale: vec![stale(1, "cupsd", Some(system("cups.service")))],
            ..Default::default()
        };
        let f = finding_from(&st);
        assert_eq!(f.severity, WARN);
        assert_eq!(f.title, "Restart needed");
        assert!(f.fix.is_none());
        assert!(f.fix_note.contains("Restart to finish updating"));
        assert!(f.items[0].starts_with("Computer: linux 6.17.3"));
    }
}
