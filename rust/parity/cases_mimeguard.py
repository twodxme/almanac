"""Parity cases for mimeguard: the diff, labels, names, summaries, the real
machine's defaults, and restore against a fake `xdg-mime` (nothing on the
machine changes)."""
import os
import tempfile
from pathlib import Path
from unittest import mock

import mimeguard

ODT = "application/vnd.oasis.opendocument.text"


def _restore(path, moved):
    lines = []
    with mock.patch.dict(os.environ, {"PATH": path}):
        n = mimeguard.restore([tuple(m) for m in moved], log=lines.append)
    return [n, lines]


def _shadow_dirs():
    """Two extra XDG data dirs, ahead of the machine's own, where the higher
    one holds a broken entry (TryExec missing, or not a key file at all) that
    shadows a valid one of the same id in the lower one. Set before GIO is
    first used in this process; the Rust side inherits the same environment.
    Nothing here has a MimeType, so the snapshot is unaffected."""
    hi = Path(tempfile.mkdtemp(prefix="almanac-parity-mimeguard-hi-"))
    lo = Path(tempfile.mkdtemp(prefix="almanac-parity-mimeguard-lo-"))
    (hi / "applications").mkdir()
    (lo / "applications").mkdir()
    entry = "[Desktop Entry]\nType=Application\nExec=true\nName={}\n"
    (hi / "applications/almanac-pbroken.desktop").write_text(entry.format("Broken Hi") + "TryExec=/nonexistent/almanac-zz\n")
    (lo / "applications/almanac-pbroken.desktop").write_text(entry.format("Fallback Lo"))
    (hi / "applications/almanac-pgarb.desktop").write_text("garbage, no group\n")
    (lo / "applications/almanac-pgarb.desktop").write_text(entry.format("Garb Lo"))
    (hi / "applications/almanac-phidden.desktop").write_text(entry.format("Hidden Hi") + "Hidden=true\n")
    (lo / "applications/almanac-phidden.desktop").write_text(entry.format("Hidden Lo"))
    (lo / "applications/almanac-pnodisp.desktop").write_text(entry.format("NoDisp Lo") + "NoDisplay=true\n")
    rest = os.environ.get("XDG_DATA_DIRS") or "/usr/local/share:/usr/share"
    os.environ["XDG_DATA_DIRS"] = f"{hi}:{lo}:{rest}"


