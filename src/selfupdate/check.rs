//! What an update would consist of: a checkout's commits, a package's origin
//! and the newest release, and the compiled build's own rules.

use super::*;

/// What an update would consist of, without performing one: kind,
/// version, behind, latest, note, can_update.
///
/// `check`, with an exception the Python lets through as `Err`. This is a
/// compiled build, so it is `compiled_check_in` for the running binary.
pub fn try_check(fetch: bool) -> Result<Info, String> {
    let mut info = compiled_check_in(&ROOT, &running_exe(), fetch)?;
    note_release(&mut info, fetch);
    Ok(info)
}

/// Where a release is published, with the package to download.
pub const RELEASES_URL: &str = "https://github.com/twodxme/almanac/releases/latest";
pub(super) const RELEASE_REPO: &str = "twodxme/almanac";

/// How long GitHub's answer about Almanac's own newest release is kept:
/// minutes, not the hours other repositories get, so a release shows on
/// the Updates page soon after it is published. One request per quarter
/// hour is well inside the API budget.
pub(super) const RELEASE_TTL: u64 = 15 * 60;

/// Where pacman's copy of Almanac came from, as far as can be told without
/// guessing. It decides what the note promises: only a package something
/// keeps updating may be said to update with the rest of the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageOrigin {
    /// A sync repository has a package of this name, so `pacman -Syu`
    /// replaces it with each new one.
    Repo,
    /// No repository has it (`pacman -Qm` lists it): the package downloaded
    /// from GitHub, one built with makepkg, or one from the AUR. `aur` is
    /// whether the AUR lists this project under that name -- `None` when it
    /// was not asked or did not answer, which is never taken as a yes.
    Foreign { aur: Option<bool> },
    /// pacman could not say (no owner, or `pacman -Qm` failed).
    Unknown,
}

/// What the note says for a copy pacman owns, by where the package came
/// from. An AUR helper updates a foreign package only when the AUR has one
/// by that name, and a package installed with `pacman -U` from the releases
/// page is updated by nothing at all: the note says so rather than implying
/// an update will come.
pub fn package_note(pkg: &str, origin: PackageOrigin) -> String {
    match origin {
        PackageOrigin::Repo => format!(
            "Installed as the {pkg} package from your distribution's repositories, \
             so it updates with the rest of the system."
        ),
        PackageOrigin::Foreign { aur: Some(true) } => format!(
            "Installed as the {pkg} package, which no repository carries but the AUR does, \
             so an AUR helper such as yay or paru updates it with the rest of the system."
        ),
        PackageOrigin::Foreign { .. } => format!(
            "Installed as the {pkg} package from outside your repositories (the package \
             downloaded from GitHub, or one built with makepkg), so nothing updates it by \
             itself. Download the latest one from {RELEASES_URL} and install it with \
             sudo pacman -U."
        ),
        PackageOrigin::Unknown => format!(
            "Installed as the {pkg} package. Update it with your package manager; \
             if you installed the package downloaded from GitHub, download the \
             latest one from {RELEASES_URL} and install it with sudo pacman -U."
        ),
    }
}

/// Whether "check for updates" should offer the releases page: a newer
/// release is out and the note says nothing updates this package by itself
/// (a foreign package the AUR does not carry). A repository or AUR package
/// is updated by pacman or the AUR helper, so pointing it at a `pacman -U`
/// download would steer it off the path that keeps it current; there, and
/// when the origin is unknown, the note is said as before.
pub fn offers_download(info: &Info) -> bool {
    !info.can_update
        && info.kind == PACKAGE
        && !info.latest.is_empty()
        && matches!(
            info.origin,
            Some(PackageOrigin::Foreign {
                aur: None | Some(false)
            })
        )
}

