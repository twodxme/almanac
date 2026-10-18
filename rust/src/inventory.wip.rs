//! Port of `inventory.py`.
//!
//! Enumerate installed software across every source on this machine.
//!
//! Deliberately read-only. Nothing here mutates package state -- the GUI delegates
//! every install/update to the tool that actually owns it (pacman, yay, omarchy
//! update). The one exception is AppImages, which no package manager tracks, so
//! this module reads a registry that the app itself owns.
//!
//! Naming: Python's "private" helpers that other modules call
//! (`_load_registry`, `_pending_repo`, `_git_cache`, ...) drop the leading
//! underscore here (`load_registry`, `pending_repo`, `git_cache`), the same
//! rule the UI sections use for methods.

// Interface phase: bodies are `todo!()`, so parameters are not read yet.
#![allow(unused_variables)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// Runtime data, deliberately not where the code lives.
//
// Almanac installs as a git checkout that updates itself, so anything it
// writes has to sit outside that checkout -- otherwise `git pull` is fighting
// the app's own state, and a fresh clone would have 180MB of someone else's
// AppImages in it. Everything else derives from this one path.

/// `STATE`: `$XDG_STATE_HOME/almanac` (default `~/.local/state/almanac`).
/// A function rather than a constant because it reads the environment; the
/// Python read it once at import, which is the same answer for a process that
/// does not change its own environment.
pub fn state() -> PathBuf {
    // os.environ.get(...) uses the variable whenever it is set, even empty.
    match std::env::var_os("XDG_STATE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac"),
        None => crate::util::home().join(".local/state").join("almanac"),
    }
}

/// `REGISTRY`: the AppImage registry the app itself owns.
pub fn registry() -> PathBuf {
    state().join("appimages.json")
}

// Sources, in the order they should appear.
pub const REPO: &str = "repo";
pub const AUR: &str = "aur";
pub const APPIMAGE: &str = "appimage";
pub const LOCAL: &str = "local";
pub const FLATPAK: &str = "flatpak";
pub const ORPHAN: &str = "orphan";
pub const SYSTEM: &str = "system";
/// AI coding agents: installed by their own scripts or a version manager, so no
/// package manager reports them. See agents.py.
pub const AGENT: &str = "agent";
/// Version-managed developer tools: mise, asdf and friends own software that
/// no package manager reports on.
pub const TOOL: &str = "tool";

/// Updates worth pulling out of the crowd. Two kinds:
///   security   — network-facing or credential-handling; a stale one is exposure
///   core       — can stop the machine booting, or needs a reboot to take effect
///
/// A slice of pairs rather than a map so the Python dict's iteration order
/// (security first) is kept.
pub const CRITICAL: &[(&str, &[&str])] = &[
    (
        "security",
        &[
            "chromium", "google-chrome", "firefox", "brave", "vivaldi", "opera",
            "openssl", "openssh", "gnutls", "curl", "wget", "nss", "ca-certificates",
            "sudo", "polkit", "1password", "bitwarden", "keepassxc", "gnupg",
            "wpa_supplicant", "networkmanager", "dnsmasq", "nginx", "openvpn",
            "wireguard-tools", "tailscale", "nordvpn-bin",
        ],
    ),
    (
        "core",
        &[
            "linux", "linux-lts", "linux-zen", "linux-firmware", "glibc", "systemd",
            "mkinitcpio", "limine", "grub", "mesa", "pacman", "btrfs-progs",
        ],
    ),
];

/// 'security', 'core', or '' -- empty for the ordinary majority.
pub fn criticality(name: &str) -> String {
    todo!("port: inventory::criticality")
}

pub const STALE_DAYS: i64 = 180;

// Things that legitimately stop changing. A font or an icon theme two years
// old is finished, not abandoned, and flagging it would train the user to
// ignore the warning that matters.
pub const FINISHED_PREFIXES: &[&str] = &["ttf-", "otf-", "noto-", "adobe-source-"];
pub const FINISHED_SUFFIXES: &[&str] =
    &["-font", "-fonts", "-icon-theme", "-themes", "-cursor-theme"];

/// What the AUR says about one of its packages (one value of `aur_health`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AurHealth {
    pub out_of_date: bool,
    pub last_modified: f64,
    pub maintainer: String,
}

