"""Parity cases for snapshots.py.

Readers run against the real machine (snapper list-configs, /proc/cmdline,
the pacman.log tail and journalctl are all read-only). The step that would
take a snapshot is never run for real: create_step is checked for its label,
and its step is run with subprocess.run stood in for. finding() is checked
on the real machine and with each of its inputs stood in for.
"""
import subprocess
from unittest import mock

import snapshots


def _configs_fake(has, out):
    def run():
        calls = []

        def fake_run(cmd, timeout=20):
            calls.append(list(cmd))
            return out
        with mock.patch.object(snapshots.shutil, "which",
                               side_effect=lambda n: "/usr/bin/snapper" if has else None), \
             mock.patch.object(snapshots, "_run", side_effect=fake_run):
            result = snapshots.snapper_configs()
        return {"result": result, "calls": calls}
    return ("snapper_configs_fake", {"has": has, "out": out}, run)


def _step_fake(configs, description, mode="ok", returncode=0, stdout="", stderr=""):
    def run():
        ran = []

        def fake_run(argv, **kw):
            ran.append(list(argv))
            if mode == "oserror":
                raise FileNotFoundError(2, "No such file or directory", argv[0])
            if mode == "timeout":
                raise subprocess.TimeoutExpired(argv, 120)
            return mock.Mock(returncode=returncode, stdout=stdout, stderr=stderr)
        with mock.patch.object(snapshots, "snapper_configs", return_value=configs):
            res = snapshots.create_step(description)
        if res is None:
            return None
        label, step = res
        lines = []
        with mock.patch.object(snapshots.subprocess, "run", side_effect=fake_run):
            step(lines.append)
        return {"label": label, "argv": ran, "lines": lines}
    return ("create_step_fake", {"configs": configs, "description": description,
                                 "mode": mode, "returncode": returncode,
                                 "stdout": stdout, "stderr": stderr}, run)


def _finding_fake(number="", restore=False, configs=(), tx=None, before=False):
    def run():
        t = None if tx is None else snapshots.Transaction(**tx)
        with mock.patch.object(snapshots, "booted_snapshot", return_value=number), \
             mock.patch.object(snapshots.shutil, "which",
                               side_effect=lambda n: "/usr/bin/x" if restore else None), \
             mock.patch.object(snapshots, "snapper_configs", return_value=list(configs)), \
             mock.patch.object(snapshots, "last_transaction", return_value=t), \
             mock.patch.object(snapshots, "snapshot_before", return_value=before):
            return snapshots.finding()
    return ("finding_fake", {"number": number, "restore": restore,
                             "configs": list(configs), "tx": tx, "before": before}, run)


LOG = (
    "[2026-10-16T22:52:33-0700] [PACMAN] starting full system upgrade\n"
    "[2026-10-16T22:52:40-0700] [ALPM] transaction started\n"
    "[2026-10-16T22:52:40-0700] [ALPM] running '05-snap-pac-pre.hook'...\n"
    "[2026-10-16T22:52:41-0700] [ALPM] upgraded a (1-1 -> 2-1)\n"
    "[2026-10-16T22:52:42-0700] [ALPM] transaction completed\n"
    "[2026-10-16T23:02:32-0700] [ALPM] transaction started\n"
    "[2026-10-16T23:02:33-0700] [ALPM] upgraded b (1-1 -> 2-1)\n"
    "[2026-10-16T23:02:34-0700] [ALPM] transaction completed\n")

