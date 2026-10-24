//! The omarchy plugin registry, and searching it.

use super::*;

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
    (*plugin_registry_shared(force)).clone()
}

/// The registry last parsed from the cache file, by the file's (mtime,
/// length): 8 MB of JSON, read on every Discover search, was most of the
/// search's time. Parsed again only when the file changes.
pub(super) static REGISTRY_PARSED: std::sync::Mutex<Option<(std::time::SystemTime, u64, std::sync::Arc<Value>)>> =
    std::sync::Mutex::new(None);

/// `plugin_registry` without the copy: the parsed registry is shared, so a
/// caller that only reads it does not clone 8 MB.
pub fn plugin_registry_shared(force: bool) -> std::sync::Arc<Value> {
    use std::sync::Arc;
    let path = registry_cache_path();
    let read = || -> Option<(f64, Arc<Value>)> {
        let meta = std::fs::metadata(&path).ok()?;
        let modified = meta.modified().ok()?;
        let mtime = modified.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let age = util::now() - mtime;
        let mut parsed = REGISTRY_PARSED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((m, len, v)) = parsed.as_ref() {
            if *m == modified && *len == meta.len() {
                return Some((age, v.clone()));
            }
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let v = Arc::new(py_loads(&text).ok()?);
        *parsed = Some((modified, meta.len(), v.clone()));
        Some((age, v))
    };
    // JSON null is Python's None here, the same as no cache.
    let (age, cached) = match read() {
        Some((age, v)) if !v.is_null() => (age, Some(v)),
        Some((age, _)) => (age, None),
        None => (1e9, None),
    };

    if let Some(c) = &cached {
        if !force && age < REGISTRY_TTL {
            return c.clone();
        }
    }

    let fallback = || match &cached {
        Some(c) if truthy(c) => c.clone(),
        _ => Arc::new(json!({})),
    };
    let data = match http_json(REGISTRY_URL, 30) {
        Ok(d) => d,
        Err(_) => return fallback(),
    };
    let write = || -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, py_dumps(&data, None))
    };
    if write().is_err() {
        return fallback();
    }
    Arc::new(data)
}

/// `_plugin_name`: (name, description, author) for a registry entry.
///
/// Only a couple of entries ship catalog metadata, so for the rest the repo
/// URL is all there is -- the last path segment is the plugin's identity.
///
/// The values are the entry's own, as the Python returns them (a null
/// description stays None). Err where the Python raises: an entry that is
/// not an object, or a repo that is truthy but not a string.
pub fn plugin_name(entry: &Value) -> Result<(Value, Value, Value), String> {
    let Value::Object(e) = entry else {
        return Err(format!("'{}' object has no attribute 'get'", py_type(entry)));
    };
    let mut cat = e.get("catalog").cloned().unwrap_or(Value::Null);
    if let Value::Array(a) = &cat {
        if let Some(first) = a.first() {
            cat = first.clone();
        }
    }
    if let Value::Object(c) = &cat {
        if c.get("name").is_some_and(truthy) {
            let get = |k: &str| c.get(k).cloned().unwrap_or(json!(""));
            return Ok((get("name"), get("description"), get("author")));
        }
    }

    let repo_full = match e.get("repo") {
        Some(r) if real_str(r).is_some() => real_str(r).unwrap_or_default(),
        Some(v) if truthy(v) => {
            return Err(format!("'{}' object has no attribute 'rstrip'", py_type(v)))
        }
        _ => String::new(),
    };
    let repo = repo_full.trim_end_matches('/');
    let parts: Vec<&str> = repo.split('/').collect();
    let name = parts.last().copied().unwrap_or(repo).to_string();
    let author = if parts.len() >= 2 { parts[parts.len() - 2].to_string() } else { String::new() };
    Ok((json!(name), json!(""), json!(author)))
}

