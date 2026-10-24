//! The update source an AppImage carries, version keys, and its newest GitHub release.

use std::path::Path;

use num_bigint::BigInt;
use serde_json::Value;

use crate::ghcache;

use super::{LatestRelease, UpdateSpec};
use super::pyisms::{fnmatch, py_int, py_norm, py_path, py_str, py_strip, py_type, run_bytes, truthy, version_re};

/// The raw .upd_info string, or '' when the AppImage carries none.
pub fn read_update_info(path: &Path) -> String {
    let p = py_norm(path);
    let arg = |s: &'static str| std::ffi::OsStr::new(s);
    let Some(out) = run_bytes(
        &[arg("objcopy"), arg("-O"), arg("binary"), arg("--only-section=.upd_info"), p.as_os_str(), arg("/dev/stdout")],
        20,
        None,
    ) else {
        return String::new();
    };
    let bytes: Vec<u8> = out.into_iter().filter(|&b| b != 0).collect();
    py_strip(&String::from_utf8_lossy(&bytes)).to_string()
}

/// gh-releases-zsync|owner|repo|release|pattern -> a dict we can act on.
///
/// `None` is the Python's `{}`.
pub fn parse_update_info(info: &str) -> Option<UpdateSpec> {
    let parts: Vec<&str> = info.split('|').collect();
    if parts.len() >= 5 && parts[0].starts_with("gh-releases") {
        return Some(UpdateSpec {
            kind: "github".into(),
            owner: parts[1].into(),
            repo: parts[2].into(),
            release: Some(parts[3].into()),
            pattern: Some(parts[4].into()),
        });
    }
    None
}

