"""Parity cases for maintenance.py.

Every check runs once against the real machine as it is (all of them are
read-only: du-style walks, `paccache -d` dry runs, `journalctl --disk-usage`,
`systemctl --failed`, `find /etc`, `pacman -Qk`, statvfs). The parsing is also
checked with the commands stood in for, the way test_app.py patches `_run`,
`_du_mb`, `_usage` and `subprocess.run`.

The checks that read a fixed place (the yay cache, the trash, the AppImage
store, ~/.local/share/applications, ~/.local/bin, the runtime directory) are
pointed at fixture directories under a fresh temp dir; the Rust side gets the
same path through its seam (`_yay_cache_at`, `_trash_at`, ...).

The fixes delete things, so each one runs on its own copy of a fixture: one
built here for Rust, one built inside the lambda for Python. Both report what
they logged (the fixture path written as `<root>`) and what is left on disk.
"""
import json
import os
import tempfile
import time
from pathlib import Path
from unittest import mock

import inventory
import maintenance as m

TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-maint-"))


def _sur(v):
    """Lone surrogates (bytes that are not UTF-8, under surrogateescape)
    cannot cross into a Rust String or through serde's JSON, so both sides
    spell each one as the text \\udcXX, the way repr() shows it."""
    if isinstance(v, str):
        return "".join(f"\\u{ord(c):04x}" if 0xD800 <= ord(c) <= 0xDFFF else c for c in v)
    if isinstance(v, list):
        return [_sur(x) for x in v]
    if isinstance(v, dict):
        return {k: _sur(x) for k, x in v.items()}
    return v


def fd(f):
    """A Finding as the Rust hook reports it: fields, callables as a marker,
    plus the `actionable` property."""
    d = {k: getattr(f, k) for k in m.Finding.__dataclass_fields__}
    d["fix"] = "<callable>" if callable(f.fix) else f.fix
    d["actionable"] = f.actionable
    return _sur(d)


def mk(args):
    """Finding(**args), with fix="callable" standing for a function."""
    kw = dict(args)
    if kw.get("fix") == "callable":
        kw["fix"] = print
    return m.Finding(**kw)


def _mkdir(p):
    p.mkdir(parents=True, exist_ok=True)
    return p


def _write(p, data=b"x", size=None):
    _mkdir(p.parent)
    if isinstance(data, str):
        data = data.encode()
    p.write_bytes(data)
    if size is not None:
        os.truncate(p, size)        # sparse: st_size is what the checks read
    return p


_n = [0]


def fresh(name):
    _n[0] += 1
    return _mkdir(TMP / f"{name}-{_n[0]}")


# ---- fixtures ---------------------------------------------------------------

def fx_yay(root):
    y = _mkdir(root / "yay")
    _write(y / "pkg-a" / "PKGBUILD", "pkgname=a\n")
    _write(y / "pkg-a" / "src" / "big.tar", size=3 * 1048576)
    _write(y / "pkg-b" / "файл.zst", size=700 * 1048576)   # unicode name, sparse
    os.symlink("pkg-a", y / "link-to-dir")                 # not followed
    os.symlink("nowhere", y / "dangling")                  # counted by lstat
    return root


def fx_trash(root):
    t = _mkdir(root / "Trash")
    _write(t / "files" / "a.txt", "hello")
    _write(t / "files" / "dir" / "nested.bin", size=60 * 1048576)
    _write(t / "files" / "ünïcödé.pdf", size=1024)
    os.symlink("/nonexistent/target", t / "files" / "broken")
    _write(t / "info" / "a.txt.trashinfo", "[Trash Info]\n")
    return root


def fx_rollbacks(root):
    a = _mkdir(root / "apps")
    _write(a / "tool.previous", size=2048)
    _write(a / "Big.AppImage.previous", size=5 * 1048576)
    _write(a / "tool.AppImage", size=1000)
    _write(a / ".previous", size=10)                       # hidden still matches
    _write(a / "ñame.previous", size=1)
    return root


