//! Adding an AppImage: its name, a clash with one already tracked, the copy,
//! and the window's file chooser and questions.

use std::path::Path;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{Map, Value};

use crate::{appimage, inventory};
use crate::i18n::tr;
use crate::ui::shell::{Win, spawn_bg};

use super::pyisms::{path_repr, py_first_line, py_oserror, py_oserror2, py_oserror_line, py_str_repr, py_strip, utf8_error};
use super::raised;

/// The name an added AppImage is tracked under: the file stem up to the
/// first `-`, or the whole stem when that is empty (`_install_appimage`).
pub fn appimage_name(src: &Path) -> String {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    match stem.split('-').next() {
        Some(first) if !first.is_empty() => first.to_string(),
        _ => stem,
    }
}

/// What adding `src` under the key `name` would do to an AppImage already
/// tracked (`reg` is the registry, `store` where tracked AppImages live).
#[derive(Debug, PartialEq)]
pub enum AddClash {
    /// Nothing is tracked under that key, or it is this very file again:
    /// go ahead.
    Free,
    /// Another AppImage is tracked under it (`existing`, its source file or
    /// path), or its file is there. Adding would replace it -- its file,
    /// its launcher and its registry entry -- so that is asked; `spare` is
    /// a key that keeps both.
    Taken { existing: String, spare: String },
}

/// See [`AddClash`]. The key is the file name up to its first `-`, so two
/// different apps (`my-app`, `my-tool`) meet at the same one, and the
/// second used to replace the first without a word.
pub fn appimage_clash(reg: &Map<String, Value>, name: &str, src: &Path, store: &Path) -> AddClash {
    let taken = |k: &str| reg.contains_key(k) || store.join(format!("{k}.AppImage")).exists();
    if !taken(name) {
        return AddClash::Free;
    }
    let field = |k: &str| reg.get(name).and_then(|e| e.get(k)).and_then(Value::as_str).map(str::to_string);
    let src_s = src.to_string_lossy();
    if field("source_file").as_deref() == Some(&*src_s) {
        return AddClash::Free;
    }
    let existing = field("source_file")
        .or_else(|| field("path"))
        .unwrap_or_else(|| store.join(format!("{name}.AppImage")).to_string_lossy().into_owned());
    let spare = (2..).map(|n| format!("{name}-{n}")).find(|k| !taken(k)).unwrap_or_default();
    AddClash::Taken { existing, spare }
}

/// `shutil.copy2(src, dst)`: contents, then times, extended attributes and
/// permission bits (`copystat`). `Err` is the str() of the OSError raised.
fn copy2(src: &Path, dst: &Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
    let (dst, dst_repr) = if dst.is_dir() {
        let d = dst.join(src.file_name().unwrap_or_default());
        let r = py_str_repr(&d.to_string_lossy());
        (d, r)
    } else {
        (dst.to_path_buf(), path_repr(dst))
    };
    // copyfile
    if let (Ok(a), Ok(b)) = (std::fs::metadata(src), std::fs::metadata(&dst)) {
        if a.dev() == b.dev() && a.ino() == b.ino() {
            return Err(format!("{} and {dst_repr} are the same file", path_repr(src)));
        }
    }
    for p in [src, dst.as_path()] {
        if std::fs::metadata(p).is_ok_and(|m| m.file_type().is_fifo()) {
            return Err(format!("`{}` is a named pipe", p.display()));
        }
    }
    let mut fsrc = std::fs::File::open(src).map_err(|e| py_oserror(&e, src))?;
    let mut fdst = std::fs::File::create(&dst).map_err(|e| py_oserror(&e, &dst))?;
    std::io::copy(&mut fsrc, &mut fdst).map_err(|e| py_oserror2(&e, src, &dst))?;
    drop(fdst);
    // copystat
    let st = std::fs::metadata(src).map_err(|e| py_oserror(&e, src))?;
    let times = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec { tv_sec: st.atime(), tv_nsec: st.atime_nsec() as _ },
        last_modification: rustix::fs::Timespec { tv_sec: st.mtime(), tv_nsec: st.mtime_nsec() as _ },
    };
    rustix::fs::utimensat(rustix::fs::CWD, &dst, &times, rustix::fs::AtFlags::empty())
        .map_err(|e| py_oserror(&e.into(), &dst))?;
    copy_xattr(src, &dst)?;
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(st.mode() & 0o7777))
        .map_err(|e| py_oserror(&e, &dst))
}