LOGS = [
    LOG,
    LOG.split("[2026-10-16T23:02:32")[0],
    "",
    "\n\n",
    "garbage\nnot a log line\n",
    # changes before any "transaction started" are ignored
    "[2026-10-16T22:52:41-0700] [ALPM] upgraded a (1-1 -> 2-1)\n"
    "[2026-10-16T22:52:42-0700] [ALPM] transaction completed\n",
    # a transaction that changed nothing does not replace the last one
    LOG + "[2026-10-17T01:00:00-0700] [ALPM] transaction started\n"
          "[2026-10-17T01:00:01-0700] [ALPM] transaction completed\n",
    # unfinished transaction at the end
    LOG + "[2026-10-17T01:00:00-0700] [ALPM] transaction started\n"
          "[2026-10-17T01:00:01-0700] [ALPM] upgraded c (1 -> 2)\n",
    # non-ALPM changes ignored; reinstalled is not a change; hooks case-insensitive
    "[2026-10-17T01:00:00-0700] [ALPM] transaction started\n"
    "[2026-10-17T01:00:00-0700] [PACMAN] upgraded fake (1 -> 2)\n"
    "[2026-10-17T01:00:00-0700] [ALPM] reinstalled r (1)\n"
    "[2026-10-17T01:00:01-0700] [ALPM] installed c (1)\n"
    "[2026-10-17T01:00:01-0700] [ALPM] removed d (1)\n"
    "[2026-10-17T01:00:01-0700] [ALPM] downgraded e (2 -> 1)\n"
    "[2026-10-17T01:00:01-0700] [ALPM] running 'zz-TIMESHIFT-autosnap.hook'...\n"
    "[2026-10-17T01:00:02-0700] [ALPM] transaction completed\n",
    # 'snapper' mentioned outside a running line doesn't count
    "[2026-10-17T01:00:00-0700] [ALPM] transaction started\n"
    "[2026-10-17T01:00:01-0700] [ALPM] installed snapper (1)\n"
    "[2026-10-17T01:00:01-0700] [ALPM-SCRIPTLET] running snap-pac\n"
    "[2026-10-17T01:00:02-0700] [ALPM] transaction completed\n",
    # old log format, CRLF, unicode, odd timestamps
    "[2019-01-02 03:04] [ALPM] transaction started\r\n"
    "[2019-01-02 03:04] [ALPM] upgraded ünï (1 -> 2)\r\n"
    "[2019-01-02 03:04] [ALPM] transaction completed\r\n",
    "[bogus] [ALPM] transaction started "
    "[bogus] [ALPM] upgraded x (1 -> 2)\x0c"
    "[bogus] [ALPM] running 'btrfs-autosnap'\x85"
    "[bogus] [ALPM] transaction completed",
    "[2026-10-17T01:00:00Z] [ALPM] transaction started\n"
    "[] [ALPM] upgraded skipped (1 -> 2)\n"
    "[2026-10-17T01:00:00Z] [ALPM]  transaction completed\n"
    "[2026-10-17T01:00:00Z] [ALPM] upgraded y (1 -> 2)\n"
    "[2026-10-17T01:00:00Z] [ALPM] transaction completed\n",
]

TS = [
    "2026-10-16T22:52:33-0700", "2026-10-16T22:52:33+05:30", "2026-10-16T22:52:33Z",
    "2026-10-16T22:52:33.123456-07:00", "2026-10-16T22:52:33,5", "2026-10-16 22:52",
    "2026-10-16", "20261016", "20261016T225233", "2026-10-16T22", "2026-10-16T2252",
    "2026-W42-5", "2026W425", "2026-W42", "2026-10-16T24:00", "2026-12-31T24:00:00",
    "2026-10-16T24:01", "2026-02-30", "2026-13-01", "0000-01-01", "2019-01-02 03:04",
    "2019-1-2 3:04", "2019-01-02  03:04", "2019-01- 2 03:04", "2019-01-02 03:04 ",
    " 2019-01-02 03:04", "2026-10-16T22:52:33-07", "2026-10-16T22:52:33-0", "2026-10-16T22:52:33+2400",
    "2026-10-16T22:52:33.1234567", "2026-10-16T22:52:33.", "2026-10-16T22:5", "2026-10-16X22:52",
    "2026-10-16ü22:52", "", "garbage", "bogus", "1234567", "2026-10-16T22:52:33-07:00:30",
    "2026-03-08T02:30:00", "2026-11-01T01:30:00", "2026-10-16T22:52:33Z+01", "２０２６-10-16",
]

CMDLINES = [
    "root=UUID=x rw rootflags=subvol=/@/.snapshots/142/snapshot quiet",
    "root=UUID=x rw",
    "",
    "rootflags=subvol=@/.snapshots/7/snapshot",
    "rootflags=subvol=/@/.snapshots/abc/snapshot",
    "a subvol=/x/1/snapshot b subvol=/y/2/snapshot",
    "subvol=/@/.snapshots/١٢/snapshot",
    "subvol=/@/ünï/99/snapshot",
]

JOURNALS = [
    "Oct 16 23:02:22 host sudo[1]: tim : COMMAND=/usr/bin/snapper -c root create -c number -d 4.0.4-1\n",
    "nothing\n",
    "",
    "COMMAND=/usr/bin/snapper list\n",
    "COMMAND=snapper create\n",
    "COMMAND=/usr/bin/snapperx create\n",
    "COMMAND=/usr/bin/snapper -c root\ncreate\n",
    "COMMAND=/usr/bin/snapper -c root recreate\n",
    "COMMAND=/usr/bin/snapper -c root créate create-config\n",
    "COMMAND= snapper create\n",
]

