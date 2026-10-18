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

/// True when `ALMANAC_DEMO` is set to anything but "", "0" or "false".
pub fn active() -> bool {
    let v = std::env::var("ALMANAC_DEMO").unwrap_or_default();
    !matches!(v.trim(), "" | "0" | "false")
}

pub const HOME: &str = "/home/user";

/// (name, version, update, source, description, size_mb, extra)
///
/// `extra` is the Python dict as key/value pairs (only ever `critical`).
const PACKAGES: &[(&str, &str, &str, &str, &str, f64, &[(&str, &str)])] = &[
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

/// (name, version, update, description, size_mb)
const APPIMAGES: &[(&str, &str, &str, &str, f64)] = &[
    ("Inkscape", "1.4.2", "1.5.0", "Vector graphics editor", 142.0),
    ("Obsidian", "1.9.14", "", "Knowledge base that works on local Markdown", 118.5),
];

/// (name, version, update, appid, description, size_mb)
const FLATPAKS: &[(&str, &str, &str, &str, &str, f64)] = &[
    ("Blender", "4.5.3", "", "org.blender.Blender",
     "Free and open source 3D creation suite", 1180.0),
];

/// (name, version, git_behind, git_dirty, description)
const LOCAL: &[(&str, &str, i64, bool, &str)] = &[
    ("example.bar-clock", "2.1.0", 0, false,
     "A clock for the bar, built from a local checkout"),
    ("example.weather", "1.4.2", 23, false,
     "Current conditions and the next few hours, in the bar"),
    ("example.scratchpad", "0.9.1", 7, true,
     "A scratch buffer that follows you between workspaces"),
];

/// (display name, binary, installed, latest, manager, domain)
const AGENTS: &[(&str, &str, &str, &str, &str, &str)] = &[
    ("Claude Code", "claude", "2.1.301", "2.1.344", "mise", "claude.com"),
    ("Codex CLI", "codex", "0.162.1", "0.166.0", "mise", "openai.com"),
    ("Gemini CLI", "gemini", "0.64.0", "0.64.0", "self", "gemini.google.com"),
    ("opencode", "opencode", "1.19.3", "1.19.3", "self", "opencode.ai"),
    ("GitHub Copilot CLI", "copilot", "1.0.98", "1.0.98", "self", "github.com"),
];

/// (name, url, status, severity)
const MCP: &[(&str, &str, &str, &str)] = &[
    ("Documentation", "https://mcp.example.com/docs", "connected", "ok"),
    ("Issue tracker", "https://mcp.example.com/issues", "connected", "ok"),
    ("Design system", "https://mcp.example.com/design", "connected", "ok"),
    ("Analytics", "https://mcp.example.com/analytics",
     "needs authentication", "info"),
];

/// (name, description)
const SKILLS: &[(&str, &str)] = &[
    ("release-notes", "Draft release notes from the commits since the last tag"),
    ("review-checklist", "The team's review conventions, loaded on demand"),
];

pub fn packages() -> Vec<crate::inventory::App> {
    todo!("port: demo::packages")
}

pub fn appimages() -> Vec<crate::inventory::App> {
    todo!("port: demo::appimages")
}

pub fn flatpaks() -> Vec<crate::inventory::App> {
    todo!("port: demo::flatpaks")
}

pub fn local_builds() -> Vec<crate::inventory::App> {
    todo!("port: demo::local_builds")
}

pub fn agents() -> Vec<crate::inventory::App> {
    todo!("port: demo::agents")
}

pub fn collect() -> Vec<crate::inventory::App> {
    todo!("port: demo::collect")
}

/// A believable spread of health results, with real-looking sizes.
pub fn findings() -> Vec<crate::maintenance::Finding> {
    todo!("port: demo::findings")
}

/// The AI estate, with generic services rather than anyone's real ones.
///
/// Same shape as `ai::survey`: (title, description, entries) per group.
pub fn ai_groups() -> Vec<(String, String, Vec<crate::ai::Entry>)> {
    todo!("port: demo::ai_groups")
}

// Marketplace entries to show as already installed, so cards demonstrate both
// states without touching what is really on the machine.
/// Python `set`; membership is all callers use.
pub const INSTALLED_PLUGIN_IDS: &[&str] = &["omarchy.clock", "omarchy.battery"];
/// Python `set`; membership is all callers use.
pub const INSTALLED_THEMES: &[&str] = &["solitude"];

/// A believable week, so the History page can be shown without anyone's.
pub fn history() -> Vec<crate::history::Day> {
    todo!("port: demo::history")
}

/// A believable plugin list, so the Plugins page shows no one's own.
pub fn plugins() -> Vec<crate::plugins::Installed> {
    todo!("port: demo::plugins")
}

pub fn startup_apps() -> Vec<crate::autostart::Entry> {
    todo!("port: demo::startup_apps")
}

pub fn user_services() -> Vec<crate::services::Service> {
    todo!("port: demo::user_services")
}

/// Two picks with fixed figures: screenshots must not ask GitHub, and
/// must not change from one run to the next.
pub fn picks(plugins: &[crate::store::Plugin]) -> Vec<crate::picks::Pick> {
    let _ = plugins;
    todo!("port: demo::picks")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