/// `pacman -Qmq <pkg>` read as a [`PackageOrigin`]. Local and quick: no
/// network. The AUR is asked separately (`aur_listed`).
pub(super) fn package_origin(pkg: &str) -> PackageOrigin {
    if pkg.is_empty() {
        return PackageOrigin::Unknown;
    }
    // The inventory's foreign list, kept while pacman's databases stand
    // still: asking pacman about one package reads every sync database.
    match crate::inventory::is_foreign(pkg) {
        Some(true) => return PackageOrigin::Foreign { aur: None },
        Some(false) => return PackageOrigin::Repo,
        None => {}
    }
    let argv: Vec<OsString> = vec!["pacman".into(), "-Qmq".into(), pkg.into()];
    match run_capture(&argv, Some(20)) {
        Ok(out) => origin_from(
            out.code,
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        ),
        Err(_) => PackageOrigin::Unknown,
    }
}

/// The decision part of [`package_origin`], on what pacman printed. The
/// package is known to be installed (it owns the running binary), so exit 1
/// with nothing said means "not foreign"; anything else pacman says (an
/// error, a timeout) is `Unknown` rather than a guess.
pub(super) fn origin_from(code: i32, stdout: &str, stderr: &str) -> PackageOrigin {
    match (code, stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (0, false, _) => PackageOrigin::Foreign { aur: None },
        (1, true, true) => PackageOrigin::Repo,
        _ => PackageOrigin::Unknown,
    }
}

/// Whether an AUR answer (`/rpc/v5/info`) lists `pkg` as this project: that
/// name, with this repository as its upstream. A package of the same name
/// pointing somewhere else is not Almanac, and an AUR helper "updating" to
/// it would not be an update, so it counts as not listed. `None` for an
/// answer with no results list (an RPC error): that is no answer at all.
/// (`aur_listed` reads the same rule off the inventory's kept answer.)
#[cfg(test)]
pub(super) fn aur_lists(answer: &serde_json::Value, pkg: &str) -> Option<bool> {
    let results = answer.get("results")?.as_array()?;
    Some(results.iter().any(|r| {
        r.get("Name").and_then(|n| n.as_str()) == Some(pkg)
            && r.get("URL")
                .and_then(|u| u.as_str())
                .is_some_and(is_this_project)
    }))
}

/// Whether an AUR package's upstream URL is this repository.
pub(super) fn is_this_project(url: &str) -> bool {
    crate::sources::norm(url) == crate::sources::norm(&format!("https://github.com/{RELEASE_REPO}"))
}

/// What the AUR said about a package name, as last asked by this process.
pub(super) static AUR_LISTED: std::sync::Mutex<Option<(String, bool)>> =
    std::sync::Mutex::new(None);

/// Whether the AUR carries `pkg` as this project. With `fetch`, ask it --
/// through the inventory's kept AUR answer, which the scan's question about
/// every foreign package usually holds already; without, use what this
/// process last heard, so the About window does not wait on the network.
/// `None` when nobody knows.
pub(super) fn aur_listed(pkg: &str, fetch: bool) -> Option<bool> {
    if !fetch {
        let seen = AUR_LISTED.lock().ok()?;
        return seen
            .as_ref()
            .filter(|(name, _)| name == pkg)
            .map(|(_, listed)| *listed);
    }
    // pacman names are [a-z0-9@._+-]; anything else is not a name to send.
    if !pkg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c))
    {
        return None;
    }
    // Asked as part of every foreign package, the question the scan and the
    // Health check ask, so the three share one request.
    let mut names = crate::inventory::foreign_packages()
        .map(|f| f.to_vec())
        .unwrap_or_default();
    if !names.iter().any(|n| n == pkg) {
        names = vec![pkg.to_string()];
    }
    let answer = crate::inventory::aur_health_answer(&names).ok()??;
    let listed = answer.get(pkg).is_some_and(|r| is_this_project(&r.url));
    if let Ok(mut seen) = AUR_LISTED.lock() {
        *seen = Some((pkg.to_string(), listed));
    }
    Some(listed)
}

