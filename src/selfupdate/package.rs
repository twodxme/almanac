//! Updating the package downloaded from GitHub.
//!
//! No repository carries it and the AUR does not yet, so nothing else ever
//! updates it. The release's package is downloaded by version (never through
//! `releases/latest`, which can move between the check and the download),
//! checked against the SHA256SUMS published beside it and against what
//! pacman reads inside it, and only then installed with `pacman -U`.

use std::path::{Path, PathBuf};

use super::RELEASE_REPO;

/// The release asset every release carries under the same name.
pub const ASSET: &str = "almanac-x86_64.pkg.tar.zst";
/// The checksums published beside it.
pub const SUMS: &str = "SHA256SUMS";

/// A GET: the URL and the most bytes to accept, to the body.
pub type Get<'a> = &'a dyn Fn(&str, u64) -> Result<Vec<u8>, String>;

/// Largest package accepted (the real one is about 8 MB).
const MAX_PACKAGE: u64 = 64 * 1024 * 1024;

/// A release version as a tag names it, digits and dots only: it becomes
/// part of a URL and a file name.
pub fn plain_version(v: &str) -> Option<&str> {
    let v = v.trim().trim_start_matches('v');
    let ok = !v.is_empty()
        && !v.starts_with('.')
        && !v.ends_with('.')
        && !v.contains("..")
        && v.chars().all(|c| c.is_ascii_digit() || c == '.');
    ok.then_some(v)
}

/// Where release `version`'s file `name` is downloaded from.
pub fn asset_url(version: &str, name: &str) -> String {
    format!("https://github.com/{RELEASE_REPO}/releases/download/v{version}/{name}")
}

/// The sha256 SHA256SUMS gives for `name`, lower-case; None when it lists
/// no such file or the digest is not 64 hex digits.
pub fn digest_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (digest, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start();
        // `sha256sum -b` marks binary mode with a `*`.
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name && digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| digest.to_ascii_lowercase())
    })
}

/// What `pacman -Qp <file>` printed, checked: Almanac, at `version`.
pub fn check_contents(qp: &str, version: &str) -> Result<(), String> {
    let line = qp.trim();
    let (name, full) = line.split_once(' ').unwrap_or((line, ""));
    let ver = full.rsplit_once('-').map_or(full, |(v, _rel)| v);
    if name != "almanac" || ver != version {
        return Err(format!(
            "the download is not Almanac {version} (pacman reads it as \u{201c}{line}\u{201d}), \
             so it was deleted"
        ));
    }
    Ok(())
}

/// Where release `version`'s package is kept until pacman installs it: a
/// folder only this user can enter.
pub fn package_path(version: &str) -> PathBuf {
    download_dir().join(format!("almanac-{version}-x86_64.pkg.tar.zst"))
}

fn download_dir() -> PathBuf {
    crate::xdg::cache_home().join("almanac/self-update")
}

/// Download release `version`'s package to `dest`, through `get` (a URL to
/// its body), and keep it only if it hashes to what SHA256SUMS says.
pub fn fetch_verified(version: &str, dest: &Path, get: Get<'_>) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let version =
        plain_version(version).ok_or_else(|| format!("{version:?} is not a release version"))?;
    let sums = get(&asset_url(version, SUMS), 64 * 1024)?;
    let sums = String::from_utf8_lossy(&sums);
    let want = digest_for(&sums, ASSET)
        .ok_or_else(|| format!("release {version} publishes no checksum for {ASSET}"))?;
    let body = get(&asset_url(version, ASSET), MAX_PACKAGE)?;
    let got = hex::encode(Sha256::digest(&body));
    if got != want {
        return Err(
            "checksum mismatch: the download is not the file the project published, \
             so it was not installed"
                .into(),
        );
    }
    let dir = dest
        .parent()
        .ok_or_else(|| format!("{} has no folder", dest.display()))?;
    private_dir(dir)?;
    // Written beside it and renamed: a half-written package is never there
    // under the name pacman is given.
    let tmp = dest.with_extension("part");
    std::fs::write(&tmp, &body).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

/// `dir`, made if needed, entered by this user alone.
fn private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// A GET for [`fetch_verified`]: system certificates, two minutes, at most
/// `limit` bytes.
pub fn http_get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(120)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(crate::sources::system_roots())
                .build(),
        )
        .build()
        .into();
    agent
        .get(url)
        .header("User-Agent", "almanac")
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .into_body()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .map_err(|e| format!("{url}: {e}"))
}

