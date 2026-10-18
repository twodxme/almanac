//! Port of `agents.py`.
//!
//! AI coding agents, which no package manager knows about.
//!
//! These install themselves: a curl-to-shell script, a version manager, or their
//! own `update` subcommand. pacman has never heard of them, so without this they
//! are the one class of software on the machine that silently rots -- and they are
//! also the ones shipping a release most weeks.
//!
//! Two things are deliberately not hardcoded:
//!
//! * How an agent is managed. It is read from where the binary actually resolves
//!   to, so moving `claude` from mise to the native installer changes the update
//!   command here without an edit.
//! * Which agents exist. The table below is the known set, but detection is by
//!   what is on PATH; an agent that is not installed simply never appears.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::inventory::App;
use crate::util;

/// `CACHE`: `$XDG_CACHE_HOME/almanac` (or `~/.cache/almanac`).
///
/// A function rather than a constant: Python computes it at import time from
/// the environment, which Rust cannot do in a `const`.
pub fn cache() -> PathBuf {
    let base = match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => util::home().join(".cache"),
    };
    base.join("almanac")
}

/// `LATEST_CACHE`: `CACHE / "agent-latest.json"`.
pub fn latest_cache() -> PathBuf {
    cache().join("agent-latest.json")
}

pub const TTL: u64 = 6 * 3600;
pub const UA: (&str, &str) = ("User-Agent", "almanac");

/// inventory source key
pub const AGENT: &str = "agent";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spec {
    /// the binary name on PATH
    pub id: String,
    pub name: String,
    /// owner/name, for the latest release
    pub repo: String,
    pub home: String,
    /// The vendor's own domain, used only to fetch their icon from them.
    pub domain: String,
    /// Its own updater, when it has one. Used only if the binary is not under
    /// a version manager that owns it.
    pub self_update: Vec<String>,
    pub note: String,
}

fn spec(id: &str, name: &str, repo: &str, home: &str, domain: &str, self_update: &[&str], note: &str) -> Spec {
    Spec {
        id: id.into(),
        name: name.into(),
        repo: repo.into(),
        home: home.into(),
        domain: domain.into(),
        self_update: self_update.iter().map(|s| s.to_string()).collect(),
        note: note.into(),
    }
}

pub static KNOWN: LazyLock<Vec<Spec>> = LazyLock::new(|| {
    vec![
        spec("claude", "Claude Code", "anthropics/claude-code",
             "https://claude.com/claude-code", "claude.com", &["claude", "update"], ""),
        spec("codex", "Codex CLI", "openai/codex",
             "https://github.com/openai/codex", "openai.com", &[], ""),
        spec("gemini", "Gemini CLI", "google-gemini/gemini-cli",
             "https://github.com/google-gemini/gemini-cli", "gemini.google.com", &[], ""),
        spec("opencode", "opencode", "anomalyco/opencode",
             "https://opencode.ai", "opencode.ai", &["opencode", "upgrade"], ""),
        spec("copilot", "GitHub Copilot CLI", "github/copilot-cli",
             "https://github.com/github/copilot-cli", "github.com",
             &["copilot", "update"], ""),
        spec("cursor-agent", "Cursor Agent", "", "https://cursor.com/cli",
             "cursor.com", &["cursor-agent", "update"],
             "Versions are dated builds, not semantic versions."),
        spec("crush", "Crush", "charmbracelet/crush",
             "https://github.com/charmbracelet/crush", "charm.sh", &[], ""),
        spec("goose", "Goose", "block/goose",
             "https://block.github.io/goose/", "block.xyz", &[], ""),
        spec("aider", "Aider", "Aider-AI/aider", "https://aider.chat",
             "aider.chat",
             &["python", "-m", "pip", "install", "--upgrade", "aider-chat"], ""),
        spec("qwen", "Qwen Code", "QwenLM/qwen-code",
             "https://github.com/QwenLM/qwen-code", "qwen.ai", &[], ""),
        spec("amp", "Amp", "", "https://ampcode.com", "ampcode.com",
             &["amp", "update"], ""),
        spec("droid", "Factory Droid", "", "https://factory.ai", "factory.ai",
             &["droid", "update"], ""),
    ]
});

pub static BY_ID: LazyLock<BTreeMap<String, Spec>> =
    LazyLock::new(|| KNOWN.iter().map(|s| (s.id.clone(), s.clone())).collect());

pub static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+(?:\.\d+)+(?:[-+][0-9A-Za-z.]+)?").unwrap());

/// `VERSION_CACHE`: `CACHE / "agent-versions.json"`.
pub fn version_cache() -> PathBuf {
    cache().join("agent-versions.json")
}

/// Size and mtime of what the name resolves to.
///
/// A CLI's version cannot change without its binary changing, so this is a
/// sound cache key -- and asking a 215MB binary for its version costs a
/// second of process start-up every time the inventory reloads.
pub fn binary_id(path: &str) -> String {
    todo!("port: agents::_binary_id")
}