def fx_desktop(root):
    d = _mkdir(root / "applications")
    _write(d / "ok.desktop", "[Desktop Entry]\nExec=sh -c true\n")
    _write(d / "gone.desktop", "[Desktop Entry]\nExec=/nonexistent/bin/thing %U\n")
    _write(d / "quoted.desktop", "[Desktop Entry]\nExec=\"/nonexistent/q\" --x\n")
    _write(d / "tryexec.desktop", "[Desktop Entry]\nTryExec=no-such-program-almanac\nExec=sh\n")
    _write(d / "both.desktop", "[Desktop Entry]\nExec=sh\nTryExec='/nonexistent/t'\n")
    _write(d / "empty-try.desktop", "[Desktop Entry]\nTryExec=\nExec=/nonexistent/z\n")
    _write(d / "noexec.desktop", "[Desktop Entry]\nName=Nothing\n")
    _write(d / "crlf.desktop", "[Desktop Entry]\r\nExec=/nonexistent/crlf\r\n")
    _write(d / "bad-utf8.desktop", b"[Desktop Entry]\nName=\xff\xfe\nExec=/nonexistent/\xc3\xa9\n")
    _write(d / "ünï.desktop", "[Desktop Entry]\nExec=/nonexistent/ünï\n")
    _write(d / "abs-ok.desktop", "[Desktop Entry]\nExec=/bin/sh\n")
    _mkdir(d / "dir.desktop")                              # unreadable: skipped
    _write(d / "notes.txt", "Exec=/nonexistent/ignored\n")
    return root


def fx_desktop_raises(root):
    d = _mkdir(root / "applications")
    _write(d / "a.desktop", "[Desktop Entry]\nExec=\n")    # IndexError in Python
    return root


def fx_links(root):
    b = _mkdir(root / ".local" / "bin")
    _write(b / "real", "#!/bin/sh\n")
    os.symlink(b / "real", b / "good")
    os.symlink("/nonexistent/almanac-tool", b / "dead")
    os.symlink("relative-missing", b / "dëad-unicode")
    return root


def fx_runtime(root):
    _write(root / "hypr" / "live1" / "hyprland.log", size=2048)
    _write(root / "hypr" / "dead1" / "hyprland.log", size=3 * 1048576)
    _write(root / "hypr" / "dead1" / "keep.txt", "x")
    _write(root / "hypr" / "dead2" / "hyprland.log", size=150 * 1048576)
    _mkdir(root / "hypr" / "empty-dead")
    _write(root / "hypr" / "stray-file", "x")
    _write(root / "quickshell" / "by-id" / "old" / "log.qslog", size=4096)
    _write(root / "quickshell" / "by-id" / "new" / "log.qslog", size=4096)
    _write(root / "deep" / "a" / "b" / "huge.log", size=120 * 1048576)
    _write(root / "deep" / "small.log", size=10)
    return root


NOW = 2_000_000_000.0


def _age_quickshell(root):
    qs = root / "quickshell" / "by-id"
    os.utime(qs / "old", (NOW - 4 * 86400, NOW - 4 * 86400))
    os.utime(qs / "new", (NOW - 3600, NOW - 3600))


# ---- the fixes ----------------------------------------------------------------

def _report(root, run):
    lines = []
    log = lambda s: lines.append(s.replace(str(root), "<root>"))
    err = None
    try:
        run(log)
    except Exception as exc:                            # noqa: BLE001
        err = str(exc)
    left = sorted(str(p.relative_to(root)) for p in root.rglob("*"))
    return _sur({"log": lines, "left": left, "error": err})


def fix_case(name, build, py_run):
    rust_root = build(fresh(name))
    def run():
        root = build(fresh(name))
        return _report(root, lambda log: py_run(root, log))
    return name, {"root": str(rust_root)}, run


def _py_clear_yay(root, log):
    with mock.patch.object(m, "YAY_CACHE", root / "yay"):
        m._clear_yay_cache(log)


def _py_empty_trash(root, log):
    with mock.patch.object(m, "TRASH", root / "Trash"):
        m._empty_trash(log)


def _py_delete_rollbacks(root, log):
    with mock.patch.object(inventory, "REGISTRY", root / "appimages.json"):
        m._delete_rollbacks(log)


def _py_remove_dangling(root, log):
    with mock.patch.dict(os.environ, {"XDG_DATA_HOME": str(root)}):
        m._remove_dangling(log)


def _py_remove_links(root, log):
    with mock.patch.object(m, "HOME", root):
        m._remove_broken_links(log)


def _runtime_fix_case(instances):
    def build(root):
        fx_runtime(root)
        _age_quickshell(root)
        return root
    rust_root = build(fresh("rtfix"))

    class Out:
        stdout = instances

    def run():
        root = build(fresh("rtfix"))
        def go(log):
            with mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": str(root)}), \
                 mock.patch.object(m.subprocess, "run", return_value=Out()), \
                 mock.patch.object(m.time, "time", return_value=NOW):
                m._clear_runtime_logs(log)
        return _report(root, go)
    return ("fix_clear_runtime_logs",
            {"root": str(rust_root), "instances": instances, "now": NOW}, run)


# ---- fakes ----------------------------------------------------------------------

def pacman_fake(mb, n, k2, uk0):
    def run():
        with mock.patch.object(m, "_run",
                               side_effect=lambda cmd, timeout=60: k2 if "-k2" in cmd else uk0):
            return fd(_pacman_cache_with_du(mb, n))
    return "pacman_cache_fake", {"mb": mb, "n": n, "k2": k2, "uk0": uk0}, run


