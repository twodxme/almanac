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
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
    // os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"): a variable
    // that is set, even to "", wins.
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v),
        None => crate::util::home().join(".cache"),
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

// `remote = "Self"` keeps the derived (typed) impls as inherent functions,
// so the `Serialize`/`Deserialize` impls below can lay `raw` over them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, remote = "Self")]
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
    /// plugin | appimage
    pub install_kind: String,
    pub download_url: String,
    pub asset_name: String,
    pub size_mb: f64,
    /// The asset's published sha256 (hex), "" when its release has none.
    pub sha256: String,

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

    /// The dataclass never checks its fields' types: `Plugin(**row)` keeps a
    /// null description, an int id or a str star count as it is. The typed
    /// field above then holds the nearest value of its type, and the value
    /// the Python would hold is kept here, keyed by field name, so the
    /// plugin serialises exactly as the Python one. Assigning a field means
    /// removing its entry (`set_raw`/`clear_raw`).
    #[serde(skip)]
    pub raw: BTreeMap<String, Value>,
}

impl Serialize for Plugin {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.raw.is_empty() {
            return Plugin::serialize(self, s);
        }
        let mut v = Plugin::serialize(self, serde_json::value::Serializer).map_err(serde::ser::Error::custom)?;
        if let Value::Object(m) = &mut v {
            for (k, x) in &self.raw {
                if let Some(slot) = m.get_mut(k) {
                    *slot = x.clone();
                }
            }
        }
        v.serialize(s)
    }
}

impl<'de> Deserialize<'de> for Plugin {
    /// Unknown keys are ignored here and values of the wrong type kept in
    /// `raw`; `from_row` is the strict `Plugin(**row)`.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let row = Row::deserialize(d)?;
        Ok(Plugin::lenient(&row))
    }
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
            sha256: String::new(),
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
            raw: BTreeMap::new(),
        }
    }
}

/// The dataclass's fields with their defaults, in declaration order.
static DEFAULTS: LazyLock<Row> = LazyLock::new(|| match serde_json::to_value(Plugin::default()) {
    Ok(Value::Object(m)) => m,
    _ => Row::new(),
});

/// The dataclass's field names, in declaration order.
static FIELDS: LazyLock<Vec<String>> = LazyLock::new(|| DEFAULTS.keys().cloned().collect());

/// Whether `v` fits a field whose default is `default`, and if not, the
/// nearest value that does.
fn fit(default: &Value, v: &Value) -> Option<Value> {
    match default {
        Value::String(_) => match v {
            Value::String(_) => None,
            Value::Null => Some(json!("")),
            other => Some(json!(py_str(other))),
        },
        Value::Bool(_) => match v {
            Value::Bool(_) => None,
            other => Some(json!(truthy(Some(other)))),
        },
        Value::Array(_) => match v {
            Value::Array(a) if a.iter().all(Value::is_string) => None,
            other => Some(json!(py_iter(other).unwrap_or_default().iter().map(py_str).collect::<Vec<_>>())),
        },
        Value::Number(n) if n.is_f64() => match v {
            Value::Number(_) => None,
            Value::Bool(b) => Some(json!(if *b { 1.0 } else { 0.0 })),
            _ => Some(json!(0.0)),
        },
        Value::Number(_) => match v {
            Value::Number(n) if n.is_i64() => None,
            Value::Number(n) => Some(json!(match n.as_u64() {
                Some(_) => i64::MAX,
                None => {
                    let f = n.as_f64().unwrap_or(0.0);
                    if f.is_finite() { f as i64 } else { 0 }
                }
            })),
            Value::Bool(b) => Some(json!(*b as i64)),
            Value::String(s) => Some(json!(s.trim().parse::<i64>().unwrap_or(0))),
            _ => Some(json!(0)),
        },
        _ => None,
    }
}

impl Plugin {
    /// `Plugin(**row)`. Errors where the Python raises `TypeError`: a key the
    /// dataclass lacks, or a missing `id` or `name` (the two fields without a
    /// default). Types are not checked, as the dataclass does not check them.
    /// (A key the dataclass lacks broke the whole store in Python; the tests
    /// check every row loads.)
    pub fn from_row(row: &Row) -> Result<Plugin, String> {
        for k in row.keys() {
            if !FIELDS.iter().any(|f| f == k) {
                return Err(format!("TypeError: Plugin.__init__() got an unexpected keyword argument '{k}'"));
            }
        }
        let missing: Vec<&str> = ["id", "name"].into_iter().filter(|k| !row.contains_key(*k)).collect();
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|k| format!("'{k}'")).collect();
            return Err(format!(
                "TypeError: Plugin.__init__() missing {} required positional argument{}: {}",
                missing.len(),
                if missing.len() > 1 { "s" } else { "" },
                names.join(" and ")
            ));
        }
        Ok(Plugin::lenient(row))
    }

    /// The fields of `row` the dataclass has, whatever their types (see
    /// `raw`); other keys are ignored.
    fn lenient(row: &Row) -> Plugin {
        let mut typed = Row::new();
        let mut raw = BTreeMap::new();
        for (k, v) in row {
            let Some(default) = DEFAULTS.get(k) else { continue };
            match fit(default, v) {
                None => {
                    typed.insert(k.clone(), v.clone());
                }
                Some(near) => {
                    typed.insert(k.clone(), near);
                    raw.insert(k.clone(), v.clone());
                }
            }
        }
        let mut p = Plugin::deserialize(Value::Object(typed)).unwrap_or_default();
        p.raw = raw;
        p
    }

    /// Forget the raw value of `field` (it has just been assigned).
    pub fn clear_raw(&mut self, field: &str) {
        self.raw.remove(field);
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

    fn repo_parts(&self) -> Vec<&str> {
        self.repo.split('/').filter(|x| !x.is_empty()).collect()
    }

    // ---- provenance ----
    /// The account that publishes it -- the thing to actually trust.
    ///
    /// The `author` field is self-declared in the plugin's own manifest; the
    /// repository owner is not. Where they disagree, this is the one that
    /// says where the code comes from.
    pub fn owner(&self) -> String {
        let parts = self.repo_parts();
        if parts.len() > 2 { parts[2].to_string() } else { String::new() }
    }

    pub fn host(&self) -> String {
        let parts = self.repo_parts();
        if parts.len() > 1 { parts[1].to_string() } else { String::new() }
    }

    pub fn repo_name(&self) -> String {
        let parts = self.repo_parts();
        if parts.len() > 3 { parts[2..4].join("/") } else { String::new() }
    }

    /// One line naming where the code comes from, for the card.
    pub fn publisher(&self) -> String {
        if self.built_in {
            return "Omarchy · built in".into();
        }
        let repo_name = self.repo_name();
        if !repo_name.is_empty() {
            let host = self.host();
            return format!("{}/{}", if host.is_empty() { "github.com" } else { &host }, repo_name);
        }
        "unknown source".into()
    }

    /// Whether Install should be offered. An AppImage has no command --
    /// Almanac downloads it itself -- so it cannot be judged by having one;
    /// that test showed "Manual setup" on every installable ArtCraft app.
    pub fn can_install(&self) -> bool {
        if !self.raw.is_empty() {
            // On the values as the Python holds them (truthiness, `==`).
            return truthy(Some(&self.pv("installable")))
                && (truthy(Some(&self.pv("install_cmd"))) || self.pv("install_kind").as_str() == Some("appimage"));
        }
        self.installable && (!self.install_cmd.is_empty() || self.install_kind == "appimage")
    }

    /// Upstream has moved past the commit the marketplace validated.
    pub fn drifted(&self) -> bool {
        if !self.raw.is_empty() {
            let (v, u) = (self.pv("validated_commit"), self.pv("upstream_commit"));
            return truthy(Some(&v)) && truthy(Some(&u)) && !py_eq(&v, &u);
        }
        !self.validated_commit.is_empty()
            && !self.upstream_commit.is_empty()
            && self.validated_commit != self.upstream_commit
    }

    pub fn trust(&self) -> Trust {
        let t = |key: &str, label: &str, css: &str, why: String| Trust {
            key: key.into(),
            label: label.into(),
            css: css.into(),
            why,
            rank: TRUST_ORDER.iter().position(|k| *k == key).unwrap_or(0),
        };

        if self.built_in {
            return t("official", "Official", "pill-accent",
                     format!("Ships with Omarchy itself, listed by {}", self.market_name()));
        }
        let m = market(&self.market);
        if let Some(m) = m.filter(|m| !m.first_party_owner.is_empty() && self.owner() == m.first_party_owner) {
            // Verified, not assumed: the listing is only first-party if the
            // repository it points at is owned by the account the marketplace
            // publishes under. A vendor catalogue listing somebody else's
            // repository is exactly the case this must not wave through.
            let rel = if self.version.is_empty() { String::new() } else { format!(", release {}", self.version) };
            return t("first-party", "First-party", "pill-accent",
                     format!("Published by {} itself, from github.com/{}{rel}", m.name, self.owner()));
        }
        if self.upstream_status == "failed" || self.upstream_status == "unreachable" {
            return t("unreachable", "Source unreachable", "pill-warn",
                     format!("{} could not reach the repository on its last check ({})",
                             self.market_name(), pfx(&self.upstream_checked_at, 10)));
        }
        if self.review_method == "maintainer-reviewed" {
            let who = if self.reviewed_by.is_empty() { String::new() } else { format!(" by {}", self.reviewed_by) };
            let when = if self.reviewed_at.is_empty() { String::new() } else { format!(" on {}", pfx(&self.reviewed_at, 10)) };
            let base = format!("Source reviewed{who}{when} for {}", self.market_name());
            if self.drifted() {
                return t("reviewed-stale", "Changed since review", "pill-warn",
                         base + ", but upstream has committed since then");
            }
            return t("reviewed", "Reviewed", "pill-ok", base);
        }
        if self.coverage == "snapshot-verified" && !self.drifted() {
            return t("verified", "Verified", "pill-ok",
                     format!("{} automated checks passed against commit {}",
                             self.market_name(), pfx(&self.validated_commit, 7)));
        }
        if self.coverage == "snapshot-verified" || self.coverage == "update-unverified" {
            return t("stale", "Changed since check", "pill-warn",
                     format!("Validated at {}, but upstream is now at {} -- the newer \
                              commits have not been checked",
                             pfx(&self.validated_commit, 7), pfx(&self.upstream_commit, 7)));
        }
        t("unverified", "Unverified", "pill-quiet",
          format!("{} records no automated or human review", self.market_name()))
    }

    pub fn haystack(&self) -> String {
        [
            self.name.as_str(), &self.id, &self.description, &self.author,
            &self.category, &self.kind, &self.repo_name(),
            &self.market_name(), &self.tags.join(" "),
        ]
        .join(" ")
        .to_lowercase()
    }

    pub fn subtitle(&self) -> String {
        let mut bits: Vec<&str> = if self.author.is_empty() { vec![] } else { vec![&self.author] };
        if !self.kind.is_empty() {
            bits.push(&self.kind);
        }
        bits.join(" · ")
    }
}

// ---- the properties, exactly as the Python computes them ----------------------
// The dataclass keeps a cached value of the wrong type as it is (see `raw`),
// and its properties then work on that value: `" ".join` of a tag that is an
// int, `.split` of a repo that is None, `-p.stars` of a str all raise. The
// methods above work on the typed fields, which never fail, and are what the
// UI draws with; these `try_` forms see the raw values and fail where the
// Python fails. With no raw values the two agree.
impl Plugin {
    /// The field as the Python object holds it.
    fn pv(&self, field: &str) -> Value {
        if let Some(v) = self.raw.get(field) {
            return v.clone();
        }
        match field {
            "id" => json!(self.id),
            "name" => json!(self.name),
            "description" => json!(self.description),
            "author" => json!(self.author),
            "version" => json!(self.version),
            "category" => json!(self.category),
            "kind" => json!(self.kind),
            "tags" => json!(self.tags),
            "accent" => json!(self.accent),
            "repo" => json!(self.repo),
            "stars" => json!(self.stars),
            "installable" => json!(self.installable),
            "install_cmd" => json!(self.install_cmd),
            "updated" => json!(self.updated),
            "added" => json!(self.added),
            "thumb" => json!(self.thumb),
            "preview" => json!(self.preview),
            "built_in" => json!(self.built_in),
            "install_kind" => json!(self.install_kind),
            "market" => json!(self.market),
            "coverage" => json!(self.coverage),
            "review_method" => json!(self.review_method),
            "reviewed_by" => json!(self.reviewed_by),
            "reviewed_at" => json!(self.reviewed_at),
            "validated_commit" => json!(self.validated_commit),
            "upstream_commit" => json!(self.upstream_commit),
            "upstream_checked_at" => json!(self.upstream_checked_at),
            "upstream_status" => json!(self.upstream_status),
            "installed" => json!(self.installed),
            "enabled" => json!(self.enabled),
            _ => match Plugin::serialize(self, serde_json::value::Serializer) {
                Ok(Value::Object(m)) => m.get(field).cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            },
        }
    }

    /// `str` field `field`, or the AttributeError of calling `attr` on it.
    fn ps(&self, field: &str, attr: &str) -> Result<String, String> {
        Ok(as_str(&self.pv(field), attr)?.to_string())
    }

