"""Parity cases for tasks.py.

The dialogs need a display, so what is compared is everything that decides:
which commands are elevated, the removal preview (read-only: `pacman -Rs
--print` changes nothing and needs no root), the quoting in the log, and
`TaskDialog._work` itself run against a stand-in window -- the labels it
shows, every line it logs, and what `_finish` is told.

No case here runs anything that needs root: `_work` elevates pacman, and
none of its steps are pacman.
"""
import shlex
import types

import tasks

ARGVS = [
    [],
    ["pacman", "-Syu"],
    ["pacman-key", "--refresh-keys"],
    ["paccache", "-rk2"],
    ["journalctl", "--vacuum-size=500M"],
    ["journalctl", "--vacuum-time=2weeks"],
    ["journalctl", "--disk-usage"],
    ["journalctl"],
    ["yay", "-Syu"],
    ["/usr/bin/pacman", "-Syu"],
    ["PACMAN"],
    [""],
    ["flatpak", "--vacuum"],
    ["journalctl", "-x", "--vacuum"],
    ["pacmän", "ü"],
]


class _GLib:
    """GLib.idle_add, run at once: there is no main loop here."""
    @staticmethod
    def idle_add(fn, *args):
        fn(*args)


def _step(label, spec):
    if isinstance(spec, list):
        return (label, spec)
    if "fail" in spec:
        def fail(_log, msg=spec["fail"]):
            raise Exception(msg)
        return (label, fail)

    def logs(log, lines=spec["log"]):
        for line in lines:
            log(line)
    return (label, logs)


def _work(steps, cancelled=False):
    events = []
    fake = types.SimpleNamespace(
        _steps=[_step(lbl, spec) for lbl, spec in steps],
        _cancelled=cancelled,
        _proc=None,
        step_label=types.SimpleNamespace(set_text=lambda t: events.append(["label", t])),
        log=lambda line: events.append(["log", line if line.endswith("\n") else line + "\n"]),
        _finish=lambda ok: events.append(["finish", ok]),
    )
    real = tasks.GLib
    tasks.GLib = _GLib
    try:
        tasks.TaskDialog._work(fake)
    finally:
        tasks.GLib = real
    return events


WORKS = [
    [["Say hi", ["echo", "hi"]]],
    [["One", ["true"]], ["Two", ["echo", "two"]]],
    [["Mixed", ["sh", "-c", "printf 'a\\r\\nb\\rc\\n\\nd'; echo err >&2; exit 3"]],
     ["Never", ["echo", "not reached"]]],
    [["Signalled", ["sh", "-c", "kill -TERM $$"]]],
    [["Missing", ["no-such-binary-almanac", "--x"]]],
    [["Not executable", ["/etc/hostname"]]],
    [["Quoting", ["printf", "%s|%s|%s\\n", "it's a ü test", "", "$HOME *"]]],
    [["Unicode", ["echo", "héllo ✓ 日本"]]],
    [["Tail without newline", ["printf", "x\\ny"]]],
    [["Call", {"log": ["first", "second\n", ""]}], ["After", ["echo", "ok"]]],
    [["Call fails", {"fail": "download went wrong"}], ["After", ["echo", "no"]]],
    [["Cmd fails then", ["false"]], ["Call", {"log": ["x"]}]],
    [],
]


def cases():
    for argv in ARGVS:
        yield "needs_root", {"argv": argv}, lambda a=argv: tasks.needs_root(a)
        yield "elevate", {"argv": argv}, lambda a=argv: tasks.elevate(a)

    for names in (["git"], ["no-such-package-almanac"], [], ["ü"],
                  ["git", "no-such-package-almanac"], ["1password-cli"]):
        yield "preview_removal", {"names": names}, lambda n=names: tasks.preview_removal(n)

    for s in ("", "abc", "--vacuum-size=500M", "it's", "a b", "ü", "%n %v",
              "@%+=:,./-_", "$HOME", "x\ny", "\"q\""):
        yield "_quote", {"s": s}, lambda s=s: shlex.quote(s)

    for steps in WORKS:
        yield "_work", {"steps": steps}, lambda st=steps: _work(st)
    yield "_work", {"steps": WORKS[0], "cancelled": True}, lambda: _work(WORKS[0], True)


# ---- review round 1: adversarial cases ----
import os as _os
import stat as _stat
import tempfile as _tempfile

_TMP = _tempfile.mkdtemp(prefix="almanac-tasks-parity-")
_NOEXEC = _os.path.join(_TMP, "no-shebang")
with open(_NOEXEC, "w") as _f:
    _f.write("echo hi\n")
_os.chmod(_NOEXEC, _stat.S_IRWXU)
_BINARY = _os.path.join(_TMP, "binary")
with open(_BINARY, "wb") as _f:
    _f.write(b"\x7fELF\x00garbage")
_os.chmod(_BINARY, _stat.S_IRWXU)