def _pacman_cache_with_du(mb, n):
    with mock.patch.object(m, "_du_mb", return_value=(mb, n)):
        return m.pacman_cache()



def journal_fake(out):
    def run():
        with mock.patch.object(m, "_run", return_value=out):
            return fd(m.journal())
    return "journal_fake", {"out": out}, run


def failed_fake(system, user):
    def run():
        with mock.patch.object(m, "_run",
                               side_effect=lambda cmd, timeout=60: user if "--user" in cmd else system):
            return fd(m.failed_units())
    return "failed_units_fake", {"system": system, "user": user}, run


def pacnew_fake(new, save):
    def run():
        with mock.patch.object(m, "_run",
                               side_effect=lambda cmd, timeout=60: new if "*.pacnew" in cmd else save):
            return fd(m.pacnew())
    return "pacnew_fake", {"pacnew": new, "pacsave": save}, run


def orphans_fake(apps):
    def run():
        objs = []
        for a in apps:
            app = inventory.App(name=a["name"], version="1", source=inventory.ORPHAN)
            if "advice" in a:
                app.extra["advice"] = a["advice"]
            objs.append(app)
        with mock.patch.object(inventory, "orphans", return_value=objs):
            return fd(m.orphan_packages())
    return "orphan_packages_fake", {"apps": apps}, run


def aur_fake(out, health):
    def run():
        with mock.patch.object(m, "_run", return_value=out), \
             mock.patch.object(inventory, "aur_health", return_value=health):
            return fd(m.unmaintained_aur())
    return "unmaintained_aur_fake", {"out": out, "health": health}, run


def integrity_fake(stdout, stderr=""):
    class R:
        pass
    r = R()
    r.stdout, r.stderr = stdout, stderr
    def run():
        with mock.patch.object(m.subprocess, "run", return_value=r):
            return fd(m.package_integrity())
    return "package_integrity_fake", {"stdout": stdout, "stderr": stderr}, run


def disk_fake(table):
    def run():
        with mock.patch.object(m, "_usage",
                               side_effect=lambda p: tuple(table[p]) if p in table else None):
            return fd(m.disk_space())
    return "disk_space_fake", {"usage": table}, run


def H(ood=False, maint="someone", lm=0.0):
    return {"out_of_date": ood, "last_modified": lm, "maintainer": maint}


DENIED = ("warning: omarchy-settings: /etc/sudoers.d/omarchy-dns "
          "(Permission denied)\n"
          "warning: intel-ucode: /boot/intel-ucode.img (Permission denied)\n"
          "omarchy-settings: 643 total files, 4 missing files\n")
GONE = ("warning: somepkg: /usr/share/somepkg/data.bin "
        "(No such file or directory)\n"
        "somepkg: 12 total files, 1 missing file\n")


