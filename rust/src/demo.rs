//! Port of `demo.py`.
//!
//! Synthetic data for screenshots and demos.
//!
//! Almanac's screenshots would otherwise be photographs of whoever took them:
//! their installed packages, their connected services, their home directory,
//! their uptime. None of that has anything to do with the project, and a public
//! repository indexes it permanently.
//!
//! With `ALMANAC_DEMO=1` the local inventory is replaced by the fixtures below.
//! What is *not* replaced is the marketplace catalogue — those listings are
//! public data published by the marketplaces themselves, and showing the real
//! ones is the honest way to demonstrate the store.
//!
//! The fixtures are deliberately ordinary: packages any Arch machine might have,
//! versions that are plausible rather than real, and a spread of states so one
//! screenshot can show a security update, a routine update, something several
//! majors behind, and something perfectly current.

#![allow(dead_code)]

use serde_json::{json, Map, Value};

/// True when `ALMANAC_DEMO` is set to anything but "", "0" or "false".
pub fn active() -> bool {
    let Some(v) = std::env::var_os("ALMANAC_DEMO") else { return false };
    // Python decodes a value that is not UTF-8 with surrogateescape: never
    // "", "0" or "false" after strip(), so demo mode is on.
    let Some(v) = v.to_str() else { return true };
    // str.strip() also strips the ASCII separators \x1c-\x1f.
    let v = v.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c));
    !matches!(v, "" | "0" | "false")
}

pub const HOME: &str = "/home/user";

// (name, version, update, source, description, size_mb, extra)
const _PACKAGES: &[(&str, &str, &str, &str, &str, f64, &[(&str, &str)])] = &[
    ("chromium", "151.0.7620.9-1", "152.0.7700.1-1", "repo",
     "A web browser built for speed, simplicity, and security", 312.4,
     &[("critical", "security")]),
    ("openssh", "10.2p1-1", "10.3p1-1", "repo",
     "Premier connectivity tool for remote login with the SSH protocol", 7.1,
     &[("critical", "security")]),
    ("linux", "6.18.3.arch1-1", "6.18.5.arch1-1", "repo",
     "The Linux kernel and modules", 148.0, &[("critical", "core")]),
    ("mesa", "25.3.1-1", "25.3.2-1", "repo",
     "An open-source implementation of the OpenGL specification", 94.6,
     &[("critical", "core")]),
    ("firefox", "147.0-1", "148.0-1", "repo",
     "Standalone web browser from mozilla.org", 268.2, &[]),
    ("git", "2.52.0-1", "", "repo",
     "The fast distributed version control system", 28.4, &[]),
    ("ripgrep", "14.1.1-1", "", "repo",
     "A search tool that combines the usability of ag with the raw speed of grep",
     5.2, &[]),
    ("fd", "10.3.0-1", "10.4.0-1", "repo",
     "Simple, fast and user-friendly alternative to find", 3.1, &[]),
    ("bat", "0.26.1-2", "", "repo",
     "Cat clone with syntax highlighting and git integration", 6.0, &[]),
    ("htop", "3.4.1-1", "", "repo", "Interactive process viewer", 0.4, &[]),
    ("neovim", "0.11.4-1", "0.12.0-1", "repo",
     "Fork of Vim aiming to improve extensibility and usability", 32.8, &[]),
    ("tmux", "3.6-1", "", "repo",
     "A terminal multiplexer", 1.2, &[]),
    ("jq", "1.8.1-1", "", "repo",
     "Command-line JSON processor", 1.1, &[]),
    ("ffmpeg", "8.0-3", "8.0-4", "repo",
     "Complete solution to record, convert and stream audio and video", 42.7, &[]),
    ("imagemagick", "7.1.2.8-1", "", "repo",
     "An image viewing and manipulation program", 24.1, &[]),
    ("wl-clipboard", "2.2.1-3", "", "repo",
     "Command-line copy/paste utilities for Wayland", 0.2, &[]),
    ("starship", "1.24.0-1", "1.25.0-1", "aur",
     "The cross-shell prompt for astronauts", 9.8, &[]),
    ("lazygit", "0.58.0-1", "", "aur",
     "Simple terminal UI for git commands", 18.3, &[]),
    ("zoxide", "0.9.9-1", "", "aur",
     "A smarter cd command inspired by z and autojump", 2.4, &[]),
    ("btop", "1.4.5-1", "1.4.6-1", "aur",
     "A monitor of system resources, bpytop ported to C++", 3.9, &[]),
];

const _APPIMAGES: &[(&str, &str, &str, &str, f64)] = &[
    ("Inkscape", "1.4.2", "1.5.0", "Vector graphics editor", 142.0),
    ("Obsidian", "1.9.14", "", "Knowledge base that works on local Markdown", 118.5),
];

const _FLATPAKS: &[(&str, &str, &str, &str, &str, f64)] = &[
    ("Blender", "4.5.3", "", "org.blender.Blender",
     "Free and open source 3D creation suite", 1180.0),
];

const _LOCAL: &[(&str, &str, i64, bool, &str)] = &[
    ("example.bar-clock", "2.1.0", 0, false,
     "A clock for the bar, built from a local checkout"),
    ("example.weather", "1.4.2", 23, false,
     "Current conditions and the next few hours, in the bar"),
    ("example.scratchpad", "0.9.1", 7, true,
     "A scratch buffer that follows you between workspaces"),
];

// (display name, binary, installed, latest, manager, domain)
const _AGENTS: &[(&str, &str, &str, &str, &str, &str)] = &[
    ("Claude Code", "claude", "2.1.301", "2.1.344", "mise", "claude.com"),
    ("Codex CLI", "codex", "0.162.1", "0.166.0", "mise", "openai.com"),
    ("Gemini CLI", "gemini", "0.64.0", "0.64.0", "self", "gemini.google.com"),
    ("opencode", "opencode", "1.19.3", "1.19.3", "self", "opencode.ai"),
    ("GitHub Copilot CLI", "copilot", "1.0.98", "1.0.98", "self", "github.com"),
];

const _MCP: &[(&str, &str, &str, &str)] = &[
    ("Documentation", "https://mcp.example.com/docs", "connected", "ok"),
    ("Issue tracker", "https://mcp.example.com/issues", "connected", "ok"),
    ("Design system", "https://mcp.example.com/design", "connected", "ok"),
    ("Analytics", "https://mcp.example.com/analytics",
     "needs authentication", "info"),
];

const _SKILLS: &[(&str, &str)] = &[
    ("release-notes", "Draft release notes from the commits since the last tag"),
    ("review-checklist", "The team's review conventions, loaded on demand"),
];


