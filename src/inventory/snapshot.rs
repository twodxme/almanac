//! The last scan, kept on disk so a new window can show the list at once
//! while its own scan runs.
//!
//! Only what cannot mislead is shown from it: an update the last scan saw is
//! kept only while pacman's databases are exactly as they were then, and
//! only for pacman's own packages. Anything else may have been updated since
//! by other means, and "update available" for something already updated is
//! the one thing the list must never say; the fresh scan puts back whatever
//! is still pending a second later.

use super::*;

/// `$XDG_CACHE_HOME/almanac/inventory.json`.
pub fn snapshot_path() -> PathBuf {
    match std::env::var_os("XDG_CACHE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac/inventory.json"),
        None => crate::util::home().join(".cache").join("almanac/inventory.json"),
    }
}

/// What is written: the rows, and pacman's database stamp (seconds since the
/// epoch, local and sync) when they were read.
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Snapshot {
    stamp: Option<(f64, f64)>,
    apps: Vec<App>,
}

fn stamp_secs() -> Option<(f64, f64)> {
    let secs = |t: std::time::SystemTime| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    pacman_db_stamp().map(|(local, sync)| (secs(local), secs(sync)))
}

/// Keep `apps` as the last scan. A lost write only costs the next window
/// its head start.
pub fn save_snapshot(apps: &[App]) {
    save_snapshot_to(&snapshot_path(), apps, stamp_secs());
}

/// The last scan, as far as it can be trusted (see the module notes); None
/// when there is none or it cannot be read.
pub fn load_snapshot() -> Option<Vec<App>> {
    load_snapshot_from(&snapshot_path(), stamp_secs())
}

pub(super) fn save_snapshot_to(path: &Path, apps: &[App], stamp: Option<(f64, f64)>) {
    // Rows can hold values nested hundreds deep; serde recurses per level.
    let bytes = crate::util::with_stack(crate::util::DEEP_STACK, || {
        serde_json::to_vec(&Snapshot { stamp, apps: apps.to_vec() })
    });
    if let Ok(bytes) = bytes {
        let _ = crate::util::atomic_write(path, bytes);
    }
}

pub(super) fn load_snapshot_from(path: &Path, stamp: Option<(f64, f64)>) -> Option<Vec<App>> {
    let bytes = std::fs::read(path).ok()?;
    let snap: Snapshot = crate::util::with_stack(crate::util::DEEP_STACK, || serde_json::from_slice(&bytes)).ok()?;
    let unchanged = stamp.is_some() && snap.stamp == stamp;
    let mut apps = snap.apps;
    for app in &mut apps {
        let pacman = matches!(app.extra.get("real_source").and_then(Value::as_str), Some(REPO | AUR));
        if !(unchanged && pacman) {
            forget_update(app);
        }
    }
    Some(apps)
}

/// The row as it would read with nothing pending.
fn forget_update(app: &mut App) {
    app.update.clear();
    if let Some(Value::Object(raw)) = app.extra.get_mut(app::RAW_FIELDS) {
        raw.remove("update");
    }
    for (key, none) in [("critical", json!("")), ("behind", json!([0, ""])), ("git_behind", json!(0))] {
        if app.extra.contains_key(key) {
            app.extra.insert(key.into(), none);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, source: &str, real: &str, update: &str) -> App {
        let mut extra = Map::new();
        extra.insert("real_source".into(), json!(real));
        extra.insert("critical".into(), json!(if update.is_empty() { "" } else { "security" }));
        extra.insert("behind".into(), json!([1, "minor"]));
        App { name: name.into(), version: "1.0".into(), source: source.into(), update: update.into(), extra: extra.into(), ..Default::default() }
    }

    #[test]
    fn the_last_scan_comes_back_as_it_was_while_pacman_is_unchanged() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("almanac/inventory.json");
        let apps = vec![row("firefox", REPO, REPO, "2.0"), row("yay", AUR, AUR, "13.0")];
        save_snapshot_to(&path, &apps, Some((1.0, 2.0)));
        assert_eq!(load_snapshot_from(&path, Some((1.0, 2.0))).unwrap(), apps);
    }

    #[test]
    fn an_update_the_last_scan_saw_is_dropped_once_pacman_has_changed() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("inventory.json");
        save_snapshot_to(&path, &[row("firefox", REPO, REPO, "2.0")], Some((1.0, 2.0)));
        for now in [Some((5.0, 2.0)), Some((1.0, 9.0)), None] {
            let back = load_snapshot_from(&path, now).unwrap();
            assert_eq!(back[0].name, "firefox", "the row stays");
            assert!(!back[0].has_update(), "but not its update ({now:?})");
            assert_eq!(back[0].extra.get("critical"), Some(&json!("")));
            assert_eq!(back[0].extra.get("behind"), Some(&json!([0, ""])));
        }
    }

    #[test]
    fn updates_from_outside_pacman_are_never_trusted_from_the_last_scan() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("inventory.json");
        let mut plugin = row("radar", LOCAL, "", "3 commits");
        plugin.extra.remove("real_source");
        plugin.extra.insert("git_behind".into(), json!(3));
        save_snapshot_to(&path, &[plugin, row("omarchy", SYSTEM, REPO, "3.1")], Some((1.0, 2.0)));
        let back = load_snapshot_from(&path, Some((1.0, 2.0))).unwrap();
        assert!(!back[0].has_update());
        assert_eq!(back[0].extra.get("git_behind"), Some(&json!(0)));
        assert!(back[1].has_update(), "Omarchy's own packages are pacman's");
    }

    #[test]
    fn a_missing_or_broken_snapshot_is_none() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("inventory.json");
        assert!(load_snapshot_from(&path, None).is_none());
        std::fs::write(&path, "{not json").unwrap();
        assert!(load_snapshot_from(&path, None).is_none());
    }
}