def cases():
    # ---- fmt_size ----
    for mb in [0, 0.0001, 0.5, 0.99951, 1, 1.5, 2.5, 512, 1023.4, 1023.6, 1024,
               1536, 4.4 * 1024, 1048576 * 3, 1e12, -1, -0.0001, -2048,
               "inf", "-inf", "nan"]:
        # inf/nan travel as text: JSON has no spelling for them.
        yield "fmt_size", {"mb": mb}, (lambda mb=mb: m.fmt_size(float(mb)))

    # ---- Finding ----
    yield "finding_default", {}, lambda: fd(m.Finding(id="", title="", detail=""))
    for kw in [
        {"id": "a", "title": "t", "detail": "d"},
        {"id": "a", "title": "t", "detail": "d", "severity": "ok", "fix": ["true"]},
        {"id": "a", "title": "t", "detail": "d", "severity": "warn", "fix": ["true", "x"]},
        {"id": "a", "title": "t", "detail": "d", "severity": "crit", "fix": "callable"},
        {"id": "a", "title": "t", "detail": "d", "severity": "ok", "fix": "callable"},
        {"id": "ü", "title": "✓", "detail": "", "severity": "weird", "fix": [],
         "count": 3, "size_mb": 1.5, "items": ["x", "ÿ"], "url": "u", "fix_note": "n",
         "fix_label": "l"},
    ]:
        yield "finding", kw, (lambda kw=kw: fd(mk(kw)))

    # ---- reclaimable_mb ----
    groups = [
        [],
        [{"id": "journal", "title": "", "detail": "", "severity": "warn", "size_mb": 500, "fix": ["true"]}],
        [{"id": "journal", "title": "", "detail": "", "severity": "ok", "size_mb": 500, "fix": ["true"]}],
        [{"id": "journal", "title": "", "detail": "", "severity": "warn", "size_mb": 500}],
        [{"id": "disk", "title": "", "detail": "", "severity": "crit", "size_mb": 9999, "fix": ["x"]},
         {"id": "trash", "title": "", "detail": "", "severity": "info", "size_mb": 60.25, "fix": "callable"},
         {"id": "pacman-cache", "title": "", "detail": "", "severity": "warn", "size_mb": 2519.04, "fix": ["p"]},
         {"id": "yay-cache", "title": "", "detail": "", "severity": "ok", "size_mb": 100, "fix": "callable"},
         {"id": "rollbacks", "title": "", "detail": "", "severity": "info", "size_mb": 0.1, "fix": "callable"},
         {"id": "runtime-logs", "title": "", "detail": "", "severity": "warn", "size_mb": 401.5, "fix": "callable"}],
    ]
    for g in groups:
        yield "reclaimable_mb", {"findings": g}, (lambda g=g: m.reclaimable_mb([mk(x) for x in g]))

    yield "check_names", {}, lambda: {
        "checks": [c.__name__ for c in m.CHECKS],
        "by_id": {k: v.__name__ for k, v in m.CHECK_BY_ID.items()},
    }

    # ---- the real machine ----
    for name in ["pacman_cache", "yay_cache", "journal", "trash", "rollbacks",
                 "failed_units", "pacnew", "dangling_desktop", "broken_symlinks",
                 "orphan_packages", "unmaintained_aur", "package_integrity",
                 "disk_space", "runtime_logs"]:
        yield name, {}, (lambda name=name: fd(getattr(m, name)()))
    yield "run_all", {}, lambda: [fd(f) for f in m.run_all()]

    # ---- paccache dry runs ----
    no = "==> no candidate packages found for pruning"
    dry = "==> finished dry run: 26 candidates (disk space saved: 1.23 GiB)"
    yield pacman_fake(4500, 522, no, no)
    yield pacman_fake(4500, 522, dry, dry)
    yield pacman_fake(4500, 522, dry, no)
    yield pacman_fake(100, 3, "", "")
    yield pacman_fake(0.0, 0, "disk space saved: 300 MiB", "")
    yield pacman_fake(9000, 900, "disk space saved: 3.5 GiB", "disk space saved: 800MiB")
    yield pacman_fake(9000, 900, "disk space saved: 1 TiB", "")
    yield pacman_fake(9000, 900, "disk space saved: 1023 KiB", "disk space saved: 9999999999 B")
    yield pacman_fake(9000, 900, "disk space saved:   2.0   G", "")
    yield pacman_fake(9000, 900, "disk space saved: 1.2.3 GiB", "")       # float() raises
    yield pacman_fake(9000, 900, "disk space saved: . GiB", "")
    yield pacman_fake(9000, 900, "Disk Space Saved: 5 GiB", "")           # case-sensitive
    yield pacman_fake(9000, 900, "disk space saved: 300 MiB\ndisk space saved: 9 GiB", "")
    yield pacman_fake(1024, 1, "disk space saved: 1024 MiB", "disk space saved: 1 MiB")
    yield pacman_fake(1024, 1, "disk space saved: 4096 MiB", "disk space saved: 0.5 MiB")

    # ---- journal ----
    for out in ["Archived and active journals take up 1.2G in the file system.",
                "Archived and active journals take up 812.5M in the file system.",
                "Archived and active journals take up 64.0M in the file system.",
                "Archived and active journals take up 2T in the file system.",
                "Archived and active journals take up 900K in the file system.",
                "Archived and active journals take up 120B in the file system.",
                "", "No journal files were found.", "garbage 1.2.3G", "..G",
                "versions 255.4 then 1.5G", "größe 3G"]:
        yield journal_fake(out)

    # ---- failed units ----
    yield failed_fake("", "")
    yield failed_fake("foo.service loaded failed failed Foo\n", "")
    yield failed_fake("a.service loaded failed failed A\nb.mount loaded failed failed B\n",
                      "pipewire.service loaded failed failed PW\n")
    yield failed_fake("\n   \n\tx.service\n", "ü.service  y\r\nz.timer")
    yield failed_fake("● dot.service loaded failed\n", "")

    # ---- pacnew ----
    yield pacnew_fake("", "")
    yield pacnew_fake("/etc/pacman.conf.pacnew\n/etc/locale.gen.pacnew\n", "/etc/x.pacsave\n")
    yield pacnew_fake("  \n/etc/a b.pacnew \n\n", "")
    yield pacnew_fake("/etc/ünï.pacnew", "")

    # ---- orphans ----
    yield orphans_fake([])
    yield orphans_fake([{"name": "rust", "advice": "keep"}])
    yield orphans_fake([{"name": "rust", "advice": "keep"}, {"name": "libfoo", "advice": "remove"},
                        {"name": "noadvice"}, {"name": "ünï", "advice": ""}])
    yield orphans_fake([{"name": "a", "advice": "remove"}])

    # ---- AUR health ----
    yield aur_fake("", {})
    yield aur_fake("  \n ", {})
    yield aur_fake("yay-bin\nfoo\n", {"yay-bin": H(), "foo": H()})
    yield aur_fake("yay-bin\ngone-pkg\nflagged\norph\n",
                   {"yay-bin": H(), "flagged": H(ood=True), "orph": H(maint="")})
    yield aur_fake("orph ood-orph", {"orph": H(maint=""), "ood-orph": H(ood=True, maint="")})
    yield aur_fake("a\nb", {})
    yield aur_fake("ünï", {"ünï": H(ood=True)})

    # ---- package integrity ----
    yield integrity_fake("")
    yield integrity_fake("   \n")
    yield integrity_fake(DENIED)
    yield integrity_fake(GONE)
    yield integrity_fake(DENIED + GONE)
    yield integrity_fake("somepkg: 12 total files, 1 missing file\n",
                         "warning: somepkg: /usr/share/somepkg/data.bin (No such file or directory)\n")
    yield integrity_fake("a: 1234567 total files, 0 missing files\nb: 1 total files, 0 missing files\n")
    yield integrity_fake("warning: x: /etc/foo (Something Odd)\nx: 5 total files, 0 missing files\n")
    yield integrity_fake("warning: x: /etc/(weird) path (Missing)\n")
    yield integrity_fake("warning without paren\nnot a warning (No such file)\n")
    yield integrity_fake("(warning: x: /y (No Such File))\n")
    yield integrity_fake("".join(f"warning: p: /usr/lib/f{i} (No such file or directory)\n"
                                 for i in range(55)) + "p: 4000 total files, 55 missing files\n")
    yield integrity_fake("warning: ü: /usr/share/ünï (PERMISSION DENIED)\n")

    # ---- disk space ----
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    yield disk_fake({"/": [40, 1600, 97.5], "/boot": [40, 1600, 97.5], runtime: [40, 1600, 97.5]})
    yield disk_fake({})
    yield disk_fake({"/": [200000, 500000, 60.0]})
    yield disk_fake({"/": [200000, 500000, 60.0], runtime: [1500, 1600, 6.25], "/boot": [100, 1024, 90.2]})
    yield disk_fake({"/": [20000, 500000, 96.0]})
    yield disk_fake({"/": [70000, 500000, 86.0], runtime: [100, 1600, 93.75]})
    yield disk_fake({"/": [2.5, 3.5, 28.5], runtime: [2.5, 3.5, 28.5]})
    yield disk_fake({"/": [0, 1, 100.0]})

    # ---- fixture directories (read-only checks share one copy) ----
    yay = fx_yay(fresh("yay"))
    yield "yay_cache_at", {"path": str(yay / "yay")}, lambda: fd(
        _patched(m, "YAY_CACHE", yay / "yay", m.yay_cache))
    yield "yay_cache_at", {"path": str(TMP / "missing")}, lambda: fd(
        _patched(m, "YAY_CACHE", TMP / "missing", m.yay_cache))
    f = _write(fresh("file") / "plain", "x")
    yield "yay_cache_at", {"path": str(f)}, lambda: fd(_patched(m, "YAY_CACHE", f, m.yay_cache))

    tr = fx_trash(fresh("trash"))
    yield "trash_at", {"trash": str(tr / "Trash")}, lambda: fd(
        _patched(m, "TRASH", tr / "Trash", m.trash))
    yield "trash_at", {"trash": str(TMP / "nope")}, lambda: fd(
        _patched(m, "TRASH", TMP / "nope", m.trash))

    rb = fx_rollbacks(fresh("rb"))
    for store_root in [rb, TMP / "no-store"]:
        yield "rollbacks_at", {"store": str(store_root / "apps")}, (
            lambda r=store_root: fd(_patched(inventory, "REGISTRY", r / "appimages.json", m.rollbacks)))
    rb2 = fresh("rb-broken")
    _mkdir(rb2 / "apps")
    os.symlink("/nonexistent", rb2 / "apps" / "x.previous")
    yield "rollbacks_at", {"store": str(rb2 / "apps")}, lambda: fd(
        _patched(inventory, "REGISTRY", rb2 / "appimages.json", m.rollbacks))

    for dh in [fx_desktop(fresh("desk")), fx_desktop_raises(fresh("desk-raise")), TMP / "no-data"]:
        yield "dangling_desktop_in", {"data_home": str(dh)}, (lambda dh=dh: fd(
            _env({"XDG_DATA_HOME": str(dh)}, m.dangling_desktop)))

    for home in [fx_links(fresh("links")), TMP / "no-home"]:
        yield "broken_symlinks_in", {"home": str(home)}, (lambda home=home: fd(
            _patched(m, "HOME", home, m.broken_symlinks)))

    rt = fx_runtime(fresh("rt"))
    small = fresh("rt-small")
    _write(small / "hypr" / "x" / "hyprland.log", size=1000)
    for r in [rt, small, fresh("rt-empty"), TMP / "no-runtime"]:
        yield "runtime_logs_in", {"runtime": str(r)}, (lambda r=r: fd(
            _env({"XDG_RUNTIME_DIR": str(r)}, m.runtime_logs)))

    # ---- the fixes ----
    yield fix_case("fix_clear_yay_cache", fx_yay, _py_clear_yay)
    yield fix_case("fix_clear_yay_cache", lambda r: r, _py_clear_yay)       # nothing to clear
    yield fix_case("fix_empty_trash", fx_trash, _py_empty_trash)
    yield fix_case("fix_empty_trash", lambda r: r, _py_empty_trash)
    yield fix_case("fix_delete_rollbacks", fx_rollbacks, _py_delete_rollbacks)
    yield fix_case("fix_remove_dangling", fx_desktop, _py_remove_dangling)
    yield fix_case("fix_remove_dangling", fx_desktop_raises, _py_remove_dangling)
    yield fix_case("fix_remove_broken_links", fx_links, _py_remove_links)
    for inst in [json.dumps([{"instance": "live1"}]), "", "not json",
                 json.dumps({"instance": "live1"}), json.dumps(["live1"]),
                 json.dumps([{"instance": "live1"}, {"instance": "dead2"}, {}])]:
        yield _runtime_fix_case(inst)


