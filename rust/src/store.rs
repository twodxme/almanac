//! Port of `store.py`.
//!
//! Plugin marketplaces, as the marketplaces themselves publish them.
//!
//! plugins.omarchy.org builds a catalog.json for its own front end: 5,600+ entries
//! with the name, description, author, category, tags, star count, verification
//! state, install command and card artwork already resolved. Reading that is what
//! makes the cards here look like the cards there.
//!
//! The alternative -- walking the raw registry.json -- gives repo URLs and little
//! else; names have to be guessed from directory names and descriptions do not
//! exist at all. That is the difference between a storefront and a list of links.
//!
//! Structure
//! ---------
//! Everything is keyed on a `Market`, because more than one marketplace is coming.
//! A market owns its catalog URL, its cache file, and the function that maps its
//! raw entries onto `Plugin`. Adding one means adding a MARKETS entry and a
//! mapper; nothing in the UI has to learn about it, and plugins stay attributable
//! to where they came from -- which is what makes provenance sortable rather than
//! decorative.
//!
//! Each catalog is stripped to the fields the cards use on the way into its cache.
//! The Omarchy one is 14MB raw and about a tenth of that once stripped.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One catalogue row: the `Plugin` fields as a JSON object, exactly what the
/// Python keeps as a `dict` and turns into a plugin with `Plugin(**row)`.
/// This is also the shape of the `rows` list in `catalog-<market>.json`.
pub type Row = serde_json::Map<String, Value>;

/// The request header every fetch sends (`UA = {"User-Agent": "almanac"}`).
pub const UA: (&str, &str) = ("User-Agent", "almanac");

/// `CACHE`: `$XDG_CACHE_HOME/almanac` (default `~/.cache/almanac`).
/// A function rather than a constant so tests can point it at a temp dir
/// through `XDG_CACHE_HOME`, as the Python tests patch `store.CACHE`.
pub fn cache() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => crate::util::home().join(".cache"),
    }
    .join("almanac")
}

/// `THUMBS`: `CACHE / "thumbs"`.
pub fn thumbs() -> PathBuf {
    cache().join("thumbs")
}

/// The sites regenerate a few times a day.
pub const TTL: f64 = 6.0 * 3600.0;
/// Bumped whenever a mapper starts keeping a new field, so existing caches are
/// refetched instead of silently serving entries that are missing it.
pub const SCHEMA: i64 = 5;

/// The six accents the Omarchy catalog assigns. Used for the initials tile on
/// the ~800 entries with no preview image, so a card without artwork still reads
/// as a card rather than as a blank.
///
/// Ordered as the Python dict is (the UI iterates it to clear `acc-*` classes).
pub const ACCENTS: &[(&str, (&str, &str))] = &[
    ("cyan", ("#2dd4bf", "#0e7490")),
    ("coral", ("#fb7185", "#9f1239")),
    ("lime", ("#a3e635", "#3f6212")),
    ("rose", ("#f472b6", "#9d174d")),
    ("amber", ("#fbbf24", "#b45309")),
    ("violet", ("#a78bfa", "#5b21b6")),
];
pub const DEFAULT_ACCENT: (&str, &str) = ("#94a3b8", "#334155");

/// `ACCENTS.get(name)`.
pub fn accent(name: &str) -> Option<(&'static str, &'static str)> {
    ACCENTS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// What is actually known about an entry's origin.
///
/// `rank` orders the provenance sort: 0 is the strongest claim. The labels
/// are deliberately specific -- "Changed since review" and "Verified"
/// are different promises, and collapsing both to a tick would overstate one
/// of them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trust {
    pub key: String,
    pub label: String,
    pub css: String,
    pub why: String,
    pub rank: usize,
}