fn s(v: &str) -> String {
    v.to_string()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

pub fn packages() -> Vec<crate::inventory::App> {
    use crate::inventory;
    let mut out = Vec::new();
    for &(name, version, update, source, desc, size, extra) in _PACKAGES {
        let mut e: Map<String, Value> =
            extra.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
        e.insert(s("size"), json!(format!("{size:.2} MiB")));
        e.insert(s("is_app"), json!(["chromium", "firefox", "neovim"].contains(&name)));
        e.insert(s("url"), json!(format!("https://example.org/{name}")));
        e.insert(s("installed"), json!("1mo ago"));
        if !update.is_empty() {
            e.insert(s("behind"), json!(inventory::versions_behind(version, update)));
        }
        out.push(inventory::App {
            name: s(name),
            version: s(version),
            source: s(source),
            description: s(desc),
            update: s(update),
            extra: e.into(),
            ..Default::default()
        });
    }
    out
}

pub fn appimages() -> Vec<crate::inventory::App> {
    use crate::inventory;
    let mut out = Vec::new();
    for &(name, version, update, desc, size) in _APPIMAGES {
        let lower = name.to_lowercase();
        let mut extra = Map::new();
        extra.insert(s("key"), json!(lower));
        extra.insert(s("size"), json!(format!("{size:.2} MiB")));
        extra.insert(s("is_app"), json!(true));
        extra.insert(s("present"), json!(true));
        extra.insert(s("url"), json!(format!("https://example.org/{lower}")));
        if !update.is_empty() {
            extra.insert(s("behind"), json!(inventory::versions_behind(version, update)));
            extra.insert(s("download"), json!("https://example.org/download"));
        }
        out.push(inventory::App {
            name: s(name),
            version: s(version),
            source: s(inventory::APPIMAGE),
            description: s(desc),
            update: s(update),
            exec_path: format!("{HOME}/.local/state/almanac/apps/{lower}.AppImage"),
            extra: extra.into(),
        });
    }
    out
}

pub fn flatpaks() -> Vec<crate::inventory::App> {
    use crate::inventory;
    _FLATPAKS
        .iter()
        .map(|&(name, version, update, appid, desc, size)| {
            let mut extra = Map::new();
            extra.insert(s("appid"), json!(appid));
            extra.insert(s("size"), json!(format!("{size:.2} MiB")));
            extra.insert(s("is_app"), json!(true));
            inventory::App {
                name: s(name),
                version: s(version),
                source: s(inventory::FLATPAK),
                description: s(desc),
                update: s(update),
                extra: extra.into(),
                ..Default::default()
            }
        })
        .collect()
}

pub fn local_builds() -> Vec<crate::inventory::App> {
    use crate::inventory;
    let mut out = Vec::new();
    for &(name, version, behind, dirty, desc) in _LOCAL {
        let mut extra = Map::new();
        extra.insert(s("repo"), json!(format!("{HOME}/.config/omarchy/plugins/{name}")));
        extra.insert(s("git_behind"), json!(behind));
        extra.insert(s("git_dirty"), json!(dirty));
        extra.insert(s("git_subject"), json!("Tidy up the panel layout"));
        extra.insert(s("url"), json!(format!("https://github.com/example/{name}")));
        out.push(inventory::App {
            name: s(name),
            version: s(version),
            source: s(inventory::LOCAL),
            description: s(desc),
            exec_path: String::new(),
            extra: extra.into(),
            ..Default::default()
        });
    }
    out
}

pub fn agents() -> Vec<crate::inventory::App> {
    use crate::inventory;
    let mut out = Vec::new();
    for &(name, binary, version, latest, manager, domain) in _AGENTS {
        let update = if latest != version { latest } else { "" };
        let mut extra = Map::new();
        extra.insert(s("key"), json!(binary));
        extra.insert(s("binary"), json!(binary));
        extra.insert(s("manager"), json!(manager));
        extra.insert(s("domain"), json!(domain));
        extra.insert(s("is_app"), json!(false));
        extra.insert(s("url"), json!(format!("https://{domain}")));
        extra.insert(
            s("update_cmd"),
            if manager == "mise" { json!(["mise", "upgrade", binary]) } else { json!([binary, "update"]) },
        );
        extra.insert(
            s("behind"),
            if !update.is_empty() {
                json!(inventory::versions_behind(version, latest))
            } else {
                json!((0, ""))
            },
        );
        out.push(inventory::App {
            name: s(name),
            version: s(version),
            source: s(inventory::AGENT),
            description: format!("AI coding agent · managed by {manager}"),
            update: s(update),
            exec_path: format!("{HOME}/.local/bin/{binary}"),
            extra: extra.into(),
        });
    }
    out
}

pub fn collect() -> Vec<crate::inventory::App> {
    let mut out = packages();
    out.extend(appimages());
    out.extend(flatpaks());
    out.extend(local_builds());
    out.extend(agents());
    out
}

/// A believable spread of health results, with real-looking sizes.
pub fn findings() -> Vec<crate::maintenance::Finding> {
    use crate::maintenance::{self as m, Finding, Fix};
    vec![
        Finding {
            id: s("pacman-cache"),
            title: s("Package cache"),
            detail: s("3.1 GB in 412 downloaded packages. Keeping a couple \
                       of versions makes a downgrade possible; keeping all \
                       of them since install does not."),
            severity: s(m::WARN),
            count: 412,
            size_mb: 3174.0,
            fix_label: s("Trim to 2 versions"),
            fix: Some(Fix::argv(&["paccache", "-rk2"])),
            fix_note: s("Keeps the 2 most recent versions of installed \
                         packages."),
            ..Default::default()
        },
        Finding {
            id: s("journal"),
            title: s("Systemd journal"),
            detail: s("820 MB of logs. Useful for diagnosing the recent \
                       past; months of it rarely is."),
            severity: s(m::WARN),
            size_mb: 820.0,
            fix_label: s("Vacuum to 500 MB"),
            fix: Some(Fix::argv(&["journalctl", "--vacuum-size=500M"])),
            fix_note: s("Deletes the oldest journal files."),
            ..Default::default()
        },
        Finding {
            id: s("orphans"),
            title: s("Orphaned packages"),
            detail: s("3 installed as dependencies that nothing needs any \
                       more. Build tooling and debug symbols are kept \
                       deliberately."),
            severity: s(m::INFO),
            count: 3,
            items: strs(&["libexample", "libexample-utils", "oldcodec"]),
            fix_label: s("Review and remove"),
            fix: Some(Fix::argv(&["pacman", "-Rns", "--noconfirm", "libexample"])),
            fix_note: s("Removes only the packages listed above."),
            ..Default::default()
        },
        Finding {
            id: s("yay-cache"),
            title: s("AUR build cache"),
            detail: s("410 MB of cloned PKGBUILDs and build artefacts. \
                       Rebuilt on demand; nothing installed depends on it."),
            severity: s(m::INFO),
            size_mb: 410.0,
            count: 96,
            fix_label: s("Clear"),
            fix: Some(Fix::argv(&["true"])),
            fix_note: s("Deletes the AUR build cache."),
            ..Default::default()
        },
        Finding {
            id: s("rollbacks"),
            title: s("AppImage rollback copies"),
            detail: s("1 previous build kept after an update, 118 MB. It \
                       exists so a bad update can be reverted."),
            severity: s(m::INFO),
            count: 1,
            size_mb: 118.0,
            items: strs(&["inkscape.previous"]),
            fix_label: s("Delete rollback copies"),
            fix: Some(Fix::argv(&["true"])),
            fix_note: s("Deletes the .previous files."),
            ..Default::default()
        },
        Finding {
            id: s("vulnerabilities"),
            title: s("Known vulnerabilities"),
            detail: s("2 installed package(s) have known vulnerabilities; \
                       2 can be fixed by updating."),
            severity: s(m::CRIT),
            count: 2,
            items: strs(&[
                "chromium 151.0.7620.9-1 — high, arbitrary code \
                 execution; fixed in 152.0.7700.1-1 · CVE-2026-0001",
                "openssh 10.2p1-1 — medium, information disclosure; \
                 fixed in 10.3p1-1 · CVE-2026-0002",
            ]),
            fix_note: s("Update all installs every fixed version."),
            ..Default::default()
        },
        Finding {
            id: s("snapshot"),
            title: s("Update snapshots"),
            detail: s("The last update (today 09:13: fd) can be undone — a \
                       snapshot was taken just before it."),
            severity: s(m::OK),
            ..Default::default()
        },
        Finding {
            id: s("pacnew"),
            title: s("Unmerged config files"),
            detail: s("No .pacnew or .pacsave files in /etc."),
            severity: s(m::OK),
            ..Default::default()
        },
        Finding {
            id: s("failed-units"),
            title: s("Failed services"),
            detail: s("No failed units."),
            severity: s(m::OK),
            ..Default::default()
        },
        Finding {
            id: s("dangling-desktop"),
            title: s("Broken launcher entries"),
            detail: s("Every launcher entry points at something real."),
            severity: s(m::OK),
            ..Default::default()
        },
        Finding {
            id: s("broken-symlinks"),
            title: s("Broken symlinks on PATH"),
            detail: s("No broken links in ~/.local/bin."),
            severity: s(m::OK),
            ..Default::default()
        },
        Finding {
            id: s("disk"),
            title: s("Disk space"),
            detail: s("286.4 GB free of 474.9 GB on / (40% used)."),
            severity: s(m::OK),
            size_mb: 293273.0,
            ..Default::default()
        },
    ]
}

/// The AI estate, with generic services rather than anyone's real ones.
pub fn ai_groups() -> Vec<(String, String, Vec<crate::ai::Entry>)> {
    use crate::ai::{self, Entry};

    let mut harnesses = Vec::new();
    for &(name, binary, version, latest, manager, domain) in _AGENTS {
        let update = if latest != version { latest } else { "" };
        let behind = if !update.is_empty() { "2 minor behind" } else { "current" };
        harnesses.push(Entry {
            kind: s(ai::HARNESS),
            name: s(name),
            detail: format!("{manager}-managed · {HOME}/.local/bin/{binary}"),
            version: s(version),
            update: s(update),
            status: s(behind),
            severity: s(if !update.is_empty() { "warn" } else { "ok" }),
            icon_key: s(domain),
            icon_domain: s(domain),
            fallback_icon: s("utilities-terminal-symbolic"),
            action_label: s(if !update.is_empty() { "Update" } else { "" }),
            action: if !update.is_empty() { Some(strs(&["mise", "upgrade", binary])) } else { None },
            url: format!("https://{domain}"),
            ..Default::default()
        });
    }

    let servers: Vec<Entry> = _MCP
        .iter()
        .map(|&(name, url, status, sev)| Entry {
            kind: s(ai::MCP),
            name: s(name),
            detail: s(url),
            status: s(status),
            severity: s(sev),
            url: s(url),
            fallback_icon: s("network-server-symbolic"),
            action_label: s(if sev == "info" { "Authenticate" } else { "" }),
            ..Default::default()
        })
        .collect();

    let skills: Vec<Entry> = _SKILLS
        .iter()
        .map(|&(name, desc)| Entry {
            kind: s(ai::SKILL),
            name: s(name),
            detail: s(desc),
            status: s("user skill"),
            severity: s("ok"),
            fallback_icon: s("text-x-generic-symbolic"),
            ..Default::default()
        })
        .collect();

    let market = vec![Entry {
        kind: s(ai::MARKET),
        name: s("example-marketplace"),
        detail: s("24 plugins available · example/claude-plugins"),
        status: s("synced 2026-10-14"),
        severity: s("info"),
        fallback_icon: s("system-software-install-symbolic"),
        action_label: s("Sync"),
        action: Some(strs(&["true"])),
        url: s("https://github.com/example/claude-plugins"),
        ..Default::default()
    }];

    let hooks = vec![Entry {
        kind: s(ai::HOOK),
        name: s("SessionStart"),
        detail: format!("bash {HOME}/.claude/hooks/session.sh"),
        status: s("matches *"),
        severity: s("ok"),
        fallback_icon: s("system-run-symbolic"),
        ..Default::default()
    }];

    let specs = ai::group_specs();
    let mut data: std::collections::HashMap<&str, Vec<Entry>> = std::collections::HashMap::new();
    data.insert("Harnesses", harnesses);
    data.insert("MCP servers", servers);
    data.insert("Skills", skills);
    data.insert("Plugins", Vec::new());
    data.insert("Marketplaces", market);
    data.insert("Hooks", hooks);
    specs
        .into_iter()
        .map(|(title, desc, _fn)| (s(title), s(desc), data.get(title).cloned().unwrap_or_default()))
        .collect()
}

// Marketplace entries to show as already installed, so cards demonstrate both
// states without touching what is really on the machine.
pub const INSTALLED_PLUGIN_IDS: &[&str] = &["omarchy.clock", "omarchy.battery"];
pub const INSTALLED_THEMES: &[&str] = &["solitude"];

/// A believable week, so the History page can be shown without anyone's.
///
/// Python's demo.history() lets an exception reach its caller; with the real
/// clock none can happen (only a `now` outside datetime's range raises).
/// `history_result` gives the error to callers that can take it; this one,
/// like history::timeline, prints it and shows no days.
pub fn history() -> Vec<crate::history::Day> {
    history_result().unwrap_or_else(|e| {
        eprintln!("{e}");
        Vec::new()
    })
}

/// `history()`, with Python's exceptions as `Err`.
pub fn history_result() -> Result<Vec<crate::history::Day>, String> {
    history_at(crate::util::now())
}

/// The C library's local time zone. time.localtime, time.mktime and
/// datetime.fromtimestamp all go through glibc, and glibc resolves `TZ`
/// differently from chrono's Local (an unknown name such as `Europe/Nowhere`
/// or `foo` is UTC to glibc, /etc/localtime to chrono), so glibc's tzset() is
/// reproduced here:
///
/// - `TZ` unset: /etc/localtime, UTC without it;
/// - `TZ=""`: "Universal"; a leading ':' is dropped;
/// - a TZif file, absolute or under `$TZDIR` (default /usr/share/zoneinfo),
///   with its footer TZ string for times after the last transition;
/// - otherwise a POSIX TZ string, parsed the way glibc parses it: whatever
///   does not parse is left zeroed, so a name with no offset is UTC, and a
///   DST name with no rules takes its transitions from `posixrules`.
///
/// Leap-second ("right/") zones are read without their leap corrections.
mod tz {
    use chrono::Datelike;
    use std::sync::OnceLock;

    #[derive(Clone, Copy, Default, PartialEq)]
    enum Kind {
        /// `n`: zero-based day of the year, counting Feb 29.
        #[default]
        J0,
        /// `Jn`: one-based day of the year, never counting Feb 29.
        J1,
        /// `Mm.n.d`: day d of week n of month m.
        M,
    }

    /// glibc's tz_rule; Default is its memset to zero.
    #[derive(Clone, Copy, Default)]
    struct Rule {
        kind: Kind,
        m: u16,
        n: u16,
        d: u16,
        /// Time of day of the change, local time.
        secs: i64,
        /// UTC offset, seconds east.
        offset: i64,
    }

    /// tz_rules[0] (standard time, its rule the start of DST) and
    /// tz_rules[1] (DST, its rule the end).
    #[derive(Clone, Copy, Default)]
    pub(super) struct Posix {
        rules: [Rule; 2],
    }

    #[derive(Clone, Copy)]
    struct TType {
        offset: i64,
        isdst: bool,
        isstd: bool,
    }

    pub(super) struct TzFile {
        transitions: Vec<i64>,
        idxs: Vec<usize>,
        types: Vec<TType>,
        spec: Option<Posix>,
    }

    pub(super) enum Zone {
        File(TzFile),
        Posix(Posix),
    }

    /// The C string's byte at `i` (NUL past the end).
    fn at(s: &[u8], i: usize) -> u8 {
        s.get(i).copied().unwrap_or(0)
    }

    /// sscanf's `%hu` at `s[i..]`: white space skipped, an optional sign and
    /// digits read as strtoul does (saturating; '-' negates modulo 2^64), the
    /// low 16 bits kept. The value and the index after it.
    fn scan_hu(s: &[u8], mut i: usize) -> Option<(u16, usize)> {
        while matches!(at(s, i), b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') {
            i += 1;
        }
        let neg = at(s, i) == b'-';
        if matches!(at(s, i), b'+' | b'-') {
            i += 1;
        }
        if !at(s, i).is_ascii_digit() {
            return None;
        }
        let (mut v, mut over) = (0u64, false);
        while at(s, i).is_ascii_digit() {
            match v.checked_mul(10).and_then(|x| x.checked_add(u64::from(at(s, i) - b'0'))) {
                Some(x) => v = x,
                None => over = true,
            }
            i += 1;
        }
        let v = if over { u64::MAX } else if neg { v.wrapping_neg() } else { v };
        Some((v as u16, i))
    }

    /// `sscanf(s, "<sep>%hu<sep>%hu<sep>%hu%n")` with `seps` before each
    /// field (0 for none): how many fields were read (each stored as it is
    /// read, the rest keep `vals`), and the characters consumed up to the
    /// last one read.
    fn scan3(s: &[u8], i: usize, seps: [u8; 3], mut vals: [u16; 3]) -> (usize, [u16; 3], usize) {
        let (mut got, mut consumed, mut j) = (0, 0, i);
        for (k, &sep) in seps.iter().enumerate() {
            if sep != 0 {
                if at(s, j) != sep {
                    break;
                }
                j += 1;
            }
            match scan_hu(s, j) {
                Some((v, e)) => {
                    vals[k] = v;
                    j = e;
                    got += 1;
                    consumed = j - i;
                }
                None => break,
            }
        }
        (got, vals, consumed)
    }

    fn parse_tzname(s: &[u8], i: usize) -> Option<usize> {
        let mut p = i;
        while at(s, p).is_ascii_alphabetic() {
            p += 1;
        }
        if p - i >= 3 {
            return Some(p);
        }
        if at(s, i) != b'<' {
            return None;
        }
        let start = i + 1;
        let mut p = start;
        while matches!(at(s, p), b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'+' | b'-') {
            p += 1;
        }
        if at(s, p) != b'>' || p - start < 3 {
            return None;
        }
        Some(p + 1)
    }

    fn parse_offset(s: &[u8], i: &mut usize, which: usize, r: &mut [Rule; 2]) -> bool {
        let c = at(s, *i);
        if which == 0 && (c == 0 || (c != b'+' && c != b'-' && !c.is_ascii_digit())) {
            return false;
        }
        let mut j = *i;
        let sign: i64 = match c {
            b'-' => 1,
            _ => -1,
        };
        if c == b'+' || c == b'-' {
            j += 1;
        }
        *i = j;
        let (got, [hh, mm, ss], consumed) = scan3(s, j, [0, b':', b':'], [0, 0, 0]);
        if got > 0 {
            // compute_offset() clamps each field.
            r[which].offset = sign
                * (i64::from(ss.min(59)) + i64::from(mm.min(59)) * 60 + i64::from(hh.min(24)) * 3600);
        } else if which == 0 {
            // Standard time defaults to offset zero.
            r[0].offset = 0;
            return false;
        } else {
            // DST defaults to one hour later than standard time.
            r[1].offset = r[0].offset + 3600;
        }
        *i = j + consumed;
        true
    }

    fn parse_rule(s: &[u8], i: &mut usize, which: usize, r: &mut [Rule; 2]) -> bool {
        let mut j = *i;
        // Ignore comma to support string following the incorrect
        // specification in early POSIX.1 printings.
        if at(s, j) == b',' {
            j += 1;
        }
        let tzr = &mut r[which];
        let c = at(s, j);
        if c == b'J' || c.is_ascii_digit() {
            tzr.kind = if c == b'J' { Kind::J1 } else { Kind::J0 };
            if c == b'J' {
                j += 1;
                if !at(s, j).is_ascii_digit() {
                    return false;
                }
            }
            let mut d = 0u64;
            while at(s, j).is_ascii_digit() {
                d = d.saturating_mul(10).saturating_add(u64::from(at(s, j) - b'0'));
                j += 1;
            }
            if d > 365 || (tzr.kind == Kind::J1 && d == 0) {
                return false;
            }
            tzr.d = d as u16;
        } else if c == b'M' {
            tzr.kind = Kind::M;
            let (got, [m, n, d], consumed) = scan3(s, j + 1, [0, b'.', b'.'], [tzr.m, tzr.n, tzr.d]);
            (tzr.m, tzr.n, tzr.d) = (m, n, d);
            if got != 3 || !(1..=12).contains(&m) || !(1..=5).contains(&n) || d > 6 {
                return false;
            }
            j += 1 + consumed;
        } else if c == 0 {
            // The U.S. rules, "M3.2.0,M11.1.0" (2:00 is the default).
            tzr.kind = Kind::M;
            (tzr.m, tzr.n, tzr.d) = if which == 0 { (3, 2, 0) } else { (11, 1, 0) };
        } else {
            return false;
        }
        match at(s, j) {
            0 | b',' => tzr.secs = 2 * 3600,
            b'/' => {
                j += 1;
                if at(s, j) == 0 {
                    return false;
                }
                let negative = at(s, j) == b'-';
                if negative {
                    j += 1;
                }
                let (_, [hh, mm, ss], consumed) = scan3(s, j, [0, b':', b':'], [2, 0, 0]);
                j += consumed;
                let secs = i64::from(hh) * 3600 + i64::from(mm) * 60 + i64::from(ss);
                tzr.secs = if negative { -secs } else { secs };
            }
            _ => return false,
        }
        *i = j;
        true
    }

    /// __tzset_parse_tz: the rules, and whether glibc would first try
    /// __tzfile_default (a DST name and no rules).
    fn parse_tz(s: &[u8]) -> (Posix, bool) {
        let mut r = [Rule::default(); 2];
        let mut wants_default = false;
        if let Some(mut i) = parse_tzname(s, 0) {
            if parse_offset(s, &mut i, 0, &mut r) {
                if at(s, i) != 0 {
                    if let Some(e) = parse_tzname(s, i) {
                        i = e;
                        parse_offset(s, &mut i, 1, &mut r);
                        wants_default = at(s, i) == 0 || (at(s, i) == b',' && at(s, i + 1) == 0);
                    }
                    if parse_rule(s, &mut i, 0, &mut r) {
                        parse_rule(s, &mut i, 1, &mut r);
                    }
                } else {
                    // There is no DST.
                    r[1].offset = r[0].offset;
                }
            }
        }
        (Posix { rules: r }, wants_default)
    }

    const MON_YDAY: [[i64; 13]; 2] = [
        [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365],
        [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335, 366],
    ];

    fn isleap(y: i64) -> bool {
        y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
    }

    /// compute_change(): the instant of the rule's change in `year`. As in
    /// glibc, a year up to 1970 counts from 1970-01-01.
    fn compute_change(r: &Rule, year: i64) -> i64 {
        let mut t = if year > 1970 {
            ((year - 1970) * 365 + ((year - 1) / 4 - 1970 / 4) - ((year - 1) / 100 - 1970 / 100)
                + ((year - 1) / 400 - 1970 / 400))
                * 86400
        } else {
            0
        };
        match r.kind {
            Kind::J1 => {
                t += (i64::from(r.d) - 1) * 86400;
                if r.d >= 60 && isleap(year) {
                    t += 86400;
                }
            }
            Kind::J0 => t += i64::from(r.d) * 86400,
            // m is 1..12 except after a failed parse, where glibc reads
            // outside its table; no day is added then.
            Kind::M if (1..=12).contains(&r.m) => {
                let mi = usize::from(r.m);
                let days = &MON_YDAY[usize::from(isleap(year))];
                t += days[mi - 1] * 86400;
                // Zeller's Congruence: day of the week of the month's first.
                let m = i64::from(r.m);
                let m1 = (m + 9) % 12 + 1;
                let yy0 = if m <= 2 { year - 1 } else { year };
                let (yy1, yy2) = (yy0 / 100, yy0 % 100);
                let mut dow = ((26 * m1 - 2) / 10 + 1 + yy2 + yy2 / 4 + yy1 / 4 - 2 * yy1) % 7;
                if dow < 0 {
                    dow += 7;
                }
                let mut d = i64::from(r.d) - dow;
                if d < 0 {
                    d += 7;
                }
                for _ in 1..r.n {
                    if d + 7 >= days[mi] - days[mi - 1] {
                        break;
                    }
                    d += 7;
                }
                t += d * 86400;
            }
            Kind::M => {}
        }
        t - r.offset + r.secs
    }

    /// __tz_compute: the rules for the UTC year of `t`.
    fn posix_compute(p: &Posix, t: i64) -> Option<(i64, bool)> {
        let year = i64::from(chrono::DateTime::from_timestamp(t, 0)?.year());
        let c0 = compute_change(&p.rules[0], year);
        let c1 = compute_change(&p.rules[1], year);
        // The southern hemisphere's DST ends in the next year.
        let isdst = if c0 > c1 { t < c1 || t >= c0 } else { t >= c0 && t < c1 };
        Some((p.rules[usize::from(isdst)].offset, isdst))
    }

    /// __tzfile_compute.
    fn file_compute(f: &TzFile, t: i64) -> Option<(i64, bool)> {
        let ty = |i: usize| f.types.get(i).map(|x| (x.offset, x.isdst));
        let Some(&last) = f.transitions.last() else {
            return ty(f.types.iter().position(|x| !x.isdst).unwrap_or(0));
        };
        if t < f.transitions[0] {
            // Before any transition: the first non-DST type.
            return ty(f.types.iter().position(|x| !x.isdst).unwrap_or(0));
        }
        if t >= last {
            if let Some(r) = f.spec.as_ref().and_then(|p| posix_compute(p, t)) {
                return Some(r);
            }
            return ty(*f.idxs.last()?);
        }
        let i = f.transitions.partition_point(|&x| x <= t);
        ty(*f.idxs.get(i.checked_sub(1)?)?)
    }

    /// __tzfile_read on the file's bytes: the 64-bit data when there is any.
    fn read_tzfile(data: &[u8]) -> Option<TzFile> {
        let be32 = |o: usize| -> Option<usize> {
            Some(u32::from_be_bytes(data.get(o..o + 4)?.try_into().ok()?) as usize)
        };
        let header = |o: usize| -> Option<[usize; 6]> {
            if data.get(o..o + 4)? != b"TZif" {
                return None;
            }
            let mut c = [0; 6];
            for (k, v) in c.iter_mut().enumerate() {
                *v = be32(o + 20 + 4 * k)?;
            }
            Some(c)
        };
        let mut c = header(0)?;
        let (mut off, mut width) = (0usize, 4usize);
        if *data.get(4)? != 0 {
            let [isut, isstd, leap, time, types, chars] = c;
            off = 44 + time * 5 + types * 6 + chars + leap * 8 + isstd + isut;
            c = header(off)?;
            width = 8;
        }
        let [isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt] = c;
        if typecnt == 0 || isstdcnt > typecnt || isutcnt > typecnt {
            return None;
        }
        let mut p = off + 44;
        let mut transitions = Vec::with_capacity(timecnt.min(data.len()));
        for k in 0..timecnt {
            let b = data.get(p + k * width..p + (k + 1) * width)?;
            transitions.push(if width == 8 {
                i64::from_be_bytes(b.try_into().ok()?)
            } else {
                i64::from(i32::from_be_bytes(b.try_into().ok()?))
            });
        }
        p += timecnt * width;
        let idxs: Vec<usize> = data.get(p..p + timecnt)?.iter().map(|&b| usize::from(b)).collect();
        if idxs.iter().any(|&i| i >= typecnt) {
            return None;
        }
        p += timecnt;
        let mut types = Vec::with_capacity(typecnt);
        for k in 0..typecnt {
            let b = data.get(p + 6 * k..p + 6 * k + 6)?;
            types.push(TType {
                offset: i64::from(i32::from_be_bytes(b[..4].try_into().ok()?)),
                isdst: b[4] != 0,
                isstd: false,
            });
        }
        p += 6 * typecnt + charcnt + leapcnt * (width + 4);
        for (t, &flag) in types.iter_mut().zip(data.get(p..p + isstdcnt)?) {
            t.isstd = flag != 0;
        }
        p += isstdcnt;
        data.get(p..p + isutcnt)?;
        p += isutcnt;
        // The footer: "\n<TZ string>\n", read as a C string.
        let spec = match data.get(p..) {
            Some(rest) if width == 8 && rest.len() > 1 && rest[0] == b'\n' => {
                let s = &rest[1..rest.len() - 1];
                let s = &s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())];
                (!s.is_empty()).then(|| parse_tz(s).0)
            }
            _ => None,
        };
        Some(TzFile { transitions, idxs, types, spec })
    }

    fn read_named(name: &[u8]) -> Option<TzFile> {
        use std::os::unix::ffi::OsStrExt;
        if name.is_empty() {
            return None;
        }
        let path = if name[0] == b'/' {
            std::path::PathBuf::from(std::ffi::OsStr::from_bytes(name))
        } else {
            let dir = std::env::var_os("TZDIR")
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| "/usr/share/zoneinfo".into());
            std::path::Path::new(&dir).join(std::ffi::OsStr::from_bytes(name))
        };
        read_tzfile(&std::fs::read(path).ok()?)
    }

    /// __tzfile_default: `posixrules`' transitions moved to the given
    /// offsets (its footer, for later times, is kept as it is).
    fn tzfile_default(stdoff: i64, dstoff: i64) -> Option<TzFile> {
        let mut f = read_named(b"posixrules")?;
        if f.types.len() < 2 {
            return None;
        }
        // The offsets the file's own rules use: those of the latest
        // transitions to each flavour.
        let (mut rule_std, mut rule_dst) = (None, None);
        for &i in f.idxs.iter().rev() {
            let t = f.types[i];
            if !t.isdst && rule_std.is_none() {
                rule_std = Some(t.offset);
            } else if t.isdst && rule_dst.is_none() {
                rule_dst = Some(t.offset);
            }
            if rule_std.is_some() && rule_dst.is_some() {
                break;
            }
        }
        let (rule_std, rule_dst) = if f.transitions.is_empty() {
            (f.types[0].offset, f.types[0].offset)
        } else {
            let s = rule_std.unwrap_or(0);
            (s, rule_dst.unwrap_or(s))
        };
        let mut isdst = false;
        for k in 0..f.transitions.len() {
            let tt = f.types[f.idxs[k]];
            f.idxs[k] = usize::from(tt.isdst);
            f.transitions[k] += if tt.isstd {
                stdoff - rule_std
            } else if isdst {
                dstoff - rule_dst
            } else {
                stdoff - rule_std
            };
            isdst = tt.isdst;
        }
        f.types.truncate(2);
        f.types[0] = TType { offset: stdoff, isdst: false, ..f.types[0] };
        f.types[1] = TType { offset: dstoff, isdst: true, ..f.types[1] };
        Some(f)
    }

    /// tzset_internal, from the given value of `TZ`.
    pub(super) fn load(tz: Option<&[u8]>) -> Zone {
        let utc = || Zone::Posix(Posix::default());
        let Some(tz) = tz else {
            return read_named(b"/etc/localtime").map_or_else(utc, Zone::File);
        };
        let tz: &[u8] = if tz.is_empty() {
            b"Universal"
        } else if tz[0] == b':' {
            &tz[1..]
        } else {
            tz
        };
        if let Some(f) = read_named(tz) {
            return Zone::File(f);
        }
        // No data file found: default to UTC if nothing was specified.
        if tz.is_empty() || tz == b"/etc/localtime" {
            return utc();
        }
        let (p, wants_default) = parse_tz(tz);
        if wants_default {
            if let Some(f) = tzfile_default(p.rules[0].offset, p.rules[1].offset) {
                return Zone::File(f);
            }
        }
        Zone::Posix(p)
    }

    impl Zone {
        /// localtime_r's (tm_gmtoff, tm_isdst) at `t`; None where it fails.
        pub(super) fn at(&self, t: i64) -> Option<(i64, bool)> {
            match self {
                Zone::File(f) => file_compute(f, t),
                Zone::Posix(p) => posix_compute(p, t),
            }
        }
    }

    /// The process's zone. glibc reads `TZ` once for localtime and again on
    /// every mktime, but nothing here changes it.
    pub(super) fn localtime(t: i64) -> Option<(i64, bool)> {
        use std::os::unix::ffi::OsStrExt;
        static ZONE: OnceLock<Zone> = OnceLock::new();
        ZONE.get_or_init(|| load(std::env::var_os("TZ").as_deref().map(|v| v.as_bytes()))).at(t)
    }
}