def _patched(obj, name, value, fn):
    with mock.patch.object(obj, name, value):
        return fn()


def _env(env, fn):
    with mock.patch.dict(os.environ, env):
        return fn()


# ---- review round 1: suspected divergences ---------------------------------------

# A file in a PATH directory that the owner may not execute (group-exec bit
# only): os.access(X_OK) -- what shutil.which checks -- says no, a bare
# "any x bit set" test says yes. Set before the Rust side is spawned, so both
# see the same PATH.
_PATHBIN = TMP / "pathbin"
_PATHBIN.mkdir(parents=True, exist_ok=True)
(_PATHBIN / "almanac-gx-only").write_text("#!/bin/sh\n")
os.chmod(_PATHBIN / "almanac-gx-only", 0o070)
os.environ["PATH"] = f"{_PATHBIN}:{os.environ.get('PATH', '')}"


def fx_desktop_noexec(root):
    d = _mkdir(root / "applications")
    _write(d / "gx.desktop", "[Desktop Entry]\nTryExec=almanac-gx-only\nExec=sh\n")
    return root


def fx_yay_symlink(root):
    real = _mkdir(root / "real-cache")
    _write(real / "pkg" / "PKGBUILD", "pkgname=a\n")
    os.symlink("real-cache", root / "yay")
    return root


