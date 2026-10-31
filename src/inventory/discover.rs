//! Search what could be installed, across the official repos and the AUR.

use super::*;

// ---------------------------------------------------------------- discover --
pub const AVAILABLE: &str = "available";

/// Search what could be installed, across the official repos and the AUR.
/// (Python default: `limit=60`.)
///
/// Results are marked AVAILABLE so the UI can offer Install rather than
/// Update. Anything already installed is dropped: it belongs in the main
/// inventory, where its real version and update state live.
///
/// Err where the Python raises: pacman output that is not UTF-8, or an AUR
/// reply whose `results` is not a list of objects with numeric popularity
/// (the sort and the reads after it are outside the Python's try).
pub fn try_search_available(query: &str, limit: usize) -> Result<Vec<App>, String> {
    let query = py_strip(query);
    if query.chars().count() < 2 {
        return Ok(Vec::new());
    }

    let installed = installed_names()?;
    let mut found: Vec<App> = Vec::new();

    // --- official repos ---
    let out = run(&[PACMAN, "-Ss", query], 40)?;
    let (mut name, mut version, mut repo) = (String::new(), String::new(), String::new());
    for line in py_splitlines(&out) {
        if !line.starts_with(' ') {
            let head = py_split(line);
            if head.is_empty() {
                continue;
            }
            let repo_name = head[0];
            let (r, n) = repo_name.split_once('/').unwrap_or((repo_name, ""));
            repo = r.to_string();
            name = n.to_string();
            version = head.get(1).map(|s| s.to_string()).unwrap_or_default();
            continue;
        }
        if !name.is_empty() && !installed.contains(&name) {
            found.push(App {
                name: name.clone(),
                version: version.clone(),
                source: AVAILABLE.into(),
                description: py_strip(line).to_string(),
                extra: json!({"installable": true, "repo_name": repo, "origin": REPO,
                              "is_app": true})
                .as_object()
                .cloned()
                .unwrap_or_default()
                .into(),
                ..Default::default()
            });
        }
        name.clear();
    }

    // --- AUR ---
    let url = format!(
        "https://aur.archlinux.org/rpc/v5/search/{}?by=name-desc",
        py_quote(query)
    );
    // json.loads(...).get("results", []) inside the try: a reply that is not
    // an object is caught like a failed fetch.
    let results: Value = match http_json(&url, 20) {
        Ok(Value::Object(d)) => d.get("results").cloned().unwrap_or(json!([])),
        _ => json!([]),
    };
    let Value::Array(mut results) = results else {
        return Err(format!(
            "'{}' object has no attribute 'sort'",
            py_type(&results)
        ));
    };

    // Popularity first: on the AUR it is the only signal that separates a
    // maintained package from one person's abandoned experiment.
    // key=lambda r: -(r.get("Popularity") or 0)
    let mut keyed = Vec::with_capacity(results.len());
    for r in results.drain(..) {
        let Value::Object(rec) = &r else {
            return Err(format!("'{}' object has no attribute 'get'", py_type(&r)));
        };
        let pop = match rec.get("Popularity") {
            Some(v) if truthy(v) => match v {
                Value::Number(n) => n.as_f64().unwrap_or(0.0),
                Value::Bool(_) => 1.0,
                // NaN, an infinity, an int past 64 bits.
                Value::String(_)
                    if matches!(exotic(v), Some(Exotic::Float(_) | Exotic::Int(_))) =>
                {
                    py_float_value(v).unwrap_or(f64::INFINITY)
                }
                other => {
                    return Err(format!(
                        "bad operand type for unary -: '{}'",
                        py_type(other)
                    ))
                }
            },
            _ => 0.0,
        };
        keyed.push((-pop, r));
    }
    keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, rec) in &keyed {
        let Some(rec) = rec.as_object() else { continue };
        let nm = rec.get("Name").cloned().unwrap_or(json!(""));
        // `if not nm or nm in installed`: an empty list or dict is skipped
        // before the membership test could find it unhashable.
        if !truthy(&nm) {
            continue;
        }
        if let Value::Array(_) | Value::Object(_) = nm {
            return Err(format!("unhashable type: '{}'", py_type(&nm)));
        }
        if exotic(&nm).is_none() && nm.as_str().is_some_and(|n| installed.contains(n)) {
            continue;
        }
        let description = match rec.get("Description") {
            Some(d) if real_str(d).is_some() => {
                py_strip(&real_str(d).unwrap_or_default()).to_string()
            }
            Some(v) if truthy(v) => {
                return Err(format!("'{}' object has no attribute 'strip'", py_type(v)))
            }
            _ => String::new(),
        };
        // round(rec.get("Popularity") or 0, 2): an int (or True) stays an int.
        let popularity = match rec.get("Popularity") {
            Some(v) if truthy(v) => match v {
                Value::Bool(_) => json!(1),
                Value::Number(n) if n.is_f64() => {
                    json!(util::py_round_to(n.as_f64().unwrap_or(0.0), 2))
                }
                _ => v.clone(),
            },
            _ => json!(0),
        };
        let or_empty = |k: &str| -> Value {
            match rec.get(k) {
                Some(v) if truthy(v) => v.clone(),
                _ => json!(""),
            }
        };
        let mut extra = Map::new();
        extra.insert("installable".into(), json!(true));
        extra.insert("origin".into(), json!(AUR));
        extra.insert("is_app".into(), json!(true));
        extra.insert(
            "votes".into(),
            rec.get("NumVotes").cloned().unwrap_or(json!(0)),
        );
        extra.insert("popularity".into(), popularity);
        extra.insert("maintainer".into(), or_empty("Maintainer"));
        extra.insert(
            "orphaned".into(),
            json!(!rec.get("Maintainer").is_some_and(truthy)),
        );
        extra.insert(
            "flagged".into(),
            json!(rec.get("OutOfDate").is_some_and(truthy)),
        );
        extra.insert("url".into(), or_empty("URL"));
        found.push(App {
            name: py_str(&nm),
            version: get_str(rec, "Version", ""),
            source: AVAILABLE.into(),
            description,
            extra: extra.into(),
            ..Default::default()
        });
    }
    found.truncate(limit);
    Ok(found)
}
