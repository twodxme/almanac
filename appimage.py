"""AppImage update checking.

Most AppImages embed their own update source in a `.upd_info` ELF section --
the same string AppImageUpdate uses. Reading it means the update source is
discovered from the file rather than configured by hand, which is the whole
difference between "tracked" and "managed".

Only the GitHub forms are implemented. The zsync-over-http forms exist but
need a zsync client to be useful, and every AppImage seen here so far uses
gh-releases-*.
"""
from __future__ import annotations

import fnmatch
import json
import re
import subprocess
import urllib.request
from pathlib import Path

UA = {"User-Agent": "almanac"}


def read_update_info(path: Path) -> str:
    """The raw .upd_info string, or '' when the AppImage carries none."""
    try:
        out = subprocess.run(
            ["objcopy", "-O", "binary", "--only-section=.upd_info",
             str(path), "/dev/stdout"],
            capture_output=True, timeout=20).stdout
    except (OSError, subprocess.SubprocessError):
        return ""
    return out.replace(b"\x00", b"").decode("utf-8", "replace").strip()


def parse_update_info(info: str) -> dict:
    """gh-releases-zsync|owner|repo|release|pattern -> a dict we can act on."""
    parts = info.split("|")
    if len(parts) >= 5 and parts[0].startswith("gh-releases"):
        return {"kind": "github", "owner": parts[1], "repo": parts[2],
                "release": parts[3], "pattern": parts[4]}
    return {}


_VERSION = re.compile(r"(\d+(?:\.\d+)+)")


def _version_of(text: str) -> str:
    m = _VERSION.search(text or "")
    return m.group(1) if m else ""


def _vkey(v: str):
    """Comparable tuple. '0.10.2' must sort above '0.9.9', which it does not
    as text -- the reason a string compare is wrong here."""
    return tuple(int(x) for x in v.split(".")) if v else ()


def latest_github(owner: str, repo: str, pattern: str, release: str = "latest") -> dict:
    """Newest release plus the asset matching the AppImage's own pattern."""
    tag = "releases/latest" if release in ("latest", "") else f"releases/tags/{release}"
    url = f"https://api.github.com/repos/{owner}/{repo}/{tag}"
    # Shared cache: authenticated when gh is logged in, conditional otherwise,
    # and never competing with the store for the same rate limit.
    try:
        import ghcache
        data = ghcache.get_json(url)
    except Exception:
        data = None
    if not isinstance(data, dict):
        return {}                      # offline or rate-limited: no update shown

    # The pattern names a .zsync companion; the AppImage is the same name
    # without that suffix.
    want = pattern[:-6] if pattern.endswith(".zsync") else pattern
    asset_url = asset_name = ""
    for a in data.get("assets", []):
        if fnmatch.fnmatch(a.get("name", ""), want):
            asset_url = a.get("browser_download_url", "")
            asset_name = a.get("name", "")
            break

    tag_name = data.get("tag_name", "")
    return {
        "version": _version_of(tag_name) or _version_of(asset_name) or tag_name,
        "tag": tag_name,
        "url": asset_url,
        "asset": asset_name,
        "published": (data.get("published_at") or "")[:10],
        "page": data.get("html_url", ""),
    }


def check(record: dict) -> dict:
    """Given a registry record, return {} or the newer release's details."""
    path = Path(record.get("path", ""))
    if not path.exists():
        return {}

    spec = record.get("update_spec") or parse_update_info(read_update_info(path))
    if spec.get("kind") != "github":
        return {}

    latest = latest_github(spec["owner"], spec["repo"],
                           spec.get("pattern", "*.AppImage"),
                           spec.get("release", "latest"))
    if not latest.get("url"):
        return {}

    have = _version_of(record.get("version", ""))
    new = latest["version"]
    # Only report a genuinely higher version. Equal or unparseable means
    # "nothing to do" -- claiming an update that reinstalls the same build
    # would be worse than staying quiet.
    if have and new and _vkey(new) <= _vkey(have):
        return {}
    if not have and not new:
        return {}
    latest["spec"] = spec
    return latest


def download(url: str, dest: Path, progress=None) -> Path:
    """Fetch to a temporary file beside dest, then swap it in.

    Downloading over the live file would leave a half-written AppImage if the
    connection dropped; the swap is the last step and is atomic.
    """
    tmp = dest.with_suffix(".download")
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=120) as r:
        total = int(r.headers.get("Content-Length") or 0)
        done = 0
        with open(tmp, "wb") as f:
            while chunk := r.read(262144):
                f.write(chunk)
                done += len(chunk)
                if progress and total:
                    progress(done / total)
    # Keep the previous build: a bad update must be revertible.
    if dest.exists():
        dest.replace(dest.with_suffix(".previous"))
    tmp.replace(dest)
    dest.chmod(dest.stat().st_mode | 0o111)
    return dest


def install_desktop_entry(appimage_path: Path, name: str) -> bool:
    """Give a tracked AppImage a launcher entry and icon.

    An AppImage ships its own .desktop and icon; extracting them is what makes
    it a real app rather than a file we happen to know about. Exec is rewritten
    to the stored path, since the shipped one assumes the AppImage runtime's
    own mount point.
    """
    import shutil
    import tempfile

    apps_dir = Path.home() / ".local/share/applications"
    icons_dir = Path.home() / ".local/share/icons/hicolor"
    apps_dir.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(dir=str(appimage_path.parent)) as tmp:
        try:
            subprocess.run([str(appimage_path), "--appimage-extract"],
                           cwd=tmp, capture_output=True, timeout=180)
        except (OSError, subprocess.SubprocessError):
            return False
        root = Path(tmp) / "squashfs-root"
        desktops = list(root.glob("*.desktop"))
        if not desktops:
            return False

        icon_name = f"almanac-{name}"
        for size in ("512x512", "256x256", "128x128", "64x64", "48x48"):
            for src in root.glob(f"usr/share/icons/hicolor/{size}/apps/*.png"):
                d = icons_dir / size / "apps"
                d.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src, d / f"{icon_name}.png")
                break

        lines = []
        for line in desktops[0].read_text(errors="replace").splitlines():
            if line.startswith("Exec="):
                # Preserve any field codes (%F, %U) the entry declared.
                tail = line.split("=", 1)[1]
                codes = " ".join(t for t in tail.split() if t.startswith("%"))
                lines.append(f"Exec={appimage_path} {codes}".strip())
            elif line.startswith("Icon="):
                lines.append(f"Icon={icon_name}")
            elif line.startswith("TryExec="):
                lines.append(f"TryExec={appimage_path}")
            else:
                lines.append(line)
        (apps_dir / f"almanac-{name}.desktop").write_text("\n".join(lines) + "\n")

    for cmd in (["update-desktop-database", str(apps_dir)],
                ["gtk-update-icon-cache", "-f", "-t", str(icons_dir)]):
        try:
            subprocess.run(cmd, capture_output=True, timeout=30)
        except (OSError, subprocess.SubprocessError):
            pass
    return True


def remove_desktop_entry(name: str) -> None:
    apps = Path.home() / ".local/share/applications" / f"almanac-{name}.desktop"
    apps.unlink(missing_ok=True)
    for png in (Path.home() / ".local/share/icons/hicolor").glob(
            f"*/apps/almanac-{name}.png"):
        png.unlink(missing_ok=True)