pub const TRUST_ORDER: &[&str] = &[
    "official", "first-party", "reviewed", "verified",
    "reviewed-stale", "stale", "unverified", "unreachable",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub version: String,
    pub category: String,
    pub kind: String,
    pub status: String,
    pub tags: Vec<String>,
    pub accent: String,
    pub initials: String,
    pub repo: String,
    pub stars: i64,
    pub verified: bool,
    pub installable: bool,
    pub install_cmd: String,
    pub install_note: String,
    pub license: String,
    pub updated: String,
    pub added: String,
    pub thumb: String,
    pub preview: String,
    pub built_in: bool,

    // How this entry is installed. Marketplaces do not all ship the same kind
    // of thing: an Omarchy listing is a git plugin, an ArtCraft listing is a
    // signed-off AppImage release.
    /// plugin | appimage (| theme)
    pub install_kind: String,
    pub download_url: String,
    pub asset_name: String,
    pub size_mb: f64,

    // Provenance: which marketplace listed it, who publishes it, what was
    // reviewed, and whether what you would install today is still what was
    // reviewed.
    pub market: String,
    pub source_type: String,
    pub source_url: String,
    pub manifest_path: String,
    pub coverage: String,
    pub review_method: String,
    pub reviewed_by: String,
    pub reviewed_at: String,
    pub validated_commit: String,
    pub validated_at: String,
    pub validated_branch: String,
    pub upstream_commit: String,
    pub upstream_checked_at: String,
    /// The release lookup failed, so "not installable" is unknown, not a fact.
    pub unchecked: bool,
    /// Installable, but its README asks for more afterwards: see plugin_setup.py.
    pub needs_setup: bool,
    pub upstream_status: String,

    // filled in from the local system, not the catalog
    pub installed: bool,
    pub enabled: bool,
    pub local_version: String,
}

impl Default for Plugin {
    fn default() -> Self {
        Plugin {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            author: String::new(),
            version: String::new(),
            category: "Other".into(),
            kind: String::new(),
            status: String::new(),
            tags: Vec::new(),
            accent: String::new(),
            initials: String::new(),
            repo: String::new(),
            stars: 0,
            verified: false,
            installable: false,
            install_cmd: String::new(),
            install_note: String::new(),
            license: String::new(),
            updated: String::new(),
            added: String::new(),
            thumb: String::new(),
            preview: String::new(),
            built_in: false,
            install_kind: "plugin".into(),
            download_url: String::new(),
            asset_name: String::new(),
            size_mb: 0.0,
            market: "omarchy".into(),
            source_type: String::new(),
            source_url: String::new(),
            manifest_path: String::new(),
            coverage: String::new(),
            review_method: String::new(),
            reviewed_by: String::new(),
            reviewed_at: String::new(),
            validated_commit: String::new(),
            validated_at: String::new(),
            validated_branch: String::new(),
            upstream_commit: String::new(),
            upstream_checked_at: String::new(),
            unchecked: false,
            needs_setup: false,
            upstream_status: String::new(),
            installed: false,
            enabled: false,
            local_version: String::new(),
        }
    }
}

impl Plugin {
    /// `Plugin(**row)`. Errors where the Python raises `TypeError`: a key the
    /// dataclass lacks, or a value of the wrong type. (A key the dataclass
    /// lacks broke the whole store in Python; the tests check every row loads.)
    pub fn from_row(row: &Row) -> Result<Plugin, String> {
        todo!("port: store::Plugin::from_row")
    }

    // ---- marketplace ----
    pub fn market_name(&self) -> String {
        market(&self.market).map(|m| m.name.clone()).unwrap_or_else(|| self.market.clone())
    }

    pub fn page(&self) -> String {
        market(&self.market).map(|m| m.listing_url(self)).unwrap_or_else(|| self.repo.clone())
    }

    pub fn asset(&self, rel: &str) -> String {
        match market(&self.market) {
            Some(m) if !rel.is_empty() => format!("{}{}", m.site, rel),
            _ => String::new(),
        }
    }

    pub fn thumb_url(&self) -> String {
        self.asset(&self.thumb)
    }

    pub fn preview_url(&self) -> String {
        self.asset(&self.preview)
    }

    pub fn colors(&self) -> (&'static str, &'static str) {
        accent(&self.accent).unwrap_or(DEFAULT_ACCENT)
    }

    // ---- provenance ----
    /// The account that publishes it -- the thing to actually trust.
    ///
    /// The `author` field is self-declared in the plugin's own manifest; the
    /// repository owner is not. Where they disagree, this is the one that
    /// says where the code comes from.
    pub fn owner(&self) -> String {
        todo!("port: store::Plugin::owner")
    }

    pub fn host(&self) -> String {
        todo!("port: store::Plugin::host")
    }

    pub fn repo_name(&self) -> String {
        todo!("port: store::Plugin::repo_name")
    }

    /// One line naming where the code comes from, for the card.
    pub fn publisher(&self) -> String {
        todo!("port: store::Plugin::publisher")
    }

