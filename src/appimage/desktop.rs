//! The launcher entry and icon extracted from a tracked AppImage.

use std::path::{Path, PathBuf};

use crate::{util, xdg};

use super::pyisms::{fnmatch, py_norm, py_parent, py_split, py_splitlines, py_strip, run_bytes};

/// `os.path.join(dir, name)` on raw path text: no separator is added after
/// one that is already there.
fn join_os(dir: &std::ffi::OsStr, name: &std::ffi::OsStr) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStrExt;
    let mut out = dir.to_os_string();
    if !dir.as_bytes().is_empty() && !dir.as_bytes().ends_with(b"/") {
        out.push("/");
    }
    out.push(name);
    out
}

/// `os.scandir(path)`: (name, path) of each entry, in directory order.
fn scandir(path: &std::ffi::OsStr) -> std::io::Result<Vec<(String, std::ffi::OsString, std::fs::DirEntry)>> {
    let rd = std::fs::read_dir(path)?;
    Ok(rd
        .filter_map(Result::ok)
        .map(|e| {
            let name = e.file_name();
            (name.to_string_lossy().into_owned(), join_os(path, &name), e)
        })
        .collect())
}

fn has_magic(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// One step of pathlib's globber (`glob._GlobberBase.selector`), applied to
/// `parts[i..]` from `path` (raw text ending in '/' where more follows).
fn glob_select(parts: &[String], i: usize, path: std::ffi::OsString, exists: bool, out: &mut Vec<std::ffi::OsString>) {
    let n = parts.len();
    if i == n {
        // select_exists
        if exists || std::fs::symlink_metadata(&path).is_ok() {
            out.push(path);
        }
        return;
    }
    let part = parts[i].as_str();
    if part == "**" {
        // recursive_selector, not following symlinks (recurse_symlinks=False)
        let mut j = i + 1;
        while j < n && parts[j] == "**" {
            j += 1;
        }
        let dir_only = j < n;
        glob_select(parts, j, path.clone(), exists, out);
        let mut stack = vec![path];
        while let Some(p) = stack.pop() {
            let Ok(entries) = scandir(&p) else { continue };
            for (_name, entry_path, entry) in entries {
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir || !dir_only {
                    let mut ep = entry_path;
                    if dir_only {
                        ep.push("/");
                        glob_select(parts, j, ep.clone(), true, out);
                    } else {
                        out.push(ep.clone());
                    }
                    if is_dir {
                        stack.push(ep);
                    }
                }
            }
        }
    } else if part.is_empty() || part == ".." {
        // special_selector
        let mut p = path;
        p.push(part);
        if i + 1 < n {
            p.push("/");
        }
        glob_select(parts, i + 1, p, exists, out);
    } else if !has_magic(part) {
        // literal_selector: joins the literal parts that follow, too.
        let mut lit = part.to_string();
        let mut j = i + 1;
        while j < n && !has_magic(&parts[j]) {
            lit.push('/');
            lit.push_str(&parts[j]);
            j += 1;
        }
        if j < n {
            lit.push('/');
        }
        let mut p = path;
        p.push(lit);
        glob_select(parts, j, p, false, out);
    } else {
        // wildcard_selector
        let dir_only = i + 1 < n;
        let Ok(entries) = scandir(&path) else { return };
        for (name, entry_path, _entry) in entries {
            if part == "*" || fnmatch(&name, part) {
                if dir_only {
                    // DirEntry.is_dir() follows a symlink.
                    if !std::fs::metadata(&entry_path).map(|m| m.is_dir()).unwrap_or(false) {
                        continue;
                    }
                    let mut ep = entry_path;
                    ep.push("/");
                    glob_select(parts, i + 1, ep, true, out);
                } else {
                    out.push(entry_path);
                }
            }
        }
    }
}

/// `Path(root).glob(pattern)` (Python 3.14, POSIX): the pattern is split on
/// '/', so a '/' that came in with a name is a directory step like any
/// other; '**' recurses without following symlinks, wildcards match hidden
/// names too.
fn py_glob(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut parts: Vec<String> =
        pattern.split('/').filter(|x| !x.is_empty() && *x != ".").map(String::from).collect();
    if parts.is_empty() || pattern.starts_with('/') {
        return Vec::new(); // the Python raises; no caller passes such a pattern
    }
    if pattern.ends_with('/') {
        parts.push(String::new());
    }
    let start = join_os(root.as_os_str(), std::ffi::OsStr::new(""));
    let mut out = Vec::new();
    glob_select(&parts, 0, start, false, &mut out);
    out.into_iter().map(PathBuf::from).collect()
}

/// `shutil.copy2`: contents, then `copystat` -- timestamps (`os.utime` by
/// path, which needs no write access to the copy), extended attributes, and
/// the permission bits.
fn copy2(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dst = if dst.is_dir() { dst.join(src.file_name().unwrap_or_default()) } else { dst.to_path_buf() };
    // copyfile: the destination keeps its own mode until copystat.
    {
        let mut from = std::fs::File::open(src)?;
        // Python's open() of a directory is IsADirectoryError, raised before
        // the copy is created; on Linux the open itself succeeds.
        if from.metadata()?.is_dir() {
            return Err(std::io::Error::from_raw_os_error(libc::EISDIR));
        }
        let mut to = std::fs::File::create(&dst)?;
        std::io::copy(&mut from, &mut to)?;
    }
    let st = rustix::fs::stat(src)?;
    let times = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec { tv_sec: st.st_atime as _, tv_nsec: st.st_atime_nsec as _ },
        last_modification: rustix::fs::Timespec { tv_sec: st.st_mtime as _, tv_nsec: st.st_mtime_nsec as _ },
    };
    rustix::fs::utimensat(rustix::fs::CWD, &dst, &times, rustix::fs::AtFlags::empty())?;
    copy_xattr(src, &dst)?;
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(st.st_mode as u32 & 0o7777))?;
    Ok(())
}

