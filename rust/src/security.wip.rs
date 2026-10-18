//! Port of `security.py`.
//!
//! Known vulnerabilities in what is installed, from the Arch Security Tracker.
//!
//! Until this, a "security update" meant an update to a package whose *name*
//! suggested exposure -- a browser, OpenSSL, sudo. That flags a routine curl
//! release as urgent and misses a vulnerable library nobody thinks of as
//! network-facing. The Arch Security Tracker publishes every advisory group
//! (AVG) with the packages, the affected and fixed versions, the severity and
//! the CVEs, as one public JSON document. Matching it against the installed
//! versions answers the real question: *is what I am running vulnerable, and
//! does an update fix it?*
//!
//! * Fetched from security.archlinux.org, cached for six hours. Nothing about
//!   this machine is sent; the whole list is downloaded and matched locally.
//! * Versions are compared with pacman's own `vercmp`, the only comparison that
//!   agrees with pacman about epochs and pkgrels.
//! * When the tracker cannot be reached, callers get None -- "unknown", which
//!   the inventory treats differently from "no vulnerabilities".

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

pub const URL: &str = "https://security.archlinux.org/issues/all.json";
pub const TTL: i64 = 6 * 3600;
/// `{"Critical": 0, "High": 1, "Medium": 2, "Low": 3, "Unknown": 4}`
pub const SEVERITY_RANK: &[(&str, i64)] = &[
    ("Critical", 0),
    ("High", 1),
    ("Medium", 2),
    ("Low", 3),
    ("Unknown", 4),
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Vuln {
    pub package: String,
    pub installed: String,
    /// AVG-1234
    pub group: String,
    /// Critical | High | Medium | Low | Unknown
    pub severity: String,
    /// "arbitrary code execution", …
    pub kind: String,
    /// '' when no fixed version exists yet
    pub fixed: String,
    pub cves: Vec<String>,
}

impl Vuln {
    pub fn url(&self) -> String {
        format!("https://security.archlinux.org/{}", self.group)
    }

    pub fn rank(&self) -> i64 {
        SEVERITY_RANK
            .iter()
            .find(|(k, _)| *k == self.severity)
            .map(|(_, r)| *r)
            .unwrap_or(4)
    }
}

/// Whether the tracker check is on (`settings.get("net_security_tracker")`).
pub fn enabled() -> bool {
    todo!("port: security::enabled")
}

/// The tracker's advisory groups, or None when it cannot be reached or
/// has been turned off in Preferences.
///
/// Groups are kept as the tracker's raw JSON objects, so the cache file is
/// byte-compatible with the Python's.
pub fn advisories(ttl: i64) -> Option<Vec<serde_json::Value>> {
    let _ = ttl;
    todo!("port: security::advisories")
}

/// `pacman -Q` as {name: version}; empty when pacman cannot run.
pub fn installed_versions() -> HashMap<String, String> {
    todo!("port: security::installed_versions")
}

/// Every advisory affecting an installed version. None: tracker unknown.
///
/// `installed` None runs `installed_versions()`; `groups` None calls
/// `advisories(TTL)`.
pub fn vulnerabilities(
    installed: Option<&HashMap<String, String>>,
    groups: Option<&[serde_json::Value]>,
) -> Option<Vec<Vuln>> {
    let _ = (installed, groups);
    todo!("port: security::vulnerabilities")
}

/// The vulnerabilities grouped by package.
pub fn by_package(vulns: &[Vuln]) -> BTreeMap<String, Vec<Vuln>> {
    let _ = vulns;
    todo!("port: security::by_package")
}

/// The vulnerabilities a pending version resolves.
pub fn fixed_by(vulns: &[Vuln], pending: &str) -> Vec<Vuln> {
    let _ = (vulns, pending);
    todo!("port: security::fixed_by")
}

/// The Health check row.
pub fn finding() -> crate::maintenance::Finding {
    todo!("port: security::finding")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