REVIEW_WORKS = [
    # Popen([]) raises IndexError (uncaught: the thread dies, no _finish).
    [["Empty argv", []]],
    # Popen with a NUL byte raises ValueError, not OSError.
    [["NUL", ["echo", "a\x00b"]]],
    # Popen([""]) is EACCES in Python.
    [["Empty program", [""]]],
    # A directory as the program.
    [["Directory", ["/tmp"]]],
    # Executable file with no shebang / garbage ELF: ENOEXEC.
    [["No shebang", [_NOEXEC]]],
    [["Garbage ELF", [_BINARY]]],
    # Output that is not UTF-8: Python's text-mode pipe raises.
    [["Bad UTF-8", ["printf", "ok\\n\\377\\376\\n"]]],
    # start_new_session=True: the child leads its own session.
    [["Session", ["sh", "-c",
                  '[ "$(ps -o sid= -p $$ | tr -d " ")" = "$$" ] && echo session-leader || echo not-session-leader']]],
    # Controlling terminal: none after setsid.
    [["TTY", ["sh", "-c", "(exec 3</dev/tty) 2>/dev/null && echo has-tty || echo no-tty"]]],
    # close_fds: only 0, 1, 2 (and ls's own dir fd) are open in the child.
    [["FDs", ["sh", "-c", "ls /proc/self/fd | sort -n | tr '\\n' ' '"]]],
    # \r split across reads.
    [["CR split", ["sh", "-c", "printf 'a\\r'; sleep 0.3; printf '\\nb\\r'; sleep 0.3; printf 'c'"]]],
    # Trailing lone CR.
    [["CR end", ["printf", "x\\r"]]],
    # Unicode line separators: Python text iteration splits only on \n/\r.
    [["LS", ["printf", "a b\u0085c\x0cd\x1ce\n"]]],
    # Signal propagated to group via exit-by-signal codes.
    [["KILL", ["sh", "-c", "kill -KILL $$"]]],
    [["exit 255", ["sh", "-c", "exit 255"]]],
    # Calls logging embedded newlines and CRs.
    [["Call multiline", {"log": ["a\nb", "c\r", "\n", "x\n\n"]}]],
    [["Call fail empty", {"fail": ""}]],
    [["Call fail unicode", {"fail": "ü\nline2"}]],
    # repr() of the program name in the OSError message escapes non-printables.
    [["Repr LS", ["no\u2028such-almanac"]]],
    [["Repr NBSP", ["no\u00a0such-almanac"]]],
    [["Repr NEL", ["no\x85such-almanac"]]],
    [["Repr quotes", ["no'such\"almanac"]]],
]


def _review_cases():
    for steps in REVIEW_WORKS:
        yield "_work", {"steps": steps}, lambda st=steps: _work(st)
    yield "_work", {"steps": [], "cancelled": True}, lambda: _work([], True)
    yield "_work", {"steps": [["Call", {"log": ["x"]}]], "cancelled": True}, \
        lambda: _work([["Call", {"log": ["x"]}]], True)
    for names in (["x\x1f"], ["x\x1c"], ["a\x00b"], [" x"], ["x "],
                  ["--print"], ["-h"], ["git", "git"], ["x" * 300]):
        yield "preview_removal", {"names": names}, lambda n=names: tasks.preview_removal(n)
    for s in ("\x00", "a\tb", "é", "ß", "١", "a'b'c", "''", "-", "=", "~", "^", "!", "#", "{}", "\n"):
        yield "_quote", {"s": s}, lambda s=s: shlex.quote(s)
    for argv in (["journalctl", "--VACUUM"], ["journalctl", "--vacuu"], ["paccache"],
                 ["pacman-key"], [" pacman"], ["pacman "], ["sudo", "pacman", "-Syu"],
                 ["journalctl", "-vacuum-size=1M"], ["x", "--vacuum"]):
        yield "needs_root", {"argv": argv}, lambda a=argv: tasks.needs_root(a)
        yield "elevate", {"argv": argv}, lambda a=argv: tasks.elevate(a)


_orig_cases = cases


def cases():  # noqa: F811
    yield from _orig_cases()
    yield from _review_cases()


# ---- review round 2: adversarial cases ----
REVIEW2_WORKS = [
    # stdin=DEVNULL is os.open(os.devnull, os.O_RDWR) in Python: the child
    # can write to fd 0.
    [["Stdin writable", ["sh", "-c", "if echo x >&0 2>/dev/null; then echo writable; else echo readonly; fi"]]],
    [["Stdin flags", ["sh", "-c", "grep '^flags' /proc/self/fdinfo/0"]]],
    # Reading stdin gets EOF at once.
    [["Stdin read", ["sh", "-c", "cat; echo done"]]],
    # \r\n and a multi-byte character straddling the 8192-byte chunk edge.
    [["CRLF edge", ["sh", "-c", "head -c 8191 /dev/zero | tr '\\0' x; printf '\\r\\nmid\\n'; head -c 8190 /dev/zero | tr '\\0' y; printf '\\303\\274z\\n'"]]],
    # Output left by a background child after the main one exits.
    [["Late child", ["sh", "-c", "(sleep 0.3; echo late) & echo early"]]],
    # stdout closed early, still running.
    [["Closed early", ["sh", "-c", "echo one; exec >&- 2>&-; sleep 0.2; exit 4"]]],
    # NUL bytes in output.
    [["NUL out", ["printf", "a\\000b\\n"]]],
    # Form feed / vertical tab are not line ends.
    [["VT FF", ["printf", "a\\vb\\fc\\n"]]],
]


def _review2_cases():
    for steps in REVIEW2_WORKS:
        yield "_work", {"steps": steps}, lambda st=steps: _work(st)


_orig_cases2 = cases


def cases():  # noqa: F811
    yield from _orig_cases2()
    yield from _review2_cases()
