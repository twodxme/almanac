"""Vendor icons, fetched from the vendors themselves.

Every icon here comes from the domain it represents -- stripe.com's favicon is
fetched from stripe.com -- rather than from a favicon proxy. A proxy would be
one request containing the full list of every service this machine talks to,
sent to a company with no other involvement in it. These are hosts the user is
already connected to.

Nothing is fetched twice: a hit is cached as a file, and a miss is remembered
too, so a domain with no usable icon is not retried on every redraw.
"""
from __future__ import annotations

import json
import os
import re
import time
import urllib.request
from pathlib import Path

CACHE = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "almanac"
ICONS = CACHE / "icons"
MISSES = CACHE / "icon-misses.json"
MISS_TTL = 7 * 86400
UA = {"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) almanac"}

# Tried in order. apple-touch-icon is last because it is the largest and the
# least often present.
PATHS = ("/favicon.ico", "/favicon.png", "/favicon.svg",
         "/apple-touch-icon.png", "/apple-touch-icon-precomposed.png")

_EXT = {b"\x89PNG": ".png", b"\x00\x00\x01\x00": ".ico", b"GIF8": ".gif",
        b"\xff\xd8\xff": ".jpg", b"<svg": ".svg", b"<?xm": ".svg",
        b"RIFF": ".webp"}


def _kind(data: bytes) -> str:
    head = data[:4]
    for sig, ext in _EXT.items():
        if head.startswith(sig):
            return ext
    if data[:200].lstrip().startswith(b"<svg"):
        return ".svg"
    return ""


def registrable(host: str) -> str:
    """mcp.stripe.com -> stripe.com. Good enough for finding a brand icon."""
    parts = [p for p in (host or "").split(".") if p]
    if len(parts) <= 2:
        return ".".join(parts)
    # Two-part public suffixes that would otherwise be cut to nothing.
    if parts[-2] in ("co", "com", "org", "net", "ac", "gov") and len(parts) > 2:
        return ".".join(parts[-3:])
    return ".".join(parts[-2:])


def host_of(url: str) -> str:
    m = re.match(r"^[a-z]+://([^/:]+)", url or "")
    return m.group(1) if m else ""


def _load_misses() -> dict:
    try:
        return json.loads(MISSES.read_text())
    except (OSError, ValueError):
        return {}


def _save_miss(key: str) -> None:
    misses = _load_misses()
    misses[key] = time.time()
    try:
        CACHE.mkdir(parents=True, exist_ok=True)
        MISSES.write_text(json.dumps(misses))
    except OSError:
        pass


def cached(key: str) -> Path | None:
    """An already-downloaded icon, without touching the network."""
    if not key:
        return None
    for f in ICONS.glob(f"{_safe(key)}.*"):
        if f.stat().st_size:
            return f
    return None


def _safe(key: str) -> str:
    return re.sub(r"[^A-Za-z0-9._-]", "_", key)


def _fetch(url: str, timeout: int = 12) -> bytes:
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=timeout) as r:
        if r.status != 200:
            return b""
        return r.read(512 * 1024)


def fetch(key: str, url: str = "", domain: str = "") -> Path | None:
    """Get an icon for `key`, from an explicit url or a domain. Blocking.

    `key` is what the result is cached under, so two services on the same
    domain share one download.
    """
    if not key:
        return None
    hit = cached(key)
    if hit:
        return hit
    import settings
    if not settings.get("net_vendor_icons"):
        return None                     # what is cached still shows

    misses = _load_misses()
    if time.time() - misses.get(key, 0) < MISS_TTL:
        return None

    candidates = []
    if url:
        candidates.append(url)
    if domain:
        # The exact host first: mcp.example.com may have its own mark. Then
        # the registrable domain, which is where the brand icon usually is.
        for host in dict.fromkeys((domain, registrable(domain))):
            candidates += [f"https://{host}{p}" for p in PATHS]

    for cand in candidates:
        try:
            data = _fetch(cand)
        except Exception:
            continue
        if len(data) < 64:
            continue
        ext = _kind(data)
        if not ext:
            continue
        try:
            ICONS.mkdir(parents=True, exist_ok=True)
            dest = ICONS / f"{_safe(key)}{ext}"
            tmp = dest.with_suffix(dest.suffix + ".tmp")
            tmp.write_bytes(data)
            tmp.replace(dest)
            return dest
        except OSError:
            return None

    _save_miss(key)
    return None


def github_avatar(owner: str) -> str:
    """The org or user avatar, which is the brand mark for most repos."""
    return f"https://github.com/{owner}.png?size=128" if owner else ""