/// `{icon, categories}` read from a package's .desktop file (one value of
/// `desktop_map`). `Default` is the Python's `{}` for a package with none.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DesktopEntry {
    pub icon: String,
    pub categories: String,
}

/// `_staleness`: 'orphaned' | 'flagged' | 'stale' | '' — strongest signal wins.
///
/// Only the first two are authoritative: a human either abandoned the package
/// or flagged it out of date. 'stale' is a build-date proxy and is deliberately
/// limited to real applications, because libraries and fonts sit untouched for
/// years as a matter of course.
pub fn staleness(
    name: &str,
    source: &str,
    aur: &BTreeMap<String, AurHealth>,
    desk: &DesktopEntry,
    meta: &BTreeMap<String, String>,
) -> String {
    todo!("port: inventory::staleness")
}

/// Ask the AUR about its own packages: flagged out-of-date, and when the
/// packaging was last touched.
///
/// This is the only *authoritative* staleness signal available -- a human
/// maintainer flags out-of-date, and LastModified is real packaging activity.
/// Build age (below) is only a proxy. One RPC call covers every package.
/// Offline returns an empty map: offline is not a reason to show nothing.
pub fn aur_health(names: &[String]) -> BTreeMap<String, AurHealth> {
    todo!("port: inventory::aur_health")
}

/// '1737.27 KiB' -> 1.70. Sorting needs a number: as text, '9 KiB' sorts
/// above '10 MiB'.
pub fn size_mb(value: &str) -> f64 {
    todo!("port: inventory::size_mb")
}

/// pacman's 'Fri 16 Oct 2026 04:08:05 PM PDT' -> epoch seconds, so the
/// column sorts chronologically instead of by the weekday name.
pub fn date_ts(value: &str) -> f64 {
    todo!("port: inventory::date_ts")
}

/// One row of the inventory, from any source.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct App {
    pub name: String,
    pub version: String,
    pub source: String,
    pub description: String,
    /// new version if one is available
    pub update: String,
    /// what to launch, when launchable
    pub exec_path: String,
    /// Mixed-type per-source details (`dict` in the Python).
    pub extra: Map<String, Value>,
}

impl App {
    /// `App(name=..., version=..., source=...)` with every other field at
    /// its default; set the rest with struct update syntax.
    pub fn new(name: &str, version: &str, source: &str) -> App {
        App { name: name.to_string(), version: version.to_string(), source: source.to_string(), ..Default::default() }
    }

    /// `@property has_update`.
    pub fn has_update(&self) -> bool {
        !self.update.is_empty()
    }
}

/// `_run`: run a command, returning stdout. Never raises: a missing or failing
/// helper should degrade one section of the list, not take the window down.
/// (Python default timeout: 25.)
pub fn run(cmd: &[&str], timeout: u64) -> String {
    crate::util::run(cmd, timeout).stdout
}

/// `_FIELDS`: pacman -Qi field -> the key we keep it under.
pub const FIELDS: &[(&str, &str)] = &[
    ("Description", "description"),
    ("URL", "url"),
    ("Groups", "groups"),
    ("Installed Size", "size"),
    ("Build Date", "built"),
    ("Install Date", "installed"),
    ("Licenses", "license"),
];

/// `_package_info`: full metadata for every package in ONE pacman call.
///
/// pacman -Qi emits "Field : value" blocks separated by blank lines. Values
/// can wrap onto continuation lines (long dependency lists), which is why a
/// naive line-per-field parse drops data -- continuation lines are appended
/// to the field already in progress.
pub fn package_info(names: &[String]) -> BTreeMap<String, BTreeMap<String, String>> {
    todo!("port: inventory::package_info")
}

/// `_desktop_map`: package -> {icon, categories} by reading the .desktop files it owns.
///
/// One `pacman -Ql` for the whole set, then read only the desktop files it
/// points at. Looking each package up individually would be far slower.
pub fn desktop_map(names: &[String]) -> BTreeMap<String, DesktopEntry> {
    todo!("port: inventory::desktop_map")
}

/// `_pending_repo`: name -> new version, from checkupdates (which never touches the db).
pub fn pending_repo() -> BTreeMap<String, String> {
    todo!("port: inventory::pending_repo")
}