def fx_trash_readonly(root):
    t = _mkdir(root / "Trash")
    _write(t / "files" / "ro" / "x", "x")
    os.chmod(t / "files" / "ro", 0o555)
    return root


def fx_rollbacks_quote(root):
    a = _mkdir(root / "apps")
    _mkdir(a / "it's.previous")              # a directory: unlink fails
    return root


def fx_runtime_qs_symlink(root):
    old = _mkdir(root / "elsewhere" / "oldsession")
    _write(old / "log.qslog", "x")
    _write(root / "hypr" / "dead1" / "hyprland.log", size=2 * 1048576)
    qs = _mkdir(root / "quickshell" / "by-id")
    os.symlink(old, qs / "linked")
    os.utime(old, (NOW - 10 * 86400, NOW - 10 * 86400))
    return root


def fx_runtime_tree(root):
    for rel in ["z.log", "a/x.log", "a/d/e.log", "b/c/y.log", "b/z.log",
                "b/c/q/r.log", "hypr/s1/hyprland.log", ".hidden/h.log"]:
        _write(root / rel, size=101 * 1048576)
    return root


def _runtime_fix_case_with(build, instances):
    rust_root = build(fresh("rtfix2"))

    class Out:
        stdout = instances

    def run():
        root = build(fresh("rtfix2"))
        def go(log):
            with mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": str(root)}), \
                 mock.patch.object(m.subprocess, "run", return_value=Out()), \
                 mock.patch.object(m.time, "time", return_value=NOW):
                m._clear_runtime_logs(log)
        return _report(root, go)
    return ("fix_clear_runtime_logs",
            {"root": str(rust_root), "instances": instances, "now": NOW}, run)


_old_cases = cases


