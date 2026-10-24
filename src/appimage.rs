//! Port of `appimage.py`.
//!
//! AppImage update checking.
//!
//! Most AppImages embed their own update source in a `.upd_info` ELF section --
//! the same string AppImageUpdate uses. Reading it means the update source is
//! discovered from the file rather than configured by hand, which is the whole
//! difference between "tracked" and "managed".
//!
//! Only the GitHub forms are implemented. The zsync-over-http forms exist but
//! need a zsync client to be useful, and every AppImage seen here so far uses
//! gh-releases-*.
//!
//! Signature notes (callers adapt): the Python raises to its caller in a few
//! places these functions used to swallow, so `latest_github`, `check`,
//! `install_desktop_entry`, `remove_desktop_entry` and `vkey` return
//! `Result<_, String>` (the error is the Python exception, `"Type: message"`).
//! `LatestRelease::spec` is the registry's `update_spec` as JSON, stored as
//! given (a hand-written spec keeps every key it had).

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod desktop;
mod download;
mod pyisms;
mod release;
mod urllib;

pub use desktop::{install_desktop_entry, remove_desktop_entry};
pub use download::{download, download_verified, Downloaded};
pub use release::{check, latest_github, parse_update_info, read_update_info, sha256_of_digest, version_of, vkey};
#[cfg(test)]
pub(crate) use download::test_elf;

/// HTTP header sent with every request (`UA = {"User-Agent": "almanac"}`).
pub const UA: (&str, &str) = ("User-Agent", "almanac");

/// `_VERSION`: the first dotted number in a tag or asset name.
pub const VERSION_PATTERN: &str = r"(\d+(?:\.\d+)+)";

/// Where an AppImage's updates come from: the parsed `.upd_info` string
/// (`gh-releases-zsync|owner|repo|release|pattern`), or the `update_spec`
/// a registry record carries.
///
/// In Python this is a plain dict; `release` and `pattern` may be missing
/// from a hand-written registry record (the Python falls back to "latest"
/// and "*.AppImage" with `.get`), so they are optional here and omitted
/// from JSON when absent, keeping the registry file identical.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSpec {
    pub kind: String,
    pub owner: String,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// Newest release plus the asset matching the AppImage's own pattern
/// (the non-empty dict `latest_github` / `check` return). The Python's
/// empty `{}` is `None` at the call site.
///
/// `spec` is only set by `check` (`latest["spec"] = spec`), and is omitted
/// from JSON otherwise. This value is what `inventory` stores in a registry
/// record's `"latest"`, so its JSON shape must match the Python dict. It is
/// the spec exactly as `check` found it (the record's own `update_spec`
/// dict, or the parsed `.upd_info`), so it is untyped JSON like the record.
///
/// The Python passes `tag_name`, `browser_download_url`, `html_url` (and so
/// `version`, which falls back to the tag) through from the release document
/// as they are, so a release can carry `None`, a number or a list there.
/// The string fields are the common case; a value that is not a string is
/// kept in `raw` under the same key (its string field is then "") and is what
/// serialises, so the stored record is the Python's. `value(key)` gives the
/// real value either way.
///
/// `sha256` is the asset's own digest as GitHub publishes it (`"digest":
/// "sha256:<hex>"`), the hex part, or "" when the release has none; the
/// update is checked against it before it is swapped in. (Not in the
/// Python; a dict stored before it existed reads as "".)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LatestRelease {
    pub version: String,
    pub tag: String,
    pub url: String,
    pub asset: String,
    pub published: String,
    pub page: String,
    pub sha256: String,
    pub spec: Option<Value>,
    pub raw: std::collections::BTreeMap<String, Value>,
    /// A dict read back from the registry keeps its own keys, in its own
    /// order (`None` for one built here, which has the six plus `spec`).
    pub keys: Option<Vec<String>>,
    /// Keys of a stored dict other than the six and `spec`.
    pub extra: serde_json::Map<String, Value>,
}

/// The keys of the Python dict, in its order (`spec` comes last).
const RELEASE_KEYS: [&str; 7] = ["version", "tag", "url", "asset", "published", "page", "sha256"];

impl LatestRelease {
    fn field_mut(&mut self, key: &str) -> Option<&mut String> {
        Some(match key {
            "version" => &mut self.version,
            "tag" => &mut self.tag,
            "url" => &mut self.url,
            "asset" => &mut self.asset,
            "published" => &mut self.published,
            "page" => &mut self.page,
            "sha256" => &mut self.sha256,
            _ => return None,
        })
    }

    fn field(&self, key: &str) -> Option<&String> {
        Some(match key {
            "version" => &self.version,
            "tag" => &self.tag,
            "url" => &self.url,
            "asset" => &self.asset,
            "published" => &self.published,
            "page" => &self.page,
            "sha256" => &self.sha256,
            _ => return None,
        })
    }

    /// The dict's value for `key` as the Python has it (`latest["url"]`).
    pub fn value(&self, key: &str) -> Value {
        if let Some(v) = self.raw.get(key) {
            return v.clone();
        }
        match key {
            "spec" => self.spec.clone().unwrap_or(Value::Null),
            _ => self.field(key).map(|s| Value::String(s.clone())).unwrap_or(Value::Null),
        }
    }

    /// Set one of the six release keys to a value of any type.
    pub fn set(&mut self, key: &str, v: Value) {
        if let Some(keys) = self.keys.as_mut() {
            if RELEASE_KEYS.contains(&key) && !keys.iter().any(|k| k == key) {
                keys.push(key.to_string());
            }
        }
        let Some(field) = self.field_mut(key) else { return };
        match v {
            Value::String(s) => {
                *field = s;
                self.raw.remove(key);
            }
            other => {
                field.clear();
                self.raw.insert(key.to_string(), other);
            }
        }
    }
}

impl Serialize for LatestRelease {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = ser.serialize_map(None)?;
        match &self.keys {
            None => {
                for k in RELEASE_KEYS {
                    m.serialize_entry(k, &self.value(k))?;
                }
            }
            Some(keys) => {
                for k in keys {
                    if RELEASE_KEYS.contains(&k.as_str()) {
                        m.serialize_entry(k, &self.value(k))?;
                    } else if k == "spec" {
                        m.serialize_entry(k, self.spec.as_ref().unwrap_or(&Value::Null))?;
                    } else {
                        m.serialize_entry(k, self.extra.get(k).unwrap_or(&Value::Null))?;
                    }
                }
            }
        }
        let spec_listed = self.keys.as_ref().is_some_and(|ks| ks.iter().any(|k| k == "spec"));
        if let (Some(spec), false) = (&self.spec, spec_listed) {
            m.serialize_entry("spec", spec)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for LatestRelease {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let m = serde_json::Map::<String, Value>::deserialize(de)?;
        // The dict as stored: every key, in its order, `"spec": null` too.
        let mut out = LatestRelease { keys: Some(m.keys().cloned().collect()), ..Default::default() };
        for (k, v) in m {
            if RELEASE_KEYS.contains(&k.as_str()) {
                out.set(&k, v);
            } else if k == "spec" {
                out.spec = Some(v);
            } else {
                out.extra.insert(k, v);
            }
        }
        Ok(out)
    }
}