pub fn cached_version(binary: &str, path: &str) -> String {
    todo!("port: agents::_cached_version")
}

/// The version a binary reports for itself (`--version`, `version`, `-v`),
/// or "" when it reports none. Called by `verify`.
pub fn version_of(binary: &str) -> String {
    todo!("port: agents::_version_of")
}

/// `MISE_CACHE`: `CACHE / "mise-outdated.json"`.
pub fn mise_cache() -> PathBuf {
    cache().join("mise-outdated.json")
}

/// Tools mise manages that are not coding agents. They update the same way and
/// were previously invisible: nothing on the machine reported that `gh` or
/// `uv` had a newer release waiting.
///
/// tool -> (display name, vendor domain)
pub const TOOL_DOMAINS: &[(&str, (&str, &str))] = &[
    ("gh", ("GitHub CLI", "github.com")),
    ("node", ("Node.js", "nodejs.org")),
    ("uv", ("uv", "astral.sh")),
    ("python", ("Python", "python.org")),
    ("go", ("Go", "go.dev")),
    ("rust", ("Rust", "rust-lang.org")),
    ("deno", ("Deno", "deno.com")),
    ("bun", ("Bun", "bun.sh")),
    ("terraform", ("Terraform", "terraform.io")),
    ("kubectl", ("kubectl", "kubernetes.io")),
];

/// {tool: active version} for everything mise manages.
///
/// Asked of mise rather than inferred from where a binary resolves to: a
/// shimmed tool resolves to /usr/bin/mise, which looks nothing like a mise
/// install path. Five of seven agents were misread as self-managed that
/// way, and then had their updates checked against the wrong upstream.
///
/// Never cached. `mise ls` reads local state in milliseconds, and a cached
/// answer kept reporting updates that had already been installed.
///
/// Called by `verify`.
pub fn mise_installed() -> BTreeMap<String, String> {
    todo!("port: agents::_mise_installed")
}

/// What mise already knows. One call covers every tool it manages.
///
/// Cached because `mise outdated` queries every tool's upstream and takes
/// seconds; the answer does not change between two refreshes a minute apart.
/// Only its `latest` is trusted from the cache: its `current` goes stale the
/// moment anything is upgraded, so callers take that from mise_installed.
///
/// {tool: {"current", "latest", "release_url", ...}} as mise reports it.
/// Pass [`TTL`] for Python's default.
pub fn mise_outdated(ttl: u64) -> BTreeMap<String, serde_json::Value> {
    todo!("port: agents::_mise_outdated")
}

/// {repo: [latest, release_url]} from the cache, or {} once it is older than TTL.
pub fn load_latest() -> BTreeMap<String, serde_json::Value> {
    todo!("port: agents::_load_latest")
}

pub fn save_latest(versions: &BTreeMap<String, serde_json::Value>) {
    todo!("port: agents::_save_latest")
}

/// (version, release page) for the newest release, or ('','').
///
/// Goes through the shared cache, so this competes with nothing else for the
/// rate limit and costs nothing when the release has not moved.
pub fn github_latest(repo: &str) -> (String, String) {
    todo!("port: agents::_github_latest")
}

/// Every known agent that is actually installed, with its update state.
///
/// Returns inventory.App objects so agents sit in the same list, the same
/// Updates count and the same sorting as everything else -- being unmanaged
/// by pacman is a packaging detail, not a reason for a separate screen.
///
/// Python's default is `check_updates=True`.
pub fn detect(check_updates: bool) -> Vec<App> {
    todo!("port: agents::detect")
}

/// Everything mise manages that is not one of the agents above.
///
/// Same update mechanism, same staleness problem, no reason to leave them
/// out just because they are not AI tooling.
///
/// Python's default is `check_updates=True`.
pub fn mise_tools(check_updates: bool) -> Vec<App> {
    todo!("port: agents::mise_tools")
}

/// A new major version: the kind of update that changes how a tool works.
///
/// These are held back from "update all" and from scheduled runs, and shown
/// with their release notes, because a CLI agent that renames its flags or
/// moves its config is not something to discover mid-task.
pub fn major_jump(current: &str, latest: &str) -> bool {
    todo!("port: agents::major_jump")
}

/// A major release that update-all and scheduled runs leave for a person
/// -- unless Preferences says to update majors like anything else.
pub fn hold_major(current: &str, latest: &str) -> bool {
    todo!("port: agents::hold_major")
}

/// The latest release's notes from GitHub, trimmed, or ''.
///
/// Python's default is `limit=2000` (characters).
pub fn release_notes(repo: &str, limit: usize) -> String {
    todo!("port: agents::release_notes")
}

/// True when `latest` is a higher version than `current`.
///
/// A string compare says 0.9.9 > 0.10.2, which is the bug this exists to
/// avoid. Unparseable pairs (Cursor's dated builds vs a tag) compare as
/// equal rather than claiming an update that may not exist.
pub fn newer(latest: &str, current: &str) -> bool {
    todo!("port: agents::_newer")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