    /// Whether Install should be offered. An AppImage has no command --
    /// Almanac downloads it itself -- so it cannot be judged by having one;
    /// that test showed "Manual setup" on every installable ArtCraft app.
    pub fn can_install(&self) -> bool {
        self.installable && (!self.install_cmd.is_empty() || self.install_kind == "appimage")
    }

    /// Upstream has moved past the commit the marketplace validated.
    pub fn drifted(&self) -> bool {
        !self.validated_commit.is_empty()
            && !self.upstream_commit.is_empty()
            && self.validated_commit != self.upstream_commit
    }

    pub fn trust(&self) -> Trust {
        todo!("port: store::Plugin::trust")
    }

    pub fn haystack(&self) -> String {
        todo!("port: store::Plugin::haystack")
    }

    pub fn subtitle(&self) -> String {
        todo!("port: store::Plugin::subtitle")
    }
}

// ---- marketplaces ------------------------------------------------------------

/// The marketplace withholds one-click install from plugins whose README asks
/// for more than the install itself -- an API key, a package, a build step.
/// `omarchy plugin add` does not consult that flag: it clones, validates and
/// installs exactly as it does for any other plugin. So Almanac installs these
/// too, says plainly that setup follows, and walks through the setup after
/// (plugin_setup.py). Only single-plugin repositories qualify, and only with this
/// note; a failed compatibility check, a version-matched native helper, a
/// monorepo or a suite with its own installer stays manual, because there the
/// install itself is what cannot be done safely.
pub const _SETUP_NOTE: &str = "This plugin requires additional setup";

pub fn _needs_setup(e: &Value) -> bool {
    todo!("port: store::_needs_setup")
}

/// plugins.omarchy.org catalog.json -> the fields the cards render.
///
/// Keeping the other 30 fields would mean caching 14MB to display none of it.
pub fn _omarchy_entry(e: &Value) -> Row {
    todo!("port: store::_omarchy_entry")
}

/// Maps one raw catalogue entry onto `Plugin` fields.
pub type Mapper = fn(&Value) -> Row;
/// Builds a market's rows itself. `Err` where the Python loader raises.
pub type Loader = fn() -> Result<Vec<Row>, String>;

#[derive(Debug, Clone)]
pub struct Market {
    pub id: String,
    pub name: String,
    pub site: String,
    pub catalog_url: String,
    /// raw entry -> Plugin fields
    pub mapper: Option<Mapper>,
    /// or: build the rows itself
    pub loader: Option<Loader>,
    /// key holding the entry list
    pub root: String,
    /// how the site addresses one entry
    pub listing: String,
    /// The account this marketplace publishes under, when it is a vendor
    /// shipping its own software. Left empty for open marketplaces, where no
    /// listing is first-party.
    pub first_party_owner: String,
    pub enabled: bool,
}

impl Default for Market {
    fn default() -> Self {
        Market {
            id: String::new(),
            name: String::new(),
            site: String::new(),
            catalog_url: String::new(),
            mapper: None,
            loader: None,
            root: "plugins".into(),
            listing: "?plugin={id}".into(),
            first_party_owner: String::new(),
            enabled: true,
        }
    }
}

impl Market {
    /// `CACHE / f"catalog-{id}.json"`.
    pub fn cache(&self) -> PathBuf {
        cache().join(format!("catalog-{}.json", self.id))
    }

    pub fn listing_url(&self, p: &Plugin) -> String {
        if self.listing.is_empty() {
            return p.repo.clone();
        }
        format!("{}{}", self.site, self.listing.replace("{id}", &p.id))
    }

    /// The market's rows, each tagged with `market`. `Err` when the fetch or
    /// the loader fails (the Python raises; `_load_market` catches it).
    pub fn fetch(&self) -> Result<Vec<Row>, String> {
        todo!("port: store::Market::fetch")
    }
}

// ---- ArtCraft -----------------------------------------------------------------
// getartcraft.com publishes no catalog endpoint, so the listing is assembled
// from the three places the facts actually live: the vendor's own /apps page
// (which apps they ship, and the hero artwork), the GitHub organisation (what
// each one is, how it is licensed, how active it is) and each repository's
// latest release (the version and the Linux AppImage to install).
pub const ARTCRAFT_SITE: &str = "https://getartcraft.com/";
pub const ARTCRAFT_ORG: &str = "storytold";

