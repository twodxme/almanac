//! Which files an install quietly took over.
//!
//! An app's launcher entry lists the file types it can open. Installing it is
//! enough, on most desktops, to make it the default for any of those types that
//! had no explicit choice -- which is how a word processor installed to try out
//! becomes what opens every .docx, with no prompt and no record of what opened
//! them before.
//!
//! So installs are bracketed: the default application for every known file type
//! is read before and after, and anything that changed hands is shown with what
//! used to open it, and can be handed back. The reading uses GIO, the same
//! lookup file managers and `xdg-open` use, in a fresh process -- a long-running
//! one caches the desktop database and would not see the change.
//!
//! Handing a type back goes through `xdg-mime default`, the standard way to
//! record a choice, so it lands in the user's mimeapps.list like any choice made
//! in a file manager.
//!
//! Port of `mimeguard.py`.

use std::collections::BTreeMap;

/// Run in a separate interpreter: GIO's app cache in this process is stale by
/// design (it updates asynchronously from file monitors).
///
/// The Python's `_SNAPSHOT` script, verbatim.
#[allow(dead_code)]
const SNAPSHOT: &str = r#"
import json, os, sys
import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio
types = set()
dirs = [os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")]
dirs += (os.environ.get("XDG_DATA_DIRS") or "/usr/local/share:/usr/share").split(":")
for d in dirs:
    try:
        with open(os.path.join(d, "applications", "mimeinfo.cache")) as fh:
            for line in fh:
                if "=" in line and not line.startswith("["):
                    types.add(line.split("=", 1)[0].strip())
    except OSError:
        pass
out = {}
for t in sorted(types):
    app = Gio.AppInfo.get_default_for_type(t, False)
    if app is not None:
        out[t] = app.get_id() or ""
json.dump(out, sys.stdout)
"#;

/// {mime type: desktop id} for every type something can open.
pub fn snapshot() -> Option<BTreeMap<String, String>> {
    todo!("port: mimeguard::snapshot")
}

/// (type, old id, new id) for every type whose default moved.
///
/// A type that had no default before and has one now is a takeover too --
/// but only one where something else could already open it is worth asking
/// about, so those carry the old id as ''.
pub fn changes(before: Option<&BTreeMap<String, String>>,
               after: Option<&BTreeMap<String, String>>) -> Vec<(String, String, String)> {
    let _ = (before, after);
    todo!("port: mimeguard::changes")
}

pub fn app_name(desktop_id: &str) -> String {
    let _ = desktop_id;
    todo!("port: mimeguard::app_name")
}

/// A file type as a person names it: '.docx' rather than the MIME string.
pub fn label(mime: &str) -> String {
    let _ = mime;
    todo!("port: mimeguard::label")
}

/// One line per new owner: 'WordCraft now opens .docx, .odt (was LibreOffice)'.
pub fn summary(moved: &[(String, String, String)]) -> Vec<String> {
    let _ = moved;
    todo!("port: mimeguard::summary")
}

/// Give each type back to what opened it before. Returns how many.
/// `log` None is the Python default, `print`.
pub fn restore(moved: &[(String, String, String)], log: Option<&mut dyn FnMut(&str)>) -> usize {
    let _ = (moved, log);
    todo!("port: mimeguard::restore")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