def cases():
    _shadow_dirs()

    # snapshot: the real machine, read in a fresh process.
    yield "snapshot", {}, lambda: mimeguard.snapshot()

    # changes
    before = {ODT: "writer.desktop", "text/plain": "nvim.desktop"}
    after = {ODT: "wordcraft.desktop", "text/plain": "nvim.desktop"}
    yield "changes", {"before": before, "after": after}, lambda: mimeguard.changes(before, after)
    yield "changes", {"before": None, "after": {"a/b": "x"}}, lambda: mimeguard.changes(None, {"a/b": "x"})
    yield "changes", {"before": {"a/b": "x"}, "after": None}, lambda: mimeguard.changes({"a/b": "x"}, None)
    yield "changes", {"before": {}, "after": {"a/b": "x"}}, lambda: mimeguard.changes({}, {"a/b": "x"})
    yield "changes", {"before": {"a/b": "x"}, "after": {}}, lambda: mimeguard.changes({"a/b": "x"}, {})
    b2 = {"a/a": "x.desktop", "b/b": "y.desktop"}
    a2 = {"a/a": "", "b/b": "y.desktop", "c/c": "z.desktop", "d/ü": "ü.desktop"}
    yield "changes", {"before": b2, "after": a2}, lambda: mimeguard.changes(b2, a2)
    b3 = {"x/y": "same.desktop"}
    yield "changes", {"before": b3, "after": b3}, lambda: mimeguard.changes(b3, b3)
    # order: the Python walks `after` in insertion order, not sorted
    b4 = {"a/a": "old.desktop", "z/z": "old.desktop"}
    a4 = {"z/z": "new.desktop", "m/m": "new.desktop", "a/a": "new.desktop"}
    yield "changes", {"before": b4, "after": a4}, lambda: mimeguard.changes(b4, a4)

    # label
    for mime in ["text/plain", ODT, "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                 "TEXT/HTML", "image/jpeg", "image/png", "audio/mpeg", "video/mp4", "application/pdf",
                 "text/rtf", "application/rtf", "image/pict", "text/xul", "application/x-unknown-thing",
                 "", "not a mime", "téxt/plaïn", "x-scheme-handler/http", "inode/directory",
                 "text/javascript", "application/xml", "text/xml", "image/svg+xml"]:
        yield "label", {"mime": mime}, (lambda m=mime: mimeguard.label(m))

    # app_name: real ids on this machine, missing ones, malformed ones
    snap = mimeguard.snapshot() or {}
    ids = sorted(set(snap.values()))[:40]
    for did in ids + ["", "nvim.desktop", "chromium.desktop", "almanac-wordcraft.desktop",
                      "Disk Usage.desktop", "no-such-app.desktop", "no-such-app",
                      "foo.desktop.desktop", "ünïcode.desktop", "a\0b.desktop", "../x.desktop",
                      # a broken entry earlier in XDG_DATA_DIRS shadowing a valid one
                      "almanac-pbroken.desktop", "almanac-pgarb.desktop",
                      "almanac-phidden.desktop", "almanac-pnodisp.desktop"]:
        yield "app_name", {"desktop_id": did}, (lambda d=did: mimeguard.app_name(d))

    # summary
    moved = [[ODT, "writer.desktop", "almanac-wordcraft.desktop"],
             ["text/rtf", "", "almanac-wordcraft.desktop"],
             ["text/plain", "nvim.desktop", "chromium.desktop"],
             ["application/msword", "nvim.desktop", "almanac-wordcraft.desktop"],
             ["image/png", "imv.desktop", "chromium.desktop"]]
    yield "summary", {"moved": moved}, lambda: mimeguard.summary([tuple(m) for m in moved])
    yield "summary", {"moved": []}, lambda: mimeguard.summary([])
    many = [[f"image/{k}", "imv.desktop", "x.desktop"] for k in
            ["png", "jpeg", "gif", "bmp", "tiff", "webp", "svg+xml", "x-icon", "heic", "avif", "x-unknown"]]
    yield "summary", {"moved": many}, lambda: mimeguard.summary([tuple(m) for m in many])
    real = [[t, "", d] for t, d in list(snap.items())[:30]]
    yield "summary", {"moved": real}, lambda: mimeguard.summary([tuple(m) for m in real])

    # restore, against a fake xdg-mime that fails for text/fail
    tmp = tempfile.mkdtemp(prefix="almanac-parity-mimeguard-")
    fake = Path(tmp) / "xdg-mime"
    fake.write_text('#!/bin/sh\n[ "$3" = "text/fail" ] && exit 3\nexit 0\n')
    fake.chmod(0o755)
    rmoved = [["text/rtf", "writer.desktop", "wc.desktop"],
              ["text/x-new", "", "wc.desktop"],
              ["text/fail", "writer.desktop", "wc.desktop"],
              ["text/ü", "ü.desktop", "wc.desktop"]]
    yield "restore", {"path": tmp, "moved": rmoved}, lambda: _restore(tmp, rmoved)
    yield "restore", {"path": tmp, "moved": []}, lambda: _restore(tmp, [])
    # no xdg-mime at all: every type is logged
    empty = tempfile.mkdtemp(prefix="almanac-parity-mimeguard-")
    yield "restore", {"path": empty, "moved": rmoved}, lambda: _restore(empty, rmoved)
    # a NUL in an argument: subprocess raises ValueError, which restore() does not catch
    nul = [["text/rtf", "writer.desktop", "wc.desktop"], ["text/x", "a\0b.desktop", "wc.desktop"]]
    yield "restore", {"path": tmp, "moved": nul}, lambda: _restore(tmp, nul)
    nul_t = [["text/\0x", "writer.desktop", "wc.desktop"]]
    yield "restore", {"path": tmp, "moved": nul_t}, lambda: _restore(tmp, nul_t)

    # --- review round 2 ---
    # A fake xdg-mime with no shebang line: Python execs it directly and gets
    # ENOEXEC (an OSError, logged); a runner that goes through execvp would
    # fall back to /bin/sh and run it.
    noshebang = tempfile.mkdtemp(prefix="almanac-parity-mimeguard-")
    ns = Path(noshebang) / "xdg-mime"
    ns.write_text("exit 0\n")
    ns.chmod(0o755)
    one = [["text/rtf", "writer.desktop", "wc.desktop"]]
    yield "restore", {"path": noshebang, "moved": one}, lambda: _restore(noshebang, one)
    # xdg-mime exits 0 at once but leaves a child holding stdout open past
    # the 20 s timeout: Python's communicate() waits for EOF, times out and
    # logs; a runner that only waits for the direct child sees success.
    gc = tempfile.mkdtemp(prefix="almanac-parity-mimeguard-")
    g = Path(gc) / "xdg-mime"
    g.write_text("#!/bin/sh\n/usr/bin/sleep 23 &\nexit 0\n")
    g.chmod(0o755)
    yield "restore", {"path": gc, "moved": one}, lambda: _restore(gc, one)
    # a None (non-string) old id in `before`: the Python carries it through
    bn = {"a/b": None}
    yield "changes", {"before": bn, "after": {"a/b": "x"}}, lambda: mimeguard.changes(bn, {"a/b": "x"})