    /// `MARKETS.get(self.market)`: only a str can be a key, and a list or
    /// dict cannot even be looked up.
    fn py_market(&self) -> Result<Option<&'static Market>, String> {
        match self.pv("market") {
            Value::String(s) => Ok(market(&s)),
            v @ (Value::Array(_) | Value::Object(_)) => Err(unhashable(&v)),
            _ => Ok(None),
        }
    }

    pub fn try_market_name(&self) -> Result<Value, String> {
        Ok(match self.py_market()? {
            Some(m) => json!(m.name),
            None => self.pv("market"),
        })
    }

    pub fn try_page(&self) -> Result<Value, String> {
        Ok(match self.py_market()? {
            Some(m) if !m.listing.is_empty() => {
                json!(format!("{}{}", m.site, m.listing.replace("{id}", &py_str(&self.pv("id")))))
            }
            _ => self.pv("repo"),
        })
    }

    /// `str(p.market_name)`: the text an f-string or a label shows.
    pub fn try_market_name_text(&self) -> Result<String, String> {
        Ok(py_str(&self.try_market_name()?))
    }

    /// `str(p.page)`, as for `try_market_name_text`.
    pub fn try_page_text(&self) -> Result<String, String> {
        Ok(py_str(&self.try_page()?))
    }

    fn try_repo_parts(&self) -> Result<Vec<String>, String> {
        let repo = self.ps("repo", "split")?;
        Ok(repo.split('/').filter(|x| !x.is_empty()).map(str::to_string).collect())
    }

    pub fn try_owner(&self) -> Result<String, String> {
        let parts = self.try_repo_parts()?;
        Ok(if parts.len() > 2 { parts[2].clone() } else { String::new() })
    }

    pub fn try_host(&self) -> Result<String, String> {
        let parts = self.try_repo_parts()?;
        Ok(if parts.len() > 1 { parts[1].clone() } else { String::new() })
    }

    pub fn try_repo_name(&self) -> Result<String, String> {
        let parts = self.try_repo_parts()?;
        Ok(if parts.len() > 3 { parts[2..4].join("/") } else { String::new() })
    }

    pub fn try_publisher(&self) -> Result<String, String> {
        if truthy(Some(&self.pv("built_in"))) {
            return Ok("Omarchy · built in".into());
        }
        let repo_name = self.try_repo_name()?;
        if !repo_name.is_empty() {
            let host = self.try_host()?;
            return Ok(format!("{}/{}", if host.is_empty() { "github.com" } else { &host }, repo_name));
        }
        Ok("unknown source".into())
    }

    pub fn try_trust(&self) -> Result<Trust, String> {
        if self.raw.is_empty() {
            return Ok(self.trust());
        }
        let t = |key: &str, label: &str, css: &str, why: String| Trust {
            key: key.into(),
            label: label.into(),
            css: css.into(),
            why,
            rank: TRUST_ORDER.iter().position(|k| *k == key).unwrap_or(0),
        };
        let market_name = || self.try_market_name().map(|v| py_str(&v));
        let cut = |field: &str, n: usize| py_slice(&self.pv(field), n).map(|v| py_str(&v));

        if truthy(Some(&self.pv("built_in"))) {
            return Ok(t("official", "Official", "pill-accent",
                        format!("Ships with Omarchy itself, listed by {}", market_name()?)));
        }
        if let Some(m) = self.py_market()? {
            if !m.first_party_owner.is_empty() && self.try_owner()? == m.first_party_owner {
                let version = self.pv("version");
                let rel = if truthy(Some(&version)) { format!(", release {}", py_str(&version)) } else { String::new() };
                return Ok(t("first-party", "First-party", "pill-accent",
                            format!("Published by {} itself, from github.com/{}{rel}", m.name, self.try_owner()?)));
            }
        }
        let status = self.pv("upstream_status");
        if matches!(status.as_str(), Some("failed" | "unreachable")) {
            return Ok(t("unreachable", "Source unreachable", "pill-warn",
                        format!("{} could not reach the repository on its last check ({})",
                                market_name()?, cut("upstream_checked_at", 10)?)));
        }
        if self.pv("review_method").as_str() == Some("maintainer-reviewed") {
            let by = self.pv("reviewed_by");
            let who = if truthy(Some(&by)) { format!(" by {}", py_str(&by)) } else { String::new() };
            let when = if truthy(Some(&self.pv("reviewed_at"))) {
                format!(" on {}", cut("reviewed_at", 10)?)
            } else {
                String::new()
            };
            let base = format!("Source reviewed{who}{when} for {}", market_name()?);
            if self.drifted() {
                return Ok(t("reviewed-stale", "Changed since review", "pill-warn",
                            base + ", but upstream has committed since then"));
            }
            return Ok(t("reviewed", "Reviewed", "pill-ok", base));
        }
        let coverage = self.pv("coverage");
        if coverage.as_str() == Some("snapshot-verified") && !self.drifted() {
            return Ok(t("verified", "Verified", "pill-ok",
                        format!("{} automated checks passed against commit {}",
                                market_name()?, cut("validated_commit", 7)?)));
        }
        if matches!(coverage.as_str(), Some("snapshot-verified" | "update-unverified")) {
            return Ok(t("stale", "Changed since check", "pill-warn",
                        format!("Validated at {}, but upstream is now at {} -- the newer \
                                 commits have not been checked",
                                cut("validated_commit", 7)?, cut("upstream_commit", 7)?)));
        }
        Ok(t("unverified", "Unverified", "pill-quiet",
             format!("{} records no automated or human review", market_name()?)))
    }

    pub fn try_haystack(&self) -> Result<String, String> {
        // The tuple is built first (repo_name, market_name and the tags'
        // join can each raise), then joined.
        let mut parts: Vec<Value> = ["name", "id", "description", "author", "category", "kind"]
            .iter()
            .map(|f| self.pv(f))
            .collect();
        parts.push(json!(self.try_repo_name()?));
        parts.push(self.try_market_name()?);
        let tags = match self.pv("tags") {
            Value::Null | Value::Bool(_) | Value::Number(_) => {
                return Err("TypeError: can only join an iterable".into());
            }
            other => py_iter(&other)?,
        };
        parts.push(json!(py_join(" ", &tags)?));
        Ok(py_join(" ", &parts)?.to_lowercase())
    }

    pub fn try_subtitle(&self) -> Result<String, String> {
        let author = self.pv("author");
        let mut bits = if truthy(Some(&author)) { vec![author] } else { vec![] };
        let kind = self.pv("kind");
        if truthy(Some(&kind)) {
            bits.push(kind);
        }
        py_join(" · ", &bits)
    }

    /// One `_SORT_FN[key]` sort key, as the Python lambda builds it.
    fn try_sort_key(&self, key: &str) -> Result<Vec<SortKey>, String> {
        let neg_stars = || match self.pv("stars") {
            Value::Bool(b) => Ok(SortKey::I(-i128::from(b))),
            Value::Number(n) => Ok(match (n.as_i64(), n.as_u64()) {
                (Some(i), _) => SortKey::I(-i128::from(i)),
                (None, Some(u)) => SortKey::I(-i128::from(u)),
                _ => SortKey::F(-n.as_f64().unwrap_or(0.0)),
            }),
            other => Err(format!("TypeError: bad operand type for unary -: '{}'", py_type(&other))),
        };
        let lower = |field: &str| self.ps(field, "lower").map(|s| SortKey::S(s.to_lowercase()));
        let ts = |field: &str| {
            let v = self.pv(field);
            if !truthy(Some(&v)) {
                return Ok::<_, String>(SortKey::F(0.0));
            }
            Ok(SortKey::F(_ts(as_str(&v, "replace")?)))
        };
        let market_lower = || {
            let v = self.try_market_name()?;
            Ok::<_, String>(SortKey::S(as_str(&v, "lower")?.to_lowercase()))
        };
        let rank = || self.try_trust().map(|t| SortKey::I(t.rank as i128));
        let owner = || self.try_owner().map(|o| SortKey::S(o.to_lowercase()));
        Ok(match key {
            "popular" => vec![neg_stars()?, lower("name")?],
            "trending" => vec![ts("updated")?.neg_ts(), neg_stars()?],
            "new" => vec![ts("added")?.neg_ts(), lower("name")?],
            "az" => vec![lower("name")?],
            "za" => match lower("name")? {
                SortKey::S(s) => vec![SortKey::R(s)],
                other => vec![other],
            },
            "category" => vec![lower("category")?, neg_stars()?],
            "provenance" => vec![rank()?, market_lower()?, owner()?, neg_stars()?],
            "market" => vec![market_lower()?, rank()?, neg_stars()?],
            "publisher" => vec![owner()?, neg_stars()?],
            _ => Vec::new(),
        })
    }
}

/// One element of a Python sort-key tuple.
#[derive(Debug, Clone, PartialEq)]
enum SortKey {
    I(i128),
    F(f64),
    S(String),
    /// `_Rev(str)`: ordered backwards.
    R(String),
}

impl SortKey {
    /// `-_ts(...)`: negate a timestamp; anything else passes through.
    fn neg_ts(self) -> SortKey {
        match self {
            SortKey::F(f) => SortKey::F(-f),
            other => other,
        }
    }

    fn cmp(&self, other: &SortKey) -> Ordering {
        use SortKey::*;
        match (self, other) {
            (I(a), I(b)) => a.cmp(b),
            (F(a), F(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (I(a), F(b)) => (*a as f64).partial_cmp(b).unwrap_or(Ordering::Equal),
            (F(a), I(b)) => a.partial_cmp(&(*b as f64)).unwrap_or(Ordering::Equal),
            (S(a), S(b)) => a.cmp(b),
            (R(a), R(b)) => b.cmp(a),
            _ => Ordering::Equal,
        }
    }
}

/// Tuples compare element by element.
fn cmp_keys(a: &[SortKey], b: &[SortKey]) -> Ordering {
    for (x, y) in a.iter().zip(b) {
        match x.cmp(y) {
            Ordering::Equal => continue,
            o => return o,
        }
    }
    a.len().cmp(&b.len())
}

/// `sorted(items, key=_SORT_FN[key])`, as indices into `items`: every key is
/// built first, so the first one that raises is the error.
fn try_sorted(items: &[&Plugin], key: &str) -> Result<Vec<usize>, String> {
    let keys = items.iter().map(|p| p.try_sort_key(key)).collect::<Result<Vec<_>, _>>()?;
    let mut idx: Vec<usize> = (0..items.len()).collect();
    idx.sort_by(|a, b| cmp_keys(&keys[*a], &keys[*b]));
    Ok(idx)
}

/// The TypeError of using `v` as a dict key or set member.
fn unhashable(v: &Value) -> String {
    format!("TypeError: unhashable type: '{}'", py_type(v))
}

/// `sep.join(items)`: every item must be a str.
fn py_join(sep: &str, items: &[Value]) -> Result<String, String> {
    let mut out: Vec<&str> = Vec::with_capacity(items.len());
    for (i, v) in items.iter().enumerate() {
        match v {
            Value::String(s) => out.push(s),
            other => {
                return Err(format!("TypeError: sequence item {i}: expected str instance, {} found", py_type(other)));
            }
        }
    }
    Ok(out.join(sep))
}

/// `a == b` on JSON values, as Python compares them (`1 == 1.0 == True`).
fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q)),
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ => match (py_hash_key(a), py_hash_key(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
    }
}

// ---- Python value semantics ---------------------------------------------------
// The mappers read raw JSON the way the Python does: `e.get(k) or default`
// keeps whatever value is there when it is truthy, so these helpers carry
// that over exactly rather than coercing early.

/// `s[:n]`, by characters.
fn pfx(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python truthiness of a JSON value (`None` for a missing key).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `e.get(key) or default`.
fn or_(e: &Row, key: &str, default: Value) -> Value {
    match e.get(key) {
        Some(v) if truthy(Some(v)) => v.clone(),
        _ => default,
    }
}

/// `a or b` on values.
fn or_v(a: Value, b: Value) -> Value {
    if truthy(Some(&a)) { a } else { b }
}

/// `str(v)`.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_f64() => crate::util::py_float_repr(n.as_f64().unwrap_or(0.0)),
        Value::Number(n) => n.to_string(),
        other => py_repr(other),
    }
}

/// `repr(v)`.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, x)| format!("{}: {}", py_repr_str(k), py_repr(x))).collect::<Vec<_>>().join(", ")
        ),
        other => py_str(other),
    }
}

