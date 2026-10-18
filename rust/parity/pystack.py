"""How deep CPython's json nests on a worker thread: as the port assumes it,
and as this interpreter actually gets.

CPython 3.14 bounds `json.loads` by the C stack left. A worker thread starts
on a fresh stack of RLIMIT_STACK bytes, and the port takes 160 bytes a level
and 43920 before the first (52154 levels under 8 MiB, as measured here). But
glibc keeps the thread's TLS and descriptor at the top of that stack, and the
TLS a process carries depends on the libraries it has loaded, so another
machine's CPython gets a level or so more or fewer (CI's container: 52155).
The exact boundary is pinned only where this interpreter agrees with the
port's number; elsewhere only depths clear of that band are deterministic,
as on the main thread (whose start ASLR and the environment move).
"""
import json
import resource
import threading

# Levels either side of the assumed limit that no environment's TLS reaches
# (~6 KB).
MARGIN = 40

_measured = None


def assumed() -> int:
    """The port's worker-thread limit for this process's `ulimit -s`."""
    soft = resource.getrlimit(resource.RLIMIT_STACK)[0]
    size = 2 << 20 if soft == resource.RLIM_INFINITY else (max(soft, 16384) + 4095) // 4096 * 4096
    return (size - 43920) // 160


def measured() -> int:
    """This interpreter's worker-thread limit: the deepest array it decodes."""
    global _measured
    if _measured is None:
        out = {}

        def search():
            lo, hi = assumed() - 4 * MARGIN, assumed() + 4 * MARGIN
            while lo < hi:
                mid = (lo + hi + 1) // 2
                try:
                    json.loads("[" * mid + "]" * mid)
                    lo = mid
                except RecursionError:
                    hi = mid - 1
            out["n"] = lo
        t = threading.Thread(target=search)
        t.start()
        t.join()
        _measured = out["n"]
    return _measured


def exact() -> bool:
    return measured() == assumed()


def thread_depths() -> list[int]:
    """Nesting depths worth a worker-thread case here: the boundary itself
    (one under, at, one past) where it is this interpreter's, and depths
    clear of the band either way."""
    lim = assumed()
    return ([lim - 1, lim, lim + 1] if exact() else []) + [lim - MARGIN, lim + MARGIN]