/// glibc's `static localtime_offset` in mktime(): the UTC offset the last
/// call found, used as the first guess of the next. Which instant an
/// ambiguous wall-clock time resolves to depends on it, so it is kept
/// across calls exactly as the C library keeps it. glibc has one for the
/// whole process; in the Python app `time.mktime` is called only by
/// demo.history, so this is that one cell, and `mktime` below is the port's
/// only mktime: anything else that ports a `time.mktime` call must use it.
static MKTIME_OFFSET: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// `time.mktime((y, m, d, hour, minute, 0, 0, 0, -1))`: glibc's
/// __mktime_internal, probing localtime() from the last call's offset.
/// Err is CPython's OverflowError("mktime argument out of range").
pub(crate) fn mktime(date: chrono::NaiveDate, hour: u32, minute: u32) -> Result<i64, String> {
    use std::sync::atomic::Ordering;
    let overflow = || "OverflowError: mktime argument out of range".to_string();
    let n = date.and_hms_opt(hour, minute, 0).ok_or_else(overflow)?.and_utc().timestamp();
    let t0 = n + MKTIME_OFFSET.load(Ordering::Relaxed);
    let (mut t, mut t1, mut t2) = (t0, t0, t0);
    // tm_isdst of the previous probe.
    let mut dst2 = false;
    let mut remaining_probes = 6;
    loop {
        let (off, isdst) = tz::localtime(t).ok_or_else(overflow)?;
        let dt = n - (t + off);
        if dt == 0 {
            break;
        }
        // isdst < 0: dst2 <= (tm.tm_isdst != 0). Oscillating across a
        // spring-forward gap: take this probe unless only the previous one
        // was DST.
        if t == t1 && t != t2 && dst2 <= isdst {
            break;
        }
        remaining_probes -= 1;
        if remaining_probes == 0 {
            return Err(overflow());
        }
        t1 = t2;
        t2 = t;
        t += dt;
        dst2 = isdst;
    }
    MKTIME_OFFSET.store(t - n, Ordering::Relaxed);
    Ok(t)
}