/// `shutil._copyxattr`: every extended attribute, skipping the ones the
/// target filesystem or our privileges refuse. The buffers are the largest
/// `os.listxattr` / `os.getxattr` try (XATTR_LIST_MAX, XATTR_SIZE_MAX, both
/// 64 KiB, also the kernel's own limits): past that Python raises ERANGE too.
/// Names are passed through as bytes, as `os.fsdecode` / `fsencode` do.
fn copy_xattr(src: &Path, dst: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use rustix::io::Errno;
    let mut list = vec![0u8; 65536];
    let n = match rustix::fs::listxattr(src, &mut list[..]) {
        Ok(n) => n,
        Err(e) if [Errno::NOTSUP, Errno::NODATA, Errno::INVAL].contains(&e) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let mut value = vec![0u8; 65536];
    for name in list[..n].split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let name = std::ffi::OsStr::from_bytes(name);
        let r = rustix::fs::getxattr(src, name, &mut value[..])
            .and_then(|len| rustix::fs::setxattr(dst, name, &value[..len], rustix::fs::XattrFlags::empty()));
        match r {
            Ok(()) => {}
            Err(e) if [Errno::PERM, Errno::NOTSUP, Errno::NODATA, Errno::INVAL, Errno::ACCESS].contains(&e) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// `tempfile.TemporaryDirectory(dir=...)`: removed on the way out even when
/// the tree inside has read-only directories (its cleanup makes them
/// writable and retries, where a plain remove_dir_all gives up).
struct TempTree(Option<PathBuf>);

impl TempTree {
    /// `mkdtemp(prefix="tmp", dir=dir)`: "tmp" and eight characters of
    /// `_RandomNameSequence` (a-z, 0-9, '_'), made 0o700, retried on a
    /// name that exists; the path made absolute.
    fn new_in(dir: &Path) -> std::io::Result<TempTree> {
        use std::hash::BuildHasher;
        use std::os::unix::fs::DirBuilderExt;
        const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
        let state = std::collections::hash_map::RandomState::new();
        for attempt in 0..10000u64 {
            let mut name = String::from("tmp");
            for k in 0..8u64 {
                let r = state.hash_one((attempt, k, util::now().to_bits()));
                name.push(char::from(CHARS[(r % CHARS.len() as u64) as usize]));
            }
            let file = std::path::absolute(dir.join(&name))?;
            match std::fs::DirBuilder::new().mode(0o700).create(&file) {
                Ok(()) => return Ok(TempTree(Some(file))),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "No usable temporary directory name found"))
    }

    fn path(&self) -> &Path {
        self.0.as_deref().unwrap_or(Path::new(""))
    }

    /// The `with` block's exit; a failure here is what the Python raises.
    fn cleanup(mut self) -> std::io::Result<()> {
        match self.0.take() {
            Some(p) => rmtree_writable(&p),
            None => Ok(()),
        }
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = rmtree_writable(&p);
        }
    }
}

/// Give every directory under `p` (not through symlinks) owner rwx, then
/// remove the tree. Already gone is fine.
fn rmtree_writable(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fn open_up(d: &Path) {
        if let Ok(m) = std::fs::symlink_metadata(d) {
            if m.is_dir() {
                let mode = m.permissions().mode();
                if mode & 0o700 != 0o700 {
                    let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(mode | 0o700));
                }
                if let Ok(rd) = std::fs::read_dir(d) {
                    for e in rd.filter_map(Result::ok) {
                        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            open_up(&e.path());
                        }
                    }
                }
            }
        }
    }
    open_up(p);
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Give a tracked AppImage a launcher entry and icon.
///
/// An AppImage ships its own .desktop and icon; extracting them is what makes
/// it a real app rather than a file we happen to know about. Exec is rewritten
/// to the stored path, since the shipped one assumes the AppImage runtime's
/// own mount point.
///
/// `Ok(false)` when the AppImage would not extract or carries no .desktop;
/// an error is an I/O failure the Python lets propagate.
pub fn install_desktop_entry(appimage_path: &Path, name: &str) -> Result<bool, String> {
    let io = |e: std::io::Error| format!("OSError: {e}");
    let apps_dir = xdg::data_home().join("applications");
    let icons_dir = xdg::data_home().join("icons/hicolor");
    std::fs::create_dir_all(&apps_dir).map_err(io)?;
    // `str(appimage_path)`: its own bytes in argv; in the .desktop text it
    // must be UTF-8 (`write_text` refuses the surrogates a non-UTF-8 name
    // decodes to).
    let app_os = py_norm(appimage_path);
    let app_str = app_os.to_string_lossy().into_owned();
    let app_utf8 = app_os.to_str().is_some();

    let tmp = TempTree::new_in(&py_parent(appimage_path)).map_err(io)?;
    let inside = || -> Result<bool, String> {
        let argv = [app_os.as_os_str(), std::ffi::OsStr::new("--appimage-extract")];
        if run_bytes(&argv, 180, Some(tmp.path())).is_none() {
            return Ok(false);
        }
        let root = tmp.path().join("squashfs-root");
        let desktops = py_glob(&root, "*.desktop");
        let Some(desktop) = desktops.first() else {
            return Ok(false);
        };

        let icon_name = format!("almanac-{name}");
        for size in ["512x512", "256x256", "128x128", "64x64", "48x48"] {
            if let Some(src) = py_glob(&root, &format!("usr/share/icons/hicolor/{size}/apps/*.png")).first() {
                let d = icons_dir.join(size).join("apps");
                std::fs::create_dir_all(&d).map_err(io)?;
                copy2(src, &d.join(format!("{icon_name}.png"))).map_err(io)?;
            }
        }

        let raw = std::fs::read(desktop).map_err(io)?;
        let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n").replace('\r', "\n");
        let (lines, launches) = entry_lines(&text, &app_str, &icon_name);
        let unencodable = launches && !app_utf8;
        let entry = apps_dir.join(format!("almanac-{name}.desktop"));
        if unencodable {
            // open(mode="w") has created the file before the write fails.
            std::fs::File::create(&entry).map_err(io)?;
            return Err("UnicodeEncodeError: 'utf-8' codec can't encode characters: surrogates not allowed".into());
        }
        crate::util::atomic_write(entry, lines.join("\n") + "\n").map_err(io)?;
        Ok(true)
    };
    let r = inside();
    // Leaving the `with`: the cleanup's own failure is what propagates.
    tmp.cleanup().map_err(io)?;
    if !r? {
        return Ok(false);
    }

    let arg = std::ffi::OsStr::new;
    for cmd in [
        vec![arg("update-desktop-database"), apps_dir.as_os_str()],
        vec![arg("gtk-update-icon-cache"), arg("-f"), arg("-t"), icons_dir.as_os_str()],
    ] {
        let _ = run_bytes(&cmd, 30, None);
    }
    Ok(true)
}

/// The lines of an AppImage's own .desktop entry, pointed at `app` (the
/// stored AppImage) and the icon `icon_name`, and whether it had an Exec or
/// TryExec line to point.
///
/// The path is quoted for the Exec key as the Desktop Entry spec has it
/// ([`exec_arg`]): written bare, a name with a space launched its first
/// word, and one with `%` became a field code.
fn entry_lines(text: &str, app: &str, icon_name: &str) -> (Vec<String>, bool) {
    let mut lines: Vec<String> = Vec::new();
    let mut launches = false;
    for line in py_splitlines(text) {
        if line.starts_with("Exec=") || line.starts_with("TryExec=") {
            launches = true;
        }
        if line.starts_with("Exec=") {
            // Preserve any field codes (%F, %U) the entry declared.
            let tail = line.split_once('=').map(|x| x.1).unwrap_or("");
            let codes: Vec<&str> = py_split(tail).filter(|t| t.starts_with('%')).collect();
            lines.push(py_strip(&format!("Exec={} {}", exec_arg(app), codes.join(" "))).to_string());
        } else if line.starts_with("Icon=") {
            lines.push(format!("Icon={icon_name}"));
        } else if line.starts_with("TryExec=") {
            // A plain string (a path), not split into arguments: only the
            // string escapes apply.
            lines.push(format!("TryExec={}", desktop_string(app)));
        } else {
            lines.push(line.to_string());
        }
    }
    (lines, launches)
}

/// One argument of a desktop entry's `Exec` key, as the Desktop Entry spec
/// quotes it: in double quotes, with `"`, `` ` ``, `$` and `\` escaped by a
/// backslash and `%` written `%%` (alone it starts a field code). The key's
/// value is read as a string before it is split into arguments, so the
/// result is escaped once more as a string ([`desktop_string`]).
fn exec_arg(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    for c in arg.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    desktop_string(&quoted)
}

/// A desktop entry string value: a backslash doubled, and the newline, tab
/// and carriage return a line cannot hold as `\n`, `\t`, `\r`.
fn desktop_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

/// Remove the launcher entry and icons `install_desktop_entry` added.
///
/// A file that is already gone is fine (`missing_ok`); any other failure is
/// returned, as the Python raises.
pub fn remove_desktop_entry(name: &str) -> Result<(), String> {
    let unlink = |p: &Path| match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("OSError: {e}")),
    };
    let apps = xdg::data_home().join("applications").join(format!("almanac-{name}.desktop"));
    unlink(&apps)?;
    for png in py_glob(&xdg::data_home().join("icons/hicolor"), &format!("*/apps/almanac-{name}.png")) {
        unlink(&png)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy2_of_a_read_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("a.png");
        std::fs::write(&src, b"PNG").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o444)).unwrap();
        let dst = tmp.path().join("b.png");
        copy2(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"PNG");
        let (a, b) = (std::fs::metadata(&src).unwrap(), std::fs::metadata(&dst).unwrap());
        assert_eq!(b.permissions().mode() & 0o777, 0o444);
        assert_eq!(a.modified().unwrap(), b.modified().unwrap());
    }

    #[test]
    fn test_temp_tree_removes_read_only_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let t = TempTree::new_in(tmp.path()).unwrap();
        let ro = t.path().join("squashfs-root/ro");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::write(ro.join("file"), b"x").unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        t.cleanup().unwrap();
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn test_glob_splits_the_pattern_on_slashes() {
        let tmp = tempfile::tempdir().unwrap();
        for rel in ["48x48/apps/almanac-x/y.png", "48x48/apps/almanac-x/z.png", ".h/apps/almanac-t.png", "f/almanac-t.png"] {
            let p = tmp.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"").unwrap();
        }
        let rel = |v: Vec<PathBuf>| -> Vec<String> {
            v.iter().map(|p| p.strip_prefix(tmp.path()).unwrap().to_string_lossy().into_owned()).collect()
        };
        assert_eq!(rel(py_glob(tmp.path(), "*/apps/almanac-x/y.png")), ["48x48/apps/almanac-x/y.png"]);
        assert_eq!(rel(py_glob(tmp.path(), "*/apps/almanac-t.png")), [".h/apps/almanac-t.png"]);
        let mut all = rel(py_glob(tmp.path(), "**/*.png"));
        all.sort();
        assert_eq!(all.len(), 4);
    }

    /// The launcher runs the stored AppImage whatever its name holds: GLib
    /// reads the Exec line back to exactly the path, as one argument.
    #[test]
    fn test_exec_is_quoted_for_the_desktop_entry() {
        let shipped = "[Desktop Entry]\nName=X\nExec=AppRun --no-sandbox %U\nTryExec=AppRun\nIcon=x\n";
        for app in [
            "/home/u/.local/state/almanac/apps/My App.AppImage",
            "/a/100%.AppImage",
            "/a/q\"uote$HOME`x`\\back.AppImage",
            "/a/plain.AppImage",
        ] {
            let (lines, launches) = entry_lines(shipped, app, "almanac-x");
            assert!(launches);
            let exec = lines.iter().find_map(|l| l.strip_prefix("Exec=")).unwrap();
            assert!(exec.ends_with(" %U"), "{exec}");
            let text = format!("[Desktop Entry]\nType=Application\nName=X\n{}\n", lines[2..].join("\n"));
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("x.desktop");
            std::fs::write(&file, text).unwrap();
            let kf = gtk::glib::KeyFile::new();
            kf.load_from_file(&file, gtk::glib::KeyFileFlags::NONE).unwrap();
            // The string value, then the spec's argument quoting.
            let value = kf.string("Desktop Entry", "Exec").unwrap();
            let argv = gtk::glib::shell_parse_argv(value.replace("%%", "%").as_str()).unwrap();
            assert_eq!(argv[0].to_str(), Some(app), "{exec}");
            assert_eq!(argv[1].to_str(), Some("%U"));
            assert_eq!(kf.string("Desktop Entry", "TryExec").unwrap().as_str(), app);
        }
        assert_eq!(exec_arg("/a/100%.AppImage"), "\"/a/100%%.AppImage\"");
        assert_eq!(exec_arg("/a b"), "\"/a b\"");
    }

    #[test]
    fn test_copy2_of_a_directory_creates_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("dir.png");
        std::fs::create_dir(&src).unwrap();
        let dst = tmp.path().join("out.png");
        assert!(copy2(&src, &dst).is_err());
        assert!(!dst.exists());
    }
}