/// The version pacman's own sync database offers for `name`, or ''.
///
/// checkupdates reads a private, freshly synced copy of the database, so it
/// can see a release the system database has not heard of yet. Installing
/// one package without -y uses the system copy, and against that copy the
/// package is already current -- pacman skips it and reports success.
pub fn synced_version(name: &str) -> String {
    todo!("port: inventory::synced_version")
}

/// `_pending_aur`: name -> new version, from `yay -Qua`.
pub fn pending_aur() -> BTreeMap<String, String> {
    todo!("port: inventory::pending_aur")
}

/// Omarchy's own packages: the distro updating itself, which deserves to be
/// visible rather than buried as somebody's dependency.
pub const OMARCHY_PREFIX: &str = "omarchy";

/// Explicitly-installed packages, plus anything with an update waiting.
///
/// Listing only explicit installs keeps the dependency closure out of the
/// way -- nobody installed libjpeg on purpose. But a dependency with an
/// update pending is still something the user has to act on, and omarchy
/// itself arrives that way, so pending updates are always included.
pub fn packages() -> Vec<App> {
    todo!("port: inventory::packages")
}

// ---------------------------------------------------------------- appimages --

/// `_load_registry`: the AppImage registry, `{}` when missing or unreadable.
/// Keys are registry keys; values are the mixed-type records.
pub fn load_registry() -> Map<String, Value> {
    todo!("port: inventory::load_registry")
}

/// Write the AppImage registry (indent=2, trailing newline), atomically: a
/// crash mid-write cannot truncate it. Errors propagate, as the Python's
/// OSError does.
pub fn save_registry(data: &Map<String, Value>) -> Result<(), String> {
    todo!("port: inventory::save_registry")
}

pub const APPIMAGE_CHECK_SECONDS: f64 = 3600.0;

/// Registered AppImages. (Python default: `check_updates=True`.)
///
/// Asks GitHub at most hourly: a rescan happens on every package change,
/// and the release feed does not move nearly that fast.
pub fn appimages(check_updates: bool) -> Vec<App> {
    todo!("port: inventory::appimages")
}

/// `_KEEP_ORPHANS`: orphans that are usually wanted back. Build tooling gets
/// pulled in by the next AUR package and removing it just means downloading
/// it again; a debug package was almost certainly a deliberate choice.
pub const KEEP_ORPHANS: &[&str] = &[
    "rust", "cargo", "go", "python-build", "python-installer",
    "python-wheel", "python-setuptools", "meson", "ninja", "cmake",
];

/// How far behind, and at which level.
///
/// Returns (count, kind) where kind is 'major'/'minor'/'patch'/'rel'. Only the
/// first differing component is counted: 1.2.9 -> 3.0.0 is "2 major behind",
/// not "2 major and 2 minor", because once the major moves the lower numbers
/// restart and differencing them is meaningless.
///
/// Serialises as a JSON array `[count, kind]`, like the Python tuple.
pub fn versions_behind(current: &str, latest: &str) -> (i64, String) {
    todo!("port: inventory::versions_behind")
}

/// 'keep' when removing it is likely a false economy, else 'remove'.
pub fn orphan_advice(name: &str) -> String {
    todo!("port: inventory::orphan_advice")
}

/// Packages installed as dependencies that nothing requires any more.
///
/// Listed separately rather than mixed into the main inventory: they were
/// never chosen, so they are a cleanup decision, not software the user owns.
pub fn orphans() -> Vec<App> {
    todo!("port: inventory::orphans")
}

/// Same software present twice -- an AppImage plus its packaged build, or
/// a -bin and a source package side by side.
///
/// A Vec of pairs, not a map: the Python dict keeps first-seen order and the
/// UI shows the first five.
pub fn duplicates(apps: &[App]) -> Vec<(String, Vec<String>)> {
    todo!("port: inventory::duplicates")
}

/// Installed Flatpak apps. Silent no-op when flatpak is not installed, so
/// the section simply does not appear rather than erroring.
pub fn flatpaks() -> Vec<App> {
    todo!("port: inventory::flatpaks")
}

pub const GIT_CHECK_SECONDS: f64 = 3600.0;

/// `_GIT_CACHE`: `$XDG_CACHE_HOME/almanac/git.json`.
pub fn git_cache_path() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac/git.json"),
        None => crate::util::home().join(".cache").join("almanac/git.json"),
    }
}