/// localtime_r(t) as a wall-clock date and time.
fn localtime_wall(t: i64) -> Result<chrono::NaiveDateTime, String> {
    let eoverflow = || "OSError: [Errno 75] Value too large for defined data type".to_string();
    let (off, _) = tz::localtime(t).ok_or_else(eoverflow)?;
    t.checked_add(off)
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map(|d| d.naive_utc())
        .ok_or_else(eoverflow)
}

/// A float timestamp as a time_t (ROUND_FLOOR, as time.localtime takes it).
fn time_t(ts: f64) -> Result<i64, String> {
    if ts.is_nan() {
        return Err("ValueError: Invalid value NaN (not a number)".into());
    }
    let secs = ts.floor();
    if !(-9.223372036854776e18..9.223372036854776e18).contains(&secs) {
        return Err("OverflowError: timestamp out of range for platform time_t".into());
    }
    Ok(secs as i64)
}

/// `datetime.fromtimestamp(ts).date()` for a whole-second `ts`. CPython also
/// converts the time 24 hours earlier (and, inside a fold, the time at the
/// transition) to detect a fold, and each conversion must land in years
/// 1..9999 too: on 0001-01-01 the earlier one is year 0 and it raises.
fn fromtimestamp_date(ts: f64) -> Result<chrono::NaiveDate, String> {
    use chrono::Datelike;
    // utc_to_seconds(): the wall clock as seconds, only for datetime's years.
    fn seconds(dt: chrono::NaiveDateTime) -> Result<i64, String> {
        if !(1..=9999).contains(&dt.year()) {
            return Err(format!("ValueError: year {} is out of range", dt.year()));
        }
        Ok(dt.and_utc().timestamp())
    }
    const MAX_FOLD_SECONDS: i64 = 24 * 3600;
    let t = time_t(ts)?;
    let wall = localtime_wall(t)?;
    let result = seconds(wall)?;
    let probe = seconds(localtime_wall(t - MAX_FOLD_SECONDS)?)?;
    let transition = result - probe - MAX_FOLD_SECONDS;
    if transition < 0 {
        seconds(localtime_wall(t + transition)?)?;
    }
    Ok(wall.date())
}

