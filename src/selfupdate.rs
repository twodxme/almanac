//! Port of `selfupdate.py`.
//!
//! Almanac updating Almanac.
//!
//! An app whose whole purpose is telling you what is out of date should not be
//! the one thing on the machine that quietly isn't. It appears in its own Updates
//! list like anything else.
//!
//! How it updates depends on how it was installed, and that is detected rather
//! than assumed:
//!
//! * **git checkout** -- the normal case. `git fetch` then a fast-forward pull.
//!   --ff-only is the point: if local edits or a diverged history would make the
//!   pull a merge, it refuses and says so instead of creating a conflict inside a
//!   running application.
//! * **a distribution package** -- pacman owns it, so this stays out of the way
//!   and lets the package appear in the ordinary update list.
//! * **neither** -- a plain directory someone copied. There is nothing to pull,
//!   so it says that rather than inventing a mechanism.
//!
//! The running process is never swapped underneath itself: new code takes effect
//! on the next launch, and the app offers to restart rather than pretending the
//! update already applied.
//!
//! Where the Python raises past its own `except` clauses (a non-UTF-8 byte in
//! git's output under `text=True`, an encoding ElementTree cannot look up, a
//! description nested deeper than the recursion limit), the `try_*` functions
//! return `Err` with the exception, so callers stop where the Python stops.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::time::Duration;

use num_bigint::BigInt;
use serde::{Deserialize, Serialize};

use crate::util;

mod check;
mod checkout;
mod notes;
pub mod package;
mod process;
mod pystr;
mod restart;
mod tables;

pub use check::*;
pub use checkout::*;
pub use notes::*;
use process::*;
use pystr::*;
pub use restart::*;
use tables::*;

/// The Almanac checkout this binary was built in, if it runs from one.
///
/// Found at run time: a build in a checkout lives under
/// `<checkout>/target/...`, so the checkout is the nearest ancestor of the
/// running binary that holds both `Cargo.toml` and `.git`. A binary that is
/// not in a checkout falls back to where it was built, when that can be
/// trusted (`util::build_checkout`); an installed package gets a folder that
/// does not exist, so nothing is read or run from its build directory.
/// Symlinks are resolved.
pub static ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
    let from_exe = std::env::current_exe()
        .ok()
        .map(|e| py_resolve(&e))
        .and_then(|exe| {
            exe.ancestors()
                .skip(1)
                .find(|d| d.join("Cargo.toml").is_file() && d.join(".git").exists())
                .map(PathBuf::from)
        });
    let root = from_exe
        .or_else(crate::util::build_checkout)
        .unwrap_or_else(|| PathBuf::from("/nonexistent/almanac"));
    PathBuf::from(py_path(py_resolve(&root).as_os_str()))
});

/// `Path.resolve()` (strict=False): symlinks resolved as far as the path
/// exists, the missing rest appended as written.
fn py_resolve(p: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(p) {
        return real;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
            py_resolve(parent).join(name)
        }
        _ => p.to_path_buf(),
    }
}

pub const NAME: &str = "Almanac";
/// The release version, from Cargo.toml. git describe supersedes it in a
/// checkout, so this is what a package or tarball install reports. The
/// shipped metainfo's newest release must match it (a test checks), so the
/// "What's New" window always describes the version that is running.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const GIT: &str = "git";
pub const PACKAGE: &str = "package";
pub const PLAIN: &str = "plain";

pub const METAINFO: &str = "io.github.twodxme.Almanac.metainfo.xml";

/// What an update would consist of (the dict `check` returns).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Info {
    /// One of `GIT`, `PACKAGE`, `PLAIN`.
    pub kind: String,
    pub version: String,
    /// A Python int: whatever `int()` made of git's count, however large.
    #[serde(with = "py_int_json")]
    pub behind: BigInt,
    pub latest: String,
    pub note: String,
    pub can_update: bool,
    pub remote: String,
    pub dirty: bool,
    /// Where a package install came from, as the note was written for
    /// (`None` for any other kind). Not one of the Python dict's keys, so
    /// never serialized.
    #[serde(skip)]
    pub origin: Option<PackageOrigin>,
    /// The package that owns the running binary, for a package install.
    #[serde(skip)]
    pub package: Option<String>,
}