/// `_git_cache`: the git cache file, `{}` when missing or unreadable. Keys
/// are repo paths (values are `GitBehind` records) plus `"_plugins"`, the
/// cache `plugins::classify` keeps.
pub fn git_cache() -> Map<String, Value> {
    todo!("port: inventory::git_cache")
}

/// One `_git_behind` result, as stored in the git cache. Optional fields are
/// absent (not null) in the JSON, exactly like the Python dict.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitBehind {
    pub tracked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<bool>,
    pub fetched_at: f64,
}

/// `_git_behind`: how far a checkout is behind its upstream.
///
/// Only the `git fetch` is cached -- it is a network round trip per repo, so
/// it runs at most once an hour. Counting commits against the fetched refs is
/// local and cheap, so it is done every time: a cached count kept a plugin
/// "901 commits behind" for an hour after it had been pulled. A repo with no
/// upstream, or an unreachable remote, returns nothing rather than a
/// misleading zero -- "up to date" and "could not check" are different answers.
///
/// Writes the result back into `cache` under the repo path.
pub fn git_behind(repo: &Path, cache: &mut Map<String, Value>) -> GitBehind {
    todo!("port: inventory::git_behind")
}

/// `_github_page`: a git remote as a page a browser can open, or ''.
pub fn github_page(url: &str) -> String {
    todo!("port: inventory::github_page")
}

/// Software built from source here. Listed so the inventory is honest about
/// what exists; these update through their own repos, not a package manager.
/// (Python default: `check_updates=True`.)
pub fn local_builds(check_updates: bool) -> Vec<App> {
    todo!("port: inventory::local_builds")
}

/// Everything, in one list.
///
/// The six collectors are independent and each is dominated by waiting --
/// checkupdates and yay on the network, git on disk, agents on process
/// start-up. Run together the total is the slowest one rather than the sum.
pub fn collect() -> Vec<App> {
    todo!("port: inventory::collect")
}

/// Everything worth showing in a detail view, fetched on demand.
///
/// Deliberately not part of collect(): dependency lists mean a pacman call
/// per package, which would turn a 2-second scan into a minute for data that
/// is only read one row at a time.
pub fn details(app: &App) -> Map<String, Value> {
    todo!("port: inventory::details")
}

// ---------------------------------------------------------------- discover --
pub const AVAILABLE: &str = "available";

/// Search what could be installed, across the official repos and the AUR.
/// (Python default: `limit=60`.)
///
/// Results are marked AVAILABLE so the UI can offer Install rather than
/// Update. Anything already installed is dropped: it belongs in the main
/// inventory, where its real version and update state live.
pub fn search_available(query: &str, limit: usize) -> Vec<App> {
    todo!("port: inventory::search_available")
}

// ------------------------------------------------- omarchy plugin registry --
pub const PLUGIN: &str = "plugin";
pub const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/omacom/omarchy-plugin-marketplace/main/registry.json";

/// `_REGISTRY_CACHE`: `$XDG_CACHE_HOME/almanac/registry.json`.
pub fn registry_cache_path() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac/registry.json"),
        None => crate::util::home().join(".cache").join("almanac/registry.json"),
    }
}

pub const REGISTRY_TTL: f64 = 86400.0;

/// The official Omarchy plugin marketplace index. (Python default: `force=False`.)
///
/// ~5,500 entries and a megabyte of JSON, so it is cached for a day. A failed
/// fetch falls back to whatever is on disk rather than emptying the catalogue.
/// Returns the raw JSON (an empty object when nothing is available).
pub fn plugin_registry(force: bool) -> Value {
    todo!("port: inventory::plugin_registry")
}

/// `_plugin_name`: (name, description, author) for a registry entry.
///
/// Only a couple of entries ship catalog metadata, so for the rest the repo
/// URL is all there is -- the last path segment is the plugin's identity.
pub fn plugin_name(entry: &Value) -> (String, String, String) {
    todo!("port: inventory::plugin_name")
}

/// Search the official Omarchy plugin marketplace. (Python default: `limit=40`.)
///
/// Retired plugins are excluded: they were delisted, so offering them would
/// be recommending something the registry has withdrawn.
///
/// (The Python's `_reverse` str subclass -- "sorts as its own reverse, so a
/// date field reads newest-first" -- becomes a reversed comparison inside
/// this function's sort.)
pub fn search_plugins(query: &str, limit: usize) -> Vec<App> {
    todo!("port: inventory::search_plugins")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