/// `history()` with `time.time()` pinned to `now` (for parity and tests).
/// Err where the Python raises: `now` outside the platform's time_t
/// (time.localtime) or a day outside datetime's years 1..9999
/// (datetime.fromtimestamp).
pub fn history_at(now: f64) -> Result<Vec<crate::history::Day>, String> {
    use crate::history::{self as h, Day, Event};
    use chrono::NaiveDate;
    let day = 86400.0;

    // time.localtime(ts) (floored to a time_t), then the local date.
    let local_date = |ts: f64| -> Result<NaiveDate, String> { Ok(localtime_wall(time_t(ts)?)?.date()) };

    let at = |days_ago: i64, hour: u32, minute: u32| -> Result<f64, String> {
        let d = local_date(now - days_ago as f64 * day)?;
        Ok(mktime(d, hour, minute)? as f64)
    };

    let ev = |ts: f64, kind: &str, title: &str, detail: &str, items: &[&str]| {
        Event::new(ts, kind, title, detail, strs(items), "")
    };

    let mut events = vec![
        ev(at(0, 9, 14)?, h::ALMANAC, "fd 10.3.0-1 → 10.4.0-1", "", &[]),
        ev(at(0, 9, 13)?, h::PACKAGES, "Updated fd", "",
           &["upgraded fd (10.3.0-1 -> 10.4.0-1)"]),
        ev(at(0, 9, 13)?, h::SNAPSHOT, "Snapshot taken",
           "“Almanac: before updating fd”", &[]),
        ev(at(1, 18, 2)?, h::ALMANAC, "File types changed hands", "",
           &["Inkscape now opens .svg — before: Image Viewer"]),
        ev(at(1, 17, 40)?, h::ALMANAC, "Installed inkscape 1.4.2",
           "AppImage from https://github.com/example/inkscape · \
            checksum verified", &[]),
        ev(at(2, 21, 7)?, h::ALMANAC, "omarchy/shell.json restored",
           "From shell.json.bak.1; the defaults were kept", &[]),
        ev(at(2, 21, 5)?, h::RESET, "omarchy/shell.json replaced",
           "Your previous version was kept as a backup", &[]),
        ev(at(3, 10, 30)?, h::PACKAGES,
           "Updated 14 packages · installed 2 packages", "",
           &["upgraded firefox (147.0-1 -> 148.0-1)",
             "upgraded mesa (25.3.1-1 -> 25.3.2-1)",
             "installed wl-clipboard (2.2.1-3)"]),
        ev(at(3, 10, 28)?, h::SNAPSHOT, "Snapshot taken", "“4.0.4-1”", &[]),
    ];
    // Stable, like Python's sorted(reverse=True): equal times keep their order.
    events.sort_by(|a, b| b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal));
    let mut by_day: std::collections::BTreeMap<NaiveDate, Vec<Event>> = std::collections::BTreeMap::new();
    for e in events {
        // datetime.fromtimestamp(e.ts).date()
        by_day.entry(fromtimestamp_date(e.ts)?).or_default().push(e);
    }
    Ok(by_day
        .into_iter()
        .rev()
        .map(|(d, evs)| {
            let snapshot = evs.iter().any(|e| e.kind == h::SNAPSHOT);
            Day { day: d, events: evs, snapshot }
        })
        .collect())
}

