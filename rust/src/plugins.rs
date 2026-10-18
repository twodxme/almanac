//! Port of `plugins.py`.
//!
//! Installed Omarchy shell plugins: what state each is in, and acting on it.
//!
//! Counting commits behind upstream is not enough to decide whether a plugin can
//! be updated. Before anything touches one, it is classified -- the same states
//! omaplug (github.com/fross100/omaplug) settled on, which match what Omarchy's
//! own updater will and will not do:
//!
//! * CURRENT        a clean checkout at its upstream's head
//! * UPDATE         a clean checkout behind its upstream: safe to fast-forward
//! * LOCAL_CHANGES  edited, ahead of upstream, diverged from it, or detached --
//!   an update would have to decide what happens to that work
//! * LOCAL          not Omarchy's to update: a plain folder, a checkout with no
//!   origin, or a **symlink**. Omarchy installs a plugin being
//!   developed as a symlink to its author's workspace, and
//!   `omarchy plugin update` would happily fast-forward that
//!   workspace; this never offers to.
//! * ERROR          the state could not be read (fetch failed, git broke)
//!
//! Updates go through `omarchy plugin update <id> --yes`, which validates the
//! result and rolls back a plugin that fails validation -- something a bare
//! `git pull` never did.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The per-path fetch cache `classify` / `installed` read and update:
/// `str(path) -> {"remote": sha, "fetched_at": time}`. In the Python this is a
/// plain dict, usually `cache.setdefault("_plugins", {})` of inventory's cache.
pub type FetchCache = serde_json::Map<String, serde_json::Value>;

pub const CURRENT: &str = "current";
pub const UPDATE: &str = "update";
pub const LOCAL_CHANGES: &str = "local-changes";
pub const LOCAL: &str = "local";
pub const ERROR: &str = "error";
pub const FETCH_EVERY: f64 = 3600.0;

/// Bulk-update scopes, by the marketplace provenance a plugin carries:
/// `(key, label, allowed trust keys)`. `None`: anything, listed or not.
pub const SCOPES: &[(&str, &str, Option<&[&str]>)] = &[
    ("verified", "Verified only",
     Some(&["official", "first-party", "reviewed", "verified"])),
    ("changed", "Verified, and changed since review",
     Some(&["official", "first-party", "reviewed", "verified", "reviewed-stale", "stale"])),
    ("all", "All plugins", None), // None: anything, listed or not
];

pub fn plugins_dir() -> PathBuf {
    todo!("port: plugins::plugins_dir")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub id: String,
    pub kind: String,
    /// why: "symlink", "modified", "ahead", "3 commits"…
    pub detail: String,
    pub behind: i64,
    pub url: String,
}

impl State {
    /// `State(id, kind, detail, behind, url)`, as the Python constructs it.
    pub fn new(id: &str, kind: &str, detail: &str, behind: i64, url: &str) -> State {
        State {
            id: id.to_string(),
            kind: kind.to_string(),
            detail: detail.to_string(),
            behind,
            url: url.to_string(),
        }
    }

    pub fn updatable(&self) -> bool {
        self.kind == UPDATE
    }

    pub fn describe(&self) -> String {
        todo!("port: plugins::State::describe")
    }
}

/// `git -C d *args` with prompts disabled (GIT_TERMINAL_PROMPT=0, batch-mode ssh).
#[allow(dead_code)]
fn _git(_d: &Path, _args: &[&str], _timeout: u64) -> crate::util::Output {
    todo!("port: plugins::_git")
}

/// The state of the plugin in `d`. `cache` holds fetch results by path,
/// so the network is asked at most once an hour per plugin; with `fetch`
/// False it is not asked at all, and an unknown upstream reads as CURRENT
/// with the detail "unchecked".
pub fn classify(_d: &Path, _cache: Option<&mut FetchCache>, _fetch: bool) -> State {
    todo!("port: plugins::classify")
}

/// The upstream head, fetched -- as Omarchy fetches it, `origin HEAD` --
/// at most once an hour; counted against locally between fetches.
#[allow(dead_code)]
fn _remote(_d: &Path, _cache: Option<&mut FetchCache>, _fetch: bool) -> String {
    todo!("port: plugins::_remote")
}