/// The vendor groups these only by name, so the grouping is ours. It exists so
/// ArtCraft entries land in the same category filter as everything else rather
/// than in a bucket of their own.
pub const _ARTCRAFT_CATEGORY: &[(&str, &str)] = &[
    ("photocraft", "Graphics"), ("vectorcraft", "Graphics"),
    ("lightcraft", "Graphics"), ("designcraft", "Graphics"),
    ("figcraft", "Design"), ("cadcraft", "Design"),
    ("filmcraft", "Video"), ("effectcraft", "Video"),
    ("soundcraft", "Audio"),
    ("wordcraft", "Productivity"), ("gridcraft", "Productivity"),
    ("deckcraft", "Productivity"), ("pdfcraft", "Productivity"),
];
pub const _ACCENT_CYCLE: &[&str] = &["violet", "cyan", "coral", "lime", "amber", "rose"];

/// ArtCraft's listing, built from its /apps page, the GitHub organisation and
/// each repository's latest release. `Err` when the /apps page cannot be
/// fetched (the Python lets urlopen raise).
pub fn _artcraft_rows() -> Result<Vec<Row>, String> {
    todo!("port: store::_artcraft_rows")
}

/// Every built-in marketplace, in the Python dict's insertion order
/// (omarchy, artcraft, themes).
pub static MARKETS: LazyLock<Vec<Market>> = LazyLock::new(|| {
    vec![
        Market {
            id: "omarchy".into(),
            name: "Omarchy Plugins".into(),
            site: "https://plugins.omarchy.org/".into(),
            catalog_url: "https://plugins.omarchy.org/catalog.json".into(),
            mapper: Some(_omarchy_entry),
            ..Market::default()
        },
        Market {
            id: "artcraft".into(),
            name: "ArtCraft".into(),
            site: ARTCRAFT_SITE.into(),
            loader: Some(_artcraft_rows),
            listing: "apps/{id}".into(),
            first_party_owner: ARTCRAFT_ORG.into(),
            ..Market::default()
        },
        Market {
            id: "themes".into(),
            name: "Omarchy Themes".into(),
            site: "https://opengraph.githubassets.com/".into(),
            loader: Some(_theme_rows),
            listing: String::new(),
            ..Market::default()
        },
    ]
});

/// `MARKETS.get(id)`.
pub fn market(id: &str) -> Option<&'static Market> {
    MARKETS.iter().find(|m| m.id == id)
}

/// Marketplaces to load: built in, and not turned off in Preferences.
pub fn enabled_markets() -> Vec<&'static Market> {
    todo!("port: store::enabled_markets")
}

// ---- loading -----------------------------------------------------------------

/// A catalogue with rows that could not be checked is retried this soon rather
/// than kept for the full TTL: one rate-limited refresh otherwise reported four
/// installable apps as having no Linux build for six hours.
pub const RETRY: f64 = 10.0 * 60.0;

/// One market's rows: from its cache while fresh, otherwise fetched and
/// cached; on failure, whatever the (expired) cache still holds, or nothing.
pub fn _load_market(m: &Market, force: bool) -> Vec<Row> {
    todo!("port: store::_load_market")
}

/// Every listed plugin from every enabled marketplace.
///
/// Memoised process-wide like the Python's `_cached` (behind a lock); returns
/// a copy, so callers that `annotate` must annotate the list they hold.
pub fn catalog(force: bool) -> Vec<Plugin> {
    todo!("port: store::catalog")
}

/// id -> enabled, for everything the shell already knows about.
pub fn installed_ids() -> BTreeMap<String, bool> {
    todo!("port: store::installed_ids")
}

/// registry key -> installed version, for the AppImages this app tracks.
pub fn installed_appimages() -> BTreeMap<String, String> {
    todo!("port: store::installed_appimages")
}

/// Mark what is already on the system, so cards can say so.
pub fn annotate(plugins: &mut [Plugin]) {
    todo!("port: store::annotate")
}

// ---- sorting and filtering ---------------------------------------------------

/// A sort's comparator: the Python's sort-key lambda, as an ordering.
pub type SortFn = fn(&Plugin, &Plugin) -> Ordering;

/// ISO-8601 to a sortable number. Unknown dates sort oldest.
pub fn _ts(value: &str) -> f64 {
    todo!("port: store::_ts")
}

// The Python's `_Rev(str)` -- "reverse string ordering without a second sort
// pass" -- is just a reversed comparison here (`sort_za`).