/// A believable plugin list, so the Plugins page shows no one's own.
pub fn plugins() -> Vec<crate::plugins::Installed> {
    use crate::plugins::{self as p, Installed, State};
    let up = State::new("example.weather", p::UPDATE, "behind", 23,
                        "https://github.com/example/weather");
    // No address: a real account's avatar must not appear in a screenshot.
    let edits = State::new("example.scratchpad", p::LOCAL_CHANGES, "modified", 7, "");
    let current = State::new("example.bar-clock", p::CURRENT, "", 0,
                             "https://github.com/example/bar-clock");
    let dev = State::new("example.devtool", p::LOCAL, "symlink", 0, "");
    let row = |id: &str, name: &str, desc: &str, version: &str, kinds: &[&str], enabled: bool,
               state: State| Installed {
        id: s(id),
        name: s(name),
        description: s(desc),
        version: s(version),
        kinds: strs(kinds),
        enabled,
        state: Some(state),
        ..Default::default()
    };
    let mut rows = vec![
        row("example.weather", "Weather", "Current conditions and the \
             next few hours, in the bar", "1.4.2", &["bar-widget"], true, up),
        row("example.scratchpad", "Scratchpad", "A scratch buffer that \
             follows you between workspaces", "0.9.1", &["panel"], true, edits),
        row("example.bar-clock", "Bar clock", "A clock for the bar",
            "2.1.0", &["bar-widget"], false, current),
        row("example.devtool", "Plugin I am writing", "Linked from a \
             workspace", "0.1.0", &["panel"], true, dev),
    ];
    let builtin: &[(&str, &str, &str, &[&str], bool)] = &[
        ("omarchy.clock", "Clock", "The time and date", &["bar-widget"], true),
        ("omarchy.bluetooth", "Bluetooth", "Devices and pairing", &["bar-widget"], true),
        ("omarchy.network", "Network", "Wi-Fi and wired", &["bar-widget"], true),
        ("omarchy.battery", "Battery", "Charge and power profile", &["bar-widget"], true),
        ("omarchy.weather", "Weather", "Forecast in the bar", &["bar-widget"], false),
        ("omarchy.bar", "Bar", "The bar itself", &["bar"], true),
    ];
    for &(pid, name, desc, kinds, on) in builtin {
        rows.push(Installed {
            id: s(pid),
            name: s(name),
            description: s(desc),
            version: s("1.0.0"),
            kinds: strs(kinds),
            enabled: on,
            can_disable: pid != "omarchy.bar",
            built_in: true,
            ..Default::default()
        });
    }
    rows
}

