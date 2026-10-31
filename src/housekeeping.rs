//! Housekeeping for what Almanac keeps on disk, which otherwise only grows:
//! card thumbnails and GitHub API answers in `~/.cache/almanac` that nothing
//! has used for a month, cache files no version reads any more, and AppImage
//! rollback copies and download leftovers of apps that are no longer
//! registered.
//!
//! `run_if_due` does it at most once a day; the window calls it on a
//! background thread at start-up. Nothing here fails loudly: a file that
//! cannot be read or removed is left for next time.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

/// A cached thumbnail or GitHub answer not used for this long goes.
pub const UNUSED_FOR: Duration = Duration::from_secs(30 * 24 * 3600);

/// How often `run_if_due` runs.
pub const EVERY: Duration = Duration::from_secs(24 * 3600);

/// A leftover of an unregistered AppImage this young is left alone: it may
/// be a download in progress whose app is registered only once it is done.
pub const ORPHAN_AGE: Duration = Duration::from_secs(24 * 3600);

/// Cache files earlier versions wrote and nothing reads any more.
const STALE: &[&str] = &["store-catalog.json"];

/// What an AppImage leaves beside `<key>.AppImage` in the apps directory
/// (see `appimage::download_verified` and `sources::install_appimage`),
/// longest first so `.previous.new` is not read as `.new`.
const LEFTOVERS: &[&str] = &[
    ".previous.new",
    ".download.lock",
    ".previous",
    ".download",
    ".unverified",
];

/// What one pass removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Swept {
    pub files: usize,
    pub bytes: u64,
}

impl Swept {
    fn add(&mut self, other: Swept) {
        self.files += other.files;
        self.bytes += other.bytes;
    }
}

/// The file whose mtime says when housekeeping last ran.
fn stamp() -> PathBuf {
    crate::store::cache().join(".housekeeping")
}

/// `run` once per `EVERY`, judged by the stamp file; None when it was not
/// due. Never in demo mode, which must not touch the machine's files.
pub fn run_if_due() -> Option<Swept> {
    if crate::demo::active() {
        return None;
    }
    let now = SystemTime::now();
    let stamp = stamp();
    let last = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok();
    if last.is_some_and(|t| now.duration_since(t).is_ok_and(|age| age < EVERY)) {
        return None;
    }
    // Stamped first: two windows starting together sweep once.
    if let Some(parent) = stamp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&stamp, b"");
    Some(run(now))
}

/// One pass over everything above.
pub fn run(now: SystemTime) -> Swept {
    let mut swept = Swept::default();
    if let Some(cutoff) = now.checked_sub(UNUSED_FOR) {
        swept.add(prune_unused(&crate::store::thumbs(), cutoff));
        swept.add(prune_unused(&crate::ghcache::dir(), cutoff));
    }
    let cache = crate::store::cache();
    for name in STALE {
        swept.add(remove(&cache.join(name)));
    }
    if let Some(cutoff) = now.checked_sub(ORPHAN_AGE) {
        swept.add(prune_orphans(
            &crate::inventory::state().join("apps"),
            cutoff,
        ));
    }
    swept
}

/// Remove one regular file, counting it.
fn remove(path: &Path) -> Swept {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && std::fs::remove_file(path).is_ok() => Swept {
            files: 1,
            bytes: m.len(),
        },
        _ => Swept::default(),
    }
}

/// The regular files directly in `dir` last modified before `cutoff`.
fn older_than(dir: &Path, cutoff: SystemTime) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.metadata()
                .ok()
                .filter(|m| m.is_file())
                .and_then(|m| m.modified().ok())
                .is_some_and(|t| t < cutoff)
        })
        .map(|e| e.path())
        .collect()
}

/// Files in a cache directory not written (or, for thumbnails, used: see
/// `store::fetch_thumb`) since `cutoff`. Both caches refetch what they miss.
pub fn prune_unused(dir: &Path, cutoff: SystemTime) -> Swept {
    let mut swept = Swept::default();
    for path in older_than(dir, cutoff) {
        swept.add(remove(&path));
    }
    swept
}

/// Rollback copies (`<key>.previous`) and download leftovers in the apps
/// directory whose `<key>` no registered AppImage has, older than `cutoff`.
///
/// The registry is read under its lock, so no install or removal changes it
/// meanwhile. A registry that cannot be read leaves everything in place:
/// read as empty, it would make every rollback copy look orphaned.
pub fn prune_orphans(apps: &Path, cutoff: SystemTime) -> Swept {
    let mut swept = Swept::default();
    let candidates: Vec<(PathBuf, String)> = older_than(apps, cutoff)
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_str()?.to_string();
            let stem = LEFTOVERS
                .iter()
                .find_map(|s| name.strip_suffix(s))?
                .to_string();
            Some((p, stem))
        })
        .collect();
    if candidates.is_empty() {
        return swept;
    }
    let Ok(_lock) = crate::inventory::lock_registry() else {
        return swept;
    };
    let Some(registered) = registered_stems(apps) else {
        return swept;
    };
    for (path, stem) in candidates {
        if !registered.contains(&stem) {
            swept.add(remove(&path));
        }
    }
    swept
}

