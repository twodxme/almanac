//! AppImages: the registry the app owns, and the rows read from it.

use super::*;

// ---------------------------------------------------------------- appimages --

/// `_load_registry`: the AppImage registry as the Python returns it --
/// whatever JSON the file holds (a list stays a list), `{}` when it is
/// missing or unreadable.
pub fn load_registry_value() -> Value {
    py_read_json(&registry()).unwrap_or_else(|| json!({}))
}

/// `load_registry_value` for callers that work on the registry as a map.
/// Keys are registry keys; values are the mixed-type records. A registry
/// that is valid JSON but not an object, which the Python hands on as is
/// (and every `.items()` then fails on), is `{}` here.
pub fn load_registry() -> Map<String, Value> {
    match load_registry_value() {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Write the AppImage registry (indent=2, trailing newline), atomically: a
/// crash mid-write cannot truncate it. Errors propagate, as the Python's
/// OSError does. A read-modify-write holds [`lock_registry`] around both
/// halves; [`update_registry`] does that.
pub fn save_registry(data: &Map<String, Value>) -> Result<(), String> {
    let reg = registry();
    if let Some(parent) = reg.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = registry_tmp(&reg);
    let text = py_dumps(&Value::Object(data.clone()), Some(2)) + "\n";
    if let Err(e) = std::fs::write(&tmp, text) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    std::fs::rename(&tmp, &reg).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    }) // atomic: a crash mid-write cannot truncate it
}