pub fn startup_apps() -> Vec<crate::autostart::Entry> {
    use crate::autostart::Entry;
    let rows: &[(&str, &str, &str, bool)] = &[
        ("Password manager", "Unlocks with the session", "dialog-password-symbolic", true),
        ("Input method", "Typing in other scripts", "preferences-desktop-keyboard-symbolic", false),
        ("Print queue", "Shows printer jobs", "printer-symbolic", false),
    ];
    rows.iter()
        .map(|&(n, c, i, on)| Entry {
            filename: format!("{}.desktop", n.to_lowercase()),
            name: s(n),
            comment: s(c),
            icon: s(i),
            enabled: on,
            path: std::path::PathBuf::from("/etc/xdg/autostart"),
            system: None,
            ours: false,
        })
        .collect()
}

pub fn user_services() -> Vec<crate::services::Service> {
    use crate::services::{self as sv, Service};
    let rows: &[(&str, &str, bool)] = &[
        ("syncthing.service", "File synchronisation", true),
        ("omarchy-sleep-lock.service", "Lock before suspend", true),
        ("backup.timer", "Nightly backup", false),
        ("wireplumber.service", "Multimedia session manager", true),
    ];
    rows.iter()
        .map(|&(u, d, on)| Service {
            unit: s(u),
            description: s(d),
            enabled: on,
            active: on,
            essential: sv::essential_for(u),
            omarchy: u.starts_with("omarchy"),
        })
        .collect()
}

/// Two picks with fixed figures: screenshots must not ask GitHub, and
/// must not change from one run to the next.
pub fn picks(plugins: &[crate::store::Plugin]) -> Vec<crate::picks::Pick> {
    _picks(plugins, &crate::picks::eligible)
}