TX = {"started": 1792216353.0, "changes": ["upgraded b (1-1 -> 2-1)"], "hooks_snapshot": False}
MANY = {"started": 1792216353.0,
        "changes": [f"upgraded p{i} (1 -> 2)" for i in range(45)] + ["odd", "installed  spaced  (1)"],
        "hooks_snapshot": False}


def cases():
    yield "snapper_configs", {}, lambda: snapshots.snapper_configs()
    for has, out in [(True, "config,subvolume\nroot,/\n"), (False, "config,subvolume\nroot,/\n"),
                     (True, ""), (True, "config,subvolume\n"), (True, "config,subvolume\nroot,/\n  \nhome,/home\n,x\n"),
                     (True, "config\nünï\r\nb"), (True, "header\n\x1c\nx,y")]:
        yield _configs_fake(has, out)

    yield "booted_snapshot", {"cmdline": None}, lambda: snapshots.booted_snapshot()
    for c in CMDLINES:
        yield "booted_snapshot", {"cmdline": c}, (lambda c=c: snapshots.booted_snapshot(c))

    for t in TS:
        yield "_ts", {"text": t}, (lambda t=t: snapshots._ts(t))

    yield "last_transaction", {"log_text": None}, lambda: snapshots.last_transaction()
    for log in LOGS:
        yield "last_transaction", {"log_text": log}, (lambda log=log: snapshots.last_transaction(log))

    for j in JOURNALS:
        yield "snapshot_before", {"started": 0, "journal_text": j}, (lambda j=j: snapshots.snapshot_before(0, j))
    # the real journal, for a fixed past moment and for the last real update
    for started in (1792216353.0, 0.0):
        yield "snapshot_before", {"started": started, "journal_text": None}, \
            (lambda s=started: snapshots.snapshot_before(s))
    real = snapshots.last_transaction()
    if real is not None:
        yield "snapshot_before", {"started": real.started, "journal_text": None}, \
            (lambda s=real.started: snapshots.snapshot_before(s))

    yield "create_step", {"description": "x"}, \
        lambda: (lambda r: None if r is None else r[0])(snapshots.create_step("x"))
    yield _step_fake([], "x")
    yield _step_fake(["root"], "x", "fail", 1, "", "Request dismissed")
    yield _step_fake(["root"], "Updating 4 packages", "ok", 0, "", "")
    yield _step_fake(["home", "root"], "x" * 80)
    yield _step_fake(["home", "data"], "ünïcødé " * 12)
    yield _step_fake(["root"], "x", "fail", 126, "out line\nlast out\n", "")
    yield _step_fake(["root"], "x", "fail", 1, "", "  \n  ")
    yield _step_fake(["root"], "x", "fail", 1, "ignored", "a\nb\r\nlast err  \n")
    yield _step_fake(["root"], "x", "oserror")
    yield _step_fake(["root"], "it's \"quoted\"", "timeout")
    yield _step_fake(["root"], "tab\there", "timeout")

    yield "finding", {}, lambda: snapshots.finding()
    yield _finding_fake(number="142", restore=True)
    yield _finding_fake(number="142", restore=False)
    yield _finding_fake(configs=[])
    yield _finding_fake(configs=["root"], tx=None)
    yield _finding_fake(configs=["root"], tx=TX, before=True)
    yield _finding_fake(configs=["root"], tx=TX, before=False)
    yield _finding_fake(configs=["root"], tx=TX, before=None)
    yield _finding_fake(configs=["root"], tx=dict(TX, hooks_snapshot=True), before=None)
    yield _finding_fake(configs=["root"], tx=MANY, before=False)
    yield _finding_fake(configs=["root"], tx=dict(MANY, changes=MANY["changes"][:5]), before=True)
    yield _finding_fake(configs=["root"], tx=dict(TX, changes=["odd"], started=0.0), before=None)


# --- review round 1: suspected differences --------------------------------

