# Update all inside the Almanac window

Date: 2026-11-21 · Status: draft for approval · Project 1 of 3
(2: the polished view's later refinements; 3: autonomous self-repair — separate designs)

## Why

Update all opens a terminal (`xdg-terminal-exec`) that runs `omarchy update`. On
2026-11-19 that terminal was closed while `omarchy update` waited at its yes/no
prompt. The orphan held the Omarchy update lock for two days, every later update
exited 1 at once, and Almanac's report blamed the packages. (Commit `fcb15fc`
now detects and offers to stop such an orphan; this project removes the way it
happens.)

Tim wants every update self-contained in the Almanac window, through the GUI,
no terminal panes — and beautiful.

## Goals

- Update all runs `omarchy update` with no terminal window, start to finish.
- Progress is a **step timeline** (approved mockup `assets/2026-11-21-update-view.html`, left):
  Snapshot → Signing keys → Download → Installing (expands to the app rows, same
  icons as the Updates list, each ticking off with its new version) → Omarchy
  migrations → Checking everything works. Overall bar, step "n of 6", a
  "Safe to keep working" line, raw log behind **Details**.
- On failure the step that failed turns red, says why in plain words, says
  whether anything changed, and carries the fix button when Almanac has one
  (mockup `assets/2026-11-21-update-view.html`, right).
- The sudo password is asked for in an Almanac dialog, once per update.
- Any question the update asks mid-way is put to Tim in the window, never lost.
- The existing post-update checks/report (`postupdate.rs`) run as today, fed
  from this run instead of from the terminal's `script` file.

## Non-goals

- Re-implementing Omarchy's update steps (snapshots, keyring, migrations,
  hooks, mise, AUR, boot verification). Omarchy stays in charge.
- Autonomous repair (project 3).
- Non-Omarchy systems change only as far as they share the runner; `yay -Syu`
  / `pacman -Syu` paths may keep the terminal for now.

## Anti-goals (failure even if it "works")

- An update that can be left waiting with no visible way to answer it.
- Bypassing Omarchy's pacman guard (`OMARCHY_ALLOW_DIRECT_PACMAN`).
- Storing the password anywhere, or keeping it after the update ends.
- A timeline that claims a step is done that wasn't; when unsure, show
  "Updating…" with the log rather than a confident wrong tick.
- A look that differs from the rest of Almanac (one visual system across pages).

## Approaches (Tim picked 1)

1. **Run `omarchy update -y` under a hidden pseudo-terminal inside Almanac.**
   Omarchy does the work; Almanac shows it. Risk: Omarchy renames things and a
   step stops ticking — mitigated by the fallback below. **Chosen.**
2. Almanac performs each step itself (snapshot, keyring, `pkexec pacman -Syu`,
   migrations). Exact progress, but duplicates and drifts from Omarchy's script
   and must defeat its guard. Rejected.
3. Embed a real terminal widget (VTE) in the window. Robust, but it is still a
   terminal on screen, which is what Tim asked to be rid of. Rejected.

## Design