/// `_version_of`: the first dotted version number in `text`, or "".
pub fn version_of(text: &str) -> String {
    version_re()
        .captures(text)
        .and_then(|m| m.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

/// `_vkey`: comparable tuple. '0.10.2' must sort above '0.9.9', which it does
/// not as text -- the reason a string compare is wrong here.
///
/// Fallible like the Python's `int()`: `check` also calls it on a release's
/// version, which falls back to the raw tag name ("nightly") when the tag
/// carries no number, and Python raises ValueError there.
pub fn vkey(v: &str) -> Result<Vec<BigInt>, String> {
    if v.is_empty() {
        return Ok(Vec::new());
    }
    v.split('.').map(py_int).collect()
}

/// The hex sha256 of a GitHub release asset's `digest` (`"sha256:<hex>"`),
/// lowercased; "" when there is none, or it is not a sha256 digest.
pub fn sha256_of_digest(digest: Option<&Value>) -> String {
    match digest.and_then(Value::as_str).and_then(|d| d.strip_prefix("sha256:")) {
        Some(hex) if hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()) => hex.to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn release_url(owner: &str, repo: &str, release: &Value) -> String {
    let latest = matches!(release.as_str(), Some("latest") | Some(""));
    let tag = if latest { "releases/latest".to_string() } else { format!("releases/tags/{}", py_str(release)) };
    format!("https://api.github.com/repos/{owner}/{repo}/{tag}")
}

/// The body of `latest_github` after the fetch: `data` is what the cache
/// gave back, `pattern` is `None` when the spec's pattern is not a string.
fn latest_from(data: Option<Value>, pattern: Option<&Value>) -> Result<Option<LatestRelease>, String> {
    let Some(Value::Object(data)) = data else {
        return Ok(None); // offline or rate-limited: no update shown
    };
    let pattern = match pattern {
        Some(Value::String(s)) => s.as_str(),
        Some(other) => {
            return Err(format!("AttributeError: '{}' object has no attribute 'endswith'", py_type(other)))
        }
        None => "*.AppImage",
    };

    // The pattern names a .zsync companion; the AppImage is the same name
    // without that suffix.
    let want = pattern.strip_suffix(".zsync").unwrap_or(pattern);
    let mut asset_url = Value::String(String::new());
    let mut asset_name = String::new();
    let mut asset_sha = String::new();
    let empty = Value::Array(Vec::new());
    let assets = data.get("assets").unwrap_or(&empty);
    let items: Vec<&Value> = match assets {
        Value::Array(a) => a.iter().collect(),
        Value::Object(o) if o.is_empty() => Vec::new(),
        Value::String(s) if s.is_empty() => Vec::new(),
        Value::Object(_) | Value::String(_) => {
            return Err("AttributeError: 'str' object has no attribute 'get'".into())
        }
        other => return Err(format!("TypeError: '{}' object is not iterable", py_type(other))),
    };
    for a in items {
        let Some(a) = a.as_object() else {
            return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(a)));
        };
        let name = match a.get("name") {
            None => "",
            Some(Value::String(s)) => s.as_str(),
            Some(other) => {
                return Err(format!(
                    "TypeError: expected str, bytes or os.PathLike object, not {}",
                    py_type(other)
                ))
            }
        };
        if fnmatch(name, want) {
            // Passed through as it is: None or a number survives into "url".
            asset_url = a.get("browser_download_url").cloned().unwrap_or_else(|| Value::String(String::new()));
            asset_name = name.to_string();
            asset_sha = sha256_of_digest(a.get("digest"));
            break;
        }
    }

    let tag_name = data.get("tag_name").cloned().unwrap_or_else(|| Value::String(String::new()));
    // `_version_of(tag_name) or _version_of(asset_name) or tag_name`
    let mut version = Value::String(version_of_value(&tag_name)?);
    if version.as_str() == Some("") {
        let v = version_of(&asset_name);
        version = if v.is_empty() { tag_name.clone() } else { Value::String(v) };
    }
    // `(data.get("published_at") or "")[:10]`
    let published = match data.get("published_at").filter(|v| truthy(v)) {
        None => Value::String(String::new()),
        Some(Value::String(s)) => Value::String(s.chars().take(10).collect()),
        Some(Value::Array(a)) => Value::Array(a.iter().take(10).cloned().collect()),
        Some(Value::Object(_)) => return Err("KeyError: slice(None, 10, None)".into()),
        Some(other) => return Err(format!("TypeError: '{}' object is not subscriptable", py_type(other))),
    };
    let page = data.get("html_url").cloned().unwrap_or_else(|| Value::String(String::new()));
    let mut out = LatestRelease::default();
    out.set("version", version);
    out.set("tag", tag_name);
    out.set("url", asset_url);
    out.set("asset", Value::String(asset_name));
    out.set("published", published);
    out.set("page", page);
    out.set("sha256", Value::String(asset_sha));
    Ok(Some(out))
}

/// `_version_of(value)` for whatever a release document holds: a falsy
/// value is searched as "" (`text or ""`), any other non-string is the
/// TypeError `re.search` raises.
fn version_of_value(v: &Value) -> Result<String, String> {
    match v {
        _ if !truthy(v) => Ok(String::new()),
        Value::String(s) => Ok(version_of(s)),
        other => Err(format!("TypeError: expected string or bytes-like object, got '{}'", py_type(other))),
    }
}

/// `latest_github` with the fetch and the loosely-typed arguments the
/// registry can hand `check` made explicit.
fn latest_impl(
    owner: &str,
    repo: &str,
    pattern: Option<&Value>,
    release: &Value,
    fetch: &dyn Fn(&str) -> Option<Value>,
) -> Result<Option<LatestRelease>, String> {
    let url = release_url(owner, repo, release);
    // Shared cache: authenticated when gh is logged in, conditional otherwise,
    // and never competing with the store for the same rate limit.
    let data = fetch(&url);
    latest_from(data, pattern)
}

// `try: data = ghcache.get_json(url) / except Exception: data = None`:
// what get_json raises is "no update shown" here.
fn fetch_github(url: &str) -> Option<Value> {
    ghcache::try_get_json(url, ghcache::DEFAULT_TTL, true, ghcache::ACCEPT).ok().flatten()
}

/// Newest release plus the asset matching the AppImage's own pattern.
///
/// `None` when GitHub gave nothing (offline or rate-limited: no update
/// shown). `release` is "latest" (or "") or a tag name. An error is a
/// malformed release document the Python would raise on.
pub fn latest_github(owner: &str, repo: &str, pattern: &str, release: &str) -> Result<Option<LatestRelease>, String> {
    latest_impl(
        owner,
        repo,
        Some(&Value::String(pattern.into())),
        &Value::String(release.into()),
        &fetch_github,
    )
}

type LatestFn<'a> = &'a dyn Fn(&str, &str, Option<&Value>, &Value) -> Result<Option<LatestRelease>, String>;

fn check_impl(record: &Value, latest_fn: LatestFn) -> Result<Option<LatestRelease>, String> {
    // `record.get(...)`: anything but a dict has no such method.
    let Some(record) = record.as_object() else {
        return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(record)));
    };
    let get = |k: &str| record.get(k);
    let path = match get("path") {
        None => py_path(""),
        Some(Value::String(s)) => py_path(s),
        Some(other) => {
            return Err(format!(
                "TypeError: argument should be a str or an os.PathLike object, not '{}'",
                py_type(other)
            ))
        }
    };
    if !path.exists() {
        return Ok(None);
    }

    let spec: Value = match get("update_spec") {
        Some(v) if truthy(v) => v.clone(),
        _ => parse_update_info(&read_update_info(&path))
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or_else(|| Value::Object(Default::default())),
    };
    let Some(obj) = spec.as_object() else {
        return Err(format!("AttributeError: '{}' object has no attribute 'get'", py_type(&spec)));
    };
    if obj.get("kind").and_then(Value::as_str) != Some("github") {
        return Ok(None);
    }

    let owner = obj.get("owner").ok_or("KeyError: 'owner'")?;
    let repo = obj.get("repo").ok_or("KeyError: 'repo'")?;
    let default_release = Value::String("latest".into());
    let latest = latest_fn(
        &py_str(owner),
        &py_str(repo),
        obj.get("pattern"),
        obj.get("release").unwrap_or(&default_release),
    )?;
    let Some(mut latest) = latest else { return Ok(None) };
    if !truthy(&latest.value("url")) {
        return Ok(None);
    }

    let have = match get("version") {
        Some(v) if !truthy(v) => String::new(),
        None => String::new(),
        Some(Value::String(s)) => version_of(s),
        Some(_) => return Err("TypeError: expected string or bytes-like object".into()),
    };
    // `new` is a string, or the tag's own falsy value (None, 0, []): a truthy
    // tag that is not a string has already raised in `_version_of`.
    let new_v = latest.value("version");
    let new = new_v.as_str().unwrap_or("").to_string();
    // Only report a genuinely higher version. Equal or unparseable means
    // "nothing to do" -- claiming an update that reinstalls the same build
    // would be worse than staying quiet.
    if !have.is_empty() && !new.is_empty() && vkey(&new)? <= vkey(&have)? {
        return Ok(None);
    }
    if have.is_empty() && !truthy(&new_v) {
        return Ok(None);
    }
    latest.spec = Some(spec);
    Ok(Some(latest))
}