/// The Python type name, for error messages.
fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// Python's `str.isspace()` for one character.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// The characters whose titlecase mapping is not their uppercase mapping
/// (Unicode 16, as Python 3.14 has it): the digraphs (`ǆ` -> `ǅ`), the
/// ligatures (`ß` -> `Ss`, `ﬁ` -> `Fi`), Georgian and the Greek letters with
/// a subscript iota. Sorted by character.
const TITLECASE: &[(char, &str)] = &[
    ('\u{DF}', "\u{53}\u{73}"), ('\u{1C4}', "\u{1C5}"), ('\u{1C5}', "\u{1C5}"), ('\u{1C6}', "\u{1C5}"),
    ('\u{1C7}', "\u{1C8}"), ('\u{1C8}', "\u{1C8}"), ('\u{1C9}', "\u{1C8}"), ('\u{1CA}', "\u{1CB}"),
    ('\u{1CB}', "\u{1CB}"), ('\u{1CC}', "\u{1CB}"), ('\u{1F1}', "\u{1F2}"), ('\u{1F2}', "\u{1F2}"),
    ('\u{1F3}', "\u{1F2}"), ('\u{587}', "\u{535}\u{582}"), ('\u{10D0}', "\u{10D0}"), ('\u{10D1}', "\u{10D1}"),
    ('\u{10D2}', "\u{10D2}"), ('\u{10D3}', "\u{10D3}"), ('\u{10D4}', "\u{10D4}"), ('\u{10D5}', "\u{10D5}"),
    ('\u{10D6}', "\u{10D6}"), ('\u{10D7}', "\u{10D7}"), ('\u{10D8}', "\u{10D8}"), ('\u{10D9}', "\u{10D9}"),
    ('\u{10DA}', "\u{10DA}"), ('\u{10DB}', "\u{10DB}"), ('\u{10DC}', "\u{10DC}"), ('\u{10DD}', "\u{10DD}"),
    ('\u{10DE}', "\u{10DE}"), ('\u{10DF}', "\u{10DF}"), ('\u{10E0}', "\u{10E0}"), ('\u{10E1}', "\u{10E1}"),
    ('\u{10E2}', "\u{10E2}"), ('\u{10E3}', "\u{10E3}"), ('\u{10E4}', "\u{10E4}"), ('\u{10E5}', "\u{10E5}"),
    ('\u{10E6}', "\u{10E6}"), ('\u{10E7}', "\u{10E7}"), ('\u{10E8}', "\u{10E8}"), ('\u{10E9}', "\u{10E9}"),
    ('\u{10EA}', "\u{10EA}"), ('\u{10EB}', "\u{10EB}"), ('\u{10EC}', "\u{10EC}"), ('\u{10ED}', "\u{10ED}"),
    ('\u{10EE}', "\u{10EE}"), ('\u{10EF}', "\u{10EF}"), ('\u{10F0}', "\u{10F0}"), ('\u{10F1}', "\u{10F1}"),
    ('\u{10F2}', "\u{10F2}"), ('\u{10F3}', "\u{10F3}"), ('\u{10F4}', "\u{10F4}"), ('\u{10F5}', "\u{10F5}"),
    ('\u{10F6}', "\u{10F6}"), ('\u{10F7}', "\u{10F7}"), ('\u{10F8}', "\u{10F8}"), ('\u{10F9}', "\u{10F9}"),
    ('\u{10FA}', "\u{10FA}"), ('\u{10FD}', "\u{10FD}"), ('\u{10FE}', "\u{10FE}"), ('\u{10FF}', "\u{10FF}"),
    ('\u{1F80}', "\u{1F88}"), ('\u{1F81}', "\u{1F89}"), ('\u{1F82}', "\u{1F8A}"), ('\u{1F83}', "\u{1F8B}"),
    ('\u{1F84}', "\u{1F8C}"), ('\u{1F85}', "\u{1F8D}"), ('\u{1F86}', "\u{1F8E}"), ('\u{1F87}', "\u{1F8F}"),
    ('\u{1F88}', "\u{1F88}"), ('\u{1F89}', "\u{1F89}"), ('\u{1F8A}', "\u{1F8A}"), ('\u{1F8B}', "\u{1F8B}"),
    ('\u{1F8C}', "\u{1F8C}"), ('\u{1F8D}', "\u{1F8D}"), ('\u{1F8E}', "\u{1F8E}"), ('\u{1F8F}', "\u{1F8F}"),
    ('\u{1F90}', "\u{1F98}"), ('\u{1F91}', "\u{1F99}"), ('\u{1F92}', "\u{1F9A}"), ('\u{1F93}', "\u{1F9B}"),
    ('\u{1F94}', "\u{1F9C}"), ('\u{1F95}', "\u{1F9D}"), ('\u{1F96}', "\u{1F9E}"), ('\u{1F97}', "\u{1F9F}"),
    ('\u{1F98}', "\u{1F98}"), ('\u{1F99}', "\u{1F99}"), ('\u{1F9A}', "\u{1F9A}"), ('\u{1F9B}', "\u{1F9B}"),
    ('\u{1F9C}', "\u{1F9C}"), ('\u{1F9D}', "\u{1F9D}"), ('\u{1F9E}', "\u{1F9E}"), ('\u{1F9F}', "\u{1F9F}"),
    ('\u{1FA0}', "\u{1FA8}"), ('\u{1FA1}', "\u{1FA9}"), ('\u{1FA2}', "\u{1FAA}"), ('\u{1FA3}', "\u{1FAB}"),
    ('\u{1FA4}', "\u{1FAC}"), ('\u{1FA5}', "\u{1FAD}"), ('\u{1FA6}', "\u{1FAE}"), ('\u{1FA7}', "\u{1FAF}"),
    ('\u{1FA8}', "\u{1FA8}"), ('\u{1FA9}', "\u{1FA9}"), ('\u{1FAA}', "\u{1FAA}"), ('\u{1FAB}', "\u{1FAB}"),
    ('\u{1FAC}', "\u{1FAC}"), ('\u{1FAD}', "\u{1FAD}"), ('\u{1FAE}', "\u{1FAE}"), ('\u{1FAF}', "\u{1FAF}"),
    ('\u{1FB2}', "\u{1FBA}\u{345}"), ('\u{1FB3}', "\u{1FBC}"), ('\u{1FB4}', "\u{386}\u{345}"), ('\u{1FB7}', "\u{391}\u{342}\u{345}"),
    ('\u{1FBC}', "\u{1FBC}"), ('\u{1FC2}', "\u{1FCA}\u{345}"), ('\u{1FC3}', "\u{1FCC}"), ('\u{1FC4}', "\u{389}\u{345}"),
    ('\u{1FC7}', "\u{397}\u{342}\u{345}"), ('\u{1FCC}', "\u{1FCC}"), ('\u{1FF2}', "\u{1FFA}\u{345}"), ('\u{1FF3}', "\u{1FFC}"),
    ('\u{1FF4}', "\u{38F}\u{345}"), ('\u{1FF7}', "\u{3A9}\u{342}\u{345}"), ('\u{1FFC}', "\u{1FFC}"), ('\u{FB00}', "\u{46}\u{66}"),
    ('\u{FB01}', "\u{46}\u{69}"), ('\u{FB02}', "\u{46}\u{6C}"), ('\u{FB03}', "\u{46}\u{66}\u{69}"), ('\u{FB04}', "\u{46}\u{66}\u{6C}"),
    ('\u{FB05}', "\u{53}\u{74}"), ('\u{FB06}', "\u{53}\u{74}"), ('\u{FB13}', "\u{544}\u{576}"), ('\u{FB14}', "\u{544}\u{565}"),
    ('\u{FB15}', "\u{544}\u{56B}"), ('\u{FB16}', "\u{54E}\u{576}"), ('\u{FB17}', "\u{544}\u{56D}"),
];

/// The titlecase letters (category Lt): cased, though neither upper- nor
/// lower-case.
const TITLE_LT: &[char] = &[
    '\u{1C5}', '\u{1C8}', '\u{1CB}', '\u{1F2}', '\u{1FBC}', '\u{1FCC}', '\u{1FFC}',
];

/// Python's `str.title()`: the titlecase mapping for the first cased
/// character after any uncased one, the lowercase mapping for the rest.
///
/// (CPython also lowers a capital sigma to a final sigma by context; every
/// caller here titles text it has already lower-cased, which holds no
/// capital sigma, so that rule never applies.)
fn py_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for ch in s.chars() {
        if prev_cased {
            out.extend(ch.to_lowercase());
        } else {
            match TITLECASE.binary_search_by_key(&ch, |(c, _)| *c) {
                Ok(i) => out.push_str(TITLECASE[i].1),
                Err(_) => out.extend(ch.to_uppercase()),
            }
        }
        prev_cased = ch.is_lowercase()
            || ch.is_uppercase()
            || TITLE_LT.contains(&ch)
            || matches!(ch, '\u{1F88}'..='\u{1F8F}' | '\u{1F98}'..='\u{1F9F}' | '\u{1FA8}'..='\u{1FAF}');
    }
    out
}

/// `json.loads(text)`. Python reads two things serde_json rejects: the
/// constants `NaN`, `Infinity` and `-Infinity`, and a `\uXXXX` escape of a
/// lone surrogate. A `Value` can hold neither, so the constants load as
/// null and a lone surrogate as U+FFFD -- the file still loads, as it does
/// there, instead of counting as broken.
fn py_json_loads(text: &str) -> Result<Value, String> {
    let cs: Vec<char> = text.chars().collect();
    let hex4 = |at: usize| -> Option<u32> {
        let s: String = cs.get(at..at + 4)?.iter().collect();
        if s.chars().all(|c| c.is_ascii_hexdigit()) { u32::from_str_radix(&s, 16).ok() } else { None }
    };
    let surrogate = |u: u32| (0xD800..0xE000).contains(&u);
    let mut out = String::with_capacity(text.len());
    let mut in_str = false;
    let mut i = 0;
    'scan: while i < cs.len() {
        let c = cs[i];
        if in_str {
            if c == '\\' && i + 1 < cs.len() {
                if cs[i + 1] == 'u' {
                    if let Some(u) = hex4(i + 2).filter(|u| surrogate(*u)) {
                        // A high surrogate followed by a low one is a pair.
                        let low = (u < 0xDC00 && cs.get(i + 6) == Some(&'\\') && cs.get(i + 7) == Some(&'u'))
                            .then(|| hex4(i + 8))
                            .flatten()
                            .filter(|l| (0xDC00..0xE000).contains(l));
                        if low.is_some() {
                            out.extend(&cs[i..i + 12]);
                            i += 12;
                        } else {
                            out.push_str("\\ufffd");
                            i += 6;
                        }
                        continue;
                    }
                }
                out.push(c);
                out.push(cs[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            out.push(c);
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
        } else {
            for tok in ["NaN", "Infinity", "-Infinity"] {
                let n = tok.chars().count();
                if cs.get(i..i + n).is_some_and(|w| w.iter().copied().eq(tok.chars())) {
                    out.push_str("null");
                    i += n;
                    continue 'scan;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    serde_json::from_str(&out).map_err(|e| format!("JSONDecodeError: {e}"))
}

/// `str.splitlines()`.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let brk = matches!(c, '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}'
                              | '\u{85}' | '\u{2028}' | '\u{2029}');
        if brk {
            out.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' {
                if let Some(&(j, '\n')) = it.peek() {
                    it.next();
                    end = j + 1;
                }
            }
            start = end;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The zero of every run of ten Unicode decimal digits (category Nd), which
/// is what Python's `int()` accepts as a digit.
const ND_ZEROS: &[u32] = &[
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0,
    0xFF10, 0x104A0, 0x10D30, 0x10D40, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450,
    0x114D0, 0x11650, 0x116C0, 0x116D0, 0x116DA, 0x11730, 0x118E0, 0x11950, 0x11BF0, 0x11C50,
    0x11D50, 0x11DA0, 0x11F50, 0x16130, 0x16A60, 0x16AC0, 0x16B50, 0x16D70, 0x1CCF0, 0x1D7CE,
    0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E4F0, 0x1E5F1, 0x1E950, 0x1FBF0,
];

/// `unicodedata.decimal(c)`.
fn py_decimal(c: char) -> Option<u32> {
    let u = c as u32;
    ND_ZEROS.iter().find(|z| (**z..**z + 10).contains(&u)).map(|z| u - z)
}

/// A Python int as JSON: exact while it fits 64 bits, the nearest float
/// beyond (JSON numbers here are no wider).
fn int_value(n: &num_bigint::BigInt) -> Value {
    if let Ok(i) = i64::try_from(n) {
        return Value::from(i);
    }
    if let Ok(u) = u64::try_from(n) {
        return Value::from(u);
    }
    let f: f64 = n.to_string().parse().unwrap_or(0.0);
    Value::from(if f.is_finite() { f } else { f64::MAX.copysign(f) })
}

/// `int(v)` for a JSON value: Python's `int()` of a bool, number or string,
/// of any size.
fn py_int(v: &Value) -> Result<Value, String> {
    use num_bigint::BigInt;
    match v {
        Value::Bool(b) => Ok(json!(*b as i64)),
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                return Ok(v.clone());
            }
            let f = n.as_f64().unwrap_or(f64::NAN);
            if f.is_nan() {
                return Err("ValueError: cannot convert float NaN to integer".into());
            }
            if f.is_infinite() {
                return Err("OverflowError: cannot convert float infinity to integer".into());
            }
            let t = f.trunc();
            Ok(int_value(&BigInt::parse_bytes(format!("{t:.0}").as_bytes(), 10).unwrap_or_default()))
        }
        Value::String(s) => {
            let bad = || format!("ValueError: invalid literal for int() with base 10: {}", py_repr_str(s));
            let t = py_strip(s);
            let (neg, digits) = match t.chars().next() {
                Some('-') => (true, &t[1..]),
                Some('+') => (false, &t[1..]),
                _ => (false, t),
            };
            if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
                return Err(bad());
            }
            let mut ascii = String::new();
            for c in digits.chars() {
                if c == '_' {
                    continue;
                }
                let d = py_decimal(c).ok_or_else(bad)?;
                ascii.push(char::from(b'0' + d as u8));
            }
            let n = BigInt::parse_bytes(ascii.as_bytes(), 10).ok_or_else(bad)?;
            Ok(int_value(&if neg { -n } else { n }))
        }
        other => Err(format!(
            "TypeError: int() argument must be a string, a bytes-like object or a real number, not '{}'",
            py_type(other)
        )),
    }
}

/// `str.isprintable()` for one character, as far as Rust can tell without
/// the Unicode category tables: controls, separators other than the space,
/// and the common format characters are not.
fn py_printable(c: char) -> bool {
    let u = c as u32;
    !(c.is_control()
        || matches!(u, 0xa0 | 0xad | 0x1680 | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064
                    | 0x2066..=0x206f | 0x3000 | 0xfeff | 0xfff9..=0xfffb | 0x600..=0x605 | 0x61c
                    | 0x6dd | 0x70f | 0xd800..=0xdfff | 0xe000..=0xf8ff | 0xf0000..=0x10ffff))
}

/// `repr(s)` for a str.
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if c != ' ' && !py_printable(c) => {
                let u = c as u32;
                out.push_str(&if u < 0x100 {
                    format!("\\x{u:02x}")
                } else if u < 0x10000 {
                    format!("\\u{u:04x}")
                } else {
                    format!("\\U{u:08x}")
                });
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `shlex.quote(s)`.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".into();
    }
    let safe = |c: char| c.is_ascii_alphanumeric() || "%+,-./:=@_".contains(c);
    if s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// `e.get(...)` on something that is not a dict raises AttributeError.
fn as_dict<'a>(e: &'a Value) -> Result<&'a Row, String> {
    e.as_object()
        .ok_or_else(|| format!("AttributeError: '{}' object has no attribute 'get'", py_type(e)))
}

/// `v.lower()` / `v.lstrip()` ... on a value that must be a str.
fn as_str<'a>(v: &'a Value, attr: &str) -> Result<&'a str, String> {
    v.as_str()
        .ok_or_else(|| format!("AttributeError: '{}' object has no attribute '{attr}'", py_type(v)))
}

/// The identity of a JSON value as a set element or dict key: equal values
/// compare equal as Python's do (`1 == 1.0 == True`, `0 == False`). None for
/// a list or dict, which are unhashable.
fn py_hash_key(v: &Value) -> Option<String> {
    Some(match v {
        Value::Null => "None".into(),
        Value::Bool(b) => format!("n:{}", *b as i64),
        Value::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => format!("n:{i}"),
            (None, Some(u), _) => format!("n:{u}"),
            // An integral float equals the int of the same value, however
            // large (`1e19 == 10**19`): both are keyed by its exact digits.
            (_, _, Some(f)) if f == 0.0 => "n:0".into(),
            (_, _, Some(f)) if f.is_finite() && f.fract() == 0.0 => format!("n:{f:.0}"),
            (_, _, f) => format!("f:{}", f.unwrap_or(0.0)),
        },
        Value::String(s) => format!("s:{s}"),
        Value::Array(_) | Value::Object(_) => return None,
    })
}

/// `v[:n]`: a str's first n characters, a list's first n items. Anything
/// else raises as it does in Python (a dict looks the slice up as a key).
fn py_slice(v: &Value, n: usize) -> Result<Value, String> {
    match v {
        Value::String(s) => Ok(json!(pfx(s, n))),
        Value::Array(a) => Ok(Value::Array(a.iter().take(n).cloned().collect())),
        Value::Object(_) => Err(format!("KeyError: slice(None, {n}, None)")),
        other => Err(format!("TypeError: '{}' object is not subscriptable", py_type(other))),
    }
}