/// `shutil._copyxattr`: what the file systems cannot carry is skipped.
fn copy_xattr(src: &Path, dst: &Path) -> Result<(), String> {
    use rustix::io::Errno;
    use std::os::unix::ffi::OsStrExt;
    let list = || -> rustix::io::Result<Vec<u8>> {
        let n = rustix::fs::listxattr(src, &mut [0u8; 0][..])?;
        let mut buf = vec![0u8; n];
        let n = rustix::fs::listxattr(src, &mut buf[..])?;
        buf.truncate(n);
        Ok(buf)
    };
    let names = match list() {
        Ok(n) => n,
        Err(e) if [Errno::NOTSUP, Errno::NODATA, Errno::INVAL].contains(&e) => return Ok(()),
        Err(e) => return Err(py_oserror(&e.into(), src)),
    };
    for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
        let name = std::ffi::OsStr::from_bytes(name);
        let copied = (|| -> Result<(), (Errno, &Path)> {
            let n = rustix::fs::getxattr(src, name, &mut [0u8; 0][..]).map_err(|e| (e, src))?;
            let mut value = vec![0u8; n];
            let n = rustix::fs::getxattr(src, name, &mut value[..]).map_err(|e| (e, src))?;
            value.truncate(n);
            rustix::fs::setxattr(dst, name, &value, rustix::fs::XattrFlags::empty()).map_err(|e| (e, dst))
        })();
        if let Err((e, p)) = copied {
            if ![Errno::PERM, Errno::NOTSUP, Errno::NODATA, Errno::INVAL, Errno::ACCESS].contains(&e) {
                return Err(py_oserror(&e.into(), p));
            }
        }
    }
    Ok(())
}

/// `subprocess.run([str(dest), "--version"], capture_output=True,
/// text=True, timeout=12).stdout`: `Ok(None)` for the OSError or
/// SubprocessError (a timeout) the caller catches, `Err` for the
/// UnicodeDecodeError it does not. As in `communicate`, the timeout covers
/// reading both pipes to their end, not only the process exiting.
fn version_output(dest: &Path) -> Result<Option<String>, String> {
    let argv = [dest.as_os_str(), std::ffi::OsStr::new("--version")];
    let out = crate::util::run_bytes(&argv, &crate::util::RunOpts {
        timeout: Some(Duration::from_secs(12)),
        stdin: crate::util::Stdin::Inherit,
        ..Default::default()
    });
    if out.failed_to_start || out.timed_out {
        return Ok(None);
    }
    let mut texts = Vec::new();
    for bytes in [&out.stdout, &out.stderr] {
        match std::str::from_utf8(bytes) {
            Ok(t) => texts.push(t.replace("\r\n", "\n").replace('\r', "\n")),
            Err(e) => return Err(format!("UnicodeDecodeError: {}", utf8_error(bytes, &e))),
        }
    }
    Ok(texts.into_iter().next())
}

impl Win {
    pub fn on_add_appimage(&self) {
        if self.demo_refuses() {
            return;
        }
        let dialog = gtk::FileDialog::builder().title(tr("Choose an AppImage to track")).build();
        let w = self.downgrade();
        dialog.open(Some(&self.window), None::<&gio::Cancellable>, move |result| {
            if let Some(win) = w.upgrade() {
                win.on_appimage_chosen(result);
            }
        });
    }

    pub fn on_appimage_chosen(&self, result: Result<gio::File, glib::Error>) {
        let Ok(gfile) = result else {
            return; // cancelled
        };
        match gfile.path() {
            Some(path) => self.install_appimage(&path),
            None => raised("TypeError: argument should be a str or an os.PathLike object where \
                            __fspath__ returns a str, not 'NoneType'"),
        }
    }

