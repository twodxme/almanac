#!/usr/bin/env python3
"""What the parity cases depend on, measured on this machine, for the record
(CI prints it before the parity run):

- the interpreter, and the stack limit (`ulimit -s`);
- how deep CPython's json nests before RecursionError, on the main thread
  and on a worker thread: CPython 3.14 bounds it by the C stack left, the
  cases pin the 8 MiB values, and the port's limits are those;
- whether the live AUR query (sources' only network case) gets an answer,
  from Python and from the port (ALMANAC_PARITY_BIN).

    python3 rust/parity/environment.py
"""
import json
import os
import resource
import subprocess
import sys
import threading
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent.parent))


def json_limit(open_, close, leaf=""):
    lo, hi = 1000, 400000
    while lo < hi:
        mid = (lo + hi + 1) // 2
        try:
            json.loads(open_ * mid + leaf + close * mid)
            lo = mid
        except RecursionError:
            hi = mid - 1
    return lo


def limits():
    return json_limit("[", "]"), json_limit('{"a": ', "}", "1")


def main():
    soft, hard = resource.getrlimit(resource.RLIMIT_STACK)
    kb = lambda v: "unlimited" if v == resource.RLIM_INFINITY else f"{v // 1024} kB"
    print(f"python {sys.version.split()[0]}, uid {os.getuid()}, stack {kb(soft)} (hard {kb(hard)})")
    out = {}
    t = threading.Thread(target=lambda: out.update(t=limits()))
    t.start()
    t.join()
    print(f"json nesting (arrays, objects): main thread {limits()}, worker thread {out['t']}")

    url = "https://aur.archlinux.org/rpc/v5/info?arg[]=yay"
    import sources
    try:
        print("AUR from Python:", sources._get(url)[:120])
    except Exception as exc:                            # noqa: BLE001
        print("AUR from Python:", f"{type(exc).__name__}: {exc}")
    binary = os.environ.get("ALMANAC_PARITY_BIN")
    if binary:
        req = {"module": "sources", "cases": [["_get", {"url": url}]]}
        r = subprocess.run([binary], input=json.dumps(req), capture_output=True, text=True, timeout=120)
        print("AUR from the port:", (r.stdout or r.stderr)[:200])


if __name__ == "__main__":
    main()