/// The `<key>` of every registered AppImage in `apps`: each entry's key, and
/// the stem of its `path` when that lies in `apps`. None when the registry
/// cannot be read as an object.
fn registered_stems(apps: &Path) -> Option<Vec<String>> {
    let reg = crate::inventory::registry();
    let Value::Object(map) = crate::inventory::load_registry_value() else {
        return None;
    };
    if map.is_empty() && std::fs::read_to_string(&reg).is_ok_and(|t| t.trim() != "{}") {
        return None; // there, but not readable: `load_registry_value` said {}
    }
    let mut stems: Vec<String> = map.keys().cloned().collect();
    for entry in map.values() {
        let Some(path) = entry.get("path").and_then(Value::as_str) else {
            continue;
        };
        let path = Path::new(path);
        if path.parent() == Some(apps) {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                stems.push(stem.to_string());
            }
        }
    }
    Some(stems)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aged(path: &Path, age: Duration) {
        std::fs::write(path, b"x").unwrap();
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
    }

    const DAY: Duration = Duration::from_secs(24 * 3600);

    #[test]
    fn unused_cache_files_go_and_recent_ones_stay() {
        let d = tempfile::tempdir().unwrap();
        aged(&d.path().join("old.webp"), 31 * DAY);
        aged(&d.path().join("new.webp"), 2 * DAY);
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let swept = prune_unused(d.path(), SystemTime::now() - UNUSED_FOR);
        assert_eq!(swept, Swept { files: 1, bytes: 1 });
        assert!(!d.path().join("old.webp").exists());
        assert!(d.path().join("new.webp").exists());
        assert!(d.path().join("sub").is_dir());
        // a missing directory is nothing to do
        assert_eq!(
            prune_unused(&d.path().join("nope"), SystemTime::now()),
            Swept::default()
        );
    }

    /// Runs `f` with the state and cache directories in a temp dir.
    fn sandboxed(f: impl FnOnce(&Path)) {
        let _env = crate::util::TEST_ENV
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let (state, cache) = (
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("XDG_CACHE_HOME"),
        );
        std::env::set_var("XDG_STATE_HOME", d.path().join("state"));
        std::env::set_var("XDG_CACHE_HOME", d.path().join("cache"));
        f(d.path());
        for (k, v) in [("XDG_STATE_HOME", state), ("XDG_CACHE_HOME", cache)] {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
    }

    #[test]
    fn rollback_copies_of_unregistered_apps_go_once_a_day_old() {
        sandboxed(|_| {
            let apps = crate::inventory::state().join("apps");
            std::fs::create_dir_all(&apps).unwrap();
            let kept = apps.join("kept.AppImage");
            let reg = serde_json::json!({
                "kept": {"path": kept.to_str().unwrap()},
                "byname": {"path": apps.join("other-name.AppImage").to_str().unwrap()},
            });
            std::fs::write(crate::inventory::registry(), reg.to_string()).unwrap();
            for name in [
                "kept.previous",
                "kept.download",
                "other-name.previous",
                "gone.previous",
                "gone.unverified",
                "gone.previous.new",
                "gone.AppImage",
                "notes.txt",
            ] {
                aged(&apps.join(name), 3 * DAY);
            }
            aged(&apps.join("fresh.download"), Duration::from_secs(60));
            let swept = prune_orphans(&apps, SystemTime::now() - ORPHAN_AGE);
            assert_eq!(swept.files, 3);
            for gone in ["gone.previous", "gone.unverified", "gone.previous.new"] {
                assert!(!apps.join(gone).exists(), "{gone}");
            }
            // registered apps keep theirs; the app itself, anything that is
            // not a leftover, and a download in progress are not touched
            for kept in [
                "kept.previous",
                "kept.download",
                "other-name.previous",
                "gone.AppImage",
                "notes.txt",
                "fresh.download",
            ] {
                assert!(apps.join(kept).exists(), "{kept}");
            }
        });
    }

    #[test]
    fn an_unreadable_registry_keeps_every_rollback_copy() {
        sandboxed(|_| {
            let apps = crate::inventory::state().join("apps");
            std::fs::create_dir_all(&apps).unwrap();
            aged(&apps.join("app.previous"), 3 * DAY);
            std::fs::write(crate::inventory::registry(), "{ not json").unwrap();
            assert_eq!(
                prune_orphans(&apps, SystemTime::now() - ORPHAN_AGE),
                Swept::default()
            );
            std::fs::write(crate::inventory::registry(), "[]").unwrap();
            assert_eq!(
                prune_orphans(&apps, SystemTime::now() - ORPHAN_AGE),
                Swept::default()
            );
            assert!(apps.join("app.previous").exists());
            // an empty registry that says so is read as empty
            std::fs::write(crate::inventory::registry(), "{}\n").unwrap();
            assert_eq!(
                prune_orphans(&apps, SystemTime::now() - ORPHAN_AGE).files,
                1
            );
        });
    }

    #[test]
    fn a_pass_runs_once_a_day_and_drops_known_stale_files() {
        sandboxed(|_| {
            let cache = crate::store::cache();
            std::fs::create_dir_all(cache.join("thumbs")).unwrap();
            std::fs::write(cache.join("store-catalog.json"), b"{}").unwrap();
            aged(&cache.join("thumbs/old.png"), 40 * DAY);
            let swept = run_if_due().expect("due on the first run");
            assert_eq!(swept.files, 2);
            assert!(!cache.join("store-catalog.json").exists());
            assert!(run_if_due().is_none(), "not due again the same day");
        });
    }
}
