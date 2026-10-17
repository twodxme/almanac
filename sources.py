"""Install from a link: every way a project ships, compared.

A GitHub project usually reaches an Arch machine several ways at once: an
official package, an AppImage on its releases page, an AUR package that
builds from source, an AUR `-bin` that repackages someone's binary. They are
not equally trustworthy, and the difference is invisible from the project
page. Asked to install "this link", this lays the options out side by side
and recommends one, with the reason:

1. **The official repositories** -- built and signed by the distribution.
2. **The project's own AppImage**, when its checksum can be verified: GitHub
   publishes a SHA-256 for every release asset, and many projects also ship a
   checksum file.
3. **An AUR package that builds from source**, when it points at this same
   repository and people actually use it (votes).
4. An AppImage with nothing to verify it against, or an AUR binary
   repackage, last -- and an AUR package whose upstream URL is a different
   repository is flagged, since that is how a lookalike gets installed.

Nothing is installed by resolving. The download of an AppImage is checked
against its checksum before it is made executable, and a mismatch deletes it.
"""
from __future__ import annotations

import hashlib
import json
import re
import subprocess
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path

UA = {"User-Agent": "almanac"}
_GH = re.compile(r"(?:https?://)?(?:www\.)?github\.com/([\w.-]+)/([\w.-]+?)(?:\.git)?(?:[/#?].*)?$")
_X86 = re.compile(r"(x86[_-]64|amd64|x64)", re.I)
_OTHER_ARCH = re.compile(r"(aarch64|arm64|armv7|armhf|i[3-6]86|riscv)", re.I)

REPO, APPIMAGE, AUR = "repo", "appimage", "aur"


@dataclass
class Option:
    kind: str
    name: str                       # package name, or asset file name
    version: str
    title: str                      # "Official package", "AppImage"…
    reasons: list = field(default_factory=list)
    warnings: list = field(default_factory=list)
    rank: int = 99                  # lower is better
    url: str = ""                   # AppImage download
    sha256: str = ""                # expected digest, when known
    size_mb: float = 0.0


@dataclass
class Resolution:
    owner: str
    repo: str
    description: str = ""
    stars: int = 0
    license: str = ""
    archived: bool = False
    options: list = field(default_factory=list)
    error: str = ""

    @property
    def url(self) -> str:
        return f"https://github.com/{self.owner}/{self.repo}"

    @property
    def best(self) -> Option | None:
        return self.options[0] if self.options else None


def parse(link: str) -> tuple[str, str] | None:
    m = _GH.match((link or "").strip())
    if not m:
        return None
    return m.group(1), m.group(2)


def _get(url: str, timeout=20):
    req = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def _official(name: str) -> Option | None:
    try:
        out = subprocess.run(["env", "LC_ALL=C", "pacman", "-Si", name],
                             capture_output=True, text=True, timeout=20).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    fields = dict(l.split(":", 1) for l in out.splitlines() if ":" in l
                  and not l.startswith(" "))
    fields = {k.strip(): v.strip() for k, v in fields.items()}
    if not fields.get("Version"):
        return None
    return Option(REPO, name, fields["Version"],
                  f"Official package ({fields.get('Repository', 'repo')})",
                  ["Built and signed by your distribution",
                   "Updated with the rest of the system"], rank=0)


def _norm(url: str) -> str:
    u = (url or "").lower().strip().rstrip("/").removesuffix(".git")
    return re.sub(r"^https?://(www\.)?", "", u)


def _aur(names: list[str], homes: list[str]) -> list[Option]:
    """AUR packages by these names. `homes` are the URLs that count as this
    project: its repository, and the homepage the repository itself names."""
    query = "&".join(f"arg[]={urllib.parse.quote(n)}" for n in names)
    try:
        data = json.loads(_get(f"https://aur.archlinux.org/rpc/v5/info?{query}"))
    except Exception:                                   # noqa: BLE001
        return []
    out = []
    want = {_norm(h) for h in homes if h}
    for rec in data.get("results", []):
        name = rec.get("Name", "")
        votes = int(rec.get("NumVotes") or 0)
        url = _norm(rec.get("URL") or "")
        binary = name.endswith(("-bin", "-appimage"))
        reasons, warnings = [], []
        same = url in want
        if same:
            reasons.append("Points at this project")
        else:
            warnings.append(f"Its upstream is {rec.get('URL') or 'not stated'}, "
                            f"which is not this project")
        reasons.append(f"{votes} vote(s) on the AUR")
        if not rec.get("Maintainer"):
            warnings.append("Orphaned: nobody maintains it")
        if rec.get("OutOfDate"):
            warnings.append("Flagged out of date")
        if binary:
            warnings.append("Repackages a prebuilt binary; nothing is built "
                            "from the source you can read")
        elif name.endswith("-git"):
            warnings.append("Builds the newest commit, not a release")
        else:
            reasons.append("Builds from the published source")
        rank = 10
        if not same:
            rank += 40
        if binary:
            rank += 10
        if name.endswith("-git"):
            rank += 5
        if votes < 5:
            rank += 15
        if not rec.get("Maintainer") or rec.get("OutOfDate"):
            rank += 20
        out.append(Option(AUR, name, rec.get("Version", ""), "AUR package",
                          reasons, warnings, rank))
    return out


