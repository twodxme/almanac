"""Which files an install quietly took over.

An app's launcher entry lists the file types it can open. Installing it is
enough, on most desktops, to make it the default for any of those types that
had no explicit choice -- which is how a word processor installed to try out
becomes what opens every .docx, with no prompt and no record of what opened
them before.

So installs are bracketed: the default application for every known file type
is read before and after, and anything that changed hands is shown with what
used to open it, and can be handed back. The reading uses GIO, the same
lookup file managers and `xdg-open` use, in a fresh process -- a long-running
one caches the desktop database and would not see the change.

Handing a type back goes through `xdg-mime default`, the standard way to
record a choice, so it lands in the user's mimeapps.list like any choice made
in a file manager.
"""
from __future__ import annotations

import json
import mimetypes
import subprocess
import sys

# Run in a separate interpreter: GIO's app cache in this process is stale by
# design (it updates asynchronously from file monitors).
_SNAPSHOT = r"""
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
"""


def snapshot() -> dict[str, str] | None:
    """{mime type: desktop id} for every type something can open."""
    try:
        r = subprocess.run([sys.executable, "-I", "-c", _SNAPSHOT],
                           capture_output=True, text=True, timeout=60)
        return json.loads(r.stdout) if r.returncode == 0 else None
    except (OSError, subprocess.SubprocessError, ValueError):
        return None


def changes(before: dict | None, after: dict | None) -> list[tuple[str, str, str]]:
    """(type, old id, new id) for every type whose default moved.

    A type that had no default before and has one now is a takeover too --
    but only one where something else could already open it is worth asking
    about, so those carry the old id as ''.
    """
    if not before or not after:
        return []
    out = []
    for t, new in after.items():
        old = before.get(t, "")
        if new and new != old:
            out.append((t, old, new))
    return out


def app_name(desktop_id: str) -> str:
    if not desktop_id:
        return "nothing"
    try:
        import gi
        gi.require_version("Gio", "2.0")
        from gi.repository import Gio
        info = Gio.DesktopAppInfo.new(desktop_id)
        if info is not None:
            return info.get_display_name() or desktop_id
    except (ImportError, ValueError, TypeError):
        pass
    return desktop_id.removesuffix(".desktop")


def label(mime: str) -> str:
    """A file type as a person names it: '.docx' rather than the MIME string."""
    return mimetypes.guess_extension(mime, strict=False) or mime


def summary(moved: list[tuple[str, str, str]]) -> list[str]:
    """One line per new owner: 'WordCraft now opens .docx, .odt (was LibreOffice)'."""
    by_new: dict[str, list] = {}
    for t, old, new in moved:
        by_new.setdefault(new, []).append((t, old))
    lines = []
    for new, pairs in by_new.items():
        kinds = ", ".join(sorted({label(t) for t, _o in pairs})[:8])
        olds = sorted({app_name(o) for _t, o in pairs if o})
        was = f" — before: {', '.join(olds)}" if olds else ""
        lines.append(f"{app_name(new)} now opens {kinds}{was}")
    return lines


def restore(moved: list[tuple[str, str, str]], log=print) -> int:
    """Give each type back to what opened it before. Returns how many."""
    n = 0
    for t, old, _new in moved:
        if not old:
            continue                    # nothing to give it back to
        try:
            subprocess.run(["xdg-mime", "default", old, t],
                           capture_output=True, timeout=20, check=True)
            n += 1
        except (OSError, subprocess.SubprocessError):
            log(f"could not restore {t}")
    return n
