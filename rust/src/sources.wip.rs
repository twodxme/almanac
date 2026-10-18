//! Port of `sources.py`.
//!
//! Install from a link: every way a project ships, compared.
//!
//! A GitHub project usually reaches an Arch machine several ways at once: an
//! official package, an AppImage on its releases page, an AUR package that
//! builds from source, an AUR `-bin` that repackages someone's binary. They are
//! not equally trustworthy, and the difference is invisible from the project
//! page. Asked to install "this link", this lays the options out side by side
//! and recommends one, with the reason:
//!
//! 1. **The official repositories** -- built and signed by the distribution.
//! 2. **The project's own AppImage**, when its checksum can be verified: GitHub
//!    publishes a SHA-256 for every release asset, and many projects also ship a
//!    checksum file.
//! 3. **An AUR package that builds from source**, when it points at this same
//!    repository and people actually use it (votes).
//! 4. An AppImage with nothing to verify it against, or an AUR binary
//!    repackage, last -- and an AUR package whose upstream URL is a different
//!    repository is flagged, since that is how a lookalike gets installed.
//!
//! Nothing is installed by resolving. The download of an AppImage is checked
//! against its checksum before it is made executable, and a mismatch deletes it.
//!
//! NOTE: this module defines a struct named `Option` (the Python class name),
//! which shadows the prelude's `Option` inside this file; the standard one is
//! always written `std::option::Option` here.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// HTTP header sent with every request (`UA = {"User-Agent": "almanac"}`).
pub const UA: (&str, &str) = ("User-Agent", "almanac");

/// `_GH`: a GitHub repository link, with or without scheme, `www.`, `.git`,
/// or a trailing path/fragment/query.
pub const GH_PATTERN: &str = r"^(?:https?://)?(?:www\.)?github\.com/([\w.-]+)/([\w.-]+?)(?:\.git)?(?:[/#?].*)?$";
/// `_X86` (case-insensitive): an asset name that says it is an x86-64 build.
pub const X86_PATTERN: &str = r"(?i)(x86[_-]64|amd64|x64)";
/// `_OTHER_ARCH` (case-insensitive): an asset name for some other architecture.
pub const OTHER_ARCH_PATTERN: &str = r"(?i)(aarch64|arm64|armv7|armhf|i[3-6]86|riscv)";

pub const REPO: &str = "repo";
pub const APPIMAGE: &str = "appimage";
pub const AUR: &str = "aur";

/// One way to install the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Option {
    pub kind: String,
    /// package name, or asset file name
    pub name: String,
    pub version: String,
    /// "Official package", "AppImage"…
    pub title: String,
    pub reasons: Vec<String>,
    pub warnings: Vec<String>,
    /// lower is better
    pub rank: i64,
    /// AppImage download
    pub url: String,
    /// expected digest, when known
    pub sha256: String,
    pub size_mb: f64,
}

impl Default for Option {
    fn default() -> Self {
        Option {
            kind: String::new(),
            name: String::new(),
            version: String::new(),
            title: String::new(),
            reasons: Vec::new(),
            warnings: Vec::new(),
            rank: 99,
            url: String::new(),
            sha256: String::new(),
            size_mb: 0.0,
        }
    }
}

/// Everything known about a link: the repository and its install options,
/// best first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Resolution {
    pub owner: String,
    pub repo: String,
    pub description: String,
    pub stars: i64,
    pub license: String,
    pub archived: bool,
    pub options: Vec<Option>,
    pub error: String,
}

impl Resolution {
    /// `@property url`
    pub fn url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    /// `@property best`
    pub fn best(&self) -> std::option::Option<&Option> {
        self.options.first()
    }
}

/// `(owner, repo)` of a GitHub repository link, or `None`.
pub fn parse(link: &str) -> std::option::Option<(String, String)> {
    let _ = link;
    todo!("port: sources::parse")
}

/// `_get`: GET `url` with the Almanac user agent, returning the body.
/// Raises (here: `Err`) on any network error.
pub fn get(url: &str, timeout: u64) -> Result<Vec<u8>, String> {
    let _ = (url, timeout);
    todo!("port: sources::_get")
}

/// `_official`: the official repository package of this name, if pacman
/// knows one.
pub fn official(name: &str) -> std::option::Option<Option> {
    let _ = name;
    todo!("port: sources::_official")
}

/// `_norm`: a URL lowercased, without scheme, `www.`, trailing `/` or `.git`,
/// so two spellings of one repository compare equal.
pub fn norm(url: &str) -> String {
    let _ = url;
    todo!("port: sources::_norm")
}

/// `_aur`: AUR packages by these names. `homes` are the URLs that count as this
/// project: its repository, and the homepage the repository itself names.
pub fn aur(names: &[&str], homes: &[&str]) -> Vec<Option> {
    let _ = (names, homes);
    todo!("port: sources::_aur")
}

/// `_sums`: {file name: sha256} from any checksum file in the release.
///
/// `assets` are the GitHub release's `assets` objects, as returned.
pub fn sums(assets: &[serde_json::Value]) -> BTreeMap<String, String> {
    let _ = assets;
    todo!("port: sources::_sums")
}

/// `_appimage`: the project's own Linux x86-64 AppImage from a GitHub release
/// object, with its checksum when one is published.
pub fn appimage(release: &serde_json::Value) -> std::option::Option<Option> {
    let _ = release;
    todo!("port: sources::_appimage")
}

/// Look a link up and lay out every way to install it, best first. Never
/// fails: problems are reported in `Resolution::error`.
pub fn resolve(link: &str) -> Resolution {
    let _ = link;
    todo!("port: sources::resolve")
}

/// Download, verify, register and add a launcher. Raises on failure.
///
/// `log` receives one line of progress at a time (the task runner's log).
pub fn install_appimage(res: &Resolution, opt: &Option, log: &dyn Fn(&str)) -> Result<PathBuf, String> {
    let _ = (res, opt, log);
    todo!("port: sources::install_appimage")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> std::option::Option<serde_json::Value> {
    None
}
