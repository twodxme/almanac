//! Port of `icons.py`.
//!
//! Vendor icons, fetched from the vendors themselves.
//!
//! Every icon here comes from the domain it represents -- stripe.com's favicon is
//! fetched from stripe.com -- rather than from a favicon proxy. A proxy would be
//! one request containing the full list of every service this machine talks to,
//! sent to a company with no other involvement in it. These are hosts the user is
//! already connected to.
//!
//! Nothing is fetched twice: a hit is cached as a file, and a miss is remembered
//! too, so a domain with no usable icon is not retried on every redraw.

use std::path::PathBuf;

/// `CACHE`: `$XDG_CACHE_HOME/almanac`, or `~/.cache/almanac`. Read at call
/// time (Rust has no import-time evaluation).
pub fn cache() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v),
        None => crate::util::home().join(".cache"),
    }
    .join("almanac")
}

/// `ICONS`: downloaded icons, one file per key.
pub fn icons() -> PathBuf {
    cache().join("icons")
}

/// `MISSES`: keys with no usable icon, and when that was found out.
pub fn misses() -> PathBuf {
    cache().join("icon-misses.json")
}

pub const MISS_TTL: f64 = 7.0 * 86400.0;
/// `UA`: the User-Agent header sent with every fetch.
pub const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) almanac";

// Tried in order. apple-touch-icon is last because it is the largest and the
// least often present.
pub const PATHS: &[&str] = &[
    "/favicon.ico",
    "/favicon.png",
    "/favicon.svg",
    "/apple-touch-icon.png",
    "/apple-touch-icon-precomposed.png",
];

/// `_EXT`: file signatures and the extension each one is saved under.
/// Checked in this order.
pub const EXT: &[(&[u8], &str)] = &[
    (b"\x89PNG", ".png"),
    (b"\x00\x00\x01\x00", ".ico"),
    (b"GIF8", ".gif"),
    (b"\xff\xd8\xff", ".jpg"),
    (b"<svg", ".svg"),
    (b"<?xm", ".svg"),
    (b"RIFF", ".webp"),
];

/// mcp.stripe.com -> stripe.com. Good enough for finding a brand icon.
pub fn registrable(host: &str) -> String {
    let parts: Vec<&str> = host.split('.').filter(|p| !p.is_empty()).collect();
    if parts.len() <= 2 {
        return parts.join(".");
    }
    // Two-part public suffixes that would otherwise be cut to nothing.
    let n = parts.len();
    if ["co", "com", "org", "net", "ac", "gov"].contains(&parts[n - 2]) {
        return parts[n - 3..].join(".");
    }
    parts[n - 2..].join(".")
}

pub fn host_of(url: &str) -> String {
    let re = regex::Regex::new(r"^[a-z]+://([^/:]+)").unwrap();
    re.captures(url)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

/// An already-downloaded icon, without touching the network.
pub fn cached(key: &str) -> Option<PathBuf> {
    let _ = key;
    todo!("port: icons::cached")
}

/// Get an icon for `key`, from an explicit url or a domain. Blocking.
///
/// `key` is what the result is cached under, so two services on the same
/// domain share one download. Pass "" for `url` / `domain` where the Python
/// leaves them at their defaults.
pub fn fetch(key: &str, url: &str, domain: &str) -> Option<PathBuf> {
    let _ = (key, url, domain);
    todo!("port: icons::fetch")
}

/// The org or user avatar, which is the brand mark for most repos.
pub fn github_avatar(owner: &str) -> String {
    if owner.is_empty() {
        String::new()
    } else {
        format!("https://github.com/{owner}.png?size=128")
    }
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
