"""Parity cases for selfupdate.py.

`root` in the args stands in for selfupdate.ROOT (patched on the Python
side), `env` sets variables on both sides for the call (None removes).

Nothing here touches the real checkout beyond reading it: the real ROOT is
only asked read-only questions (no fetch, no pull). Fetches, dirty trees,
diverged histories and missing remotes are all played out in throwaway git
repositories under a temp dir, with a local bare repository as `origin`.
pacman is real (it owns nothing under a temp dir) or, for the "installed as a
package" branch, a fake `pacman` script first on PATH that only prints a name.
restart() is not covered: it starts a process and exits this one.
"""
import atexit
import os
import shutil
import stat
import subprocess
import sys
import tempfile
from pathlib import Path
from unittest import mock

import selfupdate

_TMP = Path(tempfile.mkdtemp(prefix="almanac-parity-selfupdate-"))
atexit.register(shutil.rmtree, _TMP, True)

_ENV = {**os.environ, "GIT_AUTHOR_NAME": "A", "GIT_AUTHOR_EMAIL": "a@example.invalid",
        "GIT_COMMITTER_NAME": "A", "GIT_COMMITTER_EMAIL": "a@example.invalid",
        "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}


def _g(cwd, *args):
    subprocess.run(["git", "-C", str(cwd), *args], check=True, env=_ENV,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def _commit(repo, name, text, msg):
    (Path(repo) / name).write_text(text)
    _g(repo, "add", name)
    _g(repo, "commit", "-q", "-m", msg)


def _repos():
    """name -> path of each scenario."""
    r = {}
    work = _TMP / "work"
    _g(_TMP, "init", "-q", "-b", "main", str(work))
    _commit(work, "app.py", "one\n", "first")
    _g(work, "tag", "v1.0")
    _commit(work, "app.py", "two\n", "second")
    _commit(work, "app.py", "three\n", "third: ünïcødé 日本 — subject")
    bare = _TMP / "origin.git"
    _g(_TMP, "clone", "-q", "--bare", str(work), str(bare))

    def clone(name, back=0):
        d = _TMP / name
        _g(_TMP, "clone", "-q", str(bare), str(d))
        if back:
            _g(d, "reset", "-q", "--hard", f"HEAD~{back}")
        return d

    r["behind"] = clone("behind", 2)
    r["uptodate"] = clone("uptodate")
    d = clone("diverged", 1)
    _commit(d, "local.txt", "x\n", "local work")
    r["diverged"] = d
    d = clone("ahead")
    _commit(d, "local.txt", "x\n", "local work")
    _commit(d, "local2.txt", "y\n", "more local work")
    r["ahead"] = d
    d = clone("dirty", 1)
    (d / "app.py").write_text("edited\n")
    r["dirty"] = d
    d = clone("dirty_uptodate")
    (d / "app.py").write_text("edited\n")
    r["dirty_uptodate"] = d
    d = clone("untracked_only", 1)
    (d / "new.txt").write_text("untracked\n")
    r["untracked_only"] = d
    d = _TMP / "noremote"
    _g(_TMP, "init", "-q", "-b", "main", str(d))
    _commit(d, "app.py", "x\n", "only")
    r["noremote"] = d
    d = _TMP / "empty_repo"
    _g(_TMP, "init", "-q", "-b", "main", str(d))
    r["empty_repo"] = d
    d = clone("badremote", 1)
    _g(d, "remote", "set-url", "origin", str(_TMP / "does-not-exist.git"))
    r["badremote"] = d
    d = clone("http_remote", 1)
    _g(d, "remote", "set-url", "origin", "https://example.invalid/me/almanac.git")
    r["http_remote"] = d
    d = clone("http_double_git")
    _g(d, "remote", "set-url", "origin", "http://example.invalid/a.git.git/b.git")
    r["http_double_git"] = d
    d = clone("ssh_remote", 1)
    _g(d, "remote", "set-url", "origin", "git@example.invalid:me/almanac.git")
    r["ssh_remote"] = d
    d = clone("detached", 1)
    _g(d, "checkout", "-q", "--detach", "HEAD")
    r["detached"] = d
    d = clone("otherbranch")
    _g(d, "checkout", "-q", "-b", "feature")
    r["otherbranch"] = d
    d = _TMP / "ünï cødé 日本"
    _g(_TMP, "clone", "-q", str(bare), str(d))
    _g(d, "reset", "-q", "--hard", "HEAD~1")
    r["unicode"] = d
    d = clone("subdir_parent", 1)
    (d / "sub").mkdir()
    r["subdir"] = d / "sub"
    d = _TMP / "plain"
    d.mkdir()
    (d / "app.py").write_text("print()\n")
    r["plain"] = d
    r["missing"] = _TMP / "no-such-dir"
    return r


def _bin(name, script):
    d = _TMP / f"bin-{name}"
    d.mkdir(exist_ok=True)
    f = d / "pacman"
    f.write_text(script)
    f.chmod(f.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return f"{d}:{os.environ.get('PATH', '')}"


# The app runs at CPython's default recursion limit; another module's cases
# raise it for the whole runner process (cases_ghcache: 20000), which would
# let release_notes() serialize nesting the app cannot. Pin it per case.
APP_RECURSION_LIMIT = 1000


def _call(root, env, fn):
    def run():
        saved = {k: os.environ.get(k) for k in env}
        old_limit = sys.getrecursionlimit()
        sys.setrecursionlimit(APP_RECURSION_LIMIT)
        try:
            for k, v in env.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
            if root is None:
                return fn()
            with mock.patch.object(selfupdate, "ROOT", Path(root)):
                return fn()
        finally:
            sys.setrecursionlimit(old_limit)
            for k, v in saved.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v
    return run


def _args(root, env, **kw):
    a = dict(kw)
    if root is not None:
        a["root"] = str(root)
    if env:
        a["env"] = env
    return a


_META = '<?xml version="1.0" encoding="UTF-8"?>\n<component>{}</component>\n'


def _notes_roots():
    """Temp roots whose docs/ hold metainfo variants."""
    variants = {
        "simple": _META.format('<releases><release version="2.0" date="x"><description>'
                               '<p>Hello</p><ul><li>One</li><li>Two</li></ul></description>'
                               '</release><release version="1.0"><description><p>old</p>'
                               '</description></release></releases>'),
        "whitespace": _META.format('\n  <releases>\n    <release version="3">\n      <description>\n'
                                   '        intro text dropped\n        <p>a\n   b\tc</p>\n   tail  text\n'
                                   '        <p>d</p>\n      </description>\n    </release>\n  </releases>'),
        "entities": _META.format('<releases><release version="&quot;q&quot; &amp; &#x263A;">'
                                 '<description><p>a &lt; b &gt; c &amp; d &apos; e &quot; f &#169; &#x1F600;</p>'
                                 '<p title="x&quot;y&lt;z&#10;w&#9;v">attr</p></description></release></releases>'),
        "attr_ws": _META.format('<releases><release version=" 1\n\t2 "><description>'
                                '<p a="1" b=\'2\' c="tab\there&#13;">x</p></description></release></releases>'),
        "empty_elems": _META.format('<releases><release version="1"><description><p/><p></p>'
                                    '<ul><li/></ul><br   /></description></release></releases>'),
        "nested_markup": _META.format('<releases><release version="1"><description><p>This <em>is</em> '
                                      '<code>code</code> and<!-- comment --> more<?pi data?> text</p>'
                                      '<ol><li>first <em>x</em> tail</li></ol></description></release></releases>'),
        "cdata": _META.format('<releases><release version="1"><description><p><![CDATA[a <b> & c]]> d</p>'
                              '</description></release></releases>'),
        "no_description": _META.format('<releases><release version="9"/></releases>'),
        "empty_description": _META.format('<releases><release version="9"><description/></release></releases>'),
        "text_only_description": _META.format('<releases><release version="9"><description>just text</description></release></releases>'),
        "no_version": _META.format('<releases><release><description><p>v-less</p></description></release></releases>'),
        "no_releases": _META.format('<id>x</id>'),
        "empty_releases_then_full": _META.format('<releases/><releases><release version="5"><description>'
                                                 '<p>second</p></description></release></releases>'),
        "release_not_direct": _META.format('<x><releases><release version="1"><description><p>deep</p>'
                                           '</description></release></releases></x>'),
        "unicode": _META.format('<releases><release version="1.0-ünï"><description><p>日本語 — ünïcødé '
                                ' nbsp em</p><p xml:lang="de">Grüße</p></description></release></releases>'),
        "namespaced_root": '<component xmlns="http://example.invalid/ns"><releases><release version="1">'
                           '<description><p>x</p></description></release></releases></component>',
        "namespaced_children": _META.format('<releases><release version="1"><description xmlns:h="http://www.w3.org/1999/xhtml">'
                                            '<h:p h:class="c">x</h:p><q xmlns="urn:a"><r/></q></description></release></releases>'),
        "doctype": '<?xml version="1.0"?>\n<!DOCTYPE component [ <!ELEMENT component ANY> ]>\n'
                   '<!-- c -->\n<component><releases><release version="7"><description><p>dt</p>'
                   '</description></release></releases></component>',
        "bom_crlf": '﻿<?xml version="1.0"?>\r\n<component>\r\n<releases><release version="8">'
                    '<description><p>line\r\nbreak</p><p a="x\r\ny">z</p></description></release></releases></component>\r\n',
        "malformed_unclosed": _META.format('<releases><release version="1"><description><p>x</description>'),
        "malformed_entity": _META.format('<releases><release version="1"><description><p>&nbsp;</p>'
                                         '</description></release></releases>'),
        "malformed_amp": _META.format('<releases><release version="1"><description><p>a & b</p>'
                                      '</description></release></releases>'),
        "malformed_two_roots": _META.format('') + '<extra/>',
        "malformed_dup_attr": _META.format('<releases><release version="1" version="2"><description><p>x</p>'
                                           '</description></release></releases>'),
        "malformed_unbound_prefix": _META.format('<releases><release version="1"><description><h:p>x</h:p>'
                                                 '</description></release></releases>'),
        "malformed_lt_in_attr": _META.format('<releases><release version="<1"><description><p>x</p>'
                                             '</description></release></releases>'),
        "malformed_control_char": _META.format('<releases><release version="1"><description><p>\x01</p>'
                                               '</description></release></releases>'),
        "empty_file": "",
        "only_decl": '<?xml version="1.0"?>',
        "text_after_root": _META.format('') + "junk",
        "gt_in_text": _META.format('<releases><release version="1"><description><p>a > b ]> c</p>'
                                   '</description></release></releases>'),
    }
    out = {}
    for name, text in variants.items():
        root = _TMP / "notes" / name
        (root / "docs").mkdir(parents=True)
        (root / "docs" / selfupdate.METAINFO).write_bytes(text.encode("utf-8"))
        out[name] = root
    root = _TMP / "notes" / "latin1"
    (root / "docs").mkdir(parents=True)
    (root / "docs" / selfupdate.METAINFO).write_bytes(
        _META.format('<releases><release version="1"><description><p>caf\xe9</p></description>'
                     '</release></releases>').encode("latin-1"))
    out["invalid_utf8"] = root
    root = _TMP / "notes" / "is_a_dir"
    (root / "docs" / selfupdate.METAINFO).mkdir(parents=True)
    out["is_a_dir"] = root
    out["no_docs"] = _TMP / "notes" / "no_docs"
    out["no_docs"].mkdir(parents=True)
    return out


def cases():
    yield "constants", {}, lambda: {"NAME": selfupdate.NAME, "VERSION": selfupdate.VERSION,
                                    "GIT": selfupdate.GIT, "PACKAGE": selfupdate.PACKAGE,
                                    "PLAIN": selfupdate.PLAIN, "METAINFO": selfupdate.METAINFO,
                                    "ROOT": str(selfupdate.ROOT)}

    # release_notes: the real checkout, then every variant.
    yield "release_notes", {}, selfupdate.release_notes
    for name, root in _notes_roots().items():
        yield "release_notes", _args(root, None), _call(root, {}, selfupdate.release_notes)

    repos = _repos()
    fake_pkg = _bin("pkg", "#!/bin/sh\necho almanac-git\nexit 0\n")
    fake_pkg_unicode = _bin("pkgu", "#!/bin/sh\nprintf '  almänac-日本\\r\\n\\n'\nexit 0\n")
    fake_empty = _bin("empty", "#!/bin/sh\nexit 0\n")
    fake_fail = _bin("fail", "#!/bin/sh\necho almanac-git\nexit 1\n")
    no_git_path = str(_TMP / "nothing-here")

    # Readers on the real checkout (read-only: no fetch).
    for func in ["install_kind", "owning_package", "version", "remote_url", "branch", "dirty",
                 "update_steps"]:
        yield func, {}, getattr(selfupdate, func)
    yield "check", {"fetch": False}, lambda: selfupdate.check(fetch=False)
    yield "as_app", {"check_updates": False}, lambda: selfupdate.as_app(check_updates=False)
    for args in [["rev-parse", "--is-inside-work-tree"], ["status", "--porcelain"],
                 ["log", "-1", "--format=%H"], ["no-such-subcommand"], [],
                 ["rev-parse", "--verify", "ünï-nope"]]:
        yield "_git", {"args": args}, (lambda a=args: selfupdate._git(*a))

    # Every scenario, through every reader.
    for name, root in repos.items():
        for func in ["install_kind", "owning_package", "version", "remote_url", "branch",
                     "dirty", "update_steps"]:
            yield func, _args(root, None), _call(root, {}, getattr(selfupdate, func))
        for fetch in (False, True):
            yield "check", _args(root, None, fetch=fetch), \
                _call(root, {}, lambda f=fetch: selfupdate.check(fetch=f))
            yield "as_app", _args(root, None, check_updates=fetch), \
                _call(root, {}, lambda f=fetch: selfupdate.as_app(check_updates=f))
        yield "_git", _args(root, None, args=["status", "--short"]), \
            _call(root, {}, lambda: selfupdate._git("status", "--short"))

    # git missing from PATH: every _git call is (1, the OSError).
    for root in [repos["behind"], repos["plain"]]:
        env = {"PATH": no_git_path}
        yield "_git", _args(root, env, args=["status"]), \
            _call(root, env, lambda: selfupdate._git("status"))
        for func in ["install_kind", "owning_package", "version", "remote_url", "branch", "dirty"]:
            yield func, _args(root, env), _call(root, env, getattr(selfupdate, func))
        yield "check", _args(root, env, fetch=True), _call(root, env, lambda: selfupdate.check(fetch=True))
        yield "as_app", _args(root, env, check_updates=True), \
            _call(root, env, lambda: selfupdate.as_app(check_updates=True))

    # A timeout: a git alias that sleeps past it.
    root = repos["uptodate"]
    yield "_git", _args(root, None, args=["-c", "alias.slow=!sleep 3", "slow"], timeout=1), \
        _call(root, {}, lambda: selfupdate._git("-c", "alias.slow=!sleep 3", "slow", timeout=1))

    # Installed as a package (a fake pacman claims app.py), and its edges.
    for label, path in [("pkg", fake_pkg), ("pkgu", fake_pkg_unicode), ("empty", fake_empty),
                        ("fail", fake_fail)]:
        for root in [repos["plain"], repos["missing"], repos["behind"]]:
            env = {"PATH": path}
            for func in ["install_kind", "owning_package", "version"]:
                yield func, _args(root, env), _call(root, env, getattr(selfupdate, func))
            yield "check", _args(root, env, fetch=False), _call(root, env, lambda: selfupdate.check(fetch=False))
            yield "as_app", _args(root, env, check_updates=False), \
                _call(root, env, lambda: selfupdate.as_app(check_updates=False))

    # The launcher, and the exec path, follow HOME.
    for home_name in ["home-with", "hömé 日本"]:
        home = _TMP / home_name
        (home / ".local/bin").mkdir(parents=True)
        (home / ".local/bin/almanac").write_text("#!/bin/sh\n")
        env = {"HOME": str(home)}
        yield "restart_argv", _args(None, env, executable=sys.executable), _call(None, env, selfupdate.restart_argv)
        yield "as_app", _args(repos["behind"], env, check_updates=False), \
            _call(repos["behind"], env, lambda: selfupdate.as_app(check_updates=False))
    # A launcher that is a directory still "exists".
    home = _TMP / "home-dir-launcher"
    (home / ".local/bin/almanac").mkdir(parents=True)
    env = {"HOME": str(home)}
    yield "restart_argv", _args(None, env, executable=sys.executable), _call(None, env, selfupdate.restart_argv)


# ---- Review round 1: adversarial cases ----

_FAKE_GIT = r'''#!/bin/sh
# A stand-in git: answers the subcommands selfupdate asks, after "-C <root>".
shift 2
case "$1 $2" in
  "rev-parse --is-inside-work-tree") echo true; exit 0 ;;
  "rev-parse --abbrev-ref") echo main; exit 0 ;;
  "describe --tags") echo v9; exit 0 ;;
  "remote get-url") echo https://example.invalid/x.git; exit 0 ;;
  "status --porcelain") exit 0 ;;
  "fetch --quiet") exit 0 ;;
  "log -1") echo "abc subject"; exit 0 ;;
esac
case "$1" in
  rev-list) printf '%s\n' "$FAKE_COUNTS"; exit 0 ;;
  killme) kill -9 $$ ;;
  badutf8) printf 'caf\351\n'; exit 0 ;;
esac
exit 0
'''


def _gitbin(name, script, mode=0o755):
    d = _TMP / f"gitbin-{name}"
    d.mkdir(exist_ok=True)
    f = d / "git"
    if script is None:
        f.mkdir(exist_ok=True)          # a directory called git
    else:
        f.write_bytes(script.encode("latin-1") if isinstance(script, str) else script)
        f.chmod(mode)
    return str(d)


def _review_notes_roots():
    deep = "<p>" + "<b>" * 1200 + "x" + "</b>" * 1200 + "</p>"
    variants = {
        "rv_decl_latin1": '<?xml version="1.0" encoding="ISO-8859-1"?>\n<component><releases>'
                          '<release version="1"><description><p>caf\xe9</p></description>'
                          '</release></releases></component>',
        "rv_decl_ascii_nonascii": '<?xml version="1.0" encoding="US-ASCII"?>\n<component><releases>'
                                  '<release version="1"><description><p>日本</p></description>'
                                  '</release></releases></component>',
        "rv_decl_bogus_encoding": '<?xml version="1.0" encoding="bogus"?>\n<component><releases>'
                                  '<release version="1"><description><p>x</p></description>'
                                  '</release></releases></component>',
        "rv_decl_no_version": '<?xml?>\n<component><releases><release version="1"><description>'
                              '<p>x</p></description></release></releases></component>',
        "rv_internal_entity": '<?xml version="1.0"?>\n<!DOCTYPE component [ <!ENTITY who "World"> ]>\n'
                              '<component><releases><release version="1"><description><p>Hi &who;</p>'
                              '</description></release></releases></component>',
        "rv_default_attr": '<?xml version="1.0"?>\n<!DOCTYPE component [ <!ATTLIST release version CDATA "4.2"> ]>\n'
                           '<component><releases><release><description><p>x</p>'
                           '</description></release></releases></component>',
        "rv_long_charref": _META.format('<releases><release version="1"><description><p>&#x00000000000000041;</p>'
                                        '</description></release></releases>'),
        "rv_bad_namechar": _META.format('<releases><release version="1"><description><p x>y</p x>'
                                        '</description></release></releases>'),
        "rv_xmlns_xml_rebound": _META.format('<releases><release version="1"><description><p xmlns:xml="urn:x">y</p>'
                                             '</description></release></releases>'),
        "rv_xmlns_xmlns": _META.format('<releases><release version="1"><description><p xmlns:xmlns="urn:x">y</p>'
                                       '</description></release></releases>'),
        "rv_reserved_ns": _META.format('<releases><release version="1"><description>'
                                       '<p xmlns:b="http://www.w3.org/XML/1998/namespace">y</p>'
                                       '</description></release></releases>'),
        "rv_doctype_nospace": '<!DOCTYPEcomponent>\n<component><releases><release version="1"><description>'
                              '<p>x</p></description></release></releases></component>',
        "rv_deep_1200": _META.format('<releases><release version="1"><description>' + deep +
                                     '</description></release></releases>'),
    }
    out = {}
    for name, text in variants.items():
        root = _TMP / "notes" / name
        (root / "docs").mkdir(parents=True)
        enc = "latin-1" if name == "rv_decl_latin1" else "utf-8"
        (root / "docs" / selfupdate.METAINFO).write_bytes(text.encode(enc))
        out[name] = root
    root = _TMP / "notes" / "rv_utf16"
    (root / "docs").mkdir(parents=True)
    (root / "docs" / selfupdate.METAINFO).write_bytes(
        ('﻿<?xml version="1.0" encoding="UTF-16"?>\n<component><releases><release version="16">'
         '<description><p>sixteen</p></description></release></releases></component>').encode("utf-16-le"))
    out["rv_utf16"] = root
    return out


_cases_round0 = cases


def cases():
    yield from _cases_round0()

    for name, root in _review_notes_roots().items():
        yield "release_notes", _args(root, None), _call(root, {}, selfupdate.release_notes)

    plain = _TMP / "plain"

    # restart_argv with no launcher: Python [sys.executable, ROOT/app.py].
    home = _TMP / "home-without"
    home.mkdir(exist_ok=True)
    env = {"HOME": str(home)}
    yield "restart_argv", _args(None, env, executable=sys.executable), _call(None, env, selfupdate.restart_argv)

    # HOME unset: Path.home() falls back to the passwd entry.
    env = {"HOME": None}
    yield "restart_argv", _args(None, env, executable=sys.executable), _call(None, env, selfupdate.restart_argv)
    yield "as_app", _args(plain, env, check_updates=False), \
        _call(plain, env, lambda: selfupdate.as_app(check_updates=False))
    # HOME with redundant separators: Path() normalises them.
    for h in ["/tmp//almanac-x", "/tmp/./almanac-x"]:
        env = {"HOME": h}
        yield "as_app", _args(plain, env, check_updates=False), \
            _call(plain, env, lambda: selfupdate.as_app(check_updates=False))

    # git killed by a signal: Python's returncode is -9.
    fake = _gitbin("fake", _FAKE_GIT)
    env = {"PATH": f"{fake}:/usr/bin:/bin"}
    yield "_git", _args(plain, env, args=["killme"]), _call(plain, env, lambda: selfupdate._git("killme"))
    # Output that is not UTF-8: text=True raises UnicodeDecodeError.
    yield "_git", _args(plain, env, args=["badutf8"]), _call(plain, env, lambda: selfupdate._git("badutf8"))

    # rev-list counts that int() accepts but are not ASCII i64.
    for counts in ["٣\t٤", "99999999999999999999 1", "1 2 3", "７ ０"]:
        env = {"PATH": f"{fake}:/usr/bin:/bin", "FAKE_COUNTS": counts}
        yield "check", _args(plain, env, fetch=False), _call(plain, env, lambda: selfupdate.check(fetch=False))

    # A git that exists but cannot be exec'd: no shebang (ENOEXEC), a directory (EACCES).
    for label, script in [("noshebang", "echo hi\n"), ("garbage", b"\x7fELF\x00\x00garbage")]:
        d = _gitbin(label, script)
        env = {"PATH": d}
        yield "_git", _args(plain, env, args=["status"]), _call(plain, env, lambda: selfupdate._git("status"))
    d = _gitbin("dir", None)
    env = {"PATH": d}
    yield "_git", _args(plain, env, args=["status"]), _call(plain, env, lambda: selfupdate._git("status"))
    # Non-executable git file only.
    d = _gitbin("noexec", "#!/bin/sh\nexit 0\n", mode=0o644)
    env = {"PATH": d}
    yield "_git", _args(plain, env, args=["status"]), _call(plain, env, lambda: selfupdate._git("status"))

    # The TimeoutExpired message repr()s argv: non-printable characters are escaped.
    root = _TMP / "uptodate"
    a = ["-c", "alias.slow=!sleep 3 #   \x85​", "slow"]
    yield "_git", _args(root, None, args=a, timeout=1), \
        _call(root, {}, lambda: selfupdate._git(*a, timeout=1))

    # A real commit whose subject is not UTF-8, waiting upstream.
    work = _TMP / "rv_latin1_work"
    _g(_TMP, "clone", "-q", str(_TMP / "origin.git"), str(work))
    (work / "l.txt").write_text("l\n")
    _g(work, "add", "l.txt")
    subprocess.run([b"git", b"-C", str(work).encode(), b"-c", b"i18n.commitEncoding=utf-8",
                    b"commit", b"-q", b"-m", b"caf\xe9 latin1"], check=True, env=_ENV,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    bare = _TMP / "rv_latin1.git"
    _g(_TMP, "clone", "-q", "--bare", str(work), str(bare))
    d = _TMP / "rv_latin1_behind"
    _g(_TMP, "clone", "-q", str(bare), str(d))
    _g(d, "reset", "-q", "--hard", "HEAD~1")
    yield "check", _args(d, None, fetch=False), _call(d, {}, lambda: selfupdate.check(fetch=False))


# ---- Review round 2: adversarial cases ----

def _round2_notes_roots():
    lol = '<!ENTITY a "' + "x" * 100 + '">' + "".join(
        f'<!ENTITY {chr(98 + i)} "' + f"&{chr(97 + i)};" * 10 + '">' for i in range(5))
    variants = {
        "r2_billion_laughs": '<?xml version="1.0"?>\n<!DOCTYPE component [' + lol + ']>\n'
                             '<component><releases><release version="1"><description><p>&f;</p>'
                             '</description></release></releases></component>',
        "r2_entity_markup": '<?xml version="1.0"?>\n<!DOCTYPE component [ <!ENTITY m "<em>x</em> &amp; y"> ]>\n'
                            '<component><releases><release version="1"><description><p>a &m; b</p>'
                            '</description></release></releases></component>',
        "r2_entity_in_attr": '<?xml version="1.0"?>\n<!DOCTYPE component [ <!ENTITY v "1&#9;2&#10;3"> ]>\n'
                             '<component><releases><release version="&v;"><description><p t="&v;">x</p>'
                             '</description></release></releases></component>',
        "r2_external_entity": '<?xml version="1.0"?>\n<!DOCTYPE component [ <!ENTITY ext SYSTEM "file:///etc/hostname"> ]>\n'
                              '<component><releases><release version="1"><description><p>&ext;</p>'
                              '</description></release></releases></component>',
        "r2_pe_then_entity": '<?xml version="1.0"?>\n<!DOCTYPE component [ <!ENTITY % pe SYSTEM "x.dtd"> %pe; '
                             '<!ENTITY late "L"> ]>\n<component><releases><release version="1"><description>'
                             '<p>&late;</p></description></release></releases></component>',
        "r2_pe_standalone": '<?xml version="1.0" standalone="yes"?>\n<!DOCTYPE component [ <!ENTITY % pe SYSTEM "x.dtd"> %pe; '
                            '<!ENTITY late "L"> ]>\n<component><releases><release version="1"><description>'
                            '<p>&late;</p></description></release></releases></component>',
        "r2_recursive_entity": '<?xml version="1.0"?>\n<!DOCTYPE component [ <!ENTITY a "&b;"> <!ENTITY b "&a;"> ]>\n'
                               '<component><releases><release version="1"><description><p>&a;</p>'
                               '</description></release></releases></component>',
        "r2_charref_zero": _META.format('<releases><release version="1"><description><p>&#0;</p>'
                                        '</description></release></releases>'),
        "r2_charref_surrogate": _META.format('<releases><release version="1"><description><p>&#xD800;</p>'
                                             '</description></release></releases>'),
        "r2_xmlns_undeclare": '<component xmlns="urn:a"><releases xmlns=""><release version="1"><description>'
                              '<p>x</p></description></release></releases></component>',
        "r2_two_ns_attrs": _META.format('<releases><release version="1"><description>'
                                        '<p xmlns:b="urn:b" xmlns:a="urn:a" b:x="1" a:y="2" z="3">t</p><a:q xmlns:a="urn:c"/>'
                                        '</description></release></releases>'),
        "r2_ns_prefix_collision": _META.format('<releases><release version="1"><description>'
                                               '<p xmlns:ns0="urn:z" ns0:k="v" xmlns:x="urn:y" x:k="w">t</p>'
                                               '</description></release></releases>'),
        "r2_xhtml_ns": _META.format('<releases><release version="1"><description>'
                                    '<p xmlns="http://www.w3.org/1999/xhtml">h</p><s xmlns="http://www.w3.org/2000/svg"/>'
                                    '</description></release></releases>'),
        "r2_attr_order_dup_names": _META.format('<releases><release version="1"><description>'
                                                '<p z="1" a="2" m="3" xml:space="preserve">t</p>'
                                                '</description></release></releases>'),
        "r2_cr_in_text": _META.format('<releases><release version="1"><description><p>a&#13;b\rc&#13;&#10;d</p>'
                                      '</description></release></releases>'),
        "r2_tail_gt": _META.format('<releases><release version="1"><description><p>x</p>]]&gt; &amp;&lt;tail'
                                   '<p>&#xA0;&#x2028;&#x85;</p></description></release></releases>'),
        "r2_ws_only_desc": _META.format('<releases><release version="1"><description>  \n\t </description></release></releases>'),
        "r2_version_entity_ws": _META.format('<releases><release version="  &#32;a&#x20;  b "><description><p>x</p>'
                                            '</description></release></releases>'),
        "r2_release_mixed_case": _META.format('<releases><Release version="1"><description><p>x</p></description></Release>'
                                              '<release version="2"><Description><p>y</p></Description></release></releases>'),
        "r2_xml11": '<?xml version="1.1"?>\n<component><releases><release version="1"><description><p>x</p>'
                    '</description></release></releases></component>',
        "r2_decl_after_ws": '\n<?xml version="1.0"?>\n<component><releases><release version="1"><description><p>x</p>'
                            '</description></release></releases></component>',
        "r2_decl_single_quotes": "<?xml version='1.0' encoding='utf-8' standalone='no'?>\n<component><releases>"
                                 "<release version='1'><description><p>x</p></description></release></releases></component>",
        "r2_decl_bad_standalone": '<?xml version="1.0" standalone="maybe"?>\n<component><releases><release version="1">'
                                  '<description><p>x</p></description></release></releases></component>',
        "r2_pi_xml_reserved": _META.format('<releases><release version="1"><description><?xml-foo bar?><p>x</p>'
                                           '</description></release></releases>'),
        "r2_comment_double_dash": _META.format('<releases><release version="1"><description><!-- a -- b --><p>x</p>'
                                               '</description></release></releases>'),
        "r2_nonchar_fffe": _META.format('<releases><release version="1"><description><p>￾</p>'
                                        '</description></release></releases>'),
        "r2_nonchar_fdd0": _META.format('<releases><release version="1"><description><p>﷐ \U0010ffff</p>'
                                        '</description></release></releases>'),
        "r2_name_combining_start": _META.format('<releases><release version="1"><description><́p>x</́p>'
                                                '</description></release></releases>'),
        "r2_name_nonbmp": _META.format('<releases><release version="1"><description><\U00010000p>x</\U00010000p>'
                                       '</description></release></releases>'),
        "r2_name_colon_twice": _META.format('<releases><release version="1"><description><a:b:c xmlns:a="urn:a">x</a:b:c>'
                                            '</description></release></releases>'),
        "r2_name_trailing_colon": _META.format('<releases><release version="1"><description><p: >x</p:>'
                                               '</description></release></releases>'),
        "r2_attr_no_space": _META.format('<releases><release version="1"><description><p a="1"b="2">x</p>'
                                         '</description></release></releases>'),
        "r2_cdata_end_in_text": _META.format('<releases><release version="1"><description><p>a ]]> b</p>'
                                             '</description></release></releases>'),
    }
    encs = {
        "r2_enc_utf8_alias": ("utf8", "utf-8", "café"),
        "r2_enc_utf8_alias_ascii": ("utf8", "utf-8", "cafe"),
        "r2_enc_cp1252": ("windows-1252", "cp1252", "€ café"),
        "r2_enc_cp1252_undef": ("cp1252", None, b"\x81"),
        "r2_enc_latin9": ("ISO-8859-15", "iso8859-15", "€"),
        "r2_enc_koi8r": ("KOI8-R", "koi8-r", "привет"),
        "r2_enc_macroman": ("macintosh", "mac-roman", "café"),
        "r2_enc_cp437": ("IBM437", "cp437", "╬"),
        "r2_enc_ascii_word": ("ascii", "ascii", "x"),
        "r2_enc_ascii_word_high": ("ascii", None, b"\xe9"),
        "r2_enc_sjis": ("Shift_JIS", "shift_jis", "x"),
        "r2_enc_utf16_on_utf8": ("UTF-16", "utf-8", "x"),
        "r2_enc_utf8sig": ("utf-8-sig", "utf-8", "café"),
        "r2_enc_latin1_mixedcase": ("Latin-1", "latin-1", "é"),
        "r2_enc_iso_8859_1_spaces": ("iso_8859-1", "latin-1", "é"),
        "r2_enc_cp65001": ("cp65001", "utf-8", "é"),
        "r2_enc_tis620": ("TIS-620", "tis-620", "ก"),
        "r2_enc_empty": ("", "utf-8", "x"),
        "r2_enc_dotted": ("x.y", "utf-8", "x"),
        "r2_enc_hz": ("hz", None, b"~"),
    }
    out = {}
    for name, text in variants.items():
        root = _TMP / "notes" / name
        (root / "docs").mkdir(parents=True)
        (root / "docs" / selfupdate.METAINFO).write_bytes(text.encode("utf-8", "surrogatepass"))
        out[name] = root
    for name, (decl, codec, body) in encs.items():
        root = _TMP / "notes" / name
        (root / "docs").mkdir(parents=True)
        head = f'<?xml version="1.0" encoding="{decl}"?>\n<component><releases><release version="1"><description><p>'.encode()
        tail = b'</p></description></release></releases></component>'
        body_b = body if isinstance(body, bytes) else body.encode(codec)
        (root / "docs" / selfupdate.METAINFO).write_bytes(head + body_b + tail)
        out[name] = root
    # UTF-16 without BOM, declared; UTF-8 BOM with a Latin-1 declaration.
    root = _TMP / "notes" / "r2_utf16be_nobom"
    (root / "docs").mkdir(parents=True)
    (root / "docs" / selfupdate.METAINFO).write_bytes(
        ('<?xml version="1.0" encoding="UTF-16BE"?><component><releases><release version="b">'
         '<description><p>be</p></description></release></releases></component>').encode("utf-16-be"))
    out["r2_utf16be_nobom"] = root
    root = _TMP / "notes" / "r2_bom8_decl_latin1"
    (root / "docs").mkdir(parents=True)
    (root / "docs" / selfupdate.METAINFO).write_bytes(
        b'\xef\xbb\xbf<?xml version="1.0" encoding="ISO-8859-1"?><component><releases><release version="b">'
        b'<description><p>x</p></description></release></releases></component>')
    out["r2_bom8_decl_latin1"] = root
    # Nesting right around the serializer's recursion limit.
    for depth in range(984, 1000, 1):
        for withtext in (False, True):
            inner = "x" if withtext else ""
            deep = "<p>" + "<b>" * depth + inner + "</b>" * depth + "</p>"
            name = f"r2_depth_{depth}_{int(withtext)}"
            root = _TMP / "notes" / name
            (root / "docs").mkdir(parents=True)
            (root / "docs" / selfupdate.METAINFO).write_text(
                _META.format('<releases><release version="1"><description>' + deep +
                             '</description></release></releases>'))
            out[name] = root
    return out


_cases_round1 = cases


def cases():
    yield from _cases_round1()

    for name, root in _round2_notes_roots().items():
        yield "release_notes", _args(root, None), _call(root, {}, selfupdate.release_notes)

    plain = _TMP / "plain"
    home = _TMP / "home-without"

    # sys.executable vs a PATH lookup of python3.
    for path in ["/nonexistent", None]:
        env = {"HOME": str(home), "PATH": path}
        yield "restart_argv", _args(None, env, executable=sys.executable), _call(None, env, selfupdate.restart_argv)
    fakepy = _TMP / "fakepy"
    fakepy.mkdir(exist_ok=True)
    (fakepy / "python3").write_text("#!/bin/sh\n")
    (fakepy / "python3").chmod(0o755)
    env = {"HOME": str(home), "PATH": f"{fakepy}:/usr/bin:/bin"}
    yield "restart_argv", _args(None, env, executable=sys.executable), _call(None, env, selfupdate.restart_argv)

    # Odd HOME values.
    for h in ["~", "~/", "rel/home", "", "/", "~root", "//x//"]:
        env = {"HOME": h}
        yield "restart_argv", _args(None, env, executable=sys.executable), _call(None, env, selfupdate.restart_argv)
        yield "as_app", _args(plain, env, check_updates=False), \
            _call(plain, env, lambda: selfupdate.as_app(check_updates=False))

    # PATH unset: os.defpath finds git.
    env = {"PATH": None}
    yield "_git", _args(plain, env, args=["--version"]), _call(plain, env, lambda: selfupdate._git("--version"))
    yield "check", _args(_TMP / "behind", env, fetch=False), \
        _call(_TMP / "behind", env, lambda: selfupdate.check(fetch=False))
    # PATH with an empty entry and a trailing slash entry.
    env = {"PATH": ":/usr/bin/"}
    yield "_git", _args(plain, env, args=["--version"]), _call(plain, env, lambda: selfupdate._git("--version"))

    # An embedded NUL in an argument: Python raises ValueError (not caught).
    yield "_git", _args(plain, None, args=["status\x00x"]), _call(plain, {}, lambda: selfupdate._git("status\x00x"))

    # Timeout message with quotes in ROOT.
    q = _TMP / "it's \"q\""
    q.mkdir(exist_ok=True)
    a = ["-c", "alias.slow=!sleep 3", "slow"]
    yield "_git", _args(q, None, args=a, timeout=1), _call(q, {}, lambda: selfupdate._git(*a, timeout=1))
    q2 = _TMP / "it's"
    q2.mkdir(exist_ok=True)
    yield "_git", _args(q2, None, args=a, timeout=1), _call(q2, {}, lambda: selfupdate._git(*a, timeout=1))

    # Fake git edge outputs for check().
    fake = _gitbin("fake", _FAKE_GIT)
    for counts in ["0 0", "-1 -2", "+3 +0", "0_1 2", "1__0 2", "01 02", "1\x1c2", "", "   ", "a b",
                   "0 " + "9" * 4300, "0 " + "9" * 4301, "1 2", "1\xa02", "0 ¹"]:
        env = {"PATH": f"{fake}:/usr/bin:/bin", "FAKE_COUNTS": counts}
        yield "check", _args(plain, env, fetch=True), _call(plain, env, lambda: selfupdate.check(fetch=True))
        yield "as_app", _args(plain, env, check_updates=False), \
            _call(plain, env, lambda: selfupdate.as_app(check_updates=False))


# ---- The compiled build's rule (intentionally not the Python's) ----
#
# A compiled Almanac cannot apply a pull by restarting, so it never updates
# itself (selfupdate::compiled_check_in): a pacman-owned binary defers to
# pacman, and a checkout that the Python would pull says to pull and rebuild
# instead. `check` and `as_app` on the Rust side answer for that rule, with
# `exe` standing in for the running binary: inside `root`, as a binary built
# in that checkout is. That makes the kind the Python's in every case here
# (the fake pacman scripts answer the same for the binary as for app.py, and
# the real pacman owns neither), so the expected answer is the Python's with
# the rule applied:
#
# * not a git checkout: the version is the crate's (Cargo.toml), not the
#   Python release's VERSION;
# * a checkout the Python would pull: can_update is False, the note gains
#   REBUILD_NOTE, and the inventory row carries no update.
#
# Every other case (and every other function) is unchanged.

import tomllib

_CRATE_VERSION = tomllib.loads(
    (Path(__file__).resolve().parent.parent / "Cargo.toml").read_text())["package"]["version"]
REBUILD_NOTE = ("This copy is compiled, so pulling alone changes nothing that runs: "
                "pull, then rebuild it with cargo build --release.")


def _compiled_check(info):
    info = dict(info)
    if info["kind"] != selfupdate.GIT:
        info["version"] = _CRATE_VERSION
    elif info["can_update"]:
        info["can_update"] = False
        info["note"] = f"{info['note']} {REBUILD_NOTE}"
    return info


def _compiled_app(app):
    if app.extra["kind"] != selfupdate.GIT:
        app.version = _CRATE_VERSION
    elif app.update:                      # only set when can_update was
        note = f"{app.extra['note']} {REBUILD_NOTE}"
        app.update = ""
        app.extra["note"] = note
        app.description = f"This application · {note}"
    return app


_cases_python_rule = cases


def cases():
    for func, args, fn in _cases_python_rule():
        if func in ("check", "as_app"):
            if "root" in args:
                args = {**args, "exe": str(Path(args["root"]) / "rust/target/release/almanac-rs")}
            post = _compiled_check if func == "check" else _compiled_app
            fn = (lambda f=fn, p=post: p(f()))
        yield func, args, fn