def cases():                                      # noqa: F811
    yield from _old_cases()

    # Unicode digits: Python's \d and float()/int() both accept them.
    yield journal_fake("Archived and active journals take up ١٢G in the file system.")
    yield pacman_fake(9000, 900, "disk space saved: ١٢ GiB", "")
    yield integrity_fake("warning: a: /x/y (No such file or directory)\n"
                         "a: ١٢ total files, 1 missing file\n")
    # More files than an i128 holds: Python's int has no ceiling.
    yield integrity_fake("warning: a: /x/y (No such file or directory)\n"
                         "a: 123456789012345678901234567890123456789012 total files, 1 missing file\n")

    # which(): executable means X_OK for this user, not "some x bit".
    dn = fx_desktop_noexec(fresh("desk-noexec"))
    yield "dangling_desktop_in", {"data_home": str(dn)}, (lambda: fd(
        _env({"XDG_DATA_HOME": str(dn)}, m.dangling_desktop)))

    # A runtime path that is not in normal form: pathlib normalises it.
    rtn = fx_runtime(fresh("rt-norm"))
    for spelled in [str(rtn) + "/.", str(rtn).replace("/rt-norm", "//rt-norm"), str(rtn) + "/"]:
        yield "runtime_logs_in", {"runtime": spelled}, (lambda s=spelled: fd(
            _env({"XDG_RUNTIME_DIR": s}, m.runtime_logs)))

    # rglob order across a branching tree.
    rtt = fx_runtime_tree(fresh("rt-tree"))
    yield "runtime_logs_in", {"runtime": str(rtt)}, (lambda: fd(
        _env({"XDG_RUNTIME_DIR": str(rtt)}, m.runtime_logs)))

    # Fixes on unusual shapes.
    yield fix_case("fix_clear_yay_cache", fx_yay_symlink, _py_clear_yay)
    yield fix_case("fix_empty_trash", fx_trash_readonly, _py_empty_trash)
    yield fix_case("fix_delete_rollbacks", fx_rollbacks_quote, _py_delete_rollbacks)
    yield _runtime_fix_case_with(fx_runtime_qs_symlink, "")


# ---- review round 2: suspected divergences ---------------------------------------

def fx_desktop_nonutf8(root):
    d = _mkdir(root / "applications")
    (d / os.fsdecode(b"\xff-broken.desktop")).write_text("[Desktop Entry]\nExec=/nonexistent/x\n")
    return root


def fx_rollbacks_nonutf8(root):
    a = _mkdir(root / "apps")
    _write(a / os.fsdecode(b"\xfe.previous"), size=10)
    _write(a / ".previous", size=10)
    return root


def fx_links_nonutf8(root):
    b = _mkdir(root / ".local" / "bin")
    os.symlink(os.fsdecode(b"/nonexistent/\xff"), b / "dead")
    return root


def fx_runtime_ancient_qs(root):
    old = _mkdir(root / "quickshell" / "by-id" / "ancient")
    _write(old / "log.qslog", "x")
    os.utime(old, (-500000, -500000))       # before the epoch
    return root


def _runtime_fix_case_now(build, instances, now):
    rust_root = build(fresh("rtfix3"))

    class Out:
        stdout = instances

    def run():
        root = build(fresh("rtfix3"))
        def go(log):
            with mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": str(root)}), \
                 mock.patch.object(m.subprocess, "run", return_value=Out()), \
                 mock.patch.object(m.time, "time", return_value=now):
                m._clear_runtime_logs(log)
        return _report(root, go)
    return ("fix_clear_runtime_logs",
            {"root": str(rust_root), "instances": instances, "now": now}, run)


def _hypr_live(root):
    _write(root / "hypr" / "live1" / "hyprland.log", size=2048)
    return root


_old_cases_r1 = cases