/// The task that updates the GitHub package to `version`: download and
/// check it, then install it as root. Empty for a version that is not one.
pub fn update_steps(version: &str) -> Vec<crate::tasks::Step> {
    use crate::tasks::{elevate, LogFn, Step};
    use std::sync::Arc;
    let Some(version) = plain_version(version).map(str::to_string) else {
        return Vec::new();
    };
    let path = package_path(&version);
    let (v, p) = (version.clone(), path.clone());
    vec![
        Step::call(
            format!("Downloading Almanac {version} and checking it"),
            Arc::new(move |log: LogFn<'_>| {
                fetch_verified(&v, &p, &http_get)?;
                log(&format!("sha256 matches {SUMS} for v{v}"));
                let out = crate::util::run(&["/usr/bin/pacman", "-Qp", &p.to_string_lossy()], 30);
                let qp = out.stdout.clone();
                if let Err(e) = check_contents(&qp, &v) {
                    let _ = std::fs::remove_file(&p);
                    return Err(e);
                }
                log(qp.trim());
                Ok(())
            }),
        ),
        Step::argv(
            format!("Installing Almanac {version}"),
            elevate(&[
                "pacman".to_string(),
                "-U".into(),
                "--noconfirm".into(),
                path.to_string_lossy().into_owned(),
            ]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn only_a_plain_version_becomes_a_url_or_a_file_name() {
        assert_eq!(plain_version("v2.2.1"), Some("2.2.1"));
        assert_eq!(plain_version("2.10.0"), Some("2.10.0"));
        for bad in [
            "",
            "v",
            "2.2.1/../x",
            "2.2.1-rc1",
            ".2",
            "2..1",
            "2.2.",
            "2 2",
        ] {
            assert_eq!(plain_version(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_asset_is_asked_for_by_version_not_latest() {
        assert_eq!(
            asset_url("2.2.1", ASSET),
            "https://github.com/twodxme/almanac/releases/download/v2.2.1/almanac-x86_64.pkg.tar.zst"
        );
    }

    #[test]
    fn the_checksum_is_the_line_for_the_asset() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!("{a}  almanac-2.2.1-1-x86_64.pkg.tar.zst\n{b} *{ASSET}\n");
        assert_eq!(digest_for(&sums, ASSET), Some("b".repeat(64)));
        assert_eq!(
            digest_for(&format!("abc  {ASSET}\n"), ASSET),
            None,
            "not a digest"
        );
        assert_eq!(digest_for(&format!("{a}  other\n"), ASSET), None);
    }

    #[test]
    fn the_package_must_be_almanac_at_that_version() {
        assert!(check_contents("almanac 2.2.1-1\n", "2.2.1").is_ok());
        assert!(check_contents("almanac 2.2.0-1\n", "2.2.1").is_err());
        assert!(check_contents("almanac-git 2.2.1-1\n", "2.2.1").is_err());
        assert!(check_contents("", "2.2.1").is_err());
    }

    fn release(
        pkg: &'static [u8],
        sums_digest: String,
    ) -> impl Fn(&str, u64) -> Result<Vec<u8>, String> {
        move |url: &str, _limit: u64| {
            if url.ends_with(&format!("/v2.2.1/{SUMS}")) {
                Ok(format!("{sums_digest}  {ASSET}\n").into_bytes())
            } else if url.ends_with(&format!("/v2.2.1/{ASSET}")) {
                Ok(pkg.to_vec())
            } else {
                Err(format!("unexpected {url}"))
            }
        }
    }

    #[test]
    fn a_download_that_matches_its_checksum_is_kept_privately() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("cache/almanac-2.2.1-x86_64.pkg.tar.zst");
        let digest = hex::encode(Sha256::digest(b"package"));
        fetch_verified("v2.2.1", &dest, &release(b"package", digest)).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"package");
        let mode = std::fs::metadata(dest.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "others can enter the download folder");
    }

    #[test]
    fn a_download_that_does_not_match_is_never_written() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("almanac-2.2.1-x86_64.pkg.tar.zst");
        let wrong = hex::encode(Sha256::digest(b"something else"));
        let e = fetch_verified("2.2.1", &dest, &release(b"package", wrong)).unwrap_err();
        assert!(e.contains("checksum mismatch"), "{e}");
        assert!(!dest.exists());
        assert!(!dest.with_extension("part").exists());
    }

    #[test]
    fn a_release_without_a_checksum_is_not_installed() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("p.pkg.tar.zst");
        let none = |url: &str, _l: u64| {
            if url.ends_with(SUMS) {
                Ok(b"".to_vec())
            } else {
                Ok(b"package".to_vec())
            }
        };
        assert!(fetch_verified("2.2.1", &dest, &none).is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn the_install_is_pacman_by_path_through_pkexec() {
        let steps = update_steps("2.2.1");
        assert_eq!(steps.len(), 2);
        match &steps[1].action {
            crate::tasks::StepAction::Argv(a) => {
                assert_eq!(
                    a[..4],
                    ["/usr/bin/pkexec", "--keep-cwd", "/usr/bin/pacman", "-U"]
                );
                assert!(a
                    .last()
                    .unwrap()
                    .ends_with("almanac/self-update/almanac-2.2.1-x86_64.pkg.tar.zst"));
            }
            other => panic!("{other:?}"),
        }
        assert!(update_steps("../2.2.1").is_empty());
    }
}