/// The elements `for x in v` visits, for a JSON value. A dict yields its keys
/// and a str its characters, as Python does; a number or None raises.
fn py_iter(v: &Value) -> Result<Vec<Value>, String> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        Value::Object(o) => Ok(o.keys().map(|k| Value::String(k.clone())).collect()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(format!("TypeError: '{}' object is not iterable", py_type(other))),
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

/// `Err` where the Python raises: `e` not a dict, or an `installNote` that is
/// truthy but not a string.
pub fn _needs_setup(e: &Value) -> Result<bool, String> {
    let e = as_dict(e)?;
    if truthy(e.get("installAvailable")) || truthy(e.get("builtIn")) {
        return Ok(false);
    }
    if e.get("repositoryLayout").and_then(Value::as_str) != Some("root-plugin") {
        return Ok(false);
    }
    let note = or_(e, "installNote", json!(""));
    if !as_str(&note, "startswith")?.starts_with(_SETUP_NOTE) {
        return Ok(false);
    }
    Ok(py_str(&or_(e, "repo", json!(""))).starts_with("https://"))
}

/// plugins.omarchy.org catalog.json -> the fields the cards render.
///
/// Keeping the other 30 fields would mean caching 14MB to display none of it.
///
/// `Err` where the Python raises (an entry that is not a dict, a star count
/// `int()` rejects), which fails the whole fetch, as it does there.
pub fn _omarchy_entry(e: &Value) -> Result<Row, String> {
    let setup = _needs_setup(e)?;
    let e = as_dict(e)?;
    let s = |v: &str| Value::String(v.into());
    let install_cmd = or_v(
        or_v(or_(e, "installCommand", Value::Null), or_(e, "officialCommand", Value::Null)),
        if setup {
            let repo = e.get("repo").and_then(Value::as_str).unwrap_or_default();
            s(&format!("omarchy plugin add {} --enable", shlex_quote(repo)))
        } else {
            s("")
        },
    );
    let mut r = Row::new();
    r.insert("id".into(), or_(e, "id", s("")));
    r.insert("name".into(), or_v(or_(e, "name", Value::Null), or_(e, "id", s(""))));
    r.insert("description".into(), or_(e, "description", s("")));
    r.insert("author".into(), or_(e, "author", s("")));
    r.insert("version".into(), or_(e, "version", s("")));
    r.insert("category".into(), or_(e, "category", s("Other")));
    r.insert("kind".into(), or_(e, "kind", s("")));
    r.insert("status".into(), if setup { s("Needs setup") } else { or_(e, "status", s("")) });
    r.insert("tags".into(), or_(e, "tags", json!([])));
    r.insert("accent".into(), or_(e, "accent", s("")));
    r.insert("initials".into(), or_(e, "initials", s("")));
    r.insert("repo".into(), or_(e, "repo", s("")));
    r.insert("stars".into(), py_int(&or_(e, "stars", json!(0)))?);
    r.insert("verified".into(), json!(e.get("verificationStatus").and_then(Value::as_str) == Some("verified")));
    // A built-in is "installable" in the sense that it can be switched on;
    // the catalog expresses that as officialCommand rather than as an add.
    r.insert("installable".into(),
             json!(truthy(e.get("installAvailable")) || truthy(e.get("builtIn")) || setup));
    r.insert("install_cmd".into(), install_cmd);
    r.insert("needs_setup".into(), json!(setup));
    r.insert("install_note".into(), or_(e, "installNote", s("")));
    r.insert("license".into(), or_(e, "license", s("")));
    r.insert("updated".into(), or_(e, "repositoryUpdatedAt", s("")));
    r.insert("added".into(), or_v(or_(e, "listedAt", Value::Null), or_(e, "addedAt", s(""))));
    r.insert("thumb".into(), or_(e, "previewThumbnail", s("")));
    r.insert("preview".into(), or_v(or_(e, "previewImage", Value::Null), or_(e, "previewThumbnail", s(""))));
    r.insert("built_in".into(), json!(truthy(e.get("builtIn"))));
    r.insert("source_type".into(), or_(e, "sourceType", s("")));
    r.insert("source_url".into(), or_(e, "sourceUrl", s("")));
    r.insert("manifest_path".into(), or_(e, "manifestPath", s("")));
    r.insert("coverage".into(), or_(e, "verificationCoverage", s("")));
    r.insert("review_method".into(), or_(e, "verificationMethod", s("")));
    r.insert("reviewed_by".into(), or_(e, "verificationReviewedBy", s("")));
    r.insert("reviewed_at".into(), or_(e, "verificationReviewedAt", s("")));
    r.insert("validated_commit".into(), or_(e, "listingValidatedCommit", s("")));
    r.insert("validated_at".into(), or_(e, "listingValidatedAt", s("")));
    r.insert("validated_branch".into(), or_(e, "listingValidatedBranch", s("")));
    r.insert("upstream_commit".into(), or_(e, "upstreamObservedCommit", s("")));
    r.insert("upstream_checked_at".into(), or_(e, "upstreamCheckedAt", s("")));
    r.insert("upstream_status".into(), or_(e, "upstreamCheckStatus", s("")));
    Ok(r)
}

/// Maps one raw catalogue entry onto `Plugin` fields. `Err` where the Python
/// mapper raises.
pub type Mapper = fn(&Value) -> Result<Row, String>;
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

/// `urllib.request.urlopen(Request(url, headers=UA), timeout=secs).read()`.
/// urllib raises for anything but 2xx; so does this.
///
/// urllib's timeout applies to each socket operation, with no limit on the
/// whole transfer: a slow but steady download finishes, a stalled one fails
/// after `timeout_secs` without data. ureq only limits a whole phase, so the
/// body is read on a helper thread and each chunk waited for in turn.
fn http_get(url: &str, timeout_secs: u64) -> Result<Vec<u8>, String> {
    let t = std::time::Duration::from_secs(timeout_secs);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(t))
        .timeout_recv_response(Some(t))
        .build()
        .into();
    let r = agent.get(url).header(UA.0, UA.1).call().map_err(|e| format!("URLError: {e}"))?;
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut reader = r.into_body().into_reader();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(Ok(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(format!("URLError: {e}")));
                    return;
                }
            }
        }
        let _ = tx.send(Ok(Vec::new()));
    });
    let mut body = Vec::new();
    loop {
        match rx.recv_timeout(t) {
            Ok(Ok(chunk)) if chunk.is_empty() => return Ok(body),
            Ok(Ok(chunk)) => body.extend_from_slice(&chunk),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("URLError: timed out".into()),
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
        if let Some(loader) = self.loader {
            let mut out = Vec::new();
            for mut row in loader()? {
                if truthy(row.get("id")) {
                    row.insert("market".into(), Value::String(self.id.clone()));
                    out.push(row);
                }
            }
            return Ok(out);
        }
        let raw = http_get(&self.catalog_url, 60)?;
        let text = String::from_utf8(raw).map_err(|e| format!("UnicodeDecodeError: {e}"))?;
        let data: Value = py_json_loads(&text)?;
        let rows = match &data {
            Value::Object(o) => o.get(&self.root).cloned().unwrap_or(Value::Null),
            other => other.clone(),
        };
        let mut out = Vec::new();
        let entries = if truthy(Some(&rows)) { py_iter(&rows)? } else { Vec::new() };
        for e in entries {
            let Some(mapper) = self.mapper else {
                return Err(format!("AssertionError: {}: no mapper or loader", self.id));
            };
            let mut row = mapper(&e)?;
            if truthy(row.get("id")) {
                row.insert("market".into(), Value::String(self.id.clone()));
                out.push(row);
            }
        }
        Ok(out)
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

static APP_LINK: LazyLock<regex::Regex> = LazyLock::new(|| {
    // Python's `\s` on a str is `str.isspace()`, which also takes the
    // separators \x1c-\x1f; the regex crate's `\s` (White_Space) does not.
    regex::Regex::new(r#"href="/apps/([a-z0-9-]+)"[^>]*>[\s\x1c-\x1f]*([A-Za-z][A-Za-z ]{1,30}?)[\s\x1c-\x1f]*<"#)
        .expect("regex")
});
static APP_HREF: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"href="/apps/([a-z0-9-]+)""#).expect("regex"));

/// ArtCraft's listing, built from its /apps page, the GitHub organisation and
/// each repository's latest release. `Err` when the /apps page cannot be
/// fetched (the Python lets urlopen raise).
pub fn _artcraft_rows() -> Result<Vec<Row>, String> {
    let ttl = crate::ghcache::DEFAULT_TTL;
    let previous = market("artcraft").and_then(|m| std::fs::read(m.cache()).ok());
    _artcraft_rows_with(
        previous.as_deref(),
        &|| {
            let raw = http_get(&format!("{ARTCRAFT_SITE}apps"), 30)?;
            Ok(String::from_utf8_lossy(&raw).into_owned())
        },
        &|org| crate::ghcache::try_org_repos(org, ttl),
        &|repo| crate::ghcache::try_latest_release(repo, ttl),
    )
}

/// The body of `_artcraft_rows`, with what it reads passed in: the bytes of
/// the previous cache file (None when it could not be read), the /apps page,
/// and the two GitHub lookups -- the seams the Python tests mock.
pub fn _artcraft_rows_with(
    previous_cache: Option<&[u8]>,
    fetch_apps: &dyn Fn() -> Result<String, String>,
    org_repos: &dyn Fn(&str) -> Result<Option<Value>, String>,
    latest_release: &(dyn Fn(&str) -> Result<Option<Value>, String> + Sync),
) -> Result<Vec<Row>, String> {
    // What the last successful run found, keyed by slug. The GitHub API allows
    // 60 unauthenticated calls an hour and this costs 14 of them, so a refresh
    // that runs into the limit must keep the version it already knew rather
    // than caching a blank one for the next six hours.
    let mut previous: HashMap<String, Value> = HashMap::new();
    // except (OSError, ValueError, KeyError): an unreadable or broken file is
    // no previous run; a file of the wrong shape raises, as it does there.
    let blob = previous_cache
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|t| py_json_loads(t).ok());
    if let Some(blob) = blob {
        let rows = or_(as_dict(&blob)?, "rows", json!([]));
        for row in py_iter(&rows)? {
            let id = as_dict(&row)?.get("id").cloned().unwrap_or(Value::Null);
            // `previous[row.get("id")] = row`: a list or dict id is unhashable
            // and raises; any other id that is not a str can never match a slug.
            match id {
                Value::String(id) => {
                    previous.insert(id, row);
                }
                Value::Array(_) | Value::Object(_) => {
                    return Err(format!("TypeError: cannot use '{t}' as a dict key (unhashable type: '{t}')",
                                       t = py_type(&id)));
                }
                _ => {}
            }
        }
    }

    // 1. Which apps the vendor actually ships, and what they call them.
    let html = fetch_apps()?;
    let mut names: Vec<(String, String)> = Vec::new();
    let setdefault = |names: &mut Vec<(String, String)>, slug: &str, label: String| {
        if !names.iter().any(|(s, _)| s == slug) {
            names.push((slug.to_string(), label));
        }
    };
    for c in APP_LINK.captures_iter(&html) {
        setdefault(&mut names, &c[1], py_strip(&c[2]).to_string());
    }
    for c in APP_HREF.captures_iter(&html) {
        setdefault(&mut names, &c[1], py_title(&c[1]));
    }
    if names.is_empty() {
        return Ok(Vec::new());
    }

    // 2. What each repository is. One call for the whole organisation.
    let mut repos: HashMap<String, Row> = HashMap::new();
    // ghcache's lookups raise out of here, as they do in the Python (and
    // `fetch` then fails, so the cached listing is served).
    let listed = org_repos(ARTCRAFT_ORG)?.unwrap_or(Value::Null);
    let listed = if truthy(Some(&listed)) { py_iter(&listed)? } else { Vec::new() };
    for r in listed {
        if let Value::Object(r) = r {
            if truthy(r.get("name")) {
                let key = as_str(&r["name"], "lower")?.to_lowercase();
                repos.insert(key, r);
            }
        }
    }

    // 3. The release to install, for each app at once. These are independent
    //    HTTP calls, and doing them one after another was most of the load
    //    time; cached and conditional ones return in milliseconds.
    let mut slugs: Vec<String> = names.iter().map(|(s, _)| s.clone()).collect();
    slugs.sort();
    let releases: Vec<Option<Value>> = {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: Mutex<Vec<Result<Option<Value>, String>>> = Mutex::new(vec![Ok(None); slugs.len()]);
        std::thread::scope(|s| {
            for _ in 0..8.min(slugs.len()) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Some(sl) = slugs.get(i) else { break };
                    let rel = latest_release(&format!("{ARTCRAFT_ORG}/{sl}"));
                    if let Ok(mut r) = results.lock() {
                        r[i] = rel;
                    }
                });
            }
        });
        // `list(pool.map(...))`: the first lookup, in order, that raised.
        results.into_inner().unwrap_or_else(|e| e.into_inner()).into_iter().collect::<Result<_, _>>()?
    };

    let empty = Row::new();
    let mut rows = Vec::new();
    for (i, slug) in slugs.iter().enumerate() {
        let label = names.iter().find(|(s, _)| s == slug).map(|(_, l)| l.clone()).unwrap_or_default();
        let repo = repos.get(slug).unwrap_or(&empty);
        let (mut tag, mut published, mut url, mut asset) = (json!(""), json!(""), json!(""), json!(""));
        let mut sha256 = json!("");
        let mut size = json!(0);
        let rel = releases[i].clone().filter(|r| truthy(Some(r))).unwrap_or_else(|| json!({}));
        // A release with no tag is a lookup that failed (rate limit, network),
        // not an answer. "No Linux build" is only said when GitHub said so.
        let answered = rel.as_object().is_some_and(|r| truthy(r.get("tag_name")));
        if answered {
            let rel = as_dict(&rel)?;
            let t = or_(rel, "tag_name", json!(""));
            tag = json!(as_str(&t, "lstrip")?.trim_start_matches('v'));
            let p = or_(rel, "published_at", json!(""));
            published = py_slice(&p, 19)?;
            let assets = rel.get("assets").cloned().unwrap_or_else(|| json!([]));
            for a in py_iter(&assets)? {
                let a = as_dict(&a)?;
                let nm = a.get("name").cloned().unwrap_or_else(|| json!(""));
                if as_str(&nm, "endswith")?.ends_with("-linux-x86_64.AppImage") {
                    url = a.get("browser_download_url").cloned().unwrap_or_else(|| json!(""));
                    asset = nm;
                    sha256 = json!(crate::appimage::sha256_of_digest(a.get("digest")));
                    let bytes = or_(a, "size", json!(0));
                    // True / 1048576 is a number in Python, as bools are ints.
                    let bytes = match bytes {
                        Value::Bool(b) => json!(b as i64),
                        other => other,
                    };
                    let Some(bytes) = bytes.as_f64() else {
                        return Err(format!("TypeError: unsupported operand type(s) for /: '{}' and 'int'",
                                           py_type(&bytes)));
                    };
                    size = json!(crate::util::py_round_to(bytes / 1048576.0, 1));
                    break;
                }
            }
        }

        if !truthy(Some(&url)) {
            // Fall back to what was known before, so a rate-limited refresh
            // degrades to stale rather than to empty.
            let old = previous.get(slug).cloned().filter(|o| truthy(Some(o))).unwrap_or_else(|| json!({}));
            let old = as_dict(&old)?;
            let get = |k: &str, d: Value| old.get(k).cloned().unwrap_or(d);
            tag = or_v(tag, get("version", json!("")));
            url = get("download_url", json!(""));
            asset = get("asset_name", json!(""));
            size = get("size_mb", json!(0));
            sha256 = get("sha256", json!(""));
            published = or_v(published, get("added", json!("")));
        }
        let has_url = truthy(Some(&url));
        let unchecked = !has_url && !answered;

        let lic = or_(as_dict(&or_(repo, "license", json!({})))?, "spdx_id", json!(""));
        let mut r = Row::new();
        r.insert("id".into(), json!(slug));
        r.insert("name".into(), json!(label));
        r.insert("description".into(), or_(repo, "description", json!("")));
        r.insert("author".into(), json!("ArtCraft"));
        r.insert("version".into(), tag);
        r.insert("category".into(), json!(_ARTCRAFT_CATEGORY.iter()
            .find(|(k, _)| k == slug).map(|(_, v)| *v).unwrap_or("Graphics")));
        r.insert("kind".into(), json!("Desktop app"));
        r.insert("status".into(), json!(if has_url { "Available" }
                                        else if unchecked { "Not checked" } else { "No Linux build" }));
        r.insert("tags".into(), json!(["appimage", "desktop"]));
        r.insert("accent".into(), json!(_ACCENT_CYCLE[i % _ACCENT_CYCLE.len()]));
        r.insert("initials".into(), json!(pfx(&label, 2).to_uppercase()));
        r.insert("repo".into(), or_(repo, "html_url", json!(format!("https://github.com/{ARTCRAFT_ORG}/{slug}"))));
        r.insert("stars".into(), py_int(&or_(repo, "stargazers_count", json!(0)))?);
        r.insert("verified".into(), json!(has_url));
        r.insert("installable".into(), json!(has_url));
        // handled in-app, not by a shell command
        r.insert("install_cmd".into(), json!(""));
        r.insert("install_note".into(), json!(if has_url { "" } else if unchecked {
            "GitHub did not answer when this was listed; it is asked again on the next refresh."
        } else {
            "No x86_64 AppImage in the latest release."
        }));
        r.insert("unchecked".into(), json!(unchecked));
        r.insert("license".into(), lic);
        r.insert("updated".into(), or_(repo, "pushed_at", json!("")));
        r.insert("added".into(), published.clone());
        r.insert("thumb".into(), json!(format!("images/apps/{slug}/hero.webp")));
        r.insert("preview".into(), json!(format!("images/apps/{slug}/hero.webp")));
        r.insert("built_in".into(), json!(false));
        r.insert("install_kind".into(), json!("appimage"));
        r.insert("download_url".into(), url);
        r.insert("asset_name".into(), asset);
        r.insert("size_mb".into(), size);
        r.insert("sha256".into(), sha256);
        r.insert("source_type".into(), json!("vendor"));
        r.insert("source_url".into(), json!(format!("{ARTCRAFT_SITE}apps/{slug}")));
        r.insert("upstream_status".into(), json!(if has_url { "passed" } else { "" }));
        r.insert("upstream_checked_at".into(), published);
        rows.push(r);
    }
    Ok(rows)
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
///
/// `Err` where `settings.get` raises (the Python lets it out).
pub fn try_enabled_markets() -> Result<Vec<&'static Market>, String> {
    let mut out = Vec::new();
    for m in MARKETS.iter() {
        let key = format!("net_market_{}", m.id);
        if m.enabled && crate::settings::default_for(&key).is_some() && truthy(Some(&crate::settings::get(&key)?)) {
            out.push(m);
        }
    }
    Ok(out)
}

