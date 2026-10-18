#!/usr/bin/env python3
"""Run the Python app and the Rust port on the same inputs; fail on any
difference.

    python3 rust/parity/run.py                 # every module with cases
    python3 rust/parity/run.py inventory picks  # just these
    python3 rust/parity/run.py --demo theming   # with ALMANAC_DEMO=1 on both sides

Each module's cases live in `rust/parity/cases_<module>.py`, which defines

    def cases():
        yield "func_name", {"arg": ...}, lambda: python_module.func_name(...)

The first two items go to the Rust side (`almanac-parity`, which calls
`<module>::parity(func, args)`); the third is evaluated here. Both results
are normalised to JSON (dataclasses -> objects, tuples -> lists, Paths ->
strings, sets -> sorted lists) and must be equal. Floats must match within
1e-9 relative; everything else exactly.

The Rust binary is built once, with any features passed in RUST_FEATURES.
"""
from __future__ import annotations

import dataclasses
import enum
import importlib
import json
import math
import os
import re
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUST = HERE.parent
ROOT = RUST.parent
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(HERE))


def normalise(v):
    if dataclasses.is_dataclass(v) and not isinstance(v, type):
        return {f.name: normalise(getattr(v, f.name)) for f in dataclasses.fields(v)}
    if isinstance(v, enum.Enum):
        return normalise(v.value)
    if isinstance(v, dict):
        return {str(k): normalise(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [normalise(x) for x in v]
    if isinstance(v, (set, frozenset)):
        return sorted(normalise(x) for x in v)
    if isinstance(v, Path):
        return str(v)
    if isinstance(v, bytes):
        return v.decode("utf-8", "replace")
    if isinstance(v, float) and v.is_integer() and abs(v) < 2**53:
        return v                      # compared numerically below
    if v is None or isinstance(v, (str, int, float, bool)):
        return v
    if hasattr(v, "__dict__"):
        return {k: normalise(x) for k, x in vars(v).items() if not k.startswith("_")}
    return repr(v)


def same(a, b, path="$"):
    """None when equal, else a description of the first difference."""
    if isinstance(a, bool) or isinstance(b, bool):
        return None if a is b or a == b and type(a) is type(b) else f"{path}: {a!r} != {b!r}"
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        if a == b:
            return None
        if math.isclose(a, b, rel_tol=1e-9, abs_tol=1e-12):
            return None
        return f"{path}: {a!r} != {b!r}"
    if type(a) is not type(b):
        return f"{path}: {type(a).__name__} {a!r:.80} != {type(b).__name__} {b!r:.80}"
    if isinstance(a, dict):
        if a.keys() != b.keys():
            return f"{path}: keys differ: python-only {sorted(a.keys() - b.keys())}, rust-only {sorted(b.keys() - a.keys())}"
        for k in a:
            d = same(a[k], b[k], f"{path}.{k}")
            if d:
                return d
        return None
    if isinstance(a, list):
        if len(a) != len(b):
            return f"{path}: length {len(a)} != {len(b)}"
        for i, (x, y) in enumerate(zip(a, b)):
            d = same(x, y, f"{path}[{i}]")
            if d:
                return d
        return None
    return None if a == b else f"{path}: {a!r:.200} != {b!r:.200}"


# Differences that are understood and accepted, each with the reason. They are
# still printed on every run; they just do not fail it. Anything not matched
# here fails. Keep this list short and specific.
KNOWN = [
    # JSON numbers beyond 64 bits: Python's ints are unbounded, serde_json's
    # are not. Only reachable by hand-writing such a number into the AppImage
    # registry or by a git that counts more commits than exist.
    ("appimage", "check", lambda d: "100000000000000000001" in d or "1e+20" in d,
     "a registry number beyond 64 bits"),
    ("selfupdate", "check", lambda d: re.search(r"int \d{20,}", d),
     "a git 'behind' count beyond 64 bits"),
    ("selfupdate", "as_app", lambda d: re.search(r"int \d{20,}", d),
     "a git 'behind' count beyond 64 bits"),
    # Live measurements: the disk changes between the Python call and the
    # Rust one (journals and caches grow while the run is going).
    ("maintenance", "runtime_logs", lambda d: ".size_mb" in d, "live disk usage"),
    ("maintenance", "run_all", lambda d: ".size_mb" in d or ".detail" in d, "live disk usage"),
    ("maintenance", "disk_space", lambda d: True, "live disk usage"),
    ("verify", "recheck", lambda d: "free of" in d or ".size_mb" in d, "live disk usage"),
    # Harness, not port: Python's urllib builds its TLS context once per
    # process, before this case's SSL_CERT_FILE is set, when other modules'
    # cases ran first. Passes when the module runs alone.
    ("appimage", "download", lambda d: "127.0.0.1" in d, "TLS context built before the case's CA was set"),
]


def known(module: str, func: str, diff: str):
    for m, f, test, why in KNOWN:
        if m == module and f == func and test(diff):
            return why
    return None


def build() -> Path:
    feats = os.environ.get("RUST_FEATURES", "")
    cmd = [str(RUST / "tools/cargo-slot"), "cargo", "build", "-q", "--bin", "almanac-parity"]
    if feats:
        cmd += ["--features", feats]
    subprocess.run(cmd, cwd=RUST, check=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR", RUST / "target"))
    return target / "debug" / "almanac-parity"


def run_module(binary: Path, module: str) -> tuple[int, int, list[str]]:
    mod = importlib.import_module(f"cases_{module}")
    cases = list(mod.cases())
    expected, errors = [], []
    for func, args, fn in cases:
        try:
            expected.append(("ok", normalise(fn())))
        except Exception as exc:                       # noqa: BLE001
            expected.append(("raise", f"{type(exc).__name__}: {exc}"))
    req = {"module": module, "cases": [[f, a] for f, a, _ in cases]}
    out = subprocess.run([str(binary)], input=json.dumps(req), capture_output=True,
                         text=True, timeout=600)
    if out.returncode != 0:
        return len(cases), len(cases), [f"{module}: rust crashed: {out.stderr[-2000:]}"]
    got = json.loads(out.stdout)
    failed = 0
    for (func, args, _), (kind, want), wrapped in zip(cases, expected, got):
        if wrapped is None:
            failed += 1
            errors.append(f"{module}.{func}: not implemented in rust (parity returned None)")
            continue
        have = wrapped["v"]
        if kind == "raise":
            # Python raised: Rust must report an error object, {"error": ...}.
            if not (isinstance(have, dict) and "error" in have):
                failed += 1
                errors.append(f"{module}.{func}({json.dumps(args)[:120]}): python raised {want}, rust gave {json.dumps(have)[:200]}")
            continue
        d = same(want, have)
        if d:
            line = f"{module}.{func}({json.dumps(args)[:120]}): {d}"
            why = known(module, func, line)
            if why:
                errors.append(f"(known: {why}) {line}")
            else:
                failed += 1
                errors.append(line)
    return len(cases), failed, errors


def main(argv):
    if "--demo" in argv:
        os.environ["ALMANAC_DEMO"] = "1"
        argv = [a for a in argv if a != "--demo"]
    modules = argv or sorted(p.stem[len("cases_"):] for p in HERE.glob("cases_*.py"))
    binary = build()
    total = bad = 0
    for m in modules:
        n, f, errs = run_module(binary, m)
        total += n
        bad += f
        print(f"{m:16} {n - f}/{n} match")
        for e in errs[:25]:
            print("   ", e)
        if len(errs) > 25:
            print(f"    … and {len(errs) - 25} more")
    print(f"TOTAL {total - bad}/{total} match")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