TS_R1 = [
    # C fromisoformat quirks (CPython uses _datetimemodule.c, not _pydatetime)
    "2026-10-16T", "2026-10-16 ", "2026-10-16T22:52:33:5", "2026-10-16T22523312",
    "2026-10-16T22.5", "2026-10-16T22:52.5", "2026-10-16T2252.5",
    "2026-10-16T22:52:33+0700.5", "2026-10-16T22:52:33+07.5", "2026-10-16T22:52:33+05:99",
    "2026-10-16T22:52:33+05:30:99", "9999-12-31T24:00", "2026-02-28T24:00", "2026-02-30T24:00",
    "2026-10-16T24:00Z", "2026-10-16T24:00:00.000", "2026-10-16T22:52:33.000000001",
    "2026-10-16é22:52", "2026-10-16 22:52", "2026-10-16\U0001F60022:52",
    "2026-W53-1", "2020-W53-7", "2026-W00", "2026-001", "2026001",
    "2026-10-16T22:52:33-00:00", "2026-10-16T22:52:33+00", "2026-10-16T22:52:33Z ",
    "2026-10-16T22:52:33 ", "2026-10-16T22:5:33",
    # strptime: \d and \s are Unicode in Python's re
    "２０２６-10-16 22:52", "2026-10-16 ２２:52", "2019-01-02\x1c03:04", "2019-01-02\x1f03:04",
    "2019-01-02 03:04", "2019-01-02\t03:04", "2019-01-02 \n 03:04",
    "٢٠١٩-01-02 03:04", "2019-01-02 03:4", "2019-1-02 3:4", "2019-01-0 03:04",
    "2026-10-16T22:52:33+07:00:00.5", "2026-10-16T22:52:33+24:00", "2026-10-16T22:52:33-23:59:59.999999",
    "2026-W42-5T10:00", "2026W425T10", "20261016T22", "2026-10-16T22:52:33.5+01:00",
    "2026-10-16T22:52:33,123Z", "2026-10-16T22:52:33z", "2026-10-16t22:52:33", "2026-10-16T22:52:60",
    "2026-10-16T22:52:33+0530", "2026-10-16T22:52:33+053", "2026-10-16T22:52:33+05:3",
    "2026-10-16T22:52:33-07:00Z", "2026-10-16T22:52:33Z-07:00", "2026-10-16T225233-0700",
    "2026-10-16T22:5233", "2026-10-16T2252:33", "2026-10-16T22:52:33+07:00+01", "2026-10-16T-07",
    "2026-10-16T22-07", "2026-10-16T22Z", "2026-10-16T22:52:33.+01", "2026-10-16T22:52:33.12x",
    "2026-10-16TT22", "2026-10-16T12345", "2026-10-16T22:52:33+12345", "2026-10-16T22:52:33+00:00:00:00",
    "2026-10-16T22:52:33+0700:00", "2026-10-16T22:52:33+07:0000", "2026-10-16T22:52:33+070000.25",
    "0001-01-01T00:00+01:00", "9999-12-31T23:59:59-01:00", "2026-1016", "202610-16", "2026-10-16T22:52:33.1234567+01",
    "2026-W42-5\u00e910:00", "2026W42\u00e910", "2026-W4", "2026W4",
]

CMDLINES_R1 = [
    "subvol=\x1c/1/snapshot", "subvol=/@/.snapshots/\x1f/2/snapshot",
    "subvol=/@/.snapshots/３/snapshot", "subvol=/@ /4/snapshot",
]

JOURNALS_R1 = [
    "COMMAND=/usr/bin/snappeŕ create\n",
    "COMMAND=/usr/bin/snapper² create\n",
    "COMMAND=\x1csnapper create\n",
    "COMMAND=/usr/bin/snapper creaté\n",
    "COMMAND=/usr/bin/snapper create²\n",
    "COMMAND=/usr/bin/snapper ‿create\n",
    "COMMAND=/usr/bin/snapper create\n",
]

LOGS_R1 = [
    "[x] [ALPM] transaction started\n"
    "[x] [ALPM] upgraded a (1 -> 2)\n"
    "[x] [ALPM] running 'tİmeshift.hook'\n"
    "[x] [ALPM] transaction completed\n",
    "[x] [ALPM] transaction started\n"
    "[x] [ALPM] upgraded a (1 -> 2)\n"
    "[x] [ALPM] running 'tımeshift.hook'\n"
    "[x] [ALPM] transaction completed\n",
    "[x] [ALPM] transaction started\n"
    "[x] [ALPM] upgraded a (1 -> 2)\n"
    "[x] [ALPM] running 'ſnapper.hook'\n"
    "[x] [ALPM] transaction completed\n",
]


def cases_r1():
    for t in TS_R1:
        yield "_ts", {"text": t}, (lambda t=t: snapshots._ts(t))
    for c in CMDLINES_R1:
        yield "booted_snapshot", {"cmdline": c}, (lambda c=c: snapshots.booted_snapshot(c))
    for j in JOURNALS_R1:
        yield "snapshot_before", {"started": 0, "journal_text": j}, (lambda j=j: snapshots.snapshot_before(0, j))
    for log in LOGS_R1:
        yield "last_transaction", {"log_text": log}, (lambda log=log: snapshots.last_transaction(log))
    # repr of non-printable characters in the TimeoutExpired message
    for d in ["a؀b", "a͸b", "a\U000e0041b", "a᠎b", "a​b", "a\U0001d173b", "a­b"]:
        yield _step_fake(["root"], d, "timeout")
    # localtime out of range: Python raises OverflowError
    yield "snapshot_before", {"started": 1e20, "journal_text": None}, \
        lambda: snapshots.snapshot_before(1e20)
    yield _finding_fake(configs=["root"], tx=dict(TX, started=1e20), before=False)
    yield _finding_fake(configs=["root"], tx=dict(TX, started=-1e20), before=True)