/// `picks` with `picks.eligible` passed in, so parity can swap in the tests'
/// stand-in the way the Python side replaces it on the module.
fn _picks(
    plugins: &[crate::store::Plugin],
    eligible: &dyn Fn(&crate::store::Plugin) -> bool,
) -> Vec<crate::picks::Pick> {
    use crate::picks::{self as pk, Pick, Signals};
    let mut pool: Vec<&crate::store::Plugin> = plugins.iter().filter(|p| eligible(p)).collect();
    pool.sort_by_key(|p| std::cmp::Reverse(p.stars));
    let mut chosen: Vec<&crate::store::Plugin> = Vec::new();
    let mut markets: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for p in pool {
        if !markets.contains(p.market.as_str()) {
            chosen.push(p);
            markets.insert(p.market.as_str());
        }
        if chosen.len() == 2 {
            break;
        }
    }
    let figures = [
        Signals { stars_today: 48, downloads: 1312, downloads_today: true, comments: 37, ..Default::default() },
        Signals { stars_today: 21, comments: 12, ..Default::default() },
    ];
    chosen
        .into_iter()
        .zip(figures)
        .enumerate()
        .map(|(i, (p, sg))| {
            let reasons = pk::reasons(p, &sg);
            Pick { plugin: p.clone(), signals: sg, score: 1.0 - i as f64 / 10.0, reasons }
        })
        .collect()
}

// ---- parity --------------------------------------------------------------

fn to_json<T: serde::Serialize>(v: T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn parity_call(func: &str, args: &Value) -> Option<Value> {
    Some(match func {
        "active" => json!(active()),
        "HOME" => json!(HOME),
        "INSTALLED_PLUGIN_IDS" => {
            let mut v: Vec<&str> = INSTALLED_PLUGIN_IDS.to_vec();
            v.sort();
            json!(v)
        }
        "INSTALLED_THEMES" => {
            let mut v: Vec<&str> = INSTALLED_THEMES.to_vec();
            v.sort();
            json!(v)
        }
        "packages" => to_json(packages()),
        "appimages" => to_json(appimages()),
        "flatpaks" => to_json(flatpaks()),
        "local_builds" => to_json(local_builds()),
        "agents" => to_json(agents()),
        "collect" => to_json(collect()),
        "findings" => to_json(findings()),
        "ai_groups" => to_json(ai_groups()),
        "history" => {
            let days = match args.get("now").and_then(Value::as_f64) {
                Some(now) => history_at(now),
                None => history_result(),
            };
            // Day.day as ISO text, like the Python case's conversion.
            match days {
                Ok(days) => to_json(days),
                Err(e) => json!({ "error": e }),
            }
        }
        "plugins" => to_json(plugins()),
        "startup_apps" => to_json(startup_apps()),
        "user_services" => to_json(user_services()),
        "picks" => {
            let ps: Vec<crate::store::Plugin> = args
                .get("plugins")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|p| serde_json::from_value(p.clone()).ok()).collect())
                .unwrap_or_default();
            if crate::util::arg_bool(args, "eligible_stub") {
                // The tests' stand-in: not installed and on GitHub.
                let stub = |p: &crate::store::Plugin| {
                    !p.installed
                        && crate::picks::parity("_github", &json!({"repo_url": p.repo}))
                            .and_then(|v| v.as_str().map(|s| !s.is_empty()))
                            .unwrap_or(false)
                };
                to_json(_picks(&ps, &stub))
            } else {
                to_json(picks(&ps))
            }
        }
        _ => return None,
    })
}

/// Parity hook: see ARCHITECTURE.md.
///
/// `env` in the args is applied for the duration of the call (null removes
/// the variable). A call into a module that is still an unported stub is
/// reported as an error rather than taking the whole run down.
pub fn parity(func: &str, args: &Value) -> Option<Value> {
    let saved: Vec<(String, Option<std::ffi::OsString>)> = args
        .get("env")
        .and_then(|e| e.as_object())
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    let old = std::env::var_os(k);
                    match v.as_str() {
                        Some(s) => std::env::set_var(k, s),
                        None => std::env::remove_var(k),
                    }
                    (k.clone(), old)
                })
                .collect()
        })
        .unwrap_or_default();
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| parity_call(func, args)))
        .unwrap_or_else(|_| Some(json!({ "error": "blocked: panicked (a module still an unported stub?)" })));
    for (k, old) in saved {
        match old {
            Some(v) => std::env::set_var(&k, v),
            None => std::env::remove_var(&k),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // DemoModeHidesTheMachine.test_plugins_are_synthetic: the demo half —
    // plugins.installed()/enabled() in demo mode come from here.
    #[test]
    fn test_plugins_are_synthetic() {
        let rows = plugins();
        assert!(!rows.is_empty());
        assert!(rows.iter().any(|r| r.enabled));
    }

    // DemoModeHidesTheMachine.test_history_is_synthetic: the demo half —
    // history.timeline() in demo mode is this.
    #[test]
    fn test_history_is_synthetic() {
        assert!(!history().is_empty());
    }

    #[test]
    fn active_reads_the_variable() {
        // Only this test touches ALMANAC_DEMO in this module.
        let _env = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::env::var_os("ALMANAC_DEMO");
        for (v, want) in [("", false), ("0", false), ("false", false), (" 1 ", true), ("yes", true),
                          ("\x1c", false), (" \x1d0\x1e ", false), ("\u{85}", false)] {
            std::env::set_var("ALMANAC_DEMO", v);
            assert_eq!(active(), want, "{v:?}");
        }
        // Not UTF-8: Python's surrogateescape makes it a non-empty value.
        use std::os::unix::ffi::OsStrExt;
        std::env::set_var("ALMANAC_DEMO", std::ffi::OsStr::from_bytes(b"\xff"));
        assert!(active());
        match old {
            Some(v) => std::env::set_var("ALMANAC_DEMO", v),
            None => std::env::remove_var("ALMANAC_DEMO"),
        }
    }

    #[test]
    fn history_out_of_range_is_an_error() {
        for now in [3e11, 1e20, -1e12, f64::NAN] {
            assert!(history_at(now).is_err(), "{now}");
        }
    }

    #[test]
    fn zone_is_resolved_like_glibc() {
        let at = |tz: Option<&str>, t: i64| tz::load(tz.map(str::as_bytes)).at(t);
        let (winter, summer) = (1_800_000_000, 1_782_000_000); // 2027-01-15, 2026-06-21
        // An unknown name is UTC to glibc (chrono would use /etc/localtime).
        for name in ["Europe/Nowhere", "foo", "Bogus/Zone", "", ":", "/etc/passwd"] {
            assert_eq!(at(Some(name), winter), Some((0, false)), "{name:?}");
        }
        assert_eq!(at(Some("EST5"), summer), Some((-18000, false)));
        assert_eq!(at(Some("JST-9"), winter), Some((32400, false)));
        assert_eq!(at(Some("<+0530>-5:30"), winter), Some((19800, false)));
        let rule = Some("XST0XDT,M3.2.0/9,M11.1.0/10");
        assert_eq!(at(rule, summer), Some((3600, true)));
        assert_eq!(at(rule, winter), Some((0, false)));
        if std::path::Path::new("/usr/share/zoneinfo/America/Los_Angeles").exists() {
            assert_eq!(at(Some("America/Los_Angeles"), winter), Some((-28800, false)));
            assert_eq!(at(Some(":America/Los_Angeles"), summer), Some((-25200, true)));
            // Past the file's last transition: its footer's rules.
            assert_eq!(at(Some("America/Los_Angeles"), 4_118_000_000), Some((-25200, true)));
        }
    }

    #[test]
    fn fixtures_have_the_shapes_the_pages_expect() {
        assert_eq!(flatpaks().len(), 1);
        assert_eq!(local_builds().len(), 3);
        assert_eq!(findings().len(), 12);
        assert_eq!(startup_apps()[0].filename, "password manager.desktop");
        let svc = user_services();
        assert!(svc[1].omarchy && !svc[0].omarchy);
        let groups = ai_groups();
        assert_eq!(groups.len(), crate::ai::group_specs().len());
    }
}