**Running it.** Spawn `omarchy update -y` in its own session on a PTY (pacman
and gum render properly; nothing is captured as a pipe that changes their
behaviour). `-y` sets `OMARCHY_UPDATE_UNATTENDED=1`, which skips the start
confirmation and the reboot/restart `gum confirm`s (they print "Run
omarchy-system reboot when ready" instead — Almanac shows that as a "Restart to
finish" note). `omarchy update` already records itself with `script` to
`/tmp/omarchy-update.log`; Almanac reads the PTY directly and keeps its own
copy for the report's "Show log".

**Password.** Set `SUDO_ASKPASS` to an Almanac helper (the same binary,
`almanac --askpass`), which forwards sudo's prompt over a per-run Unix socket
(0600, in `$XDG_RUNTIME_DIR`, random name, peer-credential checked) to the
running Almanac. Almanac shows the password dialog in-window and returns the
answer over the socket; nothing is written to disk. sudo is asked once
(`authorize_update`'s `sudo true`) and kept fresh by Omarchy's keepalive. To
verify early: that sudo 1.9.17 uses `SUDO_ASKPASS` for a plain `sudo` when the
session's terminal is a PTY (it may prompt on the PTY instead — then the PTY
prompt "[sudo] password for …:" is answered from the same dialog; either path
ends in the same dialog). Cancel → the update stops before any change; the
timeline says so.

**Steps from processes, not wording.** Almanac polls the update's process tree
(`/proc`, children of the spawned session) every ~250 ms and maps the running
Omarchy sub-command to a step: `omarchy-snapshot` → Snapshot,
`omarchy-update-keyring` → Signing keys, `omarchy-update-system-pkgs` →
Download/Installing, `omarchy-migrate` → Omarchy migrations,
`omarchy-update-aur-pkgs` → Installing (AUR rows), `omarchy-update-boot` →
Checking everything works. Steps complete in order when a later one starts. *(Builder-check call, mine:)*
the timeline follows Omarchy's real order, so no step ticks early:
1 Snapshot (`omarchy-update-pkg-prune`, `omarchy-snapshot`) · 2 Signing keys ·
3 System packages (download, then the repo app rows) · 4 Omarchy migrations
(`omarchy-migrate`, orphans, service restarts, `omarchy-hook post-update`) ·
5 AUR and tools (`omarchy-update-mise`, `omarchy-update-aur-pkgs`; the AUR app
rows live here) · 6 Checking everything works (`omarchy-update-boot`, then
Almanac's own post-checks). Still "n of 6".
Within the package step, pacman's lines (`:: Retrieving packages…`,
`(3/5) upgrading slack-desktop`) drive Download vs Installing and the app rows.
Unknown sub-commands → current step stays as is; nothing matched for a while →
generic "Updating…" (never a wrong tick).

**Questions mid-update (Tim: hands-off + Resolve).** The run stays `-y`
(`OMARCHY_UPDATE_UNATTENDED=1`): nothing interrupts, and Almanac never reboots —
a needed restart shows as a "Restart to finish" note. Under `-y` the conflict
handler (`omarchy-update-system-pkgs-when-conflicted`) does not ask; it prints
"This upgrade needs an answer…" and exits 1. Almanac turns that into a red
"System packages" step naming the conflicting packages, with **Resolve**.
Resolve re-runs `omarchy update` with `OMARCHY_UPDATE_UNATTENDED=0` on the PTY
(still with `-y`, so no start confirmation) and, when the PTY output goes idle
on a prompt (`[Y/n]`, `[y/N]`, `Enter a selection`, gum confirm), shows the
question in that step with its answers as buttons, writing the choice to the
PTY. In this Resolve run a reboot `gum confirm` is always answered "No" by
Almanac and shown as the restart note.

**Leaving and closing (Tim: hide to background).** While an update runs,
closing the window hides it; Almanac (the `adw::Application`, held with a hold
guard) keeps running and owns the PTY until the update ends, then sends a
desktop notification ("Update finished — 5 updated" / "Update needs a look"),
and the report is waiting when the window is reopened. An explicit Quit during
a run asks first; during the system-packages step it refuses ("Quitting now
would stop pacman part-way"). A password or Resolve question while hidden
raises the window with the question. The update lock and the `fcb15fc`
preflight still guard against two runs.

**After.** On exit, the existing flow continues: `probe_after`, `build_report`,
the report dialog (now replacing the timeline's content, not stacked on it),
history entry. Exit code comes from the process directly (no `script` status
file).

**The rest of Update all.** *(Builder-check call, mine:)* Flatpak, plugins,
AppImages, agents and checkouts join the same view as a 7th step, "Other apps",
after step 6, run by Almanac as today (`update_rest_then`) but reporting into
the timeline instead of their own dialog; `flatpak update -y` is no longer
appended to the Omarchy command. The single report replaces the timeline at the
end of the whole run. (So the count is "n of 7" when there is anything besides
system packages.)

**Fallback.** No PTY, helper not usable, or not Omarchy → today's terminal path,
unchanged.

## Constraints

- Rust + GTK4/libadwaita, as the rest of Almanac; one visual system with the
  other pages; colours from the current theme.
- The demo gate (`run_task`, `ALMANAC_DEMO`) must refuse this run like any other.
- Build output never under `/tmp` (CLAUDE.md).

## Left to the builder

PTY crate choice (or `rustix`/`nix` openpty), the exact prompt patterns, the
poll interval, widget structure, how the compact progress item looks (follow
the existing header style), test seams (fake `/proc` trees, a fake updater
script that prints recorded `omarchy update` output and asks a question).
