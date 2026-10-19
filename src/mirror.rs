//! Why the update count jumped, and whether the batch needs a restart.
//!
//! A pending-update count on its own reads like something went wrong when it
//! goes from 5 to 370 overnight. Usually nothing did: on Omarchy, the Arch
//! repositories come from a stable mirror that holds packages back and
//! releases them in tested batches, so weeks of Arch arrive at once. On plain
//! Arch the same thing happens after weeks without a sync.
//!
//! Both dates are already on disk, so this reads no network:
//!
//! * pacman's own sync database (`/var/lib/pacman/sync/extra.db`) carries the
//!   mirror's Last-Modified time from the last `pacman -Sy`: the batch this
//!   machine has;
//! * checkupdates keeps a private copy it refreshes on every check
//!   (`$TMPDIR/checkup-db-$UID`), stamped the same way: the batch the mirror
//!   offers now.
//!
//! The gap between the two is how much Arch the pending updates span.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// pacman's configuration, for which mirror the Arch repositories use.
pub const PACMAN_CONF: &str = "/etc/pacman.conf";
/// The sync databases `pacman -Sy` writes.
pub const SYNC_DIR: &str = "/var/lib/pacman/sync";
/// Omarchy's held-back copy of the Arch repositories.
pub const OMARCHY_STABLE_HOST: &str = "stable-mirror.omarchy.org";
/// A gap shorter than this is ordinary day-to-day drift, not worth a note.
pub const NOTE_AFTER_DAYS: i64 = 7;

const DAY: f64 = 86_400.0;

/// Packages whose new version only takes effect after a restart: the kernel
/// and what it loads at boot, and libraries every running process has open.
/// Best known first, which is the order a note names them in.
pub const RESTART: &[&str] = &[
    "linux", "linux-lts", "linux-zen", "linux-hardened", "linux-rt", "systemd", "glibc",
    "mesa", "nvidia", "nvidia-open", "nvidia-utils", "linux-firmware", "intel-ucode",
    "amd-ucode", "dbus", "dbus-broker",
];

/// Where the Arch repositories come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Omarchy's stable mirror: packages arrive in batches.
    OmarchyStable,
    /// An ordinary Arch mirror, current to within hours.
    Rolling,
}

/// The two batch dates, as epoch seconds; None when a file is missing.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub source: Source,
    /// The batch this machine has (pacman's sync database).
    pub local: Option<f64>,
    /// The batch the mirror offers now (checkupdates' copy).
    pub upstream: Option<f64>,
}

/// One line for the Updates page: what to say, and its icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub icon: &'static str,
    pub title: String,
    pub body: String,
}

/// Whether `name`'s update needs a restart to take effect.
pub fn needs_restart(name: &str) -> bool {
    restart_rank(name) < RESTART.len()
}

/// Where `name` stands in [`RESTART`]; past the end when it is not there.
fn restart_rank(name: &str) -> usize {
    RESTART.iter().position(|r| *r == firmware_as_one(name)).unwrap_or(RESTART.len())
}

/// linux-firmware ships as a dozen split packages (linux-firmware-intel,
/// -amdgpu, ...) that update together; a note names them once.
fn firmware_as_one(name: &str) -> &str {
    if name.starts_with("linux-firmware-") { "linux-firmware" } else { name }
}

/// The mirror lines of pacman.conf and the files it includes, in order.
fn servers(conf: &Path, depth: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(conf) else { return Vec::new() };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim();
        match key.trim() {
            "Server" => out.push(value.to_string()),
            // A mirrorlist includes nothing further; one level is pacman's own
            // limit in practice, two guards against a loop.
            "Include" if depth < 2 => out.extend(servers(Path::new(value), depth + 1)),
            _ => {}
        }
    }
    out
}

/// Which kind of mirror a list of Server lines points at.
pub fn source_of(servers: &[String]) -> Source {
    let stable = servers.iter().any(|s| {
        s.split("://").nth(1).and_then(|rest| rest.split('/').next()) == Some(OMARCHY_STABLE_HOST)
    });
    if stable { Source::OmarchyStable } else { Source::Rolling }
}

fn mtime(path: &Path) -> Option<f64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// checkupdates' private database directory, as checkupdates itself works it
/// out.
pub fn checkupdates_db() -> PathBuf {
    if let Some(path) = std::env::var_os("CHECKUPDATES_DB").filter(|p| !p.is_empty()) {
        return PathBuf::from(path);
    }
    let tmp = std::env::var_os("TMPDIR").filter(|p| !p.is_empty()).unwrap_or_else(|| "/tmp".into());
    PathBuf::from(tmp).join(format!("checkup-db-{}", rustix::process::getuid().as_raw()))
}