fn sort_popular(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS popular")
}
fn sort_trending(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS trending")
}
fn sort_new(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS new")
}
fn sort_az(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS az")
}
fn sort_za(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS za")
}
fn sort_category(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS category")
}
fn sort_provenance(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS provenance")
}
fn sort_market(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS market")
}
fn sort_publisher(_a: &Plugin, _b: &Plugin) -> Ordering {
    todo!("port: store::SORTS publisher")
}

/// (key, label, sort function)
pub const SORTS: &[(&str, &str, SortFn)] = &[
    ("popular", "Most popular", sort_popular),
    ("trending", "Trending", sort_trending),
    ("new", "Newest", sort_new),
    ("az", "Name (A → Z)", sort_az),
    ("za", "Name (Z → A)", sort_za),
    ("category", "Category", sort_category),
    // Provenance: strongest claim first, then by marketplace, then by the
    // publisher -- so everything from one account lands together.
    ("provenance", "Provenance", sort_provenance),
    ("market", "Marketplace", sort_market),
    ("publisher", "Publisher", sort_publisher),
];

/// (category, count), busiest first, then by name.
pub fn categories(plugins: &[Plugin]) -> Vec<(String, usize)> {
    todo!("port: store::categories")
}

/// (market id, name, count), busiest first.
pub fn market_counts(plugins: &[Plugin]) -> Vec<(String, String, usize)> {
    todo!("port: store::market_counts")
}

/// (trust key, label, count) in provenance order.
pub fn trust_counts(plugins: &[Plugin]) -> Vec<(String, String, usize)> {
    todo!("port: store::trust_counts")
}

/// Filter and sort the store's plugins. Python defaults: `query=""`,
/// `category=""`, `sort="popular"`, `installed_only=false`,
/// `verified_only=false`, `market=""`, `trust=""`.
#[allow(clippy::too_many_arguments)]
pub fn browse(
    plugins: &[Plugin],
    query: &str,
    category: &str,
    sort: &str,
    installed_only: bool,
    verified_only: bool,
    market: &str,
    trust: &str,
) -> Vec<Plugin> {
    todo!("port: store::browse")
}

// ---- card artwork ------------------------------------------------------------

/// Local path for a card image, or None when the entry has no artwork.
pub fn thumb_path(p: &Plugin, detail: bool) -> Option<PathBuf> {
    todo!("port: store::thumb_path")
}

/// Download a card image if it is not already cached. Blocking.
pub fn fetch_thumb(p: &Plugin, detail: bool) -> Option<PathBuf> {
    todo!("port: store::fetch_thumb")
}

// ---- Omarchy themes ----------------------------------------------------------
// Themes have no catalogue of their own: omarchythemes.com renders client-side
// and ships no JSON, so the authoritative list is the `omarchy-theme` topic on
// GitHub, which is what theme authors already tag their repositories with.
//
// A theme is installed with `omarchy theme install <git-url>` and applied with
// `omarchy theme set <name>`, so unlike a plugin the install and the switch are
// two different actions -- which is why `install_kind` distinguishes them.
pub const THEME_TOPIC: &str = "omarchy-theme";
/// 100 per page, stars-ordered
pub const THEME_PAGES: u32 = 4;

/// Repositories that carry the topic but are not themes: tooling, lists, and
/// collections for other bars. Checked against the name, not the description,
/// because descriptions mention "theme" constantly and legitimately.
pub const _NOT_A_THEME: &[&str] = &[
    "awesome-", "-hook", "theme-manager", "theme-switcher",
    "waybar-themes", "-templates", "themes-collection",
    "theme-builder", "-generator", "dotfiles",
];

pub fn _looks_like_theme(repo: &Value) -> bool {
    todo!("port: store::_looks_like_theme")
}

/// Theme rows from the `omarchy-theme` GitHub topic. Never fails in practice
/// (ghcache answers None on error); `Result` only to fit `Loader`.
pub fn _theme_rows() -> Result<Vec<Row>, String> {
    todo!("port: store::_theme_rows")
}

/// Themes the user installed, as opposed to the ones Omarchy ships.
///
/// `omarchy theme remove` only accepts user-installed themes, so a stock
/// theme is treated exactly like a built-in plugin: present, applicable,
/// and not yours to delete.
pub fn removable_themes() -> BTreeSet<String> {
    todo!("port: store::removable_themes")
}

/// Theme names omarchy already knows about, lowercased for matching.
pub fn installed_themes() -> BTreeSet<String> {
    todo!("port: store::installed_themes")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
