"""A local change to a checkout, held up against the update.

Plugins and other git checkouts get patched in place: a fix that has not
been released yet, a setting the author did not expose. `git pull --ff-only`
then refuses to update -- correctly, since it will not touch uncommitted
work -- and the checkout sits behind forever, which is worse.

What the user needs to know is how their change relates to the update, and
git can answer that exactly, without touching the working tree:

* **absorbed** -- the update already contains the change, line for line. It
  can be dropped.
* **applies** -- the change still applies on top of the update. It can be
  carried forward.
* **conflicts** -- the update changed the same lines. The change is set aside
  (as a stash, so nothing is lost) and the update installed.

Each is decided by applying the change to a scratch index built from the
upstream tree (`git apply --cached --check`), forwards and in reverse. The
working tree and the real index are never touched until the user chooses.

"Absorbed" is deliberately exact. Upstream fixing the same bug a different
way is common -- and it lands in "conflicts", because deciding that two
different patches mean the same thing is a judgement git cannot make.
"""
from __future__ import annotations

import os
import subprocess
import tempfile
from dataclasses import dataclass, field

CLEAN, ABSORBED, APPLIES, CONFLICTS, UNKNOWN = (
    "clean", "absorbed", "applies", "conflicts", "unknown")


@dataclass
class PatchState:
    kind: str
    files: list = field(default_factory=list)
    upstream_commits: list = field(default_factory=list)   # touching `files`
    untracked: list = field(default_factory=list)


def _git(repo, *args, env=None, input_text=None, timeout=30):
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True,
                          text=True, timeout=timeout, env=env, input=input_text)


def analyze(repo: str, upstream: str = "@{upstream}") -> PatchState:
    try:
        diff = _git(repo, "diff", "HEAD", "--binary")
        if diff.returncode != 0:
            return PatchState(UNKNOWN)
        untracked = [l for l in _git(repo, "ls-files", "--others",
                                     "--exclude-standard").stdout.splitlines() if l]
        if not diff.stdout.strip():
            return PatchState(CLEAN, untracked=untracked)
        files = [l for l in _git(repo, "diff", "HEAD", "--name-only")
                 .stdout.splitlines() if l]
        commits = [l for l in _git(repo, "log", "--format=%h %s",
                                   f"HEAD..{upstream}", "--", *files)
                   .stdout.splitlines() if l]

        with tempfile.TemporaryDirectory() as tmp:
            env = {**os.environ, "GIT_INDEX_FILE": os.path.join(tmp, "index")}
            if _git(repo, "read-tree", upstream, env=env).returncode != 0:
                return PatchState(UNKNOWN, files, commits, untracked)

            def check(*flags):
                return _git(repo, "apply", "--cached", "--check", *flags,
                            env=env, input_text=diff.stdout).returncode == 0

            if check("-R"):
                kind = ABSORBED
            elif check():
                kind = APPLIES
            else:
                kind = CONFLICTS
        return PatchState(kind, files, commits, untracked)
    except (OSError, subprocess.SubprocessError):
        return PatchState(UNKNOWN)


def steps(repo: str, state: PatchState, label: str) -> list:
    """The task steps that update `repo` for this state."""
    stash = ["git", "-C", repo, "stash", "push", "-m",
             f"almanac: local change before updating {label}"]
    pull = ["git", "-C", repo, "pull", "--ff-only"]
    if state.kind == APPLIES:
        return [("Setting your change aside", stash),
                ("Pulling the update", pull),
                ("Putting your change back", ["git", "-C", repo, "stash", "pop"])]
    if state.kind in (ABSORBED, CONFLICTS):
        # Kept as a stash either way: "absorbed" is exact, but a stash costs
        # nothing and a lost afternoon of work costs a great deal.
        return [("Setting your change aside", stash),
                ("Pulling the update", pull)]
    return [("Pulling the update", pull)]


def explain(state: PatchState, name: str) -> tuple[str, str, str]:
    """(heading, body, action label) for the confirmation dialog."""
    files = ", ".join(state.files[:3]) + ("…" if len(state.files) > 3 else "")
    if state.kind == ABSORBED:
        return (f"The update already includes your change",
                f"Your edit to {files} is in the new version, line for line. "
                f"Updating replaces your copy with it; your copy is kept as a "
                f"git stash in case you want it back.",
                "Update")
    if state.kind == APPLIES:
        return (f"Update {name} and keep your change?",
                f"Your edit to {files} still applies on top of the update. "
                f"It is set aside, the update is pulled, and your edit is put "
                f"back.",
                "Update and keep it")
    if state.kind == CONFLICTS:
        n = len(state.upstream_commits)
        return (f"The update changes what you edited",
                f"{n or 'Some'} upstream commit(s) touch {files}, so your edit "
                f"cannot be carried forward automatically. Updating sets it "
                f"aside as a git stash — nothing is lost — and installs the "
                f"new version. It may already do what your edit did.",
                "Update and set mine aside")
    return (f"Update {name}?", "", "Update")