/// The newest release tag GitHub lists, as last fetched by this process.
pub(super) static LATEST_RELEASE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// For a package install, say so in the note when GitHub lists a newer
/// release than this build. With `fetch`, ask GitHub (through `ghcache`:
/// cached for hours, within the API budget); without, use what this
/// process last heard, so the About window does not wait on the network.
pub(super) fn note_release(info: &mut Info, fetch: bool) {
    if info.kind != PACKAGE {
        return;
    }
    let tag = if fetch {
        let tag = crate::ghcache::try_latest_release(RELEASE_REPO, RELEASE_TTL)
            .ok()
            .flatten()
            .and_then(|r| r.get("tag_name")?.as_str().map(String::from));
        if let (Some(t), Ok(mut seen)) = (&tag, LATEST_RELEASE.lock()) {
            *seen = Some(t.clone());
        }
        tag
    } else {
        LATEST_RELEASE.lock().ok().and_then(|seen| seen.clone())
    };
    if let Some(tag) = tag {
        mention_release(info, &tag);
    }
}

/// `info.note`, led by the news when `tag` (v2.2.0 or 2.2.0) is newer than
/// the version running.
pub(super) fn mention_release(info: &mut Info, tag: &str) {
    let newer = tag.trim().trim_start_matches('v');
    if newer.is_empty()
        || crate::security::vercmp(newer, &info.version) != std::cmp::Ordering::Greater
    {
        return;
    }
    info.latest = newer.to_string();
    info.note = format!(
        "Almanac {newer} is out (this is {}). {}",
        info.version, info.note
    );
}

/// `check` as the Python does it, against `root`. The app itself goes
/// through `compiled_check_in`; this is the specification it builds on.
pub fn check_in(root: &Path, fetch: bool) -> Result<Info, String> {
    let kind = install_kind_in(root)?;
    let version = version_in(root)?;
    check_as(root, kind, version, || owning_package_in(root), fetch)
}

/// The body of `check` once the kind, the version and how to name the owning
/// package are settled.
pub(super) fn check_as(
    root: &Path,
    kind: &'static str,
    version: String,
    owner: impl FnOnce() -> Result<String, String>,
    fetch: bool,
) -> Result<Info, String> {
    let mut info = Info {
        kind: kind.to_string(),
        version,
        ..Default::default()
    };

    if kind == PACKAGE {
        let pkg = owner()?;
        let origin = match package_origin(&pkg) {
            PackageOrigin::Foreign { .. } => PackageOrigin::Foreign {
                aur: aur_listed(&pkg, fetch),
            },
            other => other,
        };
        info.note = package_note(&pkg, origin);
        info.origin = Some(origin);
        return Ok(info);
    }

    if kind == PLAIN {
        info.note = "Running from a plain directory with no git checkout, so there is nothing to pull from.".to_string();
        return Ok(info);
    }

    info.remote = remote_url_in(root)?;
    info.dirty = dirty_in(root)?;
    if info.remote.is_empty() {
        info.note = "This checkout has no 'origin' remote yet, so there is nowhere to update from."
            .to_string();
        return Ok(info);
    }

    if fetch {
        let (code, out) = git_in(root, &["fetch", "--quiet", "origin"], 90)?;
        if code != 0 {
            let last = if out.is_empty() {
                ""
            } else {
                py_splitlines(&out).last().copied().unwrap_or("")
            };
            info.note = format!("Could not reach the remote: {last}");
            return Ok(info);
        }
    }

    let br = branch_in(root)?;
    let range = format!("HEAD...origin/{br}");
    let (code, counts) = git_in(root, &["rev-list", "--left-right", "--count", &range], 60)?;
    if code != 0 {
        info.note = format!("No upstream branch for {br}.");
        return Ok(info);
    }
    // ahead, behind = (int(x) for x in counts.split()); ValueError -> 0, 0
    let parts = py_split(&counts);
    let (ahead, behind) = match parts.as_slice() {
        [a, b] => match (py_int(a), py_int(b)) {
            (Some(a), Some(b)) => (a, b),
            _ => (BigInt::ZERO, BigInt::ZERO),
        },
        _ => (BigInt::ZERO, BigInt::ZERO),
    };

    info.behind = behind.clone();
    if behind != BigInt::ZERO {
        let upstream = format!("origin/{br}");
        let (code, latest) = git_in(root, &["log", "-1", "--format=%h %s", &upstream], 60)?;
        info.latest = if code == 0 { latest } else { String::new() };
    }
    if ahead != BigInt::ZERO && behind != BigInt::ZERO {
        info.note = format!(
            "This checkout has {ahead} local commit(s) the remote does not, so it cannot fast-forward. Rebase or reset it by hand."
        );
        return Ok(info);
    }
    if info.dirty {
        info.note = "There are uncommitted changes here. Commit or stash them before updating, or they would be in the way of a fast-forward.".to_string();
        return Ok(info);
    }
    if behind != BigInt::ZERO {
        info.can_update = true;
        info.note = format!("{behind} commit(s) behind origin/{br}.");
    } else {
        info.note = format!("Up to date with origin/{br}.");
    }
    Ok(info)
}