/// `try_enabled_markets`, for callers that cannot fail: where the Python
/// raises, no marketplace is loaded.
pub fn enabled_markets() -> Vec<&'static Market> {
    try_enabled_markets().unwrap_or_default()
}

// ---- loading -----------------------------------------------------------------

/// `_cached`, behind `_lock`.
static CACHED: Mutex<Option<Vec<Plugin>>> = Mutex::new(None);

/// A catalogue with rows that could not be checked is retried this soon rather
/// than kept for the full TTL: one rate-limited refresh otherwise reported four
/// installable apps as having no Linux build for six hours.
pub const RETRY: f64 = 10.0 * 60.0;

/// The file's JSON, or Err for OSError / ValueError.
fn read_blob(path: &Path) -> Result<Value, ()> {
    let text = std::fs::read_to_string(path).map_err(|_| ())?;
    py_json_loads(&text).map_err(|_| ())
}

/// One market's rows: from its cache while fresh, otherwise fetched and
/// cached; on failure, whatever the (expired) cache still holds, or nothing.
///
/// Returns exactly what the Python returns -- normally a list of row dicts,
/// but a hand-mangled cache file comes back as whatever it holds. `Err`
/// where the Python lets an exception out (rows that are not dicts, while
/// checking a fresh cache for unchecked rows).
pub fn _load_market(m: &Market, force: bool) -> Result<Value, String> {
    let path = m.cache();
    let mut rows: Option<Value> = None;
    let age = match std::fs::metadata(&path).and_then(|md| md.modified()) {
        Ok(mtime) => {
            let mtime = mtime.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            crate::util::now() - mtime
        }
        Err(_) => TTL,
    };
    if age < TTL && !force {
        if let Ok(Value::Object(blob)) = read_blob(&path) {
            if blob.get("schema").and_then(Value::as_f64) == Some(SCHEMA as f64)
                && !matches!(blob.get("schema"), Some(Value::Bool(_)))
            {
                rows = blob.get("rows").cloned().filter(|v| !v.is_null());
                if age >= RETRY {
                    let scan = rows.clone().unwrap_or(Value::Null);
                    let scan = if truthy(Some(&scan)) { py_iter(&scan)? } else { Vec::new() };
                    for r in scan {
                        if truthy(as_dict(&r)?.get("unchecked")) {
                            rows = None;
                            break;
                        }
                    }
                }
            }
        }
    }
    if let Some(rows) = rows {
        return Ok(rows);
    }

    let fetched = m.fetch().and_then(|rows| {
        let rows = Value::Array(rows.into_iter().map(Value::Object).collect());
        let blob = json!({"schema": SCHEMA, "rows": rows});
        crate::util::write_json(&path, &blob, None, false).map_err(|e| format!("OSError: {e}"))?;
        Ok(rows)
    });
    match fetched {
        Ok(rows) => Ok(rows),
        Err(_) => {
            // Offline, or the site moved. An expired cache still fills the store;
            // an empty one makes the app look broken. One market being down must
            // not take the others with it.
            let rows = match read_blob(&path) {
                Ok(Value::Object(blob)) => blob.get("rows").cloned().unwrap_or(Value::Null),
                Ok(other) => other,
                Err(()) => Value::Null,
            };
            Ok(or_v(rows, json!([])))
        }
    }
}

/// Every listed plugin from every enabled marketplace.
///
/// Memoised process-wide like the Python's `_cached` (behind a lock); returns
/// a copy, and `annotate` carries its marks back to the cached entries, as
/// marking the Python's shared objects does.
///
/// `Err` where the Python raises out of `catalog()` -- a market whose load
/// raises, rows that cannot be iterated, a row that is not a mapping or that
/// `Plugin(**r)` rejects -- and then nothing is cached, so the next call
/// tries again.
pub fn try_catalog(force: bool) -> Result<Vec<Plugin>, String> {
    let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = cached.as_ref() {
        if !force {
            return Ok(c.clone());
        }
    }
    let mut out: Vec<Plugin> = Vec::new();
    for m in try_enabled_markets()? {
        let rows = _load_market(m, force)?;
        for r in py_iter(&rows)? {
            let Value::Object(r) = r else {
                return Err(format!("TypeError: store.Plugin() argument after ** must be a mapping, not {}",
                                   py_type(&r)));
            };
            out.push(Plugin::from_row(&r)?);
        }
    }
    *cached = Some(out.clone());
    Ok(out)
}

/// `try_catalog`, for callers that cannot fail: where the Python raises,
/// the store is empty (and nothing is cached).
#[deprecated(note = "the Python raises out of catalog() and the store page shows _store_failed(str(exc)); \
                     use try_catalog and report its Err rather than showing an empty store")]