/// Almanac as an entry in its own inventory.
///
/// `as_app`, with an exception the Python lets through as `Err`: the row for
/// this compiled build (`compiled_check_in`).
pub fn try_as_app(check_updates: bool) -> Result<crate::inventory::App, String> {
    let exe = running_exe();
    // A build running from its own checkout fetches from its remote to see
    // what is new: a network round trip every scan, for news that comes
    // a few times a day. Within a few minutes of the last fetch the refs it
    // left say the same thing. ("Check for updates" always fetches.)
    let fetch =
        check_updates && !(exe.starts_with(&*ROOT) && fetched_within(&ROOT, SCAN_FETCH_SECONDS));
    let mut info = compiled_check_in(&ROOT, &exe, fetch)?;
    note_release(&mut info, check_updates);
    app_with_exe(&ROOT, info, &exe)
}

/// How recent a checkout's last fetch must be for a scan to skip its own.
pub const SCAN_FETCH_SECONDS: u64 = 600;

/// Whether `root`'s checkout fetched within the last `secs` seconds, by the
/// FETCH_HEAD every fetch rewrites. A clock that went back counts as not.
pub fn fetched_within(root: &Path, secs: u64) -> bool {
    // A linked worktree's `.git` is a file naming its git directory.
    let dot_git = root.join(".git");
    let git_dir = match std::fs::read_to_string(&dot_git) {
        Ok(text) => match text.trim().strip_prefix("gitdir:") {
            Some(dir) => root.join(dir.trim()),
            None => return false,
        },
        Err(_) => dot_git,
    };
    std::fs::metadata(git_dir.join("FETCH_HEAD"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < secs)
}

/// `as_app` for a compiled build: the same row, from `compiled_check_in`.
/// It carries an update only for the package downloaded from GitHub, which
/// nothing else updates; pacman lists a repository package, and a checkout
/// says to rebuild.
pub fn compiled_as_app_in(
    root: &Path,
    exe: &Path,
    check_updates: bool,
) -> Result<crate::inventory::App, String> {
    let info = compiled_check_in(root, exe, check_updates)?;
    app_with_exe(root, info, exe)
}

/// The row, launched as a checkout's link in ~/.local/bin, or as the binary
/// that is running (a package's, wherever pacman put it).
fn app_with_exe(root: &Path, info: Info, exe: &Path) -> Result<crate::inventory::App, String> {
    let exec_path = if info.kind == GIT {
        py_path_join(&py_home()?, ".local/bin/almanac")
            .to_string_lossy()
            .into_owned()
    } else {
        exe.to_string_lossy().into_owned()
    };
    Ok(app_from(root, info, exec_path))
}

fn app_from(root: &Path, info: Info, exec_path: String) -> crate::inventory::App {
    use serde_json::{json, Map, Value};
    let mut update = String::new();
    if info.can_update {
        update = format!("{} commit(s)", info.behind);
    } else if offers_download(&info) {
        // The GitHub package: nothing else updates it, Almanac does.
        update = info.latest.clone();
    }
    let url = if info.remote.starts_with("http") {
        info.remote.replace(".git", "")
    } else {
        String::new()
    };
    let mut extra = Map::new();
    extra.insert("key".into(), json!("almanac"));
    extra.insert("self".into(), json!(true));
    extra.insert("kind".into(), json!(info.kind));
    extra.insert("repo".into(), json!(root.to_string_lossy()));
    extra.insert("url".into(), Value::String(url));
    extra.insert("note".into(), json!(info.note));
    extra.insert("latest".into(), json!(info.latest));
    extra.insert("git_behind".into(), py_int_json::to_value(&info.behind));
    extra.insert("git_dirty".into(), json!(info.dirty));
    extra.insert("is_app".into(), json!(true));
    if let Some(pkg) = &info.package {
        extra.insert("package".into(), json!(pkg));
    }
    crate::inventory::App {
        name: NAME.to_string(),
        version: info.version.clone(),
        source: crate::inventory::SYSTEM.to_string(),
        description: format!("This application · {}", info.note),
        update,
        exec_path,
        extra: extra.into(),
    }
}

/// Just enough of `xml.etree.ElementTree` for `release_notes`, matching what
/// CPython's C XMLParser gets from expat 2.8 in namespace mode: expat's
/// encoding detection and declarations (an unknown encoding goes through
/// pyexpat's handler, so a Python error can come out of it), its name tables,
/// the internal DTD subset (general entities, attribute defaults and types;
/// parameter entities are never read, so after a reference to one later
/// declarations are skipped unless standalone="yes"), the namespace rules, and
/// `tostring(encoding="unicode")` with its recursion limit.
pub(crate) mod et;

#[cfg(test)]
mod tests {
    #[test]
    fn the_desktop_file_is_named_after_the_app() {
        // Notifications, portals and software centres find an app's desktop
        // file by its id; under another name they show it without its own.
        let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
        let id = "io.github.twodxme.Almanac";
        assert!(docs.join(format!("{id}.desktop")).is_file());
        let meta = std::fs::read_to_string(docs.join(METAINFO)).unwrap();
        assert!(meta.contains(&format!(
            "<launchable type=\"desktop-id\">{id}.desktop</launchable>"
        )));
        for pkgbuild in ["PKGBUILD", "packaging/aur/almanac/PKGBUILD"] {
            let text =
                std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(pkgbuild))
                    .unwrap();
            assert!(
                text.contains(&format!("applications/{id}.desktop")),
                "{pkgbuild}"
            );
        }
    }

    use super::*;

    #[test]
    fn a_github_package_with_a_newer_release_is_listed_as_an_update() {
        // Nothing else updates it: without the row, Update all skipped it.
        let info = Info {
            kind: PACKAGE.into(),
            version: "2.2.0".into(),
            latest: "2.2.1".into(),
            origin: Some(PackageOrigin::Foreign { aur: Some(false) }),
            ..Default::default()
        };
        let app = app_from(Path::new("/x"), info.clone(), String::new());
        assert_eq!(app.update, "2.2.1");
        // A package the AUR or a repository updates is left to them.
        let aur = Info {
            origin: Some(PackageOrigin::Foreign { aur: Some(true) }),
            ..info.clone()
        };
        assert_eq!(app_from(Path::new("/x"), aur, String::new()).update, "");
        let current = Info {
            latest: String::new(),
            ..info
        };
        assert_eq!(app_from(Path::new("/x"), current, String::new()).update, "");
    }

    /// A package install is launched from where pacman put it, and the row
    /// names that package so the inventory can list Almanac once.
    #[test]
    fn a_package_row_launches_the_installed_binary_and_names_its_package() {
        let info = super::Info {
            kind: super::PACKAGE.into(),
            package: Some("almanac".into()),
            ..Default::default()
        };
        let app =
            super::app_with_exe(Path::new("/x"), info, Path::new("/usr/bin/almanac")).unwrap();
        assert_eq!(app.exec_path, "/usr/bin/almanac");
        assert_eq!(
            app.extra_dict().get("package").and_then(|v| v.as_str()),
            Some("almanac")
        );
    }

    #[test]
    fn root_is_the_resolved_checkout_of_the_running_binary() {
        assert!(ROOT.join("Cargo.toml").is_file());
        assert_eq!(*ROOT, std::fs::canonicalize(&*ROOT).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        std::os::unix::fs::symlink(&real, real.join("link")).unwrap();
        assert_eq!(
            py_resolve(&real.join("link/missing/x")),
            real.join("missing/x")
        );
    }

    #[test]
    fn a_scan_skips_the_fetch_only_just_after_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!fetched_within(dir.path(), 600), "never fetched");
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let head = dir.path().join(".git/FETCH_HEAD");
        std::fs::write(&head, "").unwrap();
        assert!(fetched_within(dir.path(), 600));
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(&head)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(
            !fetched_within(dir.path(), 600),
            "an hour-old fetch is asked again"
        );
        // A linked worktree: `.git` names the directory FETCH_HEAD is in.
        let tree = dir.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", dir.path().join("wt").display()),
        )
        .unwrap();
        assert!(!fetched_within(&tree, 600));
        std::fs::create_dir(dir.path().join("wt")).unwrap();
        std::fs::write(dir.path().join("wt/FETCH_HEAD"), "").unwrap();
        assert!(fetched_within(&tree, 600));
    }
}