#[allow(dead_code)]
fn _count(_d: &Path, _head: &str, _remote: &str) -> i64 {
    todo!("port: plugins::_count")
}

/// One plugin on this system, built in or added, for the Plugins page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub kinds: Vec<String>,
    pub enabled: bool,
    pub can_disable: bool,
    pub built_in: bool,
    pub folder: Option<PathBuf>,
    /// only for plugins the user added
    pub state: Option<State>,
}

impl Default for Installed {
    fn default() -> Self {
        Installed {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            version: String::new(),
            kinds: Vec::new(),
            enabled: false,
            can_disable: true,
            built_in: false,
            folder: None,
            state: None,
        }
    }
}

impl Installed {
    pub fn has_panel(&self) -> bool {
        self.kinds.iter().any(|k| matches!(k.as_str(), "panel" | "bar-widget" | "overlay"))
    }
}

/// id -> (manifest, folder) for every manifest under `root`.
#[allow(dead_code)]
fn _manifests(_root: &Path) -> BTreeMap<String, (serde_json::Value, PathBuf)> {
    todo!("port: plugins::_manifests")
}

/// Every plugin Omarchy knows about, with what each one is and its state.
///
/// Omarchy's own list decides what exists and whether it is on; manifests
/// supply the description, and the user's own plugins are classified so the
/// page can say whether each can be updated.
pub fn installed(_fetch: bool, _cache: Option<&mut FetchCache>) -> Vec<Installed> {
    todo!("port: plugins::installed")
}

// ---- what to show beside each plugin -------------------------------------------

/// A plugin about a service carries that service's mark, fetched from the
/// vendor's own domain (icons.py) -- matched on the plugin's id and name only,
/// never its description, where "works with Slack" would mislabel it.
pub const VENDORS: &[(&str, &str)] = &[
    ("tailscale", "tailscale.com"), ("slack", "slack.com"),
    ("spotify", "spotify.com"), ("github", "github.com"),
    ("dropbox", "dropbox.com"), ("1password", "1password.com"),
    ("docker", "docker.com"), ("obsidian", "obsidian.md"),
    ("discord", "discord.com"), ("youtube", "youtube.com"),
    ("google", "google.com"), ("todoist", "todoist.com"),
    ("notion", "notion.so"), ("proton", "proton.me"),
    ("steam", "steampowered.com"), ("twitch", "twitch.tv"),
    ("claude", "anthropic.com"), ("anthropic", "anthropic.com"),
    ("openai", "openai.com"), ("chatgpt", "openai.com"),
    ("home assistant", "home-assistant.io"), ("jellyfin", "jellyfin.org"),
];