_cases_r0 = cases


def cases():
    yield from _cases_r0()
    yield from cases_r1()


# --- review round 2: localtime far from now, C datetime edges ---------------

TS_R2 = [
    "0001-01-01T00:00", "0001-01-01T08:00", "0001-01-01T07:52:58", "9999-12-31T23:59:59",
    "9999-12-31T23:59:59.999999+00:00", "0001-01-01T00:00:00.000001-23:59", "2026-01-00T24:00",
    "2026-00-00T24:00", "2026-10-16T22:52:33+00:00:00.5", "2026-10-16T22:52:33-00:00:00.5",
    "1883-11-18T12:00", "1500-06-01T12:00", "2026-10-16T22:52:33.", "2026-10-16T22:52:33,",
    "2026-10-16\x0022:52", "2026-10-16T22:52\x00", "２０２６-10-16 ２２:52", "2026-10-16 ٢٢:٥٢",
]


def cases_r2():
    for t in TS_R2:
        yield "_ts", {"text": t}, (lambda t=t: snapshots._ts(t))
    for started in (-6e10, -1e11, 1e12, 1e13, -1e13, 6.7e16, -6.7e16, 1e17, -1e17, 9.2e18, 1792216353.5):
        yield _finding_fake(configs=["root"], tx=dict(TX, started=started), before=False)


_cases_r1_all = cases


def cases():
    yield from _cases_r1_all()
    yield from cases_r2()


# --- review round 2 (second pass): decoded output, time zones ---------------
#
# The time-zone differences only show with TZ set for the whole run (both
# sides read it at start-up), e.g.
#   TZ=Foo                         invalid name: glibc uses UTC, chrono /etc/localtime
#   TZ='America/Los_Angeles '      same (trailing space)
#   TZ='<-03>3<-02>,M3.5.0/-2,M10.5.0/-1'   rule chrono will not parse
#   TZ='CET-1CEST,M3.5.0,M10.5.0/3'         POSIX rule before 1970
#   TZ=right/America/New_York               leap seconds
# The cases below are the ones that differ under those settings.

TS_R2B = ["1500-06-01T12:00", "1965-07-01T12:00", "1965-07-01 12:00", "1969-12-31T23:59",
          "2026-07-01T12:00", "2016-12-31T23:59:59"]


def cases_r2b():
    # A real U+FFFD in snapper's output is valid UTF-8: Python logs it,
    # it is not a decode error.
    yield _step_fake(["root"], "x", "fail", 1, "", "description � rejected")
    yield _step_fake(["root"], "x", "fail", 1, "out �\n", "")
    yield _step_fake(["root"], "x", "ok", 0, "", "warning: �")
    for t in TS_R2B:
        yield "_ts", {"text": t}, (lambda t=t: snapshots._ts(t))
    for started in (-142000000.0, -1e9, 1467331200.0, 1483228826.0):
        yield _finding_fake(configs=["root"], tx=dict(TX, started=started), before=False)


_cases_r2_all = cases


def cases():
    yield from _cases_r2_all()
    yield from cases_r2b()


# --- review round 3: glibc's localtime followed, not chrono's -----------------
#
# Also run with TZ set, e.g. TZ='AEST-10AEDT,M10.1.0,M4.1.0/3' (a southern
# rule: glibc puts every time before 1970 in DST), TZ=XXX5YYY (a DST name
# with no rule takes posixrules' transitions), TZ=right/UTC (leap seconds).

def cases_r3():
    for started in (-1e11, -5e10, -5e10 - 1, 1.5e11, 2e14, 1e15, 3e16, 6.7e16,
                    1483228826.0, 1483228827.0, 78796799.0, 78796800.0):
        yield _finding_fake(configs=["root"], tx=dict(TX, started=started), before=False)
    for t in ("1900-01-01T00:00", "1950-02-15T12:00", "9999-12-31T23:59",
              "0001-01-01T00:00", "2016-12-31T23:59:60"):
        yield "_ts", {"text": t}, (lambda t=t: snapshots._ts(t))


_cases_r2b_all = cases


def cases():
    yield from _cases_r2b_all()
    yield from cases_r3()
