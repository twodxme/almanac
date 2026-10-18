//! Port of `themeart.py`.
//!
//! Drawing what a theme actually looks like.
//!
//! GitHub's social card shows a repository: its name, its description, its owner's
//! avatar. For a theme that is a picture of the wrong thing — it tells you nothing
//! about the only question you have, which is what your desktop will look like.
//!
//! Every Omarchy theme ships `colors.toml`, and raw.githubusercontent.com serves
//! it without touching the API rate limit. So the preview is drawn here from the
//! theme's own palette: a miniature desktop with the theme's bar, window,
//! text and accent in their real colours, over its real background.
//!
//! It is a mock, not a screenshot, and it is honest about that — but it is drawn
//! entirely from values the theme itself publishes, so the colours are exactly
//! the ones you would get.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `CACHE`: `$XDG_CACHE_HOME/almanac`, or `~/.cache/almanac`. Read at call
/// time (Rust has no import-time evaluation).
pub fn cache() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v),
        None => crate::util::home().join(".cache"),
    }
    .join("almanac")
}

/// `ART`: where drawn previews are cached.
pub fn art() -> PathBuf {
    cache().join("themeart")
}

pub const RAW: &str = "https://raw.githubusercontent.com";
/// `UA`: the User-Agent header sent with every fetch.
pub const UA: &str = "almanac";

pub const W: i32 = 480;
pub const H: i32 = 270;

/// `_HEX` as a pattern string: `#rgb` or `#rrggbb`.
pub const HEX_PATTERN: &str = r"^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{6})$";

/// repo is an https://github.com/owner/name URL.
pub fn palette_url(repo: &str) -> String {
    let parts: Vec<&str> = repo.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() < 4 {
        return String::new();
    }
    format!("{RAW}/{}/{}/HEAD/colors.toml", parts[2], parts[3])
}

/// The theme's own colours. Served raw, so it costs no API quota.
pub fn fetch_palette(repo: &str) -> BTreeMap<String, String> {
    let _ = repo;
    todo!("port: themeart::fetch_palette")
}

pub fn art_path(repo: &str) -> PathBuf {
    use sha1::{Digest, Sha1};
    let digest = hex::encode(Sha1::digest(repo.as_bytes()));
    art().join(format!("{}.png", &digest[..16]))
}

/// Draw a miniature desktop in the theme's colours.
pub fn render(palette: &BTreeMap<String, String>, dest: &Path) -> Option<PathBuf> {
    let _ = (palette, dest);
    todo!("port: themeart::render")
}

/// A cached preview for a theme repository, drawing it if needed.
pub fn thumbnail(repo: &str) -> Option<PathBuf> {
    let _ = repo;
    todo!("port: themeart::thumbnail")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
