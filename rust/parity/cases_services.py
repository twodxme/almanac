"""Parity cases for services.py.

The readers run against the real machine (systemctl --user list-unit-files /
show / is-enabled are read-only). The parsing in services() is also checked
with systemctl stood in for, the way test_app.py mocks `_run`: the Rust side
gets the same two outputs and both sides report every `_run` argv too.
"""
import json
import os
import stat
import tempfile
from unittest import mock

import services


def _lossy(v):
    """Lone surrogates as U+FFFD: the Rust side's JSON cannot carry one (a
    Rust string cannot hold it), so both sides report it that way."""
    if isinstance(v, str):
        return v.encode("utf-16", "surrogatepass").decode("utf-16", "replace")
    if isinstance(v, list):
        return [_lossy(x) for x in v]
    if isinstance(v, services.Service):
        return {k: _lossy(x) for k, x in vars(v).items()}
    return v


def _fake(available, files, shown):
    def run():
        calls = []

        def fake_run(*a, **k):
            # The real _run hands argv to subprocess, which encodes it with
            # os.fsencode and raises UnicodeEncodeError (uncaught) on a lone
            # surrogate; mock.patch would skip that, so do it here.
            for x in a:
                os.fsencode(x)
            calls.append(list(a))
            return files if a[0] == "list-unit-files" else shown
        with mock.patch.object(services, "available", return_value=available), \
             mock.patch.object(services, "_run", side_effect=fake_run):
            result = services.services()
        return {"result": _lossy(result), "calls": _lossy(calls)}
    return ("services_fake", {"available": available, "files": files, "shown": shown}, run)


F = lambda units: json.dumps(units)