/// The registry's lock, held for one read-modify-write of it.
///
/// The window, its background scans and the scheduled update (another
/// process) all rewrite the whole registry. Without a lock, one that read
/// it before another wrote and then wrote its own copy dropped the other's
/// change -- the scheduled update, which reads it, downloads for minutes and
/// then writes, dropped whatever the window had done meanwhile.
pub struct RegistryLock(#[allow(dead_code)] std::fs::File);

/// Take the registry's lock (an exclusive `flock` on `appimages.lock` beside
/// it), waiting for whoever holds it. Held until the guard is dropped, or
/// the process ends. Not re-entrant: a thread that holds it must not ask
/// again.
pub fn lock_registry() -> Result<RegistryLock, String> {
    use rustix::fs::{flock, FlockOperation};
    let reg = registry();
    if let Some(parent) = reg.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(reg.with_extension("lock"))
        .map_err(|e| e.to_string())?;
    loop {
        match flock(&file, FlockOperation::LockExclusive) {
            Ok(()) => return Ok(RegistryLock(file)),
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(std::io::Error::from(e).to_string()),
        }
    }
}

/// A temporary file beside the registry for one write: named for this
/// process and write, so two writers never write into the same one.
pub fn registry_tmp(reg: &Path) -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let name = reg
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    reg.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

/// Change the registry under its lock: it is read afresh, `change` edits it
/// and says whether it changed anything, and only then is it written. So a
/// writer changes only the entries it means to, never undoing another's.
///
/// `Err` where the registry is JSON but not an object (Python's TypeError
/// of `reg[key] = ...`), leaving it as it was, and where it cannot be
/// written.
pub fn update_registry(
    change: impl FnOnce(&mut Map<String, Value>) -> Result<bool, String>,
) -> Result<(), String> {
    let _lock = lock_registry()?;
    let mut reg = match load_registry_value() {
        Value::Object(m) => m,
        Value::Array(_) => return Err("list indices must be integers or slices, not str".into()),
        other => {
            return Err(format!(
                "'{}' object does not support item assignment",
                py_type(&other)
            ))
        }
    };
    if change(&mut reg)? {
        save_registry(&reg)?;
    }
    Ok(())
}

pub const APPIMAGE_CHECK_SECONDS: f64 = 3600.0;

/// Registered AppImages. (Python default: `check_updates=True`.)
///
/// An error is what the Python raises out of `appimage.check` on a malformed
/// record (a spec without an owner, say); `collect` then drops the whole
/// AppImage section, as the Python's catch-all does.
///
/// Also Err where the Python raises on a malformed registry (not an object,
/// a record that is not one, a path that is not a string, a `latest` that
/// is truthy but not an object), and when the registry cannot be written
/// back after an update check.
pub fn appimages(check_updates: bool) -> Result<Vec<App>, String> {
    let registry = match load_registry_value() {
        Value::Object(m) => m,
        other => {
            return Err(format!(
                "'{}' object has no attribute 'items'",
                py_type(&other)
            ))
        }
    };
    let mut apps = Vec::new();
    // What each update check changed, by key: written back to the registry
    // as it is then, not as it was read here.
    let mut checked: Vec<(String, Map<String, Value>)> = Vec::new();
    let mut keys: Vec<String> = registry.keys().cloned().collect();
    keys.sort();
    for key in keys {
        let mut rec = match registry.get(&key) {
            Some(Value::Object(m)) => m.clone(),
            Some(other) => {
                return Err(format!(
                    "'{}' object has no attribute 'get'",
                    py_type(other)
                ))
            }
            None => Map::new(),
        };
        let path_s = match rec.get("path") {
            None => py_path(""),
            Some(p) if real_str(p).is_some() => py_path(&real_str(p).unwrap_or_default()),
            Some(other) => {
                return Err(format!(
                    "argument should be a str or an os.PathLike object where __fspath__ returns a str, not '{}'",
                    py_type(other)
                ))
            }
        };
        let path = Path::new(&path_s);

        // Ask GitHub at most hourly: a rescan happens on every package change,
        // and the release feed does not move nearly that fast.
        // `rec.get("latest") or {}`: a truthy non-dict is kept, and fails
        // at its first `.get` below unless a check replaces it first.
        let mut cached: Value = match rec.get("latest") {
            Some(v) if truthy(v) => v.clone(),
            _ => json!({}),
        };
        let age = util::now() - float_or_zero(&rec, "checked_at")?;
        if check_updates && path.exists() && age > APPIMAGE_CHECK_SECONDS {
            let found = crate::appimage::check(&Value::Object(rec.clone()))?;
            let found_v = match &found {
                Some(f) => serde_json::to_value(f).unwrap_or_else(|_| json!({})),
                None => json!({}),
            };
            let mut changed = Map::new();
            changed.insert("latest".into(), found_v.clone());
            changed.insert("checked_at".into(), json!(util::now()));
            if let Some(spec) = found_v.get("spec").filter(|s| truthy(s)) {
                changed.insert("update_spec".into(), spec.clone());
            }
            for (k, v) in &changed {
                rec.insert(k.clone(), v.clone());
            }
            checked.push((key.clone(), changed));
            cached = found_v;
        }

        let has_cached = truthy(&cached);
        let cached = match cached {
            Value::Object(m) => m,
            _ if !has_cached => Map::new(),
            other => {
                return Err(format!(
                    "'{}' object has no attribute 'get'",
                    py_type(&other)
                ))
            }
        };
        let cget = |k: &str| -> Value {
            if has_cached {
                cached.get(k).cloned().unwrap_or(json!(""))
            } else {
                json!("")
            }
        };
        let exists = path.exists();
        let meta = if exists {
            std::fs::metadata(path).ok()
        } else {
            None
        };
        let url = match rec.get("url") {
            Some(v) if truthy(v) => v.clone(),
            _ => cget("page"),
        };
        let mut extra = Map::new();
        extra.insert("present".into(), json!(exists));
        extra.insert("url".into(), url);
        extra.insert("key".into(), json!(key));
        extra.insert("download".into(), cget("url"));
        // The asset's published sha256, "" when the release has none.
        extra.insert("sha256".into(), cget("sha256"));
        extra.insert("published".into(), cget("published"));
        extra.insert(
            "installed_ts".into(),
            match &meta {
                Some(m) => {
                    use std::os::unix::fs::MetadataExt;
                    json!(m.mtime() as f64 + m.mtime_nsec() as f64 / 1e9)
                }
                None => json!(0),
            },
        );
        extra.insert(
            "size_mb".into(),
            match &meta {
                Some(m) => json!(m.len() as f64 / 1048576.0),
                None => json!(0),
            },
        );
        extra.insert("is_app".into(), json!(true));
        let field = |k: &str, d: Value| rec.get(k).cloned().unwrap_or(d);
        apps.push(
            App {
                source: APPIMAGE.into(),
                exec_path: path_s.clone(),
                extra: extra.into(),
                ..Default::default()
            }
            .with_values(&[
                ("name", &field("name", json!(key))),
                ("version", &field("version", json!("?"))),
                ("description", &field("description", json!("AppImage"))),
                // A falsy version (None, "") is no update: `has_update` is
                // `bool(update)`.
                ("update", &cget("version")),
            ]),
        );
    }
    if !checked.is_empty() {
        // An OSError here propagates to the caller, as in the Python. The
        // checks took a while: the registry is read again under its lock,
        // and only the checked fields of entries still there change.
        update_registry(|reg| {
            let mut any = false;
            for (key, changed) in checked {
                if let Some(Value::Object(rec)) = reg.get_mut(&key) {
                    rec.extend(changed);
                    any = true;
                }
            }
            Ok(any)
        })?;
    }
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- RegistryWrites
    /// Tests that move XDG variables run one at a time.
    fn with_state<T>(f: impl FnOnce(&Path) -> T) -> T {
        let _g = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let old = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", tmp.path());
        let r = f(tmp.path());
        match old {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
        r
    }

    #[test]
    fn test_save_is_atomic() {
        with_state(|tmp| {
            let target = tmp.join("almanac/appimages.json");
            let data = json!({"a": {"name": "a"}}).as_object().cloned().unwrap();
            save_registry(&data).unwrap();
            let back: Value =
                serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
            assert_eq!(back["a"]["name"], "a");
            // The temp file used for the swap must not be left behind.
            let tmps: Vec<_> = std::fs::read_dir(tmp.join("almanac"))
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "tmp"))
                .collect();
            assert!(tmps.is_empty());
        });
    }

    /// Writers of the registry take turns: one that holds the lock keeps
    /// the others out until it lets go, so none writes back a copy read
    /// before another's change.
    #[test]
    fn registry_writers_take_turns() {
        let _env = util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_STATE_HOME", d.path());
        save_registry(&Map::new()).unwrap();
        let held = lock_registry().unwrap();
        let writer = std::thread::spawn(|| {
            update_registry(|reg| {
                reg.insert("second".into(), json!({"version": "2"}));
                Ok(true)
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            !writer.is_finished(),
            "a writer went ahead while the registry was locked"
        );
        // The holder's own read-modify-write.
        let mut reg = load_registry();
        reg.insert("first".into(), json!({"version": "1"}));
        save_registry(&reg).unwrap();
        drop(held);
        writer.join().unwrap().unwrap();
        let reg = load_registry();
        assert!(
            reg.contains_key("first") && reg.contains_key("second"),
            "{reg:?}"
        );
        // Not JSON object: left alone, and said so.
        std::fs::write(registry(), "[1]").unwrap();
        assert!(update_registry(|_| Ok(true)).is_err());
        assert_eq!(std::fs::read_to_string(registry()).unwrap(), "[1]");
        // Each write has a temporary file of its own, none left behind.
        assert_ne!(registry_tmp(&registry()), registry_tmp(&registry()));
    }

    #[test]
    fn test_corrupt_registry_does_not_raise() {
        with_state(|tmp| {
            let target = tmp.join("almanac/appimages.json");
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, "{ this is not json").unwrap();
            assert!(
                load_registry().is_empty(),
                "a damaged registry must not take the window down"
            );
        });
    }
}