pub fn catalog(force: bool) -> Vec<Plugin> {
    try_catalog(force).unwrap_or_default()
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=secs).stdout`,
/// or None where that raises: the command cannot start, times out, or
/// writes something that is not UTF-8 (text=True decodes strictly, where
/// `util::run` replaces bad bytes). Universal newlines as text mode has them.
fn run_text(argv: &[&str], timeout_secs: u64) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let (prog, args) = argv.split_first()?;
    let mut child = crate::util::spawn_group(
        Command::new(prog).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()),
    )
    .ok()?;
    // The pipes are read on their own threads, and the deadline covers the
    // reading too, as `communicate(timeout=...)` does: a grandchild that
    // keeps stdout open past it means a timeout, not a wait for it to close.
    // A timeout kills the child's whole process group, so the readers end.
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut b);
            }
            let _ = tx.send(b);
        });
        rx
    };
    let out_rx = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err_rx = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = std::time::Instant::now();
    let deadline = start + std::time::Duration::from_secs(timeout_secs);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() >= deadline => {
                crate::util::kill_group(&mut child);
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
            Err(_) => return None,
        }
    }
    let left = || deadline.saturating_duration_since(std::time::Instant::now());
    let (Ok(out), Ok(err)) = (out_rx.recv_timeout(left()), err_rx.recv_timeout(left())) else {
        crate::util::kill_group(&mut child);
        return None;
    };
    let out = String::from_utf8(out).ok()?;
    String::from_utf8(err).ok()?;
    Some(out.replace("\r\n", "\n").replace('\r', "\n"))
}

/// id -> enabled, for everything the shell already knows about.
///
/// Keys are `str(id)`. `Err` where the Python raises past its `try`: output
/// that parses but is not iterable (`null`, `5`), or an unhashable id. Ids
/// Python counts as one key (`1` and `true`) are one key here too, named
/// by the first and valued by the last.
pub fn try_installed_ids() -> Result<BTreeMap<String, bool>, String> {
    Ok(installed_id_keys()?.into_iter().map(|(_, id, on)| (py_str(&id), on)).collect())
}

/// The Python's `installed_ids()` dict as (key identity, id as given,
/// enabled), in insertion order. The id keeps its JSON type: an int id is
/// not the str id of the same digits, so `p.id in have` never matches it.
fn installed_id_keys() -> Result<Vec<(String, Value, bool)>, String> {
    let Some(out) = run_text(&["omarchy", "plugin", "list", "--json"], 25) else {
        return Ok(Vec::new());
    };
    let Ok(items) = py_json_loads(&out) else {
        return Ok(Vec::new());
    };
    let mut keys: Vec<(String, Value, bool)> = Vec::new();
    for i in py_iter(&items)? {
        if let Value::Object(i) = i {
            if let Some(id) = i.get("id").filter(|v| truthy(Some(v))) {
                let key = py_hash_key(id)
                    .ok_or_else(|| format!("TypeError: cannot use '{t}' as a dict key (unhashable type: '{t}')",
                                           t = py_type(id)))?;
                let on = truthy(i.get("enabled"));
                match keys.iter_mut().find(|(k, _, _)| *k == key) {
                    Some(slot) => slot.2 = on,
                    None => keys.push((key, id.clone(), on)),
                }
            }
        }
    }
    Ok(keys)
}

/// `try_installed_ids`, for callers that cannot fail: nothing installed
/// where the Python raises.
pub fn installed_ids() -> BTreeMap<String, bool> {
    try_installed_ids().unwrap_or_default()
}

/// registry key -> installed version, for the AppImages this app tracks.
///
/// The version is whatever the registry holds when it is truthy (the Python
/// does not make it a str), else "".
pub fn installed_appimages() -> BTreeMap<String, Value> {
    // `(v.get("version") or "")` on each record; a record that is not a
    // dict raised in the Python and was caught as "nothing installed".
    let mut out = BTreeMap::new();
    for (k, v) in crate::inventory::load_registry() {
        let Value::Object(v) = v else {
            return BTreeMap::new();
        };
        out.insert(k, or_(&v, "version", json!("")));
    }
    out
}

/// Mark what is already on the system, so cards can say so.
///
/// `Err` where the Python raises: `installed_ids` past its `try`, before
/// anything is marked, or a plugin whose id or name cannot be looked up, with
/// the plugins before it marked.
///
/// The Python's `catalog()` hands out the cached `Plugin` objects
/// themselves, so marking them marks the cache every later `catalog()`
/// caller sees. Here `catalog()` hands out copies, so the marks are carried
/// back to the cached entries these plugins were copied from.
pub fn try_annotate(plugins: &mut [Plugin]) -> Result<(), String> {
    let mut done = 0;
    let r = annotate_marks(plugins, &mut done);
    sync_cache(&plugins[..done]);
    r
}

/// The body of `try_annotate`; `done` counts the plugins it got through.
fn annotate_marks(plugins: &mut [Plugin], done: &mut usize) -> Result<(), String> {
    if crate::demo::active() {
        // The catalogue stays real -- it is public data -- but what counts as
        // installed comes from the fixtures, so a screenshot never discloses
        // what is actually on the machine taking it.
        for p in plugins.iter_mut() {
            // `p.id in {...}` (a set: a list id cannot be looked up), then
            // `p.name.lower()`.
            let id = p.pv("id");
            let listed = match &id {
                Value::String(s) => crate::demo::INSTALLED_PLUGIN_IDS.contains(&s.as_str()),
                Value::Array(_) | Value::Object(_) => return Err(unhashable(&id)),
                _ => false,
            };
            p.installed = listed
                || (p.install_kind == "theme"
                    && crate::demo::INSTALLED_THEMES.contains(&p.ps("name", "lower")?.to_lowercase().as_str()));
            p.enabled = p.installed;
            p.local_version = if p.installed { p.version.clone() } else { String::new() };
            // p.version, as it is: it may be a raw value.
            match p.raw.get("version").cloned() {
                Some(v) if p.installed => {
                    p.raw.insert("local_version".into(), v);
                }
                _ => p.clear_raw("local_version"),
            }
            p.clear_raw("installed");
            p.clear_raw("enabled");
            if p.install_kind == "theme" {
                p.built_in = false;
                p.clear_raw("built_in");
            }
            *done += 1;
        }
        return Ok(());
    }

    let have = installed_id_keys()?;
    let images = installed_appimages();
    let has_themes = plugins.iter().any(|p| p.install_kind == "theme");
    let themes = if has_themes { installed_themes() } else { BTreeSet::new() };
    let mine = if has_themes { removable_themes() } else { BTreeSet::new() };
    for p in plugins.iter_mut() {
        if p.install_kind == "theme" {
            let name = p.ps("name", "lower")?.to_lowercase();
            p.installed = themes.contains(&name);
            p.enabled = p.installed;
            // A stock theme is "built in" in exactly the sense a built-in
            // plugin is: it is there, it works, and Remove does not apply.
            p.built_in = p.installed && !mine.contains(&name);
            p.clear_raw("built_in");
        } else if p.install_kind == "appimage" {
            // The registry's keys are str: an id of any other type is in it
            // only if it cannot be looked up at all (and then raises).
            let id = p.pv("id");
            let found = match &id {
                Value::String(s) => images.get(s),
                Value::Array(_) | Value::Object(_) => return Err(unhashable(&id)),
                _ => None,
            };
            p.installed = found.is_some();
            p.enabled = p.installed;
            match found {
                Some(Value::String(v)) => {
                    p.local_version = v.clone();
                    p.clear_raw("local_version");
                }
                Some(v) => {
                    p.local_version = py_str(v);
                    p.raw.insert("local_version".into(), v.clone());
                }
                None => {
                    p.local_version = String::new();
                    p.clear_raw("local_version");
                }
            }
        } else {
            let id = p.pv("id");
            let key = py_hash_key(&id).ok_or_else(|| unhashable(&id))?;
            let on = have.iter().find(|(k, _, _)| *k == key).map(|(_, _, on)| *on);
            p.installed = on.is_some();
            p.enabled = on.unwrap_or(false);
        }
        p.clear_raw("installed");
        p.clear_raw("enabled");
        *done += 1;
    }
    Ok(())
}

/// The fields `annotate` sets, which say nothing about which listing a
/// plugin is.
const MARKS: &[&str] = &["installed", "enabled", "local_version", "built_in"];

/// `p` with its marks cleared, to compare listings by.
fn unmarked(p: &Plugin) -> Plugin {
    let mut q = Plugin { installed: false, enabled: false, local_version: String::new(), ..p.clone() };
    if q.install_kind == "theme" {
        q.built_in = false;
    }
    for m in MARKS {
        q.raw.remove(*m);
    }
    q
}

/// Carry the marks of freshly annotated plugins back to the cached entries
/// they are copies of (same listing, whatever it was marked before).
fn sync_cache(plugins: &[Plugin]) {
    if plugins.is_empty() {
        return;
    }
    let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
    let Some(cache) = cached.as_mut() else { return };
    let mut by_id: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, c) in cache.iter().enumerate() {
        by_id.entry((c.market.as_str(), c.id.as_str())).or_default().push(i);
    }
    let mut hits: Vec<(usize, &Plugin)> = Vec::new();
    for p in plugins {
        let Some(candidates) = by_id.get(&(p.market.as_str(), p.id.as_str())) else { continue };
        let me = unmarked(p);
        for &i in candidates {
            if unmarked(&cache[i]) == me {
                hits.push((i, p));
            }
        }
    }
    for (i, p) in hits {
        let c = &mut cache[i];
        c.installed = p.installed;
        c.enabled = p.enabled;
        c.local_version = p.local_version.clone();
        c.built_in = p.built_in;
        for m in MARKS {
            match p.raw.get(*m) {
                Some(v) => {
                    c.raw.insert((*m).to_string(), v.clone());
                }
                None => {
                    c.raw.remove(*m);
                }
            }
        }
    }
}

/// `try_annotate`, for callers that cannot fail: where the Python raises,
/// the plugins are left as they were.
pub fn annotate(plugins: &mut [Plugin]) {
    let _ = try_annotate(plugins);
}

/// `plugins = catalog(force); annotate(plugins)`, as the app's workers write
/// it: the Python's `catalog()` hands out the cached list itself, so
/// `annotate` marks the very objects every later `catalog()` caller gets.
/// This marks the cached entries in place (under the cache's lock) and
/// returns a copy of them as marked.
///
/// `Err` where either step raises in the Python; marks made before an
/// `annotate` failure stay on the cache, as they do on the Python's objects.
pub fn try_catalog_annotated(force: bool) -> Result<Vec<Plugin>, String> {
    let fresh = try_catalog(force)?;
    let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
    let Some(cache) = cached.as_mut() else {
        // Only a test clearing the cache in between gets here: annotate the
        // list catalog() returned, as the Python would.
        drop(cached);
        let mut plugins = fresh;
        try_annotate(&mut plugins)?;
        return Ok(plugins);
    };
    let mut done = 0;
    annotate_marks(cache, &mut done)?;
    Ok(cache.clone())
}

// ---- sorting and filtering ---------------------------------------------------

/// A sort's comparator: the Python's sort-key lambda, as an ordering.
pub type SortFn = fn(&Plugin, &Plugin) -> Ordering;

/// ISO-8601 to a sortable number. Unknown dates sort oldest.
pub fn _ts(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    // datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp(),
    // except ValueError: 0.0
    _fromisoformat_ts(&value.replace('Z', "+00:00")).unwrap_or(0.0)
}

// `util::iso_ts` covers the common shapes only; `_ts` orders by whatever the
// marketplaces publish, so it follows Python 3.14's `datetime.fromisoformat`
// grammar in full (basic and week dates, `HHMM`, any separator character,
// `24:00`, `+HH` offsets), with the C implementation's strict digits.

/// `int(s)` for exactly `n` ASCII digits.
fn iso_digits(s: &[char], n: usize) -> Option<i64> {
    if s.len() != n || n == 0 || !s.iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(s.iter().fold(0i64, |a, c| a * 10 + i64::from(*c as u8 - b'0')))
}

/// `_find_isoformat_datetime_separator`.
fn iso_separator(s: &[char]) -> Option<usize> {
    let len = s.len();
    if len == 7 {
        return Some(7);
    }
    if len < 7 {
        return None;
    }
    let at = |i: usize| s.get(i).copied().unwrap_or('\0');
    if at(4) == '-' {
        if at(5) == 'W' {
            if len > 8 && at(8) == '-' {
                if len == 9 {
                    return None;
                }
                if len > 10 && at(10).is_ascii_digit() {
                    return Some(8);
                }
                return Some(10);
            }
            return Some(8);
        }
        return Some(10);
    }
    if at(4) == 'W' {
        let mut idx = 7;
        while idx < len && at(idx).is_ascii_digit() {
            idx += 1;
        }
        if idx < 9 {
            return Some(idx);
        }
        return Some(if idx % 2 == 0 { 7 } else { 8 });
    }
    Some(8)
}

/// `_parse_isoformat_date`.
fn iso_date(d: &[char]) -> Option<chrono::NaiveDate> {
    if ![7, 8, 10].contains(&d.len()) {
        return None;
    }
    let year = iso_digits(&d[0..4], 4)? as i32;
    if !(1..=9999).contains(&year) {
        return None;
    }
    let has_sep = d[4] == '-';
    let mut pos = 4 + usize::from(has_sep);
    let slice = |a: usize, b: usize| &d[a.min(d.len())..b.min(d.len())];
    if slice(pos, pos + 1) == ['W'] {
        pos += 1;
        let week = iso_digits(slice(pos, pos + 2), 2)? as u32;
        pos += 2;
        let mut day = 1;
        if d.len() > pos {
            if (slice(pos, pos + 1) == ['-']) != has_sep {
                return None;
            }
            pos += usize::from(has_sep);
            day = iso_digits(slice(pos, pos + 1), 1)? as u32;
        }
        let wd = match day {
            1 => chrono::Weekday::Mon,
            2 => chrono::Weekday::Tue,
            3 => chrono::Weekday::Wed,
            4 => chrono::Weekday::Thu,
            5 => chrono::Weekday::Fri,
            6 => chrono::Weekday::Sat,
            7 => chrono::Weekday::Sun,
            _ => return None,
        };
        return chrono::NaiveDate::from_isoywd_opt(year, week, wd);
    }
    let month = iso_digits(slice(pos, pos + 2), 2)? as u32;
    pos += 2;
    if (slice(pos, pos + 1) == ['-']) != has_sep {
        return None;
    }
    pos += usize::from(has_sep);
    let day = iso_digits(slice(pos, pos + 2), 2)? as u32;
    chrono::NaiveDate::from_ymd_opt(year, month, day)
}

/// `_parse_hh_mm_ss_ff`: [hour, minute, second, microsecond].
fn iso_hms(t: &[char]) -> Option<[i64; 4]> {
    let mut comps = [0i64; 4];
    let mut pos = 0;
    let mut has_sep = false;
    for comp in 0..3 {
        if t.len() < pos + 2 {
            return None;
        }
        comps[comp] = iso_digits(&t[pos..pos + 2], 2)?;
        pos += 2;
        let next = t.get(pos).copied();
        if comp == 0 {
            has_sep = next == Some(':');
        }
        if next.is_none() || comp >= 2 {
            break;
        }
        if has_sep && next != Some(':') {
            return None;
        }
        pos += usize::from(has_sep);
    }
    if pos < t.len() {
        if t[pos] != '.' && t[pos] != ',' {
            return None;
        }
        pos += 1;
        let rest = &t[pos..];
        if !rest.iter().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n = rest.len().min(6);
        comps[3] = iso_digits(&rest[..n], n)? * 10i64.pow(6 - n as u32);
    }
    Some(comps)
}

/// `datetime.fromisoformat(s).timestamp()`, or None for a ValueError.
fn _fromisoformat_ts(s: &str) -> Option<f64> {
    let c: Vec<char> = s.chars().collect();
    if c.len() < 7 {
        return None;
    }
    let sep = iso_separator(&c)?;
    let date = iso_date(&c[..sep.min(c.len())])?;
    let tstr: &[char] = if c.len() > sep + 1 { &c[sep + 1..] } else { &[] };
    if c.len() > sep && tstr.is_empty() {
        // The C parser rejects a separator with nothing after it.
        return None;
    }
    let (mut h, mut mi, mut se, mut us, mut tz) = (0, 0, 0, 0, None::<i64>);
    let mut next_day = false;
    if !tstr.is_empty() {
        if tstr.len() < 2 {
            return None;
        }
        let find = |ch: char| tstr.iter().position(|x| *x == ch).map(|i| i + 1);
        let tz_pos = find('-').or_else(|| find('+')).or_else(|| find('Z')).unwrap_or(0);
        let timestr = if tz_pos > 0 { &tstr[..tz_pos - 1] } else { tstr };
        let comps = iso_hms(timestr)?;
        [h, mi, se, us] = comps;
        if h == 24 {
            if comps[1..].iter().all(|x| *x == 0) {
                h = 0;
                next_day = true;
            } else {
                return None;
            }
        }
        if tz_pos == tstr.len() && tstr[tstr.len() - 1] == 'Z' {
            tz = Some(0);
        } else if tz_pos > 0 {
            let tzstr = &tstr[tz_pos..];
            if [0, 1, 3].contains(&tzstr.len()) || tstr[tz_pos - 1] == 'Z' {
                return None;
            }
            let t = iso_hms(tzstr)?;
            let sign = if tstr[tz_pos - 1] == '-' { -1 } else { 1 };
            let off_us = sign * (((t[0] * 60 + t[1]) * 60 + t[2]) * 1_000_000 + t[3]);
            if off_us.abs() >= 86_400_000_000 {
                return None;
            }
            tz = Some(off_us);
        }
    }
    if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0..60).contains(&se) {
        return None;
    }
    let mut date = date;
    if next_day {
        date = date.succ_opt().filter(|d| chrono::Datelike::year(d) <= 9999)?;
    }
    let naive = date.and_hms_opt(h as u32, mi as u32, se as u32)?;
    match tz {
        Some(off_us) => {
            let total = naive.and_utc().timestamp() * 1_000_000 + us - off_us;
            Some(total as f64 / 1e6)
        }
        None => {
            // A naive time is local time, as Python takes it: CPython's
            // `local_to_seconds` with fold=0, including its ValueError when a
            // local time it probes falls outside years 1..9999.
            let secs = local_to_seconds(naive.and_utc().timestamp())?;
            Some(secs as f64 + us as f64 / 1e6)
        }
    }
}

/// CPython's `local(u)`: the local wall time of the instant `u`, read as if
/// it were UTC. None where `utc_to_seconds` rejects the year.
fn py_local(u: i64) -> Option<i64> {
    use chrono::TimeZone;
    let dt = chrono::Local.timestamp_opt(u, 0).single()?;
    let wall = dt.naive_local();
    if !(1..=9999).contains(&chrono::Datelike::year(&wall)) {
        return None;
    }
    Some(wall.and_utc().timestamp())
}

/// CPython's `local_to_seconds(..., fold=0)`, on Unix seconds: solve
/// `t = local(u)` for `u`, preferring the earlier of two solutions, and the
/// later candidate inside a gap.
fn local_to_seconds(t: i64) -> Option<i64> {
    const MAX_FOLD_SECONDS: i64 = 24 * 3600;
    let a = py_local(t)? - t;
    let u1 = t - a;
    let t1 = py_local(u1)?;
    let b;
    if t1 == t {
        // We found one solution, but it may not be the one we need.
        // Look for an earlier solution (fold is 0).
        let u2 = u1 - MAX_FOLD_SECONDS;
        b = py_local(u2)? - u2;
        if a == b {
            return Some(u1);
        }
    } else {
        b = t1 - u1;
    }
    let u2 = t - b;
    let t2 = py_local(u2)?;
    if t2 == t {
        return Some(u2);
    }
    if t1 == t {
        return Some(u1);
    }
    // We have found both offsets a and b, but neither t - a nor t - b is
    // a solution. This means t is in the gap.
    Some(u1.max(u2))
}

// The Python's `_Rev(str)` -- "reverse string ordering without a second sort
// pass" -- is just a reversed comparison here (`sort_za`).

/// `-x` as a sort key, for floats.
fn desc_f(a: f64, b: f64) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

fn sort_popular(a: &Plugin, b: &Plugin) -> Ordering {
    b.stars.cmp(&a.stars).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
}
fn sort_trending(a: &Plugin, b: &Plugin) -> Ordering {
    desc_f(_ts(&a.updated), _ts(&b.updated)).then_with(|| b.stars.cmp(&a.stars))
}
fn sort_new(a: &Plugin, b: &Plugin) -> Ordering {
    desc_f(_ts(&a.added), _ts(&b.added)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
}
fn sort_az(a: &Plugin, b: &Plugin) -> Ordering {
    a.name.to_lowercase().cmp(&b.name.to_lowercase())
}
fn sort_za(a: &Plugin, b: &Plugin) -> Ordering {
    b.name.to_lowercase().cmp(&a.name.to_lowercase())
}
fn sort_category(a: &Plugin, b: &Plugin) -> Ordering {
    a.category.to_lowercase().cmp(&b.category.to_lowercase()).then_with(|| b.stars.cmp(&a.stars))
}
fn sort_provenance(a: &Plugin, b: &Plugin) -> Ordering {
    a.trust().rank.cmp(&b.trust().rank)
        .then_with(|| a.market_name().to_lowercase().cmp(&b.market_name().to_lowercase()))
        .then_with(|| a.owner().to_lowercase().cmp(&b.owner().to_lowercase()))
        .then_with(|| b.stars.cmp(&a.stars))
}
fn sort_market(a: &Plugin, b: &Plugin) -> Ordering {
    a.market_name().to_lowercase().cmp(&b.market_name().to_lowercase())
        .then_with(|| a.trust().rank.cmp(&b.trust().rank))
        .then_with(|| b.stars.cmp(&a.stars))
}
fn sort_publisher(a: &Plugin, b: &Plugin) -> Ordering {
    a.owner().to_lowercase().cmp(&b.owner().to_lowercase()).then_with(|| b.stars.cmp(&a.stars))
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

/// `_SORT_FN.get(key)`.
fn sort_fn(key: &str) -> Option<SortFn> {
    SORTS.iter().find(|(k, _, _)| *k == key).map(|(_, _, f)| *f)
}

/// (category, count), busiest first, then by name.
pub fn categories(plugins: &[Plugin]) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for p in plugins {
        match counts.iter_mut().find(|(c, _)| *c == p.category) {
            Some(e) => e.1 += 1,
            None => counts.push((p.category.clone(), 1)),
        }
    }
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// (market id, name, count), busiest first.
pub fn market_counts(plugins: &[Plugin]) -> Vec<(String, String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for p in plugins {
        match counts.iter_mut().find(|(c, _)| *c == p.market) {
            Some(e) => e.1 += 1,
            None => counts.push((p.market.clone(), 1)),
        }
    }
    let mut rows: Vec<(String, String, usize)> = counts
        .into_iter()
        .map(|(mid, n)| {
            let name = market(&mid).map(|m| m.name.clone()).unwrap_or_else(|| mid.clone());
            (mid, name, n)
        })
        .collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    rows
}

/// (trust key, label, count) in provenance order.
pub fn trust_counts(plugins: &[Plugin]) -> Vec<(String, String, usize)> {
    try_trust_counts(&typed(plugins)).unwrap_or_default()
}

/// `trust_counts` on the values as the Python holds them: `Err` where a
/// plugin's `trust` raises.
pub fn try_trust_counts(plugins: &[Plugin]) -> Result<Vec<(String, String, usize)>, String> {
    let mut counts: HashMap<String, (String, usize)> = HashMap::new();
    for p in plugins {
        let t = p.try_trust()?;
        counts.entry(t.key).or_insert((t.label, 0)).1 += 1;
    }
    Ok(TRUST_ORDER
        .iter()
        .filter_map(|k| counts.get(*k).map(|(label, n)| (k.to_string(), label.clone(), *n)))
        .collect())
}

/// The plugins with their raw values dropped: what the typed fields say.
fn typed(plugins: &[Plugin]) -> Vec<Plugin> {
    plugins.iter().map(|p| Plugin { raw: BTreeMap::new(), ..p.clone() }).collect()
}

/// Filter and sort the store's plugins. Python defaults: `query=""`,
/// `category=""`, `sort="popular"`, `installed_only=false`,
/// `verified_only=false`, `market=""`, `trust=""`.
///
/// Where the Python raises (a cached value of the wrong type that a filter
/// or sort key trips over; see `try_browse`) the page still has to show
/// something, so this filters and sorts on the typed fields instead.
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
    let idx = browse_idx(plugins, query, category, sort, installed_only, verified_only, market, trust)
        .or_else(|_| {
            browse_idx(&typed(plugins), query, category, sort, installed_only, verified_only, market, trust)
        })
        .unwrap_or_default();
    idx.into_iter().map(|i| plugins[i].clone()).collect()
}

/// `browse`, exactly: `Err` where the Python raises.
#[allow(clippy::too_many_arguments)]
pub fn try_browse(
    plugins: &[Plugin],
    query: &str,
    category: &str,
    sort: &str,
    installed_only: bool,
    verified_only: bool,
    market: &str,
    trust: &str,
) -> Result<Vec<Plugin>, String> {
    let idx = browse_idx(plugins, query, category, sort, installed_only, verified_only, market, trust)?;
    Ok(idx.into_iter().map(|i| plugins[i].clone()).collect())
}

/// `try_browse` over borrowed plugins, answering with indices into
/// `plugins`: the store page searches on every keystroke, and copying its
/// 5,600 entries to do so cost more than the search.
#[allow(clippy::too_many_arguments)]
pub fn try_browse_refs(
    plugins: &[&Plugin],
    query: &str,
    category: &str,
    sort: &str,
    installed_only: bool,
    verified_only: bool,
    market: &str,
    trust: &str,
) -> Result<Vec<usize>, String> {
    browse_idx(plugins, query, category, sort, installed_only, verified_only, market, trust)
}

/// The body of `browse`, as indices into `plugins`.
#[allow(clippy::too_many_arguments)]
fn browse_idx<P: std::borrow::Borrow<Plugin>>(
    plugins: &[P],
    query: &str,
    category: &str,
    sort: &str,
    installed_only: bool,
    verified_only: bool,
    market: &str,
    trust: &str,
) -> Result<Vec<usize>, String> {
    let q = py_strip(query).to_lowercase();
    let mut out: Vec<usize> = (0..plugins.len()).collect();
    // Each filter is a list comprehension over what the last one kept.
    let keep = |out: Vec<usize>, f: &dyn Fn(&Plugin) -> Result<bool, String>| -> Result<Vec<usize>, String> {
        let mut kept = Vec::new();
        for i in out {
            if f(plugins[i].borrow())? {
                kept.push(i);
            }
        }
        Ok(kept)
    };
    if !market.is_empty() {
        out = keep(out, &|p| Ok(p.pv("market").as_str() == Some(market)))?;
    }
    if !trust.is_empty() {
        out = keep(out, &|p| Ok(p.try_trust()?.key == trust))?;
    }
    if !category.is_empty() {
        out = keep(out, &|p| Ok(p.pv("category").as_str() == Some(category)))?;
    }
    if verified_only {
        // "Trustworthy" here means a review of some kind exists and still
        // applies to what you would install -- not merely that a tick was set.
        out = keep(out, &|p| Ok(matches!(p.try_trust()?.key.as_str(), "official" | "reviewed" | "verified")))?;
    }
    if installed_only {
        out = keep(out, &|p| Ok(truthy(Some(&p.pv("installed")))))?;
    }

    let known = sort_fn(sort).is_some();
    let sorted = |bucket: Vec<usize>| -> Result<Vec<usize>, String> {
        if !known {
            return Ok(bucket);
        }
        let items: Vec<&Plugin> = bucket.iter().map(|i| plugins[*i].borrow()).collect();
        Ok(try_sorted(&items, sort)?.into_iter().map(|j| bucket[j]).collect())
    };
    if !q.is_empty() {
        // Rank name matches above description matches: searching "clock" should
        // not bury the plugin called Clock under everything that mentions one.
        let (mut exact, mut starts, mut named, mut loose) = (vec![], vec![], vec![], vec![]);
        for i in out {
            let p: &Plugin = plugins[i].borrow();
            let name = p.ps("name", "lower")?.to_lowercase();
            if name == q || p.ps("id", "lower")?.to_lowercase() == q {
                exact.push(i);
            } else if name.starts_with(&q) {
                starts.push(i);
            } else if name.contains(&q) {
                named.push(i);
            } else if p.try_haystack()?.contains(&q) {
                loose.push(i);
            }
        }
        let mut all = sorted(exact)?;
        all.extend(sorted(starts)?);
        all.extend(sorted(named)?);
        all.extend(sorted(loose)?);
        return Ok(all);
    }
    sorted(out)
}

// ---- card artwork ------------------------------------------------------------

/// `Path(rel).suffix`, as Python 3.14 computes it.
fn py_suffix(rel: &str) -> String {
    let name = rel.split('/').filter(|c| !c.is_empty() && *c != ".").last().unwrap_or("");
    let name = name.trim_start_matches('.');
    match name.rfind('.') {
        Some(i) => name[i..].to_string(),
        None => String::new(),
    }
}

/// Local path for a card image, or None when the entry has no artwork.
pub fn thumb_path(p: &Plugin, detail: bool) -> Option<PathBuf> {
    use sha1::{Digest, Sha1};
    if p.install_kind == "theme" {
        return Some(crate::themeart::art_path(&p.repo));
    }
    let rel = if detail { &p.preview } else { &p.thumb };
    if rel.is_empty() {
        return None;
    }
    // Keyed on the market too: two marketplaces can name an asset the same way.
    let digest = Sha1::digest(format!("{}/{}", p.market, rel).as_bytes());
    let key = pfx(&hex::encode(digest), 16);
    let suffix = py_suffix(rel);
    Some(thumbs().join(format!("{key}{}", if suffix.is_empty() { ".webp" } else { &suffix })))
}

/// Download a card image if it is not already cached. Blocking.
pub fn fetch_thumb(p: &Plugin, detail: bool) -> Option<PathBuf> {
    if p.install_kind == "theme" {
        return crate::themeart::thumbnail(&p.repo);
    }
    let dest = thumb_path(p, detail)?;
    if dest.metadata().map(|m| m.len() > 0).unwrap_or(false) {
        return Some(dest);
    }
    let url = if detail { p.preview_url() } else { p.thumb_url() };
    if url.is_empty() {
        return None;
    }
    // a missing image is not an error
    std::fs::create_dir_all(thumbs()).ok()?;
    let data = http_get(&url, 30).ok()?;
    if data.is_empty() {
        return None;
    }
    let mut tmp = dest.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, &data).ok()?;
    std::fs::rename(&tmp, &dest).ok()?;
    Some(dest)
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

/// `Err` where the Python raises: a repo that is not a dict, or a truthy
/// `name` that is not a string.
pub fn _looks_like_theme(repo: &Value) -> Result<bool, String> {
    let name = or_(as_dict(repo)?, "name", json!(""));
    let name = as_str(&name, "lower")?.to_lowercase();
    if _NOT_A_THEME.iter().any(|bad| name.contains(bad)) {
        return Ok(false);
    }
    // The convention is omarchy-<something>-theme or <something>-omarchy-theme.
    Ok(name.contains("theme") || name.contains("omarchy"))
}

static PRETTY: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^omarchy[-_]?|[-_]?theme$|[-_]?omarchy$").expect("regex"));

/// Theme rows from the `omarchy-theme` GitHub topic. `Err` only where the
/// Python raises on a malformed answer (ghcache answers None on error).
pub fn _theme_rows() -> Result<Vec<Row>, String> {
    _theme_rows_with(&|url| crate::ghcache::try_get_json(url, crate::ghcache::DEFAULT_TTL, true, crate::ghcache::ACCEPT))
}

/// The body of `_theme_rows`, with the GitHub lookup passed in (the seam the
/// Python tests mock).
pub fn _theme_rows_with(get_json: &dyn Fn(&str) -> Result<Option<Value>, String>) -> Result<Vec<Row>, String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut rows = Vec::new();
    for page in 1..=THEME_PAGES {
        let data = get_json(&format!(
            "{}/search/repositories?q=topic:{THEME_TOPIC}&sort=stars&order=desc&per_page=100&page={page}",
            "https://api.github.com"
        ))?;
        let data = data.filter(|d| truthy(Some(d))).unwrap_or_else(|| json!({}));
        let items = or_(as_dict(&data)?, "items", json!([]));
        if !truthy(Some(&items)) {
            break;
        }
        for r in py_iter(&items)? {
            let rd = as_dict(&r)?;
            let full = rd.get("full_name").cloned().unwrap_or_else(|| json!(""));
            let full_key = py_hash_key(&full)
                .ok_or_else(|| format!("TypeError: unhashable type: '{}'", py_type(&full)))?;
            if seen.contains(&full_key) || !_looks_like_theme(&r)? {
                continue;
            }
            seen.insert(full_key);
            let owner_v = or_(rd, "owner", json!({}));
            let owner_v = as_dict(&owner_v)?.get("login").cloned().unwrap_or_else(|| json!(""));
            let owner = as_str(&owner_v, "lower")?.to_string();
            let slug_v = or_(rd, "name", json!(""));
            let slug = as_str(&slug_v, "lower")?.to_lowercase();
            // omarchy-lumon-theme -> Lumon
            // Python's `$` also matches before a final newline; Rust's only
            // at the very end, so that newline is set aside for the match.
            let (body, nl) = match slug.strip_suffix('\n') {
                Some(b) => (b, "\n"),
                None => (slug.as_str(), ""),
            };
            let pretty = PRETTY.replace_all(body, "").into_owned() + nl;
            let pretty = py_title(py_strip(&pretty.replace('-', " ").replace('_', " ")));
            let shown = if pretty.is_empty() { slug.clone() } else { pretty.clone() };
            let topics = or_(rd, "topics", json!([]));
            let mut tags = vec![json!("theme")];
            tags.extend(
                py_iter(&topics)?
                    .into_iter()
                    .filter(|t| t.as_str() != Some("omarchy") && t.as_str() != Some(THEME_TOPIC))
                    .take(2),
            );
            let html_url = rd.get("html_url").cloned().unwrap_or_else(|| json!(""));
            let name_raw = py_str(rd.get("name").unwrap_or(&Value::Null));
            let clone_url = py_str(rd.get("clone_url").unwrap_or(&json!("")));
            let lic = or_(as_dict(&or_(rd, "license", json!({})))?, "spdx_id", json!(""));
            let mut row = Row::new();
            row.insert("id".into(), json!(format!("theme.{}.{slug}", owner.to_lowercase())));
            row.insert("name".into(), json!(shown));
            row.insert("description".into(), or_(rd, "description", json!("")));
            row.insert("author".into(), json!(owner));
            row.insert("version".into(), json!(""));
            row.insert("category".into(), json!("Appearance"));
            row.insert("kind".into(), json!("Theme"));
            row.insert("status".into(), json!("Available"));
            row.insert("tags".into(), Value::Array(tags));
            row.insert("accent".into(), json!(""));
            row.insert("initials".into(), json!(pfx(&shown, 2).to_uppercase()));
            row.insert("repo".into(), html_url.clone());
            row.insert("stars".into(), py_int(&or_(rd, "stargazers_count", json!(0)))?);
            row.insert("verified".into(), json!(false));
            row.insert("installable".into(), json!(true));
            row.insert("install_cmd".into(), json!(format!("omarchy theme install {clone_url}")));
            row.insert("install_note".into(), json!(""));
            row.insert("license".into(), lic);
            row.insert("updated".into(), or_(rd, "pushed_at", json!("")));
            row.insert("added".into(), or_(rd, "created_at", json!("")));
            // GitHub renders a social card per repository; for a theme
            // that is usually the author's own screenshot.
            row.insert("thumb".into(), json!(format!("1/{owner}/{name_raw}")));
            row.insert("preview".into(), json!(format!("1/{owner}/{name_raw}")));
            row.insert("built_in".into(), json!(false));
            row.insert("install_kind".into(), json!("theme"));
            row.insert("source_type".into(), json!("community"));
            row.insert("source_url".into(), html_url);
            row.insert("upstream_status".into(), json!("passed"));
            row.insert("upstream_checked_at".into(), or_(rd, "pushed_at", json!("")));
            rows.push(row);
        }
    }
    Ok(rows)
}

/// `{line.strip().lower() for line in stdout.splitlines() if line.strip()}`
/// of an omarchy command, or the empty set when it cannot run.
fn omarchy_lines(argv: &[&str]) -> BTreeSet<String> {
    let Some(stdout) = run_text(argv, 20) else {
        return BTreeSet::new();
    };
    py_splitlines(&stdout)
        .into_iter()
        .map(py_strip)
        .filter(|l| !l.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Themes the user installed, as opposed to the ones Omarchy ships.
///
/// `omarchy theme remove` only accepts user-installed themes, so a stock
/// theme is treated exactly like a built-in plugin: present, applicable,
/// and not yours to delete.
pub fn removable_themes() -> BTreeSet<String> {
    omarchy_lines(&["omarchy", "theme", "extras"])
}

/// Theme names omarchy already knows about, lowercased for matching.
pub fn installed_themes() -> BTreeSet<String> {
    omarchy_lines(&["omarchy", "theme", "list"])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests point XDG_CACHE_HOME / XDG_STATE_HOME at temp dirs; one at a time.

    fn artcraft_rows(releases: &(dyn Fn(&str, &Value) -> Option<Value> + Sync)) -> HashMap<String, Row> {
        let html = r#"<a href="/apps/wordcraft">WordCraft</a><a href="/apps/gridcraft">GridCraft</a>"#;
        let asset = json!({"name": "gridcraft-0.7.0-linux-x86_64.AppImage",
                           "browser_download_url": "https://example.invalid/g.AppImage",
                           "digest": format!("sha256:{}", "c".repeat(64)),
                           "size": 1048576});
        let found = json!({"tag_name": "v0.7.0", "assets": [asset]});
        let rows = _artcraft_rows_with(None, &|| Ok(html.to_string()), &|_| Ok(Some(json!([]))),
                                       &|r| Ok(releases(r, &found))).expect("rows");
        rows.into_iter().map(|r| (r["id"].as_str().unwrap_or_default().to_string(), r)).collect()
    }

    // CatalogueLookups: a release lookup that failed is not a release without
    // a Linux build. One rate-limited refresh listed VectorCraft -- installed,
    // from its Linux AppImage -- and three others as "No Linux build" for six
    // hours.

    #[test]
    fn test_a_failed_lookup_says_not_checked() {
        let rows = artcraft_rows(&|r, found| r.contains("gridcraft").then(|| found.clone()));
        assert_eq!(rows["gridcraft"]["status"], "Available");
        // The asset's published digest is kept, so the install can check it.
        assert_eq!(rows["gridcraft"]["sha256"], json!("c".repeat(64)));
        assert_eq!(rows["wordcraft"]["sha256"], json!(""));
        assert_eq!(rows["wordcraft"]["status"], "Not checked");
        assert_eq!(rows["wordcraft"]["unchecked"], true);
    }

    #[test]
    fn test_every_row_field_loads_into_a_plugin() {
        // Rows become Plugin(**row); a key the dataclass lacks broke the whole
        // store, which the row-level tests above could not see.
        let rows = artcraft_rows(&|r, found| r.contains("gridcraft").then(|| found.clone()));
        for mut row in rows.into_values() {
            row.insert("market".into(), json!("artcraft"));
            Plugin::from_row(&row).expect("loads");
        }
    }

    #[test]
    fn test_a_release_without_an_appimage_still_says_so() {
        let bare = json!({"tag_name": "v1.0.0", "assets": []});
        let rows = artcraft_rows(&|r, found| Some(if r.contains("gridcraft") { found.clone() } else { bare.clone() }));
        assert_eq!(rows["wordcraft"]["status"], "No Linux build");
        assert_eq!(rows["wordcraft"]["unchecked"], false);
    }

    fn fixed_loader() -> Result<Vec<Row>, String> {
        Ok(vec![json!({"id": "wordcraft", "unchecked": false}).as_object().cloned().unwrap_or_default()])
    }

    #[test]
    fn test_an_incomplete_catalogue_is_refetched_before_the_ttl() {
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().expect("tmp");
        std::env::set_var("XDG_CACHE_HOME", d.path());
        let m = Market { id: "t".into(), name: "T".into(), loader: Some(fixed_loader), ..Market::default() };
        std::fs::create_dir_all(cache()).expect("mkdir");
        std::fs::write(m.cache(), json!({"schema": SCHEMA, "rows": [{"id": "wordcraft", "unchecked": true}]})
            .to_string()).expect("write");
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs_f64(RETRY + 1.0);
        std::fs::File::options().write(true).open(m.cache()).expect("open").set_modified(past).expect("utime");
        let rows = _load_market(&m, false).expect("rows");
        std::env::remove_var("XDG_CACHE_HOME");
        assert_eq!(rows[0]["unchecked"], false, "the stale miss must be retried");
    }

    // SetupAfterInstall: plugins the marketplace marks "requires additional
    // setup" are installed like any other, and their README's setup is walked
    // through after.

    fn entry(kw: Value) -> Row {
        let mut e = json!({"id": "x", "repo": "https://github.com/a/x", "installAvailable": false,
                           "repositoryLayout": "root-plugin", "installCommand": "",
                           "installNote": "This plugin requires additional setup before it \
                                           can be enabled. Follow the upstream installation \
                                           instructions."});
        if let (Some(e), Some(kw)) = (e.as_object_mut(), kw.as_object()) {
            e.extend(kw.clone());
        }
        _omarchy_entry(&e).expect("entry")
    }

    #[test]
    fn test_a_setup_plugin_becomes_installable() {
        let row = entry(json!({}));
        assert_eq!(row["installable"], true);
        assert_eq!(row["needs_setup"], true);
        assert_eq!(row["install_cmd"], "omarchy plugin add https://github.com/a/x --enable");
        assert_eq!(row["status"], "Needs setup");
    }

    #[test]
    fn test_what_cannot_be_installed_safely_stays_manual() {
        for kw in [
            json!({"repositoryLayout": "monorepo"}),
            json!({"repositoryLayout": "suite"}),
            json!({"installNote": "Omarchy clones the current upstream repository, validates it locally…"}),
            json!({"installNote": "Nearby requires a version-matched native helper."}),
            json!({"repo": "ssh://evil/x"}),
        ] {
            let row = entry(kw.clone());
            assert_eq!(row["installable"], false, "{kw}");
            assert_eq!(row["needs_setup"], false, "{kw}");
        }
    }

    #[test]
    fn test_a_repo_url_is_quoted_into_the_command() {
        let row = entry(json!({"repo": "https://github.com/a/x;rm -rf ~"}));
        let words = shell_words::split(row["install_cmd"].as_str().unwrap_or_default()).expect("split");
        assert_eq!(words[3], "https://github.com/a/x;rm -rf ~");
    }

    #[test]
    fn test_switched_off_network_features_make_no_request() {
        // The store's part of it: every marketplace switched off loads none.
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().expect("tmp");
        std::env::set_var("XDG_STATE_HOME", d.path());
        for key in ["net_market_omarchy", "net_market_artcraft", "net_market_themes"] {
            crate::settings::set(key, json!(false)).expect("set");
        }
        let on = enabled_markets().len();
        std::env::remove_var("XDG_STATE_HOME");
        assert_eq!(on, 0);
    }

    #[test]
    fn test_an_appimage_listing_offers_install() {
        let p = Plugin { id: "x".into(), name: "X".into(), installable: true, install_cmd: String::new(),
                         install_kind: "appimage".into(), market: "artcraft".into(), ..Plugin::default() };
        assert!(p.can_install());
        let q = Plugin { id: "y".into(), name: "Y".into(), installable: false, market: "omarchy".into(),
                         ..Plugin::default() };
        assert!(!q.can_install());
    }

    // ---- review round 2: the differences it found, pinned ----

    #[test]
    fn title_uses_the_titlecase_mapping() {
        assert_eq!(py_title("\u{1c6}x"), "\u{1c5}x");
        assert_eq!(py_title("\u{df}"), "Ss");
        assert_eq!(py_title("\u{fb01}re"), "Fire");
        // a titlecase letter is cased: what follows it is lowered
        assert_eq!(py_title("x\u{1c5}y"), "X\u{1c6}y");
        assert_eq!(py_title("apple-dark"), "Apple-Dark");
    }

    #[test]
    fn json_loads_reads_what_python_reads() {
        let v = py_json_loads(r#"{"a": "\ud83d", "b": NaN, "c": [Infinity, -Infinity], "d": "\ud83d\ude00", "e": "\\ud83d"}"#)
            .expect("loads");
        assert_eq!(v["a"], json!("\u{fffd}"));
        assert_eq!(v["b"], Value::Null);
        assert_eq!(v["c"], json!([null, null]));
        assert_eq!(v["d"], json!("\u{1f600}"));
        assert_eq!(v["e"], json!("\\ud83d"));
        assert_eq!(py_json_loads(r#"{"NaN": "NaN"}"#).expect("loads"), json!({"NaN": "NaN"}));
        assert!(py_json_loads("{nope").is_err());
        assert!(py_json_loads("-NaN").is_err());
    }

    #[test]
    fn big_integral_floats_key_as_their_int() {
        assert_eq!(py_hash_key(&json!(1e19)), py_hash_key(&json!(10_000_000_000_000_000_000u64)));
        assert_eq!(py_hash_key(&json!(-0.0)), py_hash_key(&json!(0)));
        assert_ne!(py_hash_key(&json!(1.5)), py_hash_key(&json!(1)));
    }

    #[test]
    fn run_text_times_out_on_a_held_pipe() {
        // The child exits at once; the grandchild keeps stdout open.
        let start = std::time::Instant::now();
        assert_eq!(run_text(&["sh", "-c", "sleep 5 & echo hi"], 1), None);
        assert!(start.elapsed() < std::time::Duration::from_secs(4));
        assert_eq!(run_text(&["sh", "-c", "echo hi"], 5).as_deref(), Some("hi\n"));
    }

    #[test]
    fn annotate_marks_reach_the_cache() {
        let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
        let before = cached.take();
        let p = Plugin { id: "x".into(), name: "X".into(), ..Plugin::default() };
        let other = Plugin { description: "different listing".into(), ..p.clone() };
        *cached = Some(vec![p.clone()]);
        drop(cached);
        let mut q = p.clone();
        q.installed = true;
        q.enabled = true;
        let mut o = other;
        o.installed = true;
        sync_cache(&[q, o]);
        let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
        let c = &cached.as_ref().expect("cache")[0];
        assert!(c.installed && c.enabled);
        assert_eq!(c.description, "");
        *cached = before;
    }

    #[test]
    fn catalog_annotated_marks_the_cache_itself() {
        let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
        let before = cached.take();
        // Marked installed before; nothing by this id is on any machine.
        let p = Plugin { id: "almanac-test-no-such-plugin".into(), name: "X".into(), installed: true,
                         enabled: true, ..Plugin::default() };
        *cached = Some(vec![p]);
        drop(cached);
        let got = try_catalog_annotated(false);
        let mut cached = CACHED.lock().unwrap_or_else(|e| e.into_inner());
        let c = cached.as_ref().expect("cache")[0].clone();
        *cached = before;
        drop(cached);
        if let Ok(got) = got {
            assert!(!got[0].installed && !got[0].enabled);
            assert_eq!(got[0], c);
            assert!(!c.installed && !c.enabled);
        }
    }

    #[test]
    fn wrong_typed_values_raise_where_python_does() {
        let p = |r: Value| Plugin::from_row(r.as_object().expect("row")).expect("plugin");
        assert!(p(json!({"id": "x", "name": "X", "tags": ["a", 1]})).try_haystack().is_err());
        assert!(p(json!({"id": "x", "name": null})).try_haystack().is_err());
        assert!(p(json!({"id": "x", "name": "X", "repo": null})).try_owner().is_err());
        assert!(p(json!({"id": "x", "name": "X", "upstream_status": "failed", "upstream_checked_at": null}))
            .try_trust()
            .is_err());
        let rows = vec![p(json!({"id": "s", "name": "S", "stars": "12"})), p(json!({"id": "t", "name": "T", "stars": 3}))];
        assert!(try_browse(&rows, "", "", "popular", false, false, "", "").is_err());
        // the UI still gets a page: sorted on the typed fields
        assert_eq!(browse(&rows, "", "", "popular", false, false, "", "").len(), 2);
    }
}
