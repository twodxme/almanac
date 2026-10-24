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

mod notes;
mod checkout;
mod check;
mod restart;
mod process;
mod pystr;
mod tables;

pub use notes::*;
pub use checkout::*;
pub use check::*;
pub use restart::*;
use process::*;
use pystr::*;
use tables::*;

/// The Almanac checkout this binary was built in, if it runs from one.
///
/// Found at run time: a build in a checkout lives under
/// `<checkout>/target/...`, so the checkout is the nearest ancestor of the
/// running binary that holds both `Cargo.toml` and `.git`. A binary that is
/// not in a checkout (an installed package) falls back to where it was built,
/// which `compiled_check_in` only trusts when the binary lives inside it.
/// Symlinks are resolved.
pub static ROOT: LazyLock<PathBuf> = LazyLock::new(|| {
    let from_exe = std::env::current_exe().ok().map(|e| py_resolve(&e)).and_then(|exe| {
        exe.ancestors()
            .skip(1)
            .find(|d| d.join("Cargo.toml").is_file() && d.join(".git").exists())
            .map(PathBuf::from)
    });
    let root = from_exe.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    PathBuf::from(py_path(py_resolve(&root).as_os_str()))
});

/// `Path.resolve()` (strict=False): symlinks resolved as far as the path
/// exists, the missing rest appended as written.
fn py_resolve(p: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(p) {
        return real;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => py_resolve(parent).join(name),
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
}

/// Almanac as an entry in its own inventory.
///
/// `as_app`, with an exception the Python lets through as `Err`: the row for
/// this compiled build (`compiled_check_in`).
pub fn try_as_app(check_updates: bool) -> Result<crate::inventory::App, String> {
    let mut info = compiled_check_in(&ROOT, &running_exe(), check_updates)?;
    note_release(&mut info, check_updates);
    app_with_home(&ROOT, info)
}

/// `as_app` for a compiled build: the same row, from `compiled_check_in`. It
/// never carries an update (can_update is never set), so it stays out of the
/// Updates list: pacman lists the package; a checkout says to rebuild.
pub fn compiled_as_app_in(root: &Path, exe: &Path, check_updates: bool) -> Result<crate::inventory::App, String> {
    let info = compiled_check_in(root, exe, check_updates)?;
    app_with_home(root, info)
}

fn app_with_home(root: &Path, info: Info) -> Result<crate::inventory::App, String> {
    let exec_path = py_path_join(&py_home()?, ".local/bin/almanac").to_string_lossy().into_owned();
    Ok(app_from(root, info, exec_path))
}

fn app_from(root: &Path, info: Info, exec_path: String) -> crate::inventory::App {
    use serde_json::{json, Map, Value};
    let mut update = String::new();
    if info.can_update {
        update = format!("{} commit(s)", info.behind);
    }
    let url = if info.remote.starts_with("http") { info.remote.replace(".git", "") } else { String::new() };
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
mod et;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_the_resolved_checkout_of_the_running_binary() {
        assert!(ROOT.join("Cargo.toml").is_file());
        assert_eq!(*ROOT, std::fs::canonicalize(&*ROOT).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        std::os::unix::fs::symlink(&real, real.join("link")).unwrap();
        assert_eq!(py_resolve(&real.join("link/missing/x")), real.join("missing/x"));
    }
}