    /// Copy into our own store and register it.
    ///
    /// Copying rather than referencing the download means a later cleanup of
    /// ~/Downloads cannot quietly break an installed app.
    ///
    /// A different AppImage already tracked under the same name is not
    /// replaced without asking (see [`AddClash`]).
    pub fn install_appimage(&self, src: &Path) {
        let registry = inventory::registry();
        let store = registry.parent().map(Path::to_path_buf).unwrap_or_default().join("apps");
        let name = appimage_name(src);
        match appimage_clash(&inventory::load_registry(), &name, src, &store) {
            AddClash::Free => self.install_appimage_as(src, &name),
            AddClash::Taken { existing, spare } => self.ask_appimage_clash(src, &name, &existing, &spare),
        }
    }

    /// Ask whether an added AppImage replaces the one tracked under `name`,
    /// or is kept beside it as `spare`.
    fn ask_appimage_clash(&self, src: &Path, name: &str, existing: &str, spare: &str) {
        let body = format!(
            "{}\n\n{}\n{existing}\n\n{}\n{}",
            tr("Another AppImage is already tracked under this name. Adding this one \
                as it is would replace it: its file, its launcher and what Almanac \
                knows about it."),
            tr("Tracked now:"),
            tr("Adding:"),
            src.display(),
        );
        let dialog = adw::AlertDialog::new(Some(&format!("“{name}” is already tracked")), Some(&body));
        dialog.add_response("cancel", &tr("Cancel"));
        dialog.add_response("both", &format!("Keep both, as “{spare}”"));
        dialog.add_response("replace", &tr("Replace"));
        dialog.set_response_appearance("both", adw::ResponseAppearance::Suggested);
        dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let w = self.downgrade();
        let (src, name, spare) = (src.to_path_buf(), name.to_string(), spare.to_string());
        dialog.connect_response(None, move |_d, resp| {
            let Some(win) = w.upgrade() else { return };
            match resp {
                "both" => win.install_appimage_as(&src, &spare),
                "replace" => win.install_appimage_as(&src, &name),
                _ => {}
            }
        });
        dialog.present(Some(&self.window));
    }

    /// `install_appimage` once the key it is tracked under is settled.
    ///
    /// The copy, the `--version` probe (up to 12 s) and the desktop entry
    /// (extracting the icon, up to 180 s) run off the main thread; the
    /// window says what it is doing meanwhile, and the outcome when done.
    fn install_appimage_as(&self, src: &Path, name: &str) {
        self.say(&format!("Adding {name}…"), 5);
        let (src, name) = (src.to_path_buf(), name.to_string());
        let w = self.downgrade();
        spawn_bg(
            move || add_appimage(&src, &name),
            move |res| {
                let Some(win) = w.upgrade() else { return };
                match res {
                    Ok(name) => {
                        win.say(&format!("Tracking {name}"), 5);
                        win.reload();
                    }
                    Err(AddFailed::Say(msg)) => win.say(&msg, 5),
                    Err(AddFailed::Raised(exc)) => raised(&exc),
                }
            },
        );
    }
}

/// How adding an AppImage stopped short: told to the user, or raised (logged).
enum AddFailed {
    Say(String),
    Raised(String),
}

