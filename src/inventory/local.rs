//! Software built from source here: git checkouts, and how far behind
//! their upstream each is.

use super::*;

pub const GIT_CHECK_SECONDS: f64 = 3600.0;

/// `_GIT_CACHE`: `$XDG_CACHE_HOME/almanac/git.json`.
pub fn git_cache_path() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac/git.json"),
        None => crate::util::home().join(".cache").join("almanac/git.json"),
    }
}

/// `_git_cache` as the Python returns it: whatever JSON the file holds (a
/// list stays a list), `{}` when it is missing or unreadable.
pub fn git_cache_value() -> Value {
    py_read_json(&git_cache_path()).unwrap_or_else(|| json!({}))
}

/// `_git_cache` for callers that work on it as a map. Keys are repo paths
/// (values are `GitBehind` records) plus `"_plugins"`, the cache
/// `plugins::classify` keeps. A cache that is valid JSON but not an object
/// (which the Python hands on as is) is `{}` here.
pub fn git_cache() -> Map<String, Value> {
    match git_cache_value() {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// One `_git_behind` result, as stored in the git cache. Optional fields are
/// absent (not null) in the JSON, exactly like the Python dict.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitBehind {
    pub tracked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_subject: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<bool>,
    pub fetched_at: f64,
}

/// `_git_behind` as the Python dict: the record written into `cache`.
/// Err is what the Python raises outside its try: a cached entry that is
/// truthy but not a dict (AttributeError), or a non-numeric cached
/// `fetched_at` (ValueError).
pub(super) fn git_behind_raw(repo: &Path, cache: &mut Map<String, Value>) -> Result<Map<String, Value>, String> {
    // str(repo): a name that is not UTF-8 keeps its bytes (surrogateescape),
    // so the key is the one the Python writes.
    let key = match path_json(repo) {
        Value::String(k) => k,
        _ => String::new(),
    };
    let entry: Map<String, Value> = match cache.get(&key) {
        Some(Value::Object(m)) => m.clone(),
        Some(v) if truthy(v) => {
            return Err(format!("'{}' object has no attribute 'get'", py_type(v)))
        }
        _ => Map::new(),
    };
    let mut fetched_at = float_or_zero(&entry, "fetched_at")?;

    // OSError (no git), TimeoutExpired and ValueError (output that is not
    // UTF-8 is a UnicodeDecodeError) are the exceptions the Python catches.
    // Text mode also turns a "\r" in a subject into "\n".
    let git = |args: &[&str], timeout: u64| -> Result<RunResult, ()> {
        let mut v: Vec<&std::ffi::OsStr> = vec!["git".as_ref(), "-C".as_ref(), repo.as_os_str()];
        v.extend(args.iter().map(std::ffi::OsStr::new));
        subprocess_run(&v, timeout).map_err(|_| ())
    };

    let attempt = (|| -> Result<Map<String, Value>, ()> {
        if git(&["rev-parse", "--abbrev-ref", "@{upstream}"], 25)?.code != Some(0) {
            let mut r = Map::new();
            r.insert("tracked".into(), json!(false));
            return Ok(r);
        }
        if util::now() - fetched_at >= GIT_CHECK_SECONDS {
            git(&["fetch", "--quiet"], 45)?;
            fetched_at = util::now();
        }
        let behind_out = git(&["rev-list", "--count", "HEAD..@{upstream}"], 25)?;
        let behind = py_strip(&behind_out.stdout).to_string();
        let subject_out = git(&["log", "-1", "--format=%s", "@{upstream}"], 25)?;
        let subject = py_strip(&subject_out.stdout).to_string();
        let dirty = !py_strip(&git(&["status", "--porcelain"], 25)?.stdout).is_empty();
        // int(behind or 0): a non-number is the ValueError the Python catches.
        let behind_n = if behind.is_empty() { "0".to_string() } else { py_int_text(&behind).ok_or(())? };
        let mut r = Map::new();
        r.insert("tracked".into(), json!(true));
        r.insert("behind".into(), int_json(&behind_n));
        r.insert("latest_subject".into(), json!(py_prefix(&subject, 90)));
        r.insert("dirty".into(), json!(dirty));
        Ok(r)
    })();
    let mut result = match attempt {
        Ok(r) => r,
        Err(()) => {
            let mut r = if entry.is_empty() {
                let mut m = Map::new();
                m.insert("tracked".into(), json!(false));
                m
            } else {
                entry.clone()
            };
            r.insert("error".into(), json!(true));
            r
        }
    };

    result.insert("fetched_at".into(), float_json(fetched_at));
    cache.insert(key, Value::Object(result.clone()));
    Ok(result)
}

/// `_git_behind`: how far a checkout is behind its upstream.
///
/// Only the `git fetch` is cached -- it is a network round trip per repo, so
/// it runs at most once an hour. Counting commits against the fetched refs is
/// local and cheap, so it is done every time: a cached count kept a plugin
/// "901 commits behind" for an hour after it had been pulled. A repo with no
/// upstream, or an unreachable remote, returns nothing rather than a
/// misleading zero -- "up to date" and "could not check" are different answers.
///
/// Writes the result back into `cache` under the repo path. Err where the
/// Python raises: a cached `fetched_at` that is not a number.
pub fn git_behind(repo: &Path, cache: &mut Map<String, Value>) -> Result<GitBehind, String> {
    let raw = git_behind_raw(repo, cache)?;
    // Field by field: a fetched_at of inf is a float, not the str a Value
    // carries it as.
    let fetched_at = raw.get("fetched_at").and_then(py_float_value).unwrap_or(0.0);
    let mut r: GitBehind = serde_json::from_value(Value::Object(
        raw.into_iter().filter(|(k, _)| k != "fetched_at").collect(),
    ))
    .unwrap_or_default();
    r.fetched_at = fetched_at;
    Ok(r)
}

/// `_github_page`: a git remote as a page a browser can open, or ''.
pub fn github_page(url: &str) -> String {
    let u = py_strip(url);
    let mut url = u.strip_suffix(".git").unwrap_or(u).to_string();
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        url = format!("https://github.com/{rest}");
    }
    if url.starts_with("https://") { url } else { String::new() }
}