/// Otherwise, a system icon for what it does. Adwaita names only, each checked
/// to exist, so the row looks right under any icon theme that follows it.
pub const SYMBOLS: &[(&str, &str)] = &[
    ("bluetooth", "bluetooth-symbolic"),
    ("wifi", "network-wireless-symbolic"), ("network", "network-wireless-symbolic"),
    ("tailscale", "network-transmit-receive-symbolic"),
    ("vpn", "network-transmit-receive-symbolic"),
    ("audio", "audio-volume-high-symbolic"), ("volume", "audio-volume-high-symbolic"),
    ("sound", "audio-volume-high-symbolic"),
    ("microphone", "microphone-sensitivity-high-symbolic"),
    ("media", "media-playback-start-symbolic"), ("music", "media-playback-start-symbolic"),
    ("battery", "battery-full-symbolic"), ("power", "system-shutdown-symbolic"),
    ("clock", "preferences-system-time-symbolic"), ("time", "preferences-system-time-symbolic"),
    ("reminder", "alarm-symbolic"), ("alarm", "alarm-symbolic"),
    ("weather", "weather-few-clouds-symbolic"),
    ("clipboard", "edit-paste-symbolic"), ("emoji", "face-smile-symbolic"),
    ("image", "image-x-generic-symbolic"), ("screenshot", "camera-photo-symbolic"),
    ("lock", "system-lock-screen-symbolic"), ("idle", "system-lock-screen-symbolic"),
    ("polkit", "dialog-password-symbolic"), ("password", "dialog-password-symbolic"),
    ("menu", "open-menu-symbolic"), ("background", "preferences-desktop-wallpaper-symbolic"),
    ("wallpaper", "preferences-desktop-wallpaper-symbolic"),
    ("appearance", "preferences-desktop-appearance-symbolic"),
    ("theme", "preferences-desktop-appearance-symbolic"),
    ("monitor", "video-display-symbolic"), ("display", "video-display-symbolic"),
    ("nightlight", "night-light-symbolic"), ("night", "night-light-symbolic"),
    ("notification", "preferences-system-notifications-symbolic"),
    ("osd", "preferences-system-notifications-symbolic"),
    ("keyboard", "preferences-desktop-keyboard-symbolic"),
    ("update", "software-update-available-symbolic"),
    ("disk", "drive-harddisk-symbolic"), ("speedtest", "content-loading-symbolic"),
    ("workspace", "view-grid-symbolic"), ("window", "window-new-symbolic"),
    ("tray", "view-app-grid-symbolic"), ("bar", "view-app-grid-symbolic"),
    ("agent", "utilities-terminal-symbolic"), ("dev", "utilities-terminal-symbolic"),
    ("vitals", "emblem-system-symbolic"), ("system", "emblem-system-symbolic"),
    ("radar", "weather-few-clouds-symbolic"),
];
pub const GENERIC: &str = "application-x-addon-symbolic";

#[allow(dead_code)]
fn _words(_texts: &[&str]) -> String {
    todo!("port: plugins::_words")
}

/// The plugin's name and the last part of its id. The rest of an id is a
/// reverse-DNS namespace ("io.github.someone"), and the kind ("bar-widget")
/// says where it sits, not what it is -- matching either mislabels it.
#[allow(dead_code)]
fn _own_words(_i: &Installed) -> String {
    todo!("port: plugins::_own_words")
}

/// The domain of the service a plugin is about, or ''.
pub fn vendor_for(_i: &Installed) -> String {
    todo!("port: plugins::vendor_for")
}

/// A system icon for what the plugin does, from its name and id.
pub fn symbol_for(_i: &Installed) -> String {
    todo!("port: plugins::symbol_for")
}

pub fn github_owner(_url: &str) -> String {
    todo!("port: plugins::github_owner")
}

/// id -> enabled, from Omarchy's own list.
pub fn enabled() -> BTreeMap<String, bool> {
    todo!("port: plugins::enabled")
}

pub fn toggle_argv(pid: &str, on: bool) -> Vec<String> {
    vec!["omarchy".into(), "plugin".into(),
         (if on { "enable" } else { "disable" }).into(), pid.to_string()]
}

pub fn update_argv(pid: &str) -> Vec<String> {
    vec!["omarchy".into(), "plugin".into(), "update".into(), pid.to_string(), "--yes".into()]
}

/// Whether a plugin with this provenance is covered by a bulk update.
/// `trust_key` is None for a plugin the marketplace does not list.
pub fn in_scope(_trust_key: Option<&str>, _scope: &str) -> bool {
    todo!("port: plugins::in_scope")
}

/// Restart the shell -- never `omarchy-refresh-shell`, which resets
/// shell.json to its defaults. Clearing Quickshell's compiled QML cache
/// first fixes a plugin acting on stale compiled code. Detached, since the
/// shell being restarted may be what started it.
pub fn restart_argv(_clear_cache: bool) -> Vec<String> {
    todo!("port: plugins::restart_argv")
}

// ---- settings ------------------------------------------------------------------

/// The 1.4.0 scope file, `state_home/almanac/plugins.json`.
pub fn _settings_path() -> PathBuf {
    todo!("port: plugins::_settings_path")
}

pub fn scope() -> String {
    todo!("port: plugins::scope")
}

pub fn set_scope(_value: &str) {
    todo!("port: plugins::set_scope")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