// ---- A compiled build: the package manager, or a rebuild ----
//
// Not in the Python, deliberately. Python adopts new source on its next
// start, so a fast-forward pull plus a restart *is* an update. A compiled
// Almanac does not: pulling changes the source and nothing that runs, and
// restarting would start the same binary (or the Python launcher). So a
// compiled build never updates itself:
//
// * installed by pacman (the binary itself is owned by a package) -- it
//   defers to pacman exactly as the Python package does, and says what
//   will actually update it (`PackageOrigin`): the repositories, the AUR,
//   or nothing but the next download from the releases page;
// * built in a git checkout -- everything `check` reports still applies,
//   but where the Python would offer a pull, this says to pull and rebuild;
// * anything else -- nothing to update from, as for a plain directory.
//
// What is checked for ownership is the running binary, not ROOT/app.py: an
// installed binary has no checkout beside it, and ROOT then falls back to
// the directory it was built in, which may not exist any more (or may be a
// stale makepkg checkout -- which is why git only counts when the binary
// lives inside ROOT).

/// The version of this build: [`VERSION`].
pub const BUILD_VERSION: &str = VERSION;

/// What the note says, after the Python's "N commit(s) behind", in a
/// checkout that would otherwise offer to pull.
pub const REBUILD_NOTE: &str =
    "This copy is compiled, so pulling alone changes nothing that runs: \
                                pull, then rebuild it with cargo build --release.";

/// The running binary, symlinks resolved.
pub fn running_exe() -> PathBuf {
    let exe = running_program().into_iter().next().unwrap_or_default();
    py_resolve(Path::new(&exe))
}

/// `check` for a compiled build: see above.
pub fn compiled_check_in(root: &Path, exe: &Path, fetch: bool) -> Result<Info, String> {
    let in_checkout =
        exe.starts_with(root) && git_in(root, &["rev-parse", "--is-inside-work-tree"], 60)?.0 == 0;
    let mut owner: Option<String> = None;
    let kind = if in_checkout {
        GIT
    } else {
        // except (OSError, subprocess.SubprocessError): not a package
        match pacman_owner_of(exe.as_os_str().to_owned())? {
            Ok(out) => {
                let stdout = decode_text(&out.stdout)?;
                decode_text(&out.stderr)?;
                let name = py_strip(&stdout).to_string();
                if out.code == 0 && !name.is_empty() {
                    owner = Some(name);
                    PACKAGE
                } else {
                    PLAIN
                }
            }
            Err(_) => PLAIN,
        }
    };
    let version = if kind == GIT {
        version_in(root)?
    } else {
        BUILD_VERSION.to_string()
    };
    let mut info = check_as(root, kind, version, || Ok(owner.unwrap_or_default()), fetch)?;
    if info.can_update {
        info.can_update = false;
        info.note = format!("{} {REBUILD_NOTE}", info.note);
    }
    Ok(info)
}

/// Steps for the task runner. Fast-forward only, deliberately.
///
/// Each step is (title, argv).
pub fn update_steps() -> Vec<(String, Vec<String>)> {
    update_steps_in(&ROOT)
}