def _sums(assets) -> dict[str, str]:
    """{file name: sha256} from any checksum file in the release."""
    out = {}
    for a in assets:
        nm = a.get("name", "").lower()
        if not (("sha256" in nm or "checksum" in nm) and a.get("size", 0) < 65536):
            continue
        try:
            text = _get(a["browser_download_url"]).decode("utf-8", "replace")
        except Exception:                               # noqa: BLE001
            continue
        for line in text.splitlines():
            parts = line.split()
            if len(parts) >= 2 and re.fullmatch(r"[0-9a-fA-F]{64}", parts[0]):
                out[parts[-1].lstrip("*")] = parts[0].lower()
            elif len(parts) == 1 and re.fullmatch(r"[0-9a-fA-F]{64}", parts[0]) \
                    and nm.endswith(".sha256"):
                out[a["name"][: -len(".sha256")]] = parts[0].lower()
    return out


def _appimage(release: dict) -> Option | None:
    assets = release.get("assets") or []
    images = [a for a in assets if a.get("name", "").endswith(".AppImage")
              and not _OTHER_ARCH.search(a["name"])]
    if not images:
        return None
    # An explicitly x86_64 build beats one that does not say.
    images.sort(key=lambda a: 0 if _X86.search(a["name"]) else 1)
    a = images[0]
    sha = ""
    digest = a.get("digest") or ""
    if digest.startswith("sha256:"):
        sha = digest.split(":", 1)[1]
    sha = sha or _sums(assets).get(a["name"], "")
    tag = (release.get("tag_name") or "").lstrip("v")
    reasons = ["Published by the project itself, on its releases page",
               "Runs without installing anything system-wide"]
    warnings = []
    if sha:
        reasons.append("Checksum verified before it is made runnable")
    else:
        warnings.append("No checksum published to verify the download")
    return Option(APPIMAGE, a["name"], tag, "AppImage", reasons, warnings,
                  rank=5 if sha else 30, url=a.get("browser_download_url", ""),
                  sha256=sha, size_mb=round((a.get("size") or 0) / 1048576, 1))


def resolve(link: str) -> Resolution:
    import ghcache
    parsed = parse(link)
    if not parsed:
        return Resolution("", "", error="That is not a GitHub repository link.")
    owner, repo = parsed
    meta = ghcache.repo(f"{owner}/{repo}") or {}
    if not isinstance(meta, dict) or not meta.get("name"):
        return Resolution(owner, repo, error="GitHub did not return that "
                                             "repository. Check the link, or "
                                             "try again in a minute.")
    owner = meta.get("owner", {}).get("login") or owner
    repo = meta["name"]
    res = Resolution(owner, repo, meta.get("description") or "",
                     int(meta.get("stargazers_count") or 0),
                     (meta.get("license") or {}).get("spdx_id") or "",
                     bool(meta.get("archived")))

    name = repo.lower()
    opts = []
    if (o := _official(name)):
        opts.append(o)
    release = ghcache.latest_release(f"{owner}/{repo}")
    if isinstance(release, dict) and (o := _appimage(release)):
        opts.append(o)
    opts += _aur([name, f"{name}-bin", f"{name}-git", f"{name}-appimage"],
                 [res.url, meta.get("homepage") or ""])
    if res.archived:
        for o in opts:
            o.warnings.append("The repository is archived: no further updates")
    opts.sort(key=lambda o: o.rank)
    res.options = opts
    if not opts:
        res.error = ("No installable build found: no official package, no "
                     "Linux AppImage in the latest release, and nothing on "
                     "the AUR under that name.")
    return res


def install_appimage(res: Resolution, opt: Option, log=print) -> Path:
    """Download, verify, register and add a launcher. Raises on failure."""
    import appimage
    import inventory

    key = re.sub(r"[^a-z0-9._-]", "-", res.repo.lower())
    dest = Path(inventory.REGISTRY).parent / "apps" / f"{key}.AppImage"
    dest.parent.mkdir(parents=True, exist_ok=True)
    partial = dest.with_suffix(".unverified")
    log(f"{opt.name}  ({opt.size_mb:.0f} MB)")
    appimage.download(opt.url, partial)
    if opt.sha256:
        h = hashlib.sha256()
        with partial.open("rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                h.update(chunk)
        if h.hexdigest() != opt.sha256:
            partial.unlink(missing_ok=True)
            raise RuntimeError("checksum mismatch: the download is not the "
                               "file the project published, so it was deleted")
        log("checksum verified")
    partial.chmod(0o755)
    partial.replace(dest)

    # Track by the asset's naming pattern with the version wildcarded, so the
    # existing update check finds the next release.
    pattern = opt.name.replace(opt.version, "*") if opt.version else opt.name
    reg = inventory._load_registry()
    reg[key] = {
        "name": key, "path": str(dest), "version": opt.version,
        "description": res.description, "url": res.url,
        "latest": {}, "checked_at": 0,
        "update_spec": {"kind": "github", "owner": res.owner, "repo": res.repo,
                        "release": "latest", "pattern": pattern},
    }
    inventory.save_registry(reg)
    if appimage.install_desktop_entry(dest, key):
        log("launcher entry added")
    import history
    history.record(history.ALMANAC, f"Installed {res.repo} {opt.version}",
                   f"AppImage from {res.url}"
                   + (" · checksum verified" if opt.sha256 else ""))
    return dest