def cases():                                      # noqa: F811
    yield from _old_cases_r1()

    # Python's re \s is str.isspace(), which includes the information
    # separators \x1c-\x1f; Rust regex \s is White_Space and does not.
    yield pacman_fake(9000, 900, "disk space saved:\x1c5 GiB", "")

    # Python 3.12+ sum() of floats is compensated (Neumaier); a plain fold is not.
    g = [{"id": "journal", "title": "", "detail": "", "severity": "warn", "size_mb": 1e16, "fix": ["x"]},
         {"id": "trash", "title": "", "detail": "", "severity": "warn", "size_mb": 1.0, "fix": ["x"]},
         {"id": "rollbacks", "title": "", "detail": "", "severity": "warn", "size_mb": -1e16, "fix": ["x"]}]
    yield "reclaimable_mb", {"findings": g}, (lambda: m.reclaimable_mb([mk(x) for x in g]))

    # round() of a huge float is an exact Python int; `as i64` saturates, so
    # two different filesystems get the same fingerprint.
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    yield disk_fake({"/": [1e30, 2e30, 50.0], runtime: [1.5e30, 2.5e30, 40.0]})

    # Non-UTF-8 file names: Python carries them as surrogate escapes and can
    # act on them; to_string_lossy turns them into U+FFFD.
    dn = fx_desktop_nonutf8(fresh("desk-nonutf8"))
    yield "dangling_desktop_in", {"data_home": str(dn)}, (lambda: fd(
        _env({"XDG_DATA_HOME": str(dn)}, m.dangling_desktop)))
    yield fix_case("fix_remove_dangling", fx_desktop_nonutf8, _py_remove_dangling)
    rbn = fx_rollbacks_nonutf8(fresh("rb-nonutf8"))
    yield "rollbacks_at", {"store": str(rbn / "apps")}, lambda: fd(
        _patched(inventory, "REGISTRY", rbn / "appimages.json", m.rollbacks))
    ln = fx_links_nonutf8(fresh("links-nonutf8"))
    yield "broken_symlinks_in", {"home": str(ln)}, (lambda: fd(
        _patched(m, "HOME", ln, m.broken_symlinks)))

    # hyprctl JSON that Python's json accepts and serde_json does not.
    for inst in ['[{"instance": "live1", "pid": NaN}]',
                 '[{"instance": "live1", "pid": Infinity}]',
                 '[{"instance": "live1", "n": 1e400}]']:
        yield _runtime_fix_case_now(_hypr_live, inst, NOW)

    # A session directory dated before 1970: Python's st_mtime is negative,
    # the Rust reads it as 0.
    yield _runtime_fix_case_now(fx_runtime_ancient_qs, "", 100000.0)


# ---- review round 3: Python's json for hyprctl, names as code points --------

def _hypr_nonutf8(root):
    _write(root / "hypr" / os.fsdecode(b"live\xff") / "hyprland.log", size=2048)
    _write(root / "hypr" / "live1" / "hyprland.log", size=1024)
    return root


_old_cases_r2 = cases


def cases():                                      # noqa: F811
    yield from _old_cases_r2()

    for inst in ['[{"instance": "live1", "pid": -Infinity}]',
                 '[{"instance": "live1", "n": ' + "1" * 4301 + '}]',       # int digit limit
                 '[{"instance": "live1", "n": ' + "1" * 4300 + '}]',
                 '[{"instance": "live1", "n": 1.5e-400, "m": -0.0e+5}]',
                 '[{"instance": "live1"}, {"instance": "x"}, {}]',
                 '[{"instance": "live1"}, []]',                            # a list has no .get
                 '[{"instance": "live1", "instance": 3}]',                 # last key wins
                 '[{"instance": 3, "instance": "live1"}]',
                 '[{"instance": "live1"},]',                               # trailing comma
                 '[{"instance": "live1"}] x',                              # extra data
                 ' \n[{"instance" : "live\\u0031"}]\t',
                 '[{"instance": "live1\x01"}]',                            # raw control char
                 '{}', '""', '{"instance": "live1"}', '"live1"', '3', 'null',
                 '[{"a": [[{"instance": "zzz"}]], "instance": "live1"}]',
                 '[' * 60000 + ']' * 60000]:
        yield _runtime_fix_case_now(_hypr_live, inst, NOW)
    for inst in ['[{"instance": "live\\udcff"}]', '[{"instance": "live\\u00ff"}]',
                 '[{"instance": "live\\ud83d\\ude00"}]', '[]']:
        yield _runtime_fix_case_now(_hypr_nonutf8, inst, NOW)

    # Neumaier: a lone -0.0, many small terms, cancelling large ones.
    for vals in [[float("inf"), 1.0], [-0.0], [0.1] * 10, [1e100, 1.0, -1e100, 1e-100]]:
        g = [{"id": "trash", "title": "", "detail": "", "severity": "warn", "size_mb": v, "fix": ["x"]} for v in vals]
        if all(v == v and abs(v) != float("inf") for v in vals):
            yield "reclaimable_mb", {"findings": g}, (lambda g=g: m.reclaimable_mb([mk(x) for x in g]))

    # Equal huge sizes are still one filesystem; -0.4 and 0.0 round alike.
    runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
    yield disk_fake({"/": [1e30, 2e30, 50.0], runtime: [1e30, 2e30, 40.0]})
    yield disk_fake({"/": [-0.4, 0.4, 50.0], runtime: [0.0, 0.0, 40.0]})

    # \s around the figure, with every separator Python's isspace() takes.
    for sep in ["\x1f", "\x1d\x1e", "\x85", "　", "\x0b"]:
        yield pacman_fake(9000, 900, f"disk space saved:{sep}5{sep}GiB", "")