/// The state of this machine (a fixed one in demo mode).
pub fn read() -> State {
    if crate::demo::active() {
        let now = chrono::Utc::now().timestamp() as f64;
        return State {
            source: Source::OmarchyStable,
            local: Some(now - 41.0 * DAY),
            upstream: Some(now - 0.5 * DAY),
        };
    }
    State {
        source: source_of(&servers(Path::new(PACMAN_CONF), 0)),
        local: mtime(&Path::new(SYNC_DIR).join("extra.db")),
        upstream: mtime(&checkupdates_db().join("sync/extra.db")),
    }
}

/// "6 weeks", "10 days", "3 months": a span in the unit a person would use.
pub fn span(days: i64) -> String {
    let (n, unit) = match days {
        ..=13 => (days, "day"),
        14..=59 => ((days + 3) / 7, "week"),
        _ => ((days + 15) / 30, "month"),
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

fn day_text(ts: f64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(ts as i64, 0).single() {
        Some(t) => t.format("%-d %b").to_string(),
        None => String::new(),
    }
}

/// "linux, systemd and glibc", "linux, systemd, glibc and 2 more".
fn name_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_string(),
        [first @ .., last] if names.len() <= 3 => format!("{} and {last}", first.join(", ")),
        _ => format!("{} and {} more", names[..3].join(", "), names.len() - 3),
    }
}