/// Python `==` between two hashable JSON scalars (`1 == 1.0 == True`).
pub(super) fn py_eq(a: &Value, b: &Value) -> bool {
    let num = |v: &Value| match v {
        Value::Number(n) => n.as_f64(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(_) if matches!(exotic(v), Some(Exotic::Float(_) | Exotic::Int(_))) => py_float_value(v),
        _ => None,
    };
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// Search the official Omarchy plugin marketplace. (Python default: `limit=40`.)
///
/// Retired plugins are excluded: they were delisted, so offering them would
/// be recommending something the registry has withdrawn.
///
/// (The Python's `_reverse` str subclass -- "sorts as its own reverse, so a
/// date field reads newest-first" -- becomes a reversed comparison inside
/// this function's sort.)
///
/// Err where the Python raises on a registry of the wrong shape: not an
/// object, `sources` that are not a list of objects, and the like.
pub fn try_search_plugins(query: &str, limit: usize) -> Result<Vec<App>, String> {
    let shared = plugin_registry_shared(false);
    let reg: &Value = &shared;
    let Value::Object(reg) = reg else {
        return Err(format!("'{}' object has no attribute 'get'", py_type(reg)));
    };
    // sources = reg.get("sources") or [] (a list is read in place, not copied)
    let owned;
    let sources: &[Value] = match reg.get("sources") {
        Some(Value::Array(a)) if !a.is_empty() => a,
        Some(v) if truthy(v) => {
            owned = py_iter(v)?;
            &owned
        }
        _ => return Ok(Vec::new()),
    };

    // set(reg.get("retiredPluginIds") or [])
    let retired: Vec<Value> = match reg.get("retiredPluginIds") {
        Some(v) if truthy(v) => py_iter(v)?,
        _ => Vec::new(),
    };
    if let Some(bad) = retired.iter().find(|v| v.is_array() || v.is_object()) {
        return Err(format!("unhashable type: '{}'", py_type(bad)));
    }
    let installed: HashSet<String> = sorted_children(&crate::xdg::config_home().join("omarchy/plugins"))
        .into_iter()
        .filter(|d| d.is_dir())
        .map(|d| name_of(&d))
        .collect();
    let q = py_strip(query).to_lowercase();
    let mut out: Vec<App> = Vec::new();
    for entry in sources {
        let Value::Object(e) = entry else {
            return Err(format!("'{}' object has no attribute 'get'", py_type(entry)));
        };
        let repo = match e.get("repo") {
            Some(v) if truthy(v) => v.clone(),
            _ => json!(""),
        };
        let (name, desc, author) = plugin_name(entry)?;
        let hay = format!("{} {} {} {}", py_str(&name), py_str(&desc), py_str(&author), py_str(&repo))
            .to_lowercase();
        if !q.is_empty() && !hay.contains(&q) {
            continue;
        }
        if name.is_array() || name.is_object() {
            return Err(format!("unhashable type: '{}'", py_type(&name)));
        }
        if retired.iter().any(|r| py_eq(r, &name)) {
            continue;
        }
        // The sort below calls `a.name.lower()` on every row.
        let Some(name) = real_str(&name) else {
            return Err(format!("'{}' object has no attribute 'lower'", py_type(&name)));
        };
        // `desc or f"..." if author else "Omarchy plugin"` parses as
        // `(desc or f"...") if author else "Omarchy plugin"`.
        let description = if truthy(&author) {
            if truthy(&desc) { py_str(&desc) } else { format!("Omarchy plugin — {}", py_str(&author)) }
        } else {
            "Omarchy plugin".to_string()
        };
        // (entry.get("addedAt") or "")[:10]
        let added = match e.get("addedAt") {
            Some(a) if real_str(a).is_some() => match exotic(a) {
                Some(Exotic::WStr(t)) => units_json(&wstr_units(t).into_iter().take(10).collect::<Vec<_>>()),
                _ => json!(py_prefix(&real_str(a).unwrap_or_default(), 10)),
            },
            Some(Value::Array(a)) => Value::Array(a.iter().take(10).cloned().collect()),
            Some(v) if truthy(v) => {
                return Err(format!("'{}' object is not subscriptable", py_type(v)))
            }
            _ => json!(""),
        };
        let mut extra = Map::new();
        extra.insert("installable".into(), json!(true));
        extra.insert("origin".into(), json!(PLUGIN));
        extra.insert("is_app".into(), json!(true));
        extra.insert("url".into(), repo.clone());
        extra.insert("repo".into(), repo);
        extra.insert("author".into(), author);
        // A validated listing has been checked by the marketplace;
        // worth showing, since anyone can submit a repo.
        extra.insert("verified".into(), json!(e.get("listingValidatedAt").is_some_and(truthy)));
        extra.insert("added".into(), added);
        extra.insert("suite".into(), json!(e.get("type").and_then(Value::as_str) == Some("suite")));
        extra.insert("already".into(), json!(installed.contains(&name)));
        out.push(
            App { name, source: PLUGIN.into(), description, extra: extra.into(), ..Default::default() }
                // (A non-string type, null included, is kept as it is.)
                .with_values(&[("version", &e.get("type").cloned().unwrap_or(json!("plugin")))]),
        );
        // Browsing (no query) needs the whole set available to sort before
        // trimming, or "most recent" would only be the most recent of an
        // arbitrary first slice.
        if !q.is_empty() && out.len() >= limit * 3 {
            break;
        }
    }

    // Verified listings first, then the ones with real metadata.
    let key = |a: &App| -> (bool, bool, String, String) {
        let verified = a.extra.get("verified").is_some_and(truthy);
        let added_v = a.extra.get("added").cloned().unwrap_or(json!(""));
        (!verified, !truthy(&added_v), py_str(&added_v), a.name.to_lowercase())
    };
    out.sort_by(|a, b| {
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0)
            .then(ka.1.cmp(&kb.1))
            .then(kb.2.cmp(&ka.2)) // _reverse: newest first
            .then(ka.3.cmp(&kb.3))
    });
    out.truncate(limit);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_parsed_once_per_file_version() {
        let _env = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("almanac-registry-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let old = std::env::var_os("XDG_CACHE_HOME");
        std::env::set_var("XDG_CACHE_HOME", &dir);
        let path = registry_cache_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"sources": [{"repo": "https://github.com/a/one"}]}"#).unwrap();
        let a = plugin_registry_shared(false);
        let b = plugin_registry_shared(false);
        let names = |q: &str| try_search_plugins(q, 10).unwrap().into_iter().map(|a| a.name).collect::<Vec<_>>();
        let first = names("");
        // A new file (fresh, so no fetch) is read again.
        std::fs::write(&path, r#"{"sources": [{"repo": "https://github.com/a/one"}, {"repo": "https://github.com/b/two"}]}"#)
            .unwrap();
        let c = plugin_registry_shared(false);
        let second = names("");
        match old {
            Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
            None => std::env::remove_var("XDG_CACHE_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        assert!(!std::sync::Arc::ptr_eq(&a, &c));
        assert_eq!(c["sources"].as_array().map(Vec::len), Some(2));
        assert!(plugin_registry_shared(false).is_object());
        assert_eq!(first, vec!["one"]);
        assert_eq!(second, vec!["one", "two"]);
    }
}