/// Given a registry record, return {} or the newer release's details.
///
/// `record` is one value of the AppImage registry (`inventory` owns that
/// file and keeps records as untyped JSON objects, as the Python does).
/// `None` is the Python's `{}`; an error is what the Python raises on a
/// malformed record (a spec without an owner, say).
pub fn check(record: &Value) -> Result<Option<LatestRelease>, String> {
    check_impl(record, &|o, r, p, rel| latest_impl(o, r, p, rel, &fetch_github))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn latest(version: &str, url: &str) -> LatestRelease {
        LatestRelease { version: version.into(), url: url.into(), ..Default::default() }
    }

    // AppImageUpdateInfo

    #[test]
    fn test_parses_github_release_spec() {
        let spec = parse_update_info(
            "gh-releases-zsync|storytold|vectorcraft|latest|vectorcraft-*-linux-x86_64.AppImage.zsync",
        )
        .unwrap_or_default();
        assert_eq!(spec.kind, "github");
        assert_eq!(spec.owner, "storytold");
        assert_eq!(spec.repo, "vectorcraft");
    }

    #[test]
    fn test_unknown_scheme_yields_nothing() {
        // zsync-over-http needs a zsync client we do not have; claiming support
        // would mean offering an update that cannot be applied.
        assert_eq!(parse_update_info("zsync|https://example/x.zsync"), None);
        assert_eq!(parse_update_info(""), None);
    }

    #[test]
    fn test_version_ordering_is_numeric() {
        // The bug a string compare would introduce: 0.9.9 > 0.10.2 as text.
        assert!(vkey("0.9.9").unwrap() < vkey("0.10.2").unwrap());
    }

    #[test]
    fn test_same_or_older_release_is_not_an_update() {
        let fake = |_: &str, _: &str, _: Option<&Value>, _: &Value| Ok(Some(latest("1.0.0", "u")));
        let mut rec = json!({"path": file!(), "version": "1.0.0",
                             "update_spec": {"kind": "github", "owner": "o", "repo": "r"}});
        // file!() is relative to the crate root, where cargo runs tests.
        assert_eq!(check_impl(&rec, &fake), Ok(None));
        rec["version"] = json!("2.0.0");
        assert_eq!(check_impl(&rec, &fake), Ok(None));
    }

    #[test]
    fn test_newer_release_is_reported() {
        let fake = |_: &str, _: &str, _: Option<&Value>, _: &Value| Ok(Some(latest("2.0.0", "u")));
        let rec = json!({"path": file!(), "version": "1.0.0",
                         "update_spec": {"kind": "github", "owner": "o", "repo": "r"}});
        assert_eq!(check_impl(&rec, &fake).unwrap().map(|l| l.version), Some("2.0.0".to_string()));
    }

    #[test]
    fn test_vkey_compares_big_components_exactly() {
        let a = vkey(&format!("1.1{}", "0".repeat(42))).unwrap();
        let b = vkey(&format!("1.2{}", "0".repeat(42))).unwrap();
        assert!(a < b);
        assert!(vkey(&format!("1.{}", "1".repeat(5000))).is_err());
        assert!(vkey(&"1_".repeat(4300).trim_end_matches('_').to_string()).is_ok());
    }

    #[test]
    fn test_release_values_pass_through() {
        let data = json!({"tag_name": null, "html_url": 7,
                          "assets": [{"name": "a.AppImage", "browser_download_url": null}]});
        let r = latest_from(Some(data), None).unwrap().unwrap();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v, json!({"version": null, "tag": null, "url": null, "asset": "a.AppImage",
                             "published": "", "page": 7, "sha256": ""}));
        let back: LatestRelease = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), v);
        // A stored dict comes back as it was: odd keys, order, a null spec.
        let stored = json!({"url": "u", "x": [1], "version": "2", "spec": null});
        let back: LatestRelease = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(back.value("url"), json!("u"));
        assert_eq!(serde_json::to_string(&back).unwrap(), serde_json::to_string(&stored).unwrap());
        assert!(latest_from(Some(json!({"tag_name": 5})), None).is_err());
        assert!(latest_from(Some(json!({"published_at": true})), None).is_err());
    }

    // Review round 3.

    /// The asset's published digest travels with the release, so the
    /// update can be checked against it.
    #[test]
    fn test_the_asset_digest_is_kept() {
        let hex = "AB".repeat(32);
        let data = json!({"tag_name": "v2.0.0", "assets": [
            {"name": "x.AppImage.zsync", "digest": "sha256:00"},
            {"name": "x-2.0.0.AppImage", "browser_download_url": "u", "digest": format!("sha256:{hex}")}]});
        let r = latest_from(Some(data), Some(&json!("x-*.AppImage"))).unwrap().unwrap();
        assert_eq!(r.sha256, hex.to_lowercase());
        assert_eq!(serde_json::to_value(&r).unwrap()["sha256"], json!(hex.to_lowercase()));
        assert_eq!(sha256_of_digest(Some(&json!("sha512:abcd"))), "");
        assert_eq!(sha256_of_digest(Some(&json!("sha256:xyz"))), "");
        assert_eq!(sha256_of_digest(None), "");
        // A dict stored before the key existed reads as "no digest".
        let old: LatestRelease = serde_json::from_value(json!({"url": "u", "version": "2"})).unwrap();
        assert_eq!(old.sha256, "");
    }

    #[test]
    fn test_check_of_a_record_that_is_not_a_dict() {
        let fake = |_: &str, _: &str, _: Option<&Value>, _: &Value| Ok(Some(latest("2.0.0", "u")));
        for rec in [json!(null), json!([]), json!("text"), json!(5)] {
            assert!(check_impl(&rec, &fake).is_err());
        }
    }
}