/// What to say above the pending updates. `repo_pending` names the pending
/// updates from the Arch and Omarchy repositories, the ones a mirror batch
/// brings; `all_pending` counts every pending update the page lists.
pub fn notes(state: &State, repo_pending: &[String], all_pending: usize) -> Vec<Note> {
    let mut out = Vec::new();
    if let (Some(local), Some(upstream)) = (state.local, state.upstream) {
        let days = ((upstream - local) / DAY).floor() as i64;
        if days >= NOTE_AFTER_DAYS && !repo_pending.is_empty() {
            let n = repo_pending.len();
            let updates = match n {
                _ if n < all_pending => format!("{n} of these updates {}", if n == 1 { "is" } else { "are" }),
                1 => "this update is".to_string(),
                _ => format!("these {n} updates are"),
            };
            let gap = span(days);
            out.push(match state.source {
                Source::OmarchyStable => Note {
                    icon: "software-update-available-symbolic",
                    title: format!("Omarchy released a new batch of Arch packages on {}", day_text(upstream)),
                    body: format!(
                        "Omarchy holds Arch updates back and releases them in tested batches. This \
                         machine's last batch is from {}, so {updates} {gap} of Arch arriving together.",
                        day_text(local)
                    ),
                },
                Source::Rolling => Note {
                    icon: "software-update-available-symbolic",
                    title: format!("Package lists last synced on {}", day_text(local)),
                    body: format!(
                        "Arch has moved on since then, so {updates} {gap} of Arch arriving together."
                    ),
                },
            });
        }
    }
    let mut restart: Vec<&str> =
        repo_pending.iter().map(|p| firmware_as_one(p)).filter(|p| needs_restart(p)).collect();
    restart.sort_by_key(|p| restart_rank(p));
    restart.dedup();
    if !restart.is_empty() {
        let body = match restart.as_slice() {
            [one] => format!("The update to {one} only takes effect after a restart."),
            _ => format!("Updates to {} only take effect after a restart.", name_list(&restart)),
        };
        out.push(Note { icon: "system-reboot-symbolic", title: "Restart after updating".to_string(), body });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn at(days_ago_local: f64, days_ago_upstream: f64, source: Source) -> State {
        let now = 1_792_300_000.0;
        State {
            source,
            local: Some(now - days_ago_local * DAY),
            upstream: Some(now - days_ago_upstream * DAY),
        }
    }

    #[test]
    fn the_stable_mirror_is_recognised_by_host() {
        let stable = pending(&["https://stable-mirror.omarchy.org/$repo/os/$arch"]);
        assert_eq!(source_of(&stable), Source::OmarchyStable);
        let rolling = pending(&["https://geo.mirror.pkgbuild.com/$repo/os/$arch"]);
        assert_eq!(source_of(&rolling), Source::Rolling);
        // The host, not a substring of the path.
        let lookalike = pending(&["https://example.org/stable-mirror.omarchy.org/$repo"]);
        assert_eq!(source_of(&lookalike), Source::Rolling);
        assert_eq!(source_of(&[]), Source::Rolling);
    }

    #[test]
    fn includes_are_followed_and_comments_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("mirrorlist");
        std::fs::write(&list, "# Server = https://old.example/\nServer = https://stable-mirror.omarchy.org/$repo/os/$arch\n")
            .unwrap();
        let conf = dir.path().join("pacman.conf");
        std::fs::write(&conf, format!("[options]\n[core]\nInclude = {}\n[omarchy]\nServer = https://pkgs.omarchy.org/stable/$arch\n", list.display()))
            .unwrap();
        let found = servers(&conf, 0);
        assert_eq!(found, pending(&[
            "https://stable-mirror.omarchy.org/$repo/os/$arch",
            "https://pkgs.omarchy.org/stable/$arch",
        ]));
        assert_eq!(source_of(&found), Source::OmarchyStable);
    }

    #[test]
    fn a_new_batch_explains_the_jump() {
        // This machine on 2026-10-18: the batch from 8 Sep, the mirror's from 17 Oct.
        let notes = notes(&at(40.0, 1.0, Source::OmarchyStable), &pending(&["aom", "argon2", "chromium"]), 3);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].title.starts_with("Omarchy released a new batch of Arch packages on "));
        assert!(notes[0].body.contains("these 3 updates are 6 weeks of Arch"), "{}", notes[0].body);
    }

    #[test]
    fn other_pending_updates_are_not_counted_as_the_batch() {
        let notes = notes(&at(40.0, 1.0, Source::OmarchyStable), &pending(&["aom", "argon2"]), 5);
        assert!(notes[0].body.contains("so 2 of these updates are 6 weeks"), "{}", notes[0].body);
        let notes = super::notes(&at(40.0, 1.0, Source::OmarchyStable), &pending(&["aom"]), 5);
        assert!(notes[0].body.contains("so 1 of these updates is 6 weeks"), "{}", notes[0].body);
    }

    #[test]
    fn a_rolling_mirror_says_when_it_last_synced() {
        let notes = notes(&at(21.0, 0.1, Source::Rolling), &pending(&["aom"]), 1);
        assert!(notes[0].title.starts_with("Package lists last synced on "));
        assert!(notes[0].body.contains("this update is 3 weeks of Arch"), "{}", notes[0].body);
        assert!(notes[0].body.contains("this update is"), "{}", notes[0].body);
    }

    #[test]
    fn ordinary_drift_and_missing_dates_say_nothing() {
        assert!(notes(&at(3.0, 0.0, Source::OmarchyStable), &pending(&["aom"]), 1).is_empty());
        // Nothing pending: there is no jump to explain.
        assert!(notes(&at(40.0, 0.0, Source::OmarchyStable), &[], 0).is_empty());
        // checkupdates has not run yet, or pacman has never synced.
        let mut s = at(40.0, 0.0, Source::OmarchyStable);
        s.upstream = None;
        assert!(notes(&s, &pending(&["aom"]), 1).is_empty());
        s.upstream = Some(1.0);
        s.local = None;
        assert!(notes(&s, &pending(&["aom"]), 1).is_empty());
        // A sync newer than checkupdates' copy is not a gap.
        assert!(notes(&at(0.0, 30.0, Source::Rolling), &pending(&["aom"]), 1).is_empty());
    }

    #[test]
    fn restart_packages_are_named_most_important_first() {
        let notes = notes(&at(0.0, 0.0, Source::Rolling), &pending(&["mesa", "aom", "glibc", "linux", "systemd"]), 5);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].title, "Restart after updating");
        assert_eq!(notes[0].body, "Updates to linux, systemd, glibc and 1 more only take effect after a restart.");
        let one = super::notes(&at(0.0, 0.0, Source::Rolling), &pending(&["linux-firmware-intel"]), 1);
        assert_eq!(one[0].body, "The update to linux-firmware only takes effect after a restart.");
        // The firmware's split packages count once, after the better known names.
        let split = super::notes(
            &at(0.0, 0.0, Source::Rolling),
            &pending(&["linux-firmware", "linux-firmware-amdgpu", "linux-firmware-intel", "linux", "systemd"]),
            5,
        );
        assert_eq!(split[0].body, "Updates to linux, systemd and linux-firmware only take effect after a restart.");
        let two = super::notes(&at(0.0, 0.0, Source::Rolling), &pending(&["glibc", "linux"]), 2);
        assert_eq!(two[0].body, "Updates to linux and glibc only take effect after a restart.");
        assert!(super::notes(&at(0.0, 0.0, Source::Rolling), &pending(&["aom", "pacman"]), 2).is_empty());
    }

    #[test]
    fn restart_rule() {
        for name in ["linux", "linux-zen", "linux-firmware", "linux-firmware-amdgpu", "glibc", "amd-ucode"] {
            assert!(needs_restart(name), "{name}");
        }
        for name in ["linux-api-headers", "linux-tools", "pacman", "mkinitcpio", "limine", "chromium"] {
            assert!(!needs_restart(name), "{name}");
        }
    }

    #[test]
    fn spans_read_naturally() {
        assert_eq!(span(1), "1 day");
        assert_eq!(span(10), "10 days");
        assert_eq!(span(14), "2 weeks");
        assert_eq!(span(40), "6 weeks");
        assert_eq!(span(60), "2 months");
        assert_eq!(span(100), "3 months");
    }

    #[test]
    fn checkupdates_db_follows_checkupdates() {
        let uid = rustix::process::getuid().as_raw();
        let path = checkupdates_db();
        if std::env::var_os("CHECKUPDATES_DB").is_none() && std::env::var_os("TMPDIR").is_none() {
            assert_eq!(path, PathBuf::from(format!("/tmp/checkup-db-{uid}")));
        }
    }
}