/// The slow half of `install_appimage_as`, off the main thread: copy into
/// our store, ask the version, register, and give it a launcher entry.
/// Returns the name it is tracked under.
fn add_appimage(src: &Path, name: &str) -> Result<String, AddFailed> {
    use std::os::unix::fs::PermissionsExt;
    let registry = inventory::registry();
    let store = registry.parent().map(Path::to_path_buf).unwrap_or_default().join("apps");
    std::fs::create_dir_all(&store).map_err(|e| AddFailed::Raised(py_oserror_line(&e, &store)))?;
    let name = name.to_string();
    let dest = store.join(format!("{name}.AppImage"));
    let copied = copy2(src, &dest).and_then(|()| {
        let mode = std::fs::metadata(&dest).map_err(|e| py_oserror(&e, &dest))?.permissions().mode();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode | 0o111))
            .map_err(|e| py_oserror(&e, &dest))
    });
    if let Err(e) = copied {
        return Err(AddFailed::Say(format!("Could not install: {e}")));
    }

    let mut version = "?".to_string();
    match version_output(&dest) {
        Ok(Some(out)) => {
            let out = py_strip(&out);
            if !out.is_empty() && out.chars().count() < 60 {
                version = py_first_line(out).to_string();
            }
        }
        Ok(None) => {} // plenty of AppImages ignore --version
        Err(e) => return Err(AddFailed::Raised(e)),
    }

    let dest_s = dest.to_string_lossy().into_owned();
    let src_name = src.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let entry = serde_json::json!({
        "name": name,
        "path": dest_s,
        "version": version,
        "description": format!("AppImage · {src_name}"),
        "source_file": src.to_string_lossy(),
    });
    // Read afresh under the registry's lock: what changed meanwhile stays.
    inventory::update_registry(|reg| {
        reg.insert(name.clone(), entry);
        Ok(true)
    })
    .map_err(AddFailed::Raised)?;
    // A tracked AppImage should appear in the launcher like anything else,
    // not just inside this window.
    appimage::install_desktop_entry(&dest, &name).map_err(AddFailed::Raised)?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appimage_name_takes_stem_prefix() {
        assert_eq!(appimage_name(Path::new("/d/Foo-1.2-x86_64.AppImage")), "Foo");
        assert_eq!(appimage_name(Path::new("/d/-x.AppImage")), "-x");
    }

    /// Two different AppImages whose names start alike meet at one key:
    /// the second is not added over the first without asking.
    #[test]
    fn a_different_appimage_under_the_same_name_is_asked_about() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("apps");
        std::fs::create_dir_all(&store).unwrap();
        let tool = Path::new("/home/u/Downloads/my-tool-2.0.AppImage");
        assert_eq!(appimage_name(tool), "my");
        let mut reg = Map::new();
        assert_eq!(appimage_clash(&reg, "my", tool, &store), AddClash::Free);
        reg.insert("my".into(), serde_json::json!({"path": "/s/my.AppImage",
                                                    "source_file": "/home/u/Downloads/my-app-1.0.AppImage"}));
        reg.insert("my-2".into(), serde_json::json!({}));
        std::fs::write(store.join("my-3.AppImage"), b"").unwrap();
        assert_eq!(
            appimage_clash(&reg, "my", tool, &store),
            AddClash::Taken { existing: "/home/u/Downloads/my-app-1.0.AppImage".into(), spare: "my-4".into() }
        );
        // The same file added again refreshes its own entry.
        let app = Path::new("/home/u/Downloads/my-app-1.0.AppImage");
        assert_eq!(appimage_clash(&reg, "my", app, &store), AddClash::Free);
        // A file in the store that nothing tracks is in the way too.
        std::fs::write(store.join("solo.AppImage"), b"").unwrap();
        assert!(matches!(appimage_clash(&Map::new(), "solo", app, &store), AddClash::Taken { .. }));
    }

    #[test]
    fn copying_an_appimage_onto_itself_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("a.AppImage");
        std::fs::write(&f, b"keep me").unwrap();
        let err = copy2(&f, &f).unwrap_err();
        assert!(err.ends_with("are the same file"), "{err}");
        assert_eq!(std::fs::read(&f).unwrap(), b"keep me");
        let g = tmp.path().join("b.AppImage");
        copy2(&f, &g).unwrap();
        assert_eq!(std::fs::read(&g).unwrap(), b"keep me");
        let missing = tmp.path().join("nope");
        assert_eq!(copy2(&missing, &g).unwrap_err(),
                   format!("[Errno 2] No such file or directory: '{}'", missing.display()));
    }
}
