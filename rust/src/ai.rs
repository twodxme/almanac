//! Port of `ai.py`.
//!
//! The AI estate: everything on this machine that is part of an agent setup.
//!
//! A coding agent is not one program. It is a harness, the MCP servers it can
//! reach, the skills and plugins that change how it behaves, and the hooks that
//! fire around it. None of that is visible to pacman, and most of it is invisible
//! to the harness's own `--version` too, so it is the part of the system that
//! drifts furthest without anyone noticing.
//!
//! Everything here is read from the tools themselves (`claude mcp list`,
//! `claude plugin list --json`) or from the files they own, never from a list
//! kept in this module. An agent configured after this was written still shows up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util;

/// `HOME`: `Path.home()`.
pub fn home() -> PathBuf {
    util::home()
}

/// `CLAUDE`: `HOME / ".claude"`.
pub fn claude() -> PathBuf {
    home().join(".claude")
}

/// `CACHE`: `$XDG_CACHE_HOME/almanac` (or `~/.cache/almanac`).
pub fn cache() -> PathBuf {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(".cache"),
    };
    base.join("almanac")
}

/// `MCP_CACHE`: `CACHE / "mcp-list.json"`.
pub fn mcp_cache() -> PathBuf {
    cache().join("mcp-list.json")
}

/// `claude mcp list` opens a connection to every configured server, so its cost
/// scales with how many are set up and how far away they are -- 13 to 22
/// seconds here. Short enough to still catch a server going down, long enough
/// that moving between pages does not re-run it.
pub const MCP_TTL: u64 = 300;

pub const HARNESS: &str = "harness";
pub const MCP: &str = "mcp";
pub const SKILL: &str = "skill";
pub const PLUGIN: &str = "plugin";
pub const MARKET: &str = "marketplace";
pub const HOOK: &str = "hook";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub kind: String,
    pub name: String,
    pub detail: String,
    /// connected | needs auth | update | ""
    pub status: String,
    /// ok | warn | info
    pub severity: String,
    pub version: String,
    pub update: String,
    pub path: String,
    pub url: String,
    pub harness: String,
    pub action_label: String,
    /// argv list, or callable(log). Every producer in the Python passes an
    /// argv list or None, so the port carries only the argv form.
    pub action: Option<Vec<String>>,
    pub action_note: String,
    pub items: Vec<String>,
    /// Where to get the vendor's own mark. `icon_key` is the cache key, so two
    /// services from one company share a single download.
    pub icon_key: String,
    pub icon_domain: String,
    pub icon_url: String,
    pub fallback_icon: String,
}

impl Default for Entry {
    fn default() -> Self {
        Entry {
            kind: String::new(),
            name: String::new(),
            detail: String::new(),
            status: String::new(),
            severity: String::new(),
            version: String::new(),
            update: String::new(),
            path: String::new(),
            url: String::new(),
            harness: "Claude Code".into(),
            action_label: String::new(),
            action: None,
            action_note: String::new(),
            items: Vec::new(),
            icon_key: String::new(),
            icon_domain: String::new(),
            icon_url: String::new(),
            fallback_icon: "application-x-executable-symbolic".into(),
        }
    }
}

/// An org's avatar is the brand mark for most repositories.
pub fn github_avatar(repo: &str) -> String {
    let owner = repo.split('/').next().unwrap_or("");
    if owner.is_empty() { String::new() } else { format!("https://github.com/{owner}.png?size=128") }
}

/// The host of a `scheme://host...` target, or "".
pub fn host(target: &str) -> String {
    todo!("port: ai::_host")
}

/// stdout + stderr of a command, or "" when it could not run.
/// Python's default timeout is 60 seconds.
pub fn run(cmd: &[&str], timeout: u64) -> String {
    todo!("port: ai::_run")
}

/// name/description from a SKILL.md header, without a YAML dependency.
///
/// Only the two scalar keys are needed, and they may be folded onto several
/// lines with `>`; a real parser would be more than this costs.
pub fn frontmatter(path: &Path) -> BTreeMap<String, String> {
    todo!("port: ai::_frontmatter")
}

// ---- harnesses ---------------------------------------------------------------

/// A path under the home directory, written the way people say it.
pub fn tilde(path: &str) -> String {
    todo!("port: ai::_tilde")
}

pub fn harnesses() -> Vec<Entry> {
    todo!("port: ai::harnesses")
}

// ---- MCP servers -------------------------------------------------------------

/// What `claude mcp list` reports, including its health check.
///
/// Parsed from the CLI rather than from ~/.claude.json because the connectors
/// enabled through claude.ai never appear in that file -- reading the file
/// would report zero servers on a machine with twenty.
pub fn mcp_servers() -> Vec<Entry> {
    todo!("port: ai::mcp_servers")
}

// ---- skills ------------------------------------------------------------------

pub fn skills() -> Vec<Entry> {
    todo!("port: ai::skills")
}

// ---- plugins and marketplaces ------------------------------------------------

pub fn plugins() -> Vec<Entry> {
    todo!("port: ai::plugins")
}

pub fn marketplaces() -> Vec<Entry> {
    todo!("port: ai::marketplaces")
}

// ---- hooks and other extensions ----------------------------------------------

pub fn hooks() -> Vec<Entry> {
    todo!("port: ai::hooks")
}

/// A group's loader: one of the readers above.
pub type Loader = fn() -> Vec<Entry>;

/// One group as the AI page shows it: (title, description, entries).
pub type Group = (String, String, Vec<Entry>);

/// (title, kind, loader, description) in display order.
pub const GROUPS: &[(&str, &str, Loader, &str)] = &[
    ("Harnesses", HARNESS, harnesses,
     "The agent CLIs themselves."),
    ("MCP servers", MCP, mcp_servers,
     "Tools and data the agents can reach."),
    ("Skills", SKILL, skills,
     "Instructions loaded on demand to change how an agent works."),
    ("Plugins", PLUGIN, plugins,
     "Bundles of skills, commands, agents and hooks."),
    ("Marketplaces", MARKET, marketplaces,
     "Where plugins are installed from."),
    ("Hooks", HOOK, hooks,
     "Commands that run around an agent session."),
];

/// (title, description, loader) for each group, in display order.
pub fn group_specs() -> Vec<(&'static str, &'static str, Loader)> {
    GROUPS.iter().map(|&(title, _kind, f, desc)| (title, desc, f)).collect()
}

/// Run one loader; a failure becomes a single "error" entry naming the group
/// instead of taking the page down.
pub fn load_group(f: Loader, title: &str) -> Vec<Entry> {
    todo!("port: ai::load_group")
}

/// (title, description, entries) for each group. Read-only.
///
/// `claude mcp list` health-checks 28 servers and takes most of the time
/// here; the file scans should not be queued behind it.
pub fn survey() -> Vec<Group> {
    todo!("port: ai::survey")
}

/// {title: number of entries}.
pub fn counts(groups: &[Group]) -> BTreeMap<String, usize> {
    groups.iter().map(|(title, _d, entries)| (title.clone(), entries.len())).collect()
}

pub fn needs_attention(groups: &[Group]) -> usize {
    groups.iter().flat_map(|(_t, _d, entries)| entries).filter(|e| e.severity == "warn").count()
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