pub fn update_steps_in(root: &Path) -> Vec<(String, Vec<String>)> {
    let argv = [
        "git",
        "-C",
        &root.to_string_lossy(),
        "pull",
        "--ff-only",
        "--stat",
    ]
    .map(String::from);
    vec![("Updating Almanac".to_string(), argv.to_vec())]
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::process::Stdio;

    fn sh_git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.invalid")
            .env("GIT_COMMITTER_NAME", "A")
            .env("GIT_COMMITTER_EMAIL", "a@example.invalid")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git {args:?}");
    }

    /// A compiled build never offers to pull: a checkout behind its remote
    /// says to rebuild, and a binary outside the checkout ignores it.
    #[test]
    fn compiled_build_rebuilds_or_defers_to_pacman() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir(&work).unwrap();
        sh_git(&work, &["init", "-q", "-b", "main"]);
        for (i, text) in ["one", "two"].iter().enumerate() {
            std::fs::write(work.join("app.py"), text).unwrap();
            sh_git(&work, &["add", "app.py"]);
            sh_git(&work, &["commit", "-q", "-m", &format!("c{i}")]);
        }
        let clone = tmp.path().join("clone");
        sh_git(
            tmp.path(),
            &[
                "clone",
                "-q",
                work.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        sh_git(&clone, &["reset", "-q", "--hard", "HEAD~1"]);

        let exe = clone.join("target/release/almanac");
        let py = check_in(&clone, false).unwrap();
        assert!(py.can_update);
        let info = compiled_check_in(&clone, &exe, false).unwrap();
        assert_eq!(info.kind, GIT);
        assert!(!info.can_update);
        assert_eq!(info.note, format!("{} {REBUILD_NOTE}", py.note));
        assert!(info.note.contains("cargo build --release"));
        let app = compiled_as_app_in(&clone, &exe, false).unwrap();
        assert_eq!(app.update, "");

        // Installed elsewhere (/usr/bin, say): the checkout it was built in
        // is not this copy, and pacman owns nothing under a temp dir.
        let elsewhere = tmp.path().join("usr/bin/almanac");
        let info = compiled_check_in(&clone, &elsewhere, false).unwrap();
        assert_eq!(info.kind, PLAIN);
        assert_eq!(info.version, BUILD_VERSION);
        assert!(!info.can_update);
    }

    #[test]
    fn a_package_install_is_told_what_updates_it() {
        // pacman -Qmq: foreign prints the name; a repository package exits 1
        // silently; anything else is not taken as either.
        assert_eq!(
            origin_from(0, "almanac\n", ""),
            PackageOrigin::Foreign { aur: None }
        );
        assert_eq!(origin_from(1, "", ""), PackageOrigin::Repo);
        assert_eq!(
            origin_from(1, "", "error: package 'almanac' was not found\n"),
            PackageOrigin::Unknown
        );
        assert_eq!(origin_from(-1, "", ""), PackageOrigin::Unknown);
        assert_eq!(origin_from(0, "  \n", ""), PackageOrigin::Unknown);
        assert_eq!(package_origin(""), PackageOrigin::Unknown);

        // Only a repository (or an AUR that has it) updates with the system.
        let repo = package_note("almanac", PackageOrigin::Repo);
        assert!(repo.contains("rest of the system") && !repo.contains("AUR"));
        let aur = package_note("almanac", PackageOrigin::Foreign { aur: Some(true) });
        assert!(aur.contains("AUR") && aur.contains("rest of the system"));
        for unlisted in [None, Some(false)] {
            let note = package_note("almanac", PackageOrigin::Foreign { aur: unlisted });
            assert!(
                !note.contains("rest of the system") && !note.contains("AUR"),
                "{note}"
            );
            assert!(
                note.contains("nothing updates it")
                    && note.contains(RELEASES_URL)
                    && note.contains("sudo pacman -U")
            );
        }
        let unknown = package_note("almanac", PackageOrigin::Unknown);
        assert!(!unknown.contains("rest of the system") && unknown.contains(RELEASES_URL));

        // The AUR counts only for this name pointing at this project.
        let ours = serde_json::json!({"results": [{"Name": "almanac", "URL": "https://github.com/twodxme/almanac"}]});
        assert_eq!(aur_lists(&ours, "almanac"), Some(true));
        assert_eq!(aur_lists(&ours, "almanac-git"), Some(false));
        let other = serde_json::json!({"results": [{"Name": "almanac", "URL": "https://example.org/almanac"}]});
        assert_eq!(aur_lists(&other, "almanac"), Some(false));
        assert_eq!(
            aur_lists(&serde_json::json!({"results": []}), "almanac"),
            Some(false)
        );
        assert_eq!(
            aur_lists(
                &serde_json::json!({"type": "error", "error": "x"}),
                "almanac"
            ),
            None
        );
        // Without fetching, nothing heard is nothing known.
        assert_eq!(aur_listed("almanac-never-asked", false), None);
    }

    #[test]
    fn a_package_install_hears_of_a_newer_release() {
        let note = package_note("almanac", PackageOrigin::Unknown);
        assert!(
            note.contains("package manager")
                && note.contains(RELEASES_URL)
                && note.contains("sudo pacman -U")
        );
        let info = |version: &str| Info {
            kind: PACKAGE.into(),
            version: version.into(),
            note: note.clone(),
            ..Default::default()
        };
        let mut i = info("2.1.2");
        mention_release(&mut i, "v2.2.0");
        assert_eq!(i.latest, "2.2.0");
        assert!(i
            .note
            .starts_with("Almanac 2.2.0 is out (this is 2.1.2). Installed as"));
        assert!(!i.can_update, "pacman installs it, not Almanac");
        for (running, tag) in [("2.2.0", "v2.2.0"), ("2.3.0", "2.2.0"), ("2.1.2", "")] {
            let mut i = info(running);
            mention_release(&mut i, tag);
            assert_eq!(
                (i.latest.as_str(), i.note.as_str()),
                ("", note.as_str()),
                "{running} {tag}"
            );
        }
        // only a package install is told
        let mut git = Info {
            kind: GIT.into(),
            version: "2.1.2".into(),
            ..Default::default()
        };
        note_release(&mut git, false);
        assert_eq!(git.note, "");
    }

    /// A release published just after a check is seen within minutes:
    /// 2.2.2 stayed hidden for six hours behind the general GitHub cache.
    #[test]
    fn a_new_release_is_not_hidden_behind_the_general_cache() {
        const { assert!(RELEASE_TTL <= 15 * 60 && RELEASE_TTL < crate::ghcache::DEFAULT_TTL) };
        let src = include_str!("check.rs");
        let call = src.find("try_latest_release(RELEASE_REPO,").unwrap();
        assert!(src[call..call + 60].contains("RELEASE_TTL"));
    }

    // The "Open the release" toast: only where the note says nothing updates
    // the package. Repository and AUR packages, an unknown origin, an
    // up-to-date package and other kinds just hear the note.
    #[test]
    fn only_a_package_nothing_updates_is_offered_the_download() {
        let behind = |origin: PackageOrigin| Info {
            kind: PACKAGE.into(),
            latest: "2.2.0".into(),
            origin: Some(origin),
            ..Default::default()
        };
        assert!(offers_download(&behind(PackageOrigin::Foreign {
            aur: None
        })));
        assert!(offers_download(&behind(PackageOrigin::Foreign {
            aur: Some(false)
        })));
        assert!(!offers_download(&behind(PackageOrigin::Foreign {
            aur: Some(true)
        })));
        assert!(!offers_download(&behind(PackageOrigin::Repo)));
        assert!(!offers_download(&behind(PackageOrigin::Unknown)));
        let current = Info {
            latest: String::new(),
            ..behind(PackageOrigin::Foreign { aur: None })
        };
        assert!(!offers_download(&current));
        let unset = Info {
            origin: None,
            ..behind(PackageOrigin::Foreign { aur: None })
        };
        assert!(!offers_download(&unset));
        let git = Info {
            kind: GIT.into(),
            ..behind(PackageOrigin::Foreign { aur: None })
        };
        assert!(!offers_download(&git));
        // The origin rides along with the note, and never into the dict.
        let json = serde_json::to_value(behind(PackageOrigin::Repo)).unwrap();
        assert!(json.get("origin").is_none());
    }

    #[test]
    fn update_steps_fast_forward_only() {
        let steps = update_steps();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].0, "Updating Almanac");
        assert_eq!(&steps[0].1[3..], &["pull", "--ff-only", "--stat"]);
    }
}