/// The Almanac checkout the running code came from, symlinks resolved. A
/// binary cargo built inside a checkout (`<checkout>/target/.../almanac`)
/// stands for the checkout it was built in (the rule `i18n::localedir` uses). A binary that was copied elsewhere
/// (`~/.local/bin`, `~/.cargo/bin`) still came from the checkout it was
/// built in, so that is the one left to its self-update; only when that
/// checkout is gone does the program's own directory stand in.
pub(super) fn own_checkout() -> PathBuf {
    let built_from = crate::util::build_checkout();
    let exe = match std::env::current_exe() {
        Ok(e) => resolve(&e),
        // No /proc: the checkout this binary was built from.
        Err(_) => return built_from.unwrap_or_default(),
    };
    let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
    let built_in = dir
        .ancestors()
        .find(|a| {
            a.file_name().is_some_and(|n| n == "target")
                && a.parent().is_some_and(|c| c.join("Cargo.toml").is_file())
        })
        .and_then(|t| t.parent())
        .map(PathBuf::from);
    match built_in {
        Some(c) => resolve(&c),
        None => built_from.unwrap_or_else(|| resolve(&dir)),
    }
}

/// `Path.resolve()` (non-strict): never fails.
pub(super) fn resolve(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// `sorted(d.glob(pattern))` for a one-level pattern: entries of `d` (dot
/// files included, as pathlib's glob includes them), sorted by name.
pub(super) fn sorted_children(d: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(d) {
        Ok(rd) => rd.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    v.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    v
}

/// A file name as Rust text. (`App.name` is a `String`, which cannot hold
/// the surrogates Python keeps for a name that is not UTF-8: such a name
/// shows with U+FFFD. The paths handed to git and kept in `extra` are exact.)
pub(super) fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Software built from source here. Listed so the inventory is honest about
/// what exists; these update through their own repos, not a package manager.
/// (Python default: `check_updates=True`.)
///
/// Err where the Python raises (`collect` then drops the section, as the
/// Python's catch-all does): a git cache that is not an object, or with an
/// entry that is not one or whose `fetched_at` is not a number; git output
/// that is not UTF-8; a plugin manifest that is valid JSON but not an object.
pub fn local_builds(check_updates: bool) -> Result<Vec<App>, String> {
    let mut apps = Vec::new();
    let mut cache = if check_updates {
        match git_cache_value() {
            Value::Object(m) => m,
            // Every path below reaches `cache.get` or `cache.setdefault`.
            other => {
                return Err(format!("'{}' object has no attribute 'setdefault'", py_type(&other)))
            }
        }
    } else {
        Map::new()
    };
    let mut dirty_cache = false;
    // Which plugins Omarchy has switched on is a script run of its own
    // (`omarchy plugin list`): asked now, beside the git work below.
    let pdir = crate::plugins::plugins_dir();
    let pdir_ok = pdir.is_dir();
    let switched_on = pdir_ok.then(|| std::thread::spawn(crate::plugins::enabled));

    let git_extra =|repo: &Path, cache: &mut Map<String, Value>, dirty_cache: &mut bool| -> Result<Map<String, Value>, String> {
        if !check_updates || !repo.join(".git").exists() {
            return Ok(Map::new());
        }
        let info = git_behind_raw(repo, cache)?;
        *dirty_cache = true;
        let get = |k: &str, d: Value| info.get(k).cloned().unwrap_or(d);
        let mut g = Map::new();
        g.insert("git_behind".into(), get("behind", json!(0)));
        g.insert("git_subject".into(), get("latest_subject", json!("")));
        g.insert("git_dirty".into(), get("dirty", json!(false)));
        g.insert("git_tracked".into(), get("tracked", json!(false)));
        Ok(g)
    };

    // Anything cloned straight into the data directory and built there --
    // a driver, a tool installed with `make install PREFIX=~/.local`. Found
    // by being a git checkout, not by name; Almanac's own checkout is left
    // to its self-update.
    let own = own_checkout();
    for child in sorted_children(&crate::xdg::data_home()) {
        // glob("*/.git"): the entry exists, even as a dangling link.
        if std::fs::symlink_metadata(child.join(".git")).is_err() {
            continue;
        }
        let repo = child;
        if resolve(&repo) == own {
            continue;
        }
        // The path as the bytes it is: git must find the directory even when
        // its name is not UTF-8.
        let git_argv = |args: &[&'static str]| -> Vec<std::ffi::OsString> {
            let mut v: Vec<std::ffi::OsString> = vec!["git".into(), "-C".into(), repo.clone().into_os_string()];
            v.extend(args.iter().map(std::ffi::OsString::from));
            v
        };
        let rev = py_strip(&run_os(&git_argv(&["describe", "--always", "--dirty"]), 25)?).to_string();
        let origin = py_strip(&run_os(&git_argv(&["remote", "get-url", "origin"]), 25)?).to_string();
        let g = git_extra(&repo, &mut cache, &mut dirty_cache)?;
        let launcher = crate::xdg::bin_home().join(repo.file_name().unwrap_or_default());
        let mut extra = g.clone();
        extra.insert("repo".into(), path_json(&repo));
        extra.insert("is_app".into(), json!(false));
        let update = match g.get("git_behind") {
            Some(b) if truthy(b) => format!("{} commits", py_str(b)),
            _ => String::new(),
        };
        // `repo.name` and `str(launcher)` keep a name that is not UTF-8 as
        // surrogates.
        let exec = if launcher.exists() { path_json(&launcher) } else { json!("") };
        apps.push(App {
            version: if rev.is_empty() { "git".into() } else { rev },
            source: LOCAL.into(),
            description: format!(
                "Built from source{}",
                if origin.is_empty() { String::new() } else { format!(" · {origin}") }
            ),
            update,
            extra: extra.into(),
            ..Default::default()
        }
        .with_values(&[("name", &path_json(Path::new(repo.file_name().unwrap_or_default()))), ("exec_path", &exec)]));
    }
    // Omarchy shell plugins: classified before anything offers to touch them
    // (plugins.py), with whether Omarchy currently has each one switched on.
    let switched_on = switched_on.map(|h| h.join().unwrap_or_default()).unwrap_or_default();
    // pcache = cache.setdefault("_plugins", {}): an existing value is used
    // as it is, whatever it is.
    if !cache.contains_key("_plugins") {
        cache.insert("_plugins".into(), json!({}));
    }
    let children: Vec<PathBuf> =
        if pdir_ok { sorted_children(&pdir).into_iter().filter(|p| p.is_dir()).collect() } else { Vec::new() };
    // Each plugin is a few git calls and, when due, a fetch: classified all
    // at once (`plugins::classify_all`, as the Plugins page does) when the
    // cache is the usual map. Anything else goes one at a time below, with
    // the Python's errors.
    let mut classified: std::collections::VecDeque<crate::plugins::State> = match cache.get_mut("_plugins") {
        Some(Value::Object(pc)) => crate::plugins::classify_each(&children, Some(pc), check_updates).into(),
        _ => Default::default(),
    };
    for p in children {
        let pname = path_json(Path::new(p.file_name().unwrap_or_default()));
        let (mut desc, mut ver, mut pid) = (json!("Omarchy shell plugin"), json!("—"), pname.clone());
        // OSError and ValueError (bad JSON, or text that is not UTF-8) are
        // caught; a document that is not an object fails at `m.get`.
        if let Ok(text) = std::fs::read_to_string(p.join("manifest.json")) {
            match py_loads(&text) {
                Ok(Value::Object(m)) => {
                    desc = m.get("description").cloned().unwrap_or(desc);
                    ver = m.get("version").cloned().unwrap_or(ver);
                    if let Some(id) = m.get("id").filter(|v| truthy(v)) {
                        pid = id.clone();
                    }
                }
                Ok(other) => {
                    return Err(format!("'{}' object has no attribute 'get'", py_type(&other)))
                }
                Err(_) => {}
            }
        }
        let st = match cache.get_mut("_plugins") {
            Some(Value::Object(pc)) => match classified.pop_front() {
                Some(st) => st,
                None => crate::plugins::classify(&p, Some(pc), check_updates),
            },
            // None is classify's "no cache".
            Some(Value::Null) | None => crate::plugins::classify(&p, None, check_updates),
            // Anything else is handed to classify as is, which only fails
            // once `_remote` uses it: `(cache or {}).get(...)` on a truthy
            // non-dict, or `cache[key] = ...` after a fetch on a falsy one.
            Some(other) => {
                let other = other.clone();
                let st = crate::plugins::classify(&p, None, check_updates);
                let before_remote = ["symlink", "not-git", "no-origin", "no-commits", "detached"]
                    .contains(&st.detail.as_str())
                    || (st.kind == crate::plugins::ERROR && st.detail != "fetch failed");
                if !before_remote {
                    if truthy(&other) {
                        return Err(format!("'{}' object has no attribute 'get'", py_type(&other)));
                    }
                    if check_updates && st.detail != "fetch failed" {
                        return Err(format!(
                            "'{}' object does not support item assignment",
                            py_type(&other)
                        ));
                    }
                }
                st
            }
        };
        // Only a run that checked has anything worth saving; without the
        // check the cache started empty, and saving it would erase the last
        // fetch results.
        dirty_cache = dirty_cache || check_updates;
        // switched_on.get(pid): only a string id can match; an unhashable
        // one is a TypeError.
        let on = match &pid {
            Value::String(id) if exotic(&pid).is_none() => switched_on.get(id).copied(),
            Value::Array(_) | Value::Object(_) => {
                return Err(format!("unhashable type: '{}'", py_type(&pid)))
            }
            _ => None,
        };
        // An edit with an update waiting is still offered, through the
        // patches.py weighing; ahead, diverged, detached and symlinked
        // plugins are not, because no update is right for them.
        let behind = if st.updatable() || st.detail == "modified" { st.behind } else { 0 };
        let mut extra = Map::new();
        extra.insert("repo".into(), path_json(&p));
        extra.insert("is_app".into(), json!(false));
        extra.insert("plugin_id".into(), pid.clone());
        extra.insert("plugin_state".into(), json!(st.kind));
        extra.insert("plugin_detail".into(), json!(st.describe()));
        extra.insert("enabled".into(), json!(on));
        extra.insert("url".into(), json!(github_page(&st.url)));
        extra.insert("git_behind".into(), json!(behind));
        extra.insert("git_dirty".into(), json!(st.detail == "modified"));
        extra.insert("git_tracked".into(), json!(st.kind != crate::plugins::LOCAL));
        extra.insert("git_subject".into(), json!(""));
        let desc = if on != Some(false) { desc } else { json!(format!("Turned off · {}", py_str(&desc))) };
        apps.push(
            App {
                source: LOCAL.into(),
                update: if behind != 0 { format!("{behind} commits") } else { String::new() },
                extra: extra.into(),
                ..Default::default()
            }
            .with_values(&[("name", &pname), ("version", &ver), ("description", &desc)]),
        );
    }

    if dirty_cache {
        let path = git_cache_path();
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            crate::util::atomic_write(&path, py_dumps(&Value::Object(cache.clone()), Some(2)))
        };
        let _ = write(); // a lost cache only costs a refetch
    }
    Ok(apps)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- UpdatesClearAfterUpdating
    #[test]
    fn test_commit_count_is_recounted_while_the_fetch_is_cached() {
        let d = tempfile::tempdir().unwrap();
        let git = |a: &[&str], cwd: &Path| {
            let mut v = vec!["git", "-c", "user.name=t", "-c", "user.email=t@t"];
            v.extend_from_slice(a);
            let out = util::run_with(&v, &util::RunOpts { cwd: Some(cwd), ..Default::default() });
            assert!(out.ok(), "{:?}: {}", a, out.stderr);
        };
        let (up, clone) = (d.path().join("up"), d.path().join("clone"));
        git(&["init", "-q", "-b", "main", up.to_str().unwrap()], d.path());
        git(&["commit", "-q", "--allow-empty", "-m", "one"], &up);
        git(&["clone", "-q", up.to_str().unwrap(), clone.to_str().unwrap()], d.path());
        git(&["commit", "-q", "--allow-empty", "-m", "two"], &up);

        let mut cache = Map::new();
        assert_eq!(git_behind(&clone, &mut cache).unwrap().behind, Some(1));
        git(&["pull", "-q", "--ff-only"], &clone);
        // Inside the fetch window: no network, but the count is fresh.
        assert_eq!(git_behind(&clone, &mut cache).unwrap().behind, Some(0));
    }

    // ---- AnyonesMachine
    #[test]
    fn test_local_builds_are_found_by_being_checkouts_not_by_name() {
        let _g = crate::util::TEST_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("some-driver");
        assert!(util::run(&["git", "init", "-q", repo.to_str().unwrap()], 30).ok());
        let saved: Vec<_> = ["XDG_DATA_HOME", "XDG_CONFIG_HOME"]
            .iter()
            .map(|k| (k.to_string(), std::env::var_os(k)))
            .collect();
        for (k, _) in &saved {
            std::env::set_var(k, d.path());
        }
        let names: Vec<String> = std::panic::catch_unwind(|| {
            local_builds(false).unwrap().into_iter().map(|a| a.name).collect()
        })
        .unwrap_or_default();
        for (k, v) in saved {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        assert!(names.contains(&"some-driver".to_string()), "{names:?}");
    }
}