FAKES = [
    # the test_app.py case
    (True, F([{"unit_file": "mine.service", "state": "enabled"},
              {"unit_file": "socketed.service", "state": "disabled"}]),
     "Id=mine.service\nDescription=Mine\nActiveState=active\n\n"
     "Id=socketed.service\nDescription=S\nActiveState=active\n"),
    # not available: no calls at all
    (False, F([{"unit_file": "a.service", "state": "enabled"}]), "Id=a.service\n"),
    # empty / whitespace / malformed / non-list outputs
    (True, "", ""),
    (True, "  \n\t ", ""),
    (True, "{not json", ""),
    (True, F({"unit_file": "x.service"}), ""),
    (True, F("x.service"), ""),
    (True, F([]), ""),
    (True, F(["x.service", 3, None]), ""),
    # templates (@) left out; only '@' units -> no show call
    (True, F([{"unit_file": "getty@.service", "state": "enabled"}]), ""),
    # sort: essentials last, then by name; omarchy flag; missing description
    (True, F([{"unit_file": "wireplumber.service", "state": "enabled"},
              {"unit_file": "zeta.timer", "state": "disabled"},
              {"unit_file": "omarchy-sleep.service", "state": "enabled"},
              {"unit_file": "alpha.service", "state": "enabled"},
              {"unit_file": "pipewire.service", "state": "enabled"},
              {"unit_file": "user@.service", "state": "enabled"},
              {"unit_file": "nostate.service"},
              {"unit_file": "weird.service", "state": 5}]),
     "Id=wireplumber.service\nDescription=Multimedia Service Session Manager\nActiveState=active\n\n"
     "Id=zeta.timer\nDescription=\nActiveState=inactive\n\n"
     "Id=omarchy-sleep.service\nDescription=a=b=c\nActiveState=failed\n\n"
     "Id=alpha.service\nActiveState=active\nnoequals line\n\n"
     "Id=pipewire.service\nDescription=PipeWire\r\nActiveState=active\r\n\n"
     "Id=\nDescription=orphan\n\n"
     "Description=no id\n"),
    # unicode, duplicates (last state/info wins), show output missing a unit
    (True, F([{"unit_file": "ünï-名前.service", "state": "disabled"},
              {"unit_file": "dup.service", "state": "disabled"},
              {"unit_file": "dup.service", "state": "enabled"},
              {"unit_file": "absent.service", "state": "enabled"}]),
     "\n\nId=ünï-名前.service\nDescription=Ünïcødé ✓\nActiveState=inactive\n\n"
     "Id=dup.service\nDescription=first\nActiveState=inactive\n\n"
     "Id=dup.service\nDescription=second\nActiveState=active\n\n\n"),
    # Python line breaks inside a block (\x0b, \x1c,  ) and \x1c strip
    (True, F([{"unit_file": "a.service", "state": "enabled"},
              {"unit_file": "b.service", "state": "enabled"}]),
     "\x1c Id=a.service\x0bDescription=A ActiveState=active\n\n"
     "Id=b.service\x1cDescription=B\x85ActiveState=active\x1f"),
    # --- review round 1 ---
    # JSON that is not iterable: Python raises TypeError iterating it
    (True, "null", ""),
    (True, "5", ""),
    (True, "true", ""),
    # JSON Python accepts but serde_json rejects
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": NaN}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": -Infinity}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": 1e400}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": ' + "[" * 200 + "]" * 200 + '}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    (True, '[{"unit_file": "\\ud800.service", "state": "enabled"}]', ""),
    # duplicate keys: last wins on both sides
    (True, '[{"unit_file": "a.service", "unit_file": "b.service", "state": "enabled"}]',
     "Id=b.service\nDescription=B\nActiveState=inactive\n"),
    # a dict without unit_file / with a non-string one: Python raises
    (True, F([{"state": "enabled"}]), ""),
    (True, F([{"unit_file": "a.service", "state": "enabled"}, {"state": "enabled"}]),
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    (True, F([{"unit_file": 5, "state": "enabled"}]), ""),
    (True, F([{"unit_file": None, "state": "enabled"}]), ""),
    (True, F([{"unit_file": ["x.service"], "state": "enabled"}]), ""),
    (True, F([{"unit_file": "a.service", "state": "enabled"}, {"unit_file": {"@": 1}}]),
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    (True, F([{"unit_file": ["@"]}]), ""),
    # --- review round 2 ---
    # surrogate pairs join; a lone surrogate in a skipped ('@') name is harmless
    (True, '[{"unit_file": "\\ud83d\\ude00.service", "state": "enabled"}, {"unit_file": "@\\udc00"}]',
     "Id=\U0001F600.service\nDescription=E\nActiveState=inactive\n"),
    # deep nesting inside unit_file / state, and in a dict unit_file with '@'
    (True, F([{"unit_file": "a.service", "state": [[[["enabled"]]]]},
              {"unit_file": ["b", [["@"]]]}]), ""),
    (True, F([{"unit_file": {"@": [[[1]]]}}]), ""),
    (True, F([{"unit_file": ["@", {"x": 1}]}, {"unit_file": "a.service", "state": "enabled"}]),
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    # JSON Python rejects too: trailing comma, control char, bad escape,
    # leading zero, BOM, extra data, non-JSON whitespace
    (True, '[{"unit_file": "a.service", "state": "enabled"},]', ""),
    (True, '[{"unit_file": "a\x01.service", "state": "enabled"}]', ""),
    (True, '[{"unit_file": "a\\x.service", "state": "enabled"}]', ""),
    (True, '[{"unit_file": "a.service", "n": 01}]', ""),
    (True, '\ufeff[{"unit_file": "a.service", "state": "enabled"}]', ""),
    (True, '[{"unit_file": "a.service", "state": "enabled"}] x', ""),
    (True, '\u00a0[{"unit_file": "a.service", "state": "enabled"}]', ""),
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": -}]', ""),
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": 1.}]', ""),
    # numbers Python takes: exponents, negatives, fractions, big ints
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": [-0, 1.5e-3, 2E+5, 123456789012345678901234567890]}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    # top-level dict of dicts, nested whitespace everywhere
    (True, ' \n{"x": {"unit_file": "a.service"}} \r\n', ""),
    (True, ' [ { "unit_file" : "a.service" , "state" : "enabled" } ] ',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    # escapes in names and state
    (True, '[{"unit_file": "a\\u0040b.service"}, {"unit_file": "c\\/d.service", "state": "en\\u0061bled"}]',
     "Id=c/d.service\nDescription=CD\nActiveState=active\n"),
    # --- review round 2 (adversarial) ---
    # ints past sys.int_info.default_max_str_digits (4300): json.loads raises
    # ValueError ("Exceeds the limit"), caught -> units = [] -> no services
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": ' + "1" * 4301 + '}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": -' + "9" * 5000 + '}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    # exactly at the limit is still fine
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": ' + "1" * 4300 + '}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    # nesting deep enough for Python's C scanner to raise RecursionError
    # (not a ValueError, so services() raises)
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": ' + "[" * 100000 + "]" * 100000 + '}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    # a lone LOW surrogate \udc80-\udcff: os.fsencode's surrogateescape turns
    # it into a raw byte, so Python runs `show` with it and lists the unit
    (True, '[{"unit_file": "\\udcff.service", "state": "enabled"}]', ""),
    (True, '[{"unit_file": "x\\udc80.timer", "state": "disabled"}]', ""),
    # --- review round 3 ---
    # escapable and not: a low surrogate run broken by \udc7f raises
    (True, '[{"unit_file": "\\udcff\\udc7f.service", "state": "enabled"}]', ""),
    # escaped names sort by code point (U+DCFF before U+E000), lookups by
    # the exact name (two names that both read U+FFFD stay apart)
    (True, '[{"unit_file": "\\ue000.service", "state": "enabled"}, '
           '{"unit_file": "\\udcff.service", "state": "enabled"}, '
           '{"unit_file": "\\udcfe.service", "state": "disabled"}]', ""),
    # deep but within Python's limit; an int far past it inside a skipped name
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": ' + "[" * 20000 + "]" * 20000 + '}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    (True, '[{"unit_file": "@", "n": ' + "7" * 4301 + '}]', ""),
    # a float with many digits is not an int: no limit
    (True, '[{"unit_file": "a.service", "state": "enabled", "n": ' + "1" * 5000 + '.5}]',
     "Id=a.service\nDescription=A\nActiveState=inactive\n"),
    # a NUL in a name: the stand-in does not check (subprocess would)
    (True, '[{"unit_file": "a\\u0000.service", "state": "enabled"}]', ""),
]

# A systemctl stand-in on PATH for the real `_run` path: it answers a few
# magic unit names itself and hands everything else to the real systemctl,
# so the real-machine cases are unchanged. Python evaluates every case before
# the Rust side runs, in the same order, so the one-shot "arm" flag is seen
# identically by both.
_DIR = tempfile.mkdtemp(prefix="almanac-parity-systemctl-")
_SCRIPT = os.path.join(_DIR, "systemctl")
with open(_SCRIPT, "w") as fh:
    fh.write(f"""#!/bin/sh
ST={_DIR}
case "$*" in
  *"is-enabled parity-crlf-arm"*) touch "$ST/armed"; echo disabled; exit 1;;
  *"is-enabled parity-held-pipe"*) echo enabled; (sleep 25) & exit 0;;
  *"is-enabled parity-nul-arm"*) touch "$ST/nul"; echo disabled; exit 1;;
  *"is-enabled parity-bad-utf8"*) printf 'enabled\\377\\n'; exit 0;;
  *list-unit-files*)
    if [ -e "$ST/nul" ]; then
      rm -f "$ST/nul"
      printf '[{{"unit_file":"nul\\\\u0000.service","state":"enabled"}}]'
      exit 0
    fi
    if [ -e "$ST/armed" ]; then
      rm -f "$ST/armed"
      printf '[{{"unit_file":"crlf-a.service","state":"enabled"}},{{"unit_file":"crlf-b.service","state":"enabled"}}]'
      exit 0
    fi;;
  *"crlf-a.service crlf-b.service"*)
    printf 'Id=crlf-a.service\\r\\nDescription=A\\r\\nActiveState=active\\r\\n\\r\\nId=crlf-b.service\\r\\nDescription=B\\r\\nActiveState=active\\r\\n'
    exit 0;;
esac
exec /usr/bin/systemctl "$@"
""")
os.chmod(_SCRIPT, os.stat(_SCRIPT).st_mode | stat.S_IXUSR)


def cases():
    yield "available", {}, services.available
    for u in ["wireplumber.service", "hyprsunset.service",
              "xdg-desktop-portal-rewrite-launchers.service", "pipewire.socket",
              "dbus-broker.service", "", "PIPEWIRE.SERVICE", "pipewire", "名前.service"]:
        yield "essential_for", {"unit": u}, lambda u=u: services.essential_for(u)
    for u, on in [("x.service", False), ("x.service", True), ("", True),
                  ("ünï 名前.timer", False)]:
        yield "toggle_argv", {"unit": u, "on": on}, lambda u=u, on=on: services.toggle_argv(u, on)
    for u in ["pipewire.service", "wireplumber.service", "no-such-unit-xyz.service",
              "", "ünï.service", "--help"]:
        yield "is_enabled", {"unit": u}, lambda u=u: services.is_enabled(u)
    # the real machine
    yield "services", {}, services.services
    for a, f, s in FAKES:
        yield _fake(a, f, s)
    # --- review round 1: the real `_run` through the stand-in above ---
    os.environ["PATH"] = _DIR + os.pathsep + os.environ.get("PATH", "")
    # text=True turns \r\n into \n, so "\r\n\r\n" separates blocks in Python
    yield "is_enabled", {"unit": "parity-crlf-arm"}, lambda: services.is_enabled("parity-crlf-arm")
    yield "services", {}, services.services
    # stdout that is not UTF-8: Python's decode raises UnicodeDecodeError
    yield "is_enabled", {"unit": "parity-bad-utf8"}, lambda: services.is_enabled("parity-bad-utf8")
    # a child left holding stdout: Python's communicate() times out at 20 s
    yield "is_enabled", {"unit": "parity-held-pipe"}, lambda: services.is_enabled("parity-held-pipe")
    # --- review round 2 (adversarial) ---
    # an argv with a NUL: subprocess raises ValueError ("embedded null byte"),
    # which _run does not catch, so is_enabled / services() raise
    yield "is_enabled", {"unit": "a\x00b.service"}, lambda: services.is_enabled("a\x00b.service")
    yield "is_enabled", {"unit": "parity-nul-arm"}, lambda: services.is_enabled("parity-nul-arm")
    yield "services", {}, services.services
