<div align="center">

<img src="docs/logo.png" alt="" width="104">

# Almanac

**Everything installed on this machine — where it came from, how old it is,
whether anyone still maintains it, and what needs you today.**

A native GTK4 / libadwaita catalogue for [Omarchy](https://omarchy.org) and Arch.

[![License: MIT](https://img.shields.io/badge/License-MIT-78b4ff.svg)](LICENSE)
![GTK4](https://img.shields.io/badge/GTK-4-86d39b.svg)
![libadwaita 1.9](https://img.shields.io/badge/libadwaita-1.9%2B-86d39b.svg)
![Arch](https://img.shields.io/badge/Arch-pacman%20%7C%20AUR-7dd7dc.svg)

<img src="docs/screenshots/featured.png" alt="Almanac's Featured page: a hero over the desktop wallpaper, with what needs attention below" width="900">

</div>

---

## Why

`pacman` knows about packages. It does not know about the AppImage in
`~/.local/share`, the plugin you cloned from a GitHub comment, the Claude Code
binary `mise` installed last week, the MCP server that stopped authenticating,
or the gigabytes of package cache you have been carrying since install.

Those live in six different places with six different update mechanisms and no
single thing that looks at all of them. Almanac is that thing.

It is also opinionated about one thing in particular: **provenance**. Before
you install something from a marketplace, Almanac tells you the repository the
code actually comes from — which is not the same as the author name in its
manifest — and whether what you are about to install is still the commit that
was reviewed.

---

## What it does

### Featured — what needs you today

<img src="docs/screenshots/featured.png" alt="Featured" width="820">

The landing page, assembled from live data rather than an editorial list that
someone has to keep writing:

| Shelf | What it is |
|---|---|
| **Needs you** | Security updates, failed services, caches worth clearing, AUR packages that lost their maintainer. Worst first. Empty is the good outcome. |
| **New in the marketplace** | Listed in the last three weeks, filtered to what is reviewed *and* installable — "new" plus "unverified" is the worst thing to put in front of someone. |
| **Trending** | Worked on in the last week, ranked by stars, capped so no single marketplace can own the shelf. |
| **Because of what you run** | A set intersection over the tags you already have most of. No model, no profile, nothing leaves the machine. |

The hero is your own Omarchy wallpaper, so it re-skins with your theme and
downloads nothing.

### App Store — plugins, apps and themes, with provenance

<img src="docs/screenshots/app-store.png" alt="App Store" width="820">

Three catalogues in one grid, each entry carrying where it actually came from:

- **[Omarchy Plugins](https://plugins.omarchy.org)** — the official marketplace,
  read from the same `catalog.json` the website's own front end uses, so the
  cards here carry the real artwork, descriptions, stars and install commands.
- **Omarchy Themes** — 345 community themes from the `omarchy-theme` topic,
  with its own sidebar entry. Install clones it; applying it is a separate,
  explicit second step, because nobody wants their whole desktop re-skinned by
  a curious click.
- **[ArtCraft](https://getartcraft.com)** — the Craft apps, installed as
  AppImages and tracked for updates afterwards.

Every card shows a **provenance chip**:

| Chip | Meaning |
|---|---|
| `Official` | Ships with Omarchy itself |
| `First-party` | Published by the marketplace's own GitHub account — *checked*, not assumed |
| `Reviewed` | A human reviewed the source |
| `Verified` | Automated checks passed against the listed commit |
| `Changed since review` | Upstream has committed past what was reviewed — **1,336 listings are in this state** |
| `Unverified` | No review recorded |
| `Source unreachable` | The marketplace could not reach the repository on its last check |

Sort and filter by provenance, publisher, marketplace, category or stars.

### Themes — what the theme actually looks like

<img src="docs/screenshots/themes.png" alt="Themes" width="820">

GitHub's social card shows a repository, which tells you nothing about the only
question you have. Every Omarchy theme publishes `colors.toml`, so Almanac
fetches it — from `raw.githubusercontent.com`, which costs no API quota — and
draws a miniature desktop in the theme's own colours: its bar, its window, its
text, its accent, and the palette your terminal will use.

Stock themes that ship with Omarchy are marked `Official` and offer **Apply**;
community ones you installed also offer **Remove**. Same card, same slots, same
vocabulary as every other listing.

### Updates — one list for everything that updates

<img src="docs/screenshots/updates.png" alt="Updates" width="820">

Repo, AUR, Flatpak, AppImage, local git checkouts, version-managed developer
tools, AI agent CLIs and Almanac itself — with versions-behind counts, security
flagging, and per-row actions. Vendor logos are resolved from each package's own
upstream URL, which `pacman` already records.

| Source | Where it comes from | Updated with |
|---|---|---|
| Repositories | `pacman` + `checkupdates` | `pacman` |
| AUR | `yay -Qua` | `yay`, building locally |
| Omarchy | the `omarchy` packages themselves | `omarchy update` |
| Flatpak | `flatpak` | `flatpak` |
| AppImages | the `.upd_info` each AppImage embeds | downloaded in place, old build kept |
| Local builds | git checkouts under `~/.config/omarchy/plugins` | `git pull --ff-only` |
| Dev tools | `mise ls` — `gh`, `uv`, `node` and friends | `mise upgrade` |
| AI agents | `mise`, or each agent's own updater | whichever owns it |
| Almanac | its own checkout | `git pull --ff-only` |

How a thing updates is **detected, not assumed**. A shimmed `mise` binary
resolves to `/usr/bin/mise`, which looks nothing like a mise install path — so
asking the filesystem gets it wrong, and asking `mise` gets it right.

**What you just updated leaves the list on the next scan.** Only the upstream
side of a check is cached — `mise outdated`, `git fetch`, GitHub releases —
because that is the part that costs a network round trip. What is installed is
read live every time: `mise ls` takes milliseconds, and a commit count against
already-fetched refs never touches the network. **Update all** rescans by
itself when its terminal closes.

A repository update can be visible before it is installable. `checkupdates`
syncs a private copy of the package database, so it sees releases the system
copy has not heard of yet; installing one of those on its own would mean
`pacman -Sy` for a single package, the partial upgrade Arch warns against.
For those rows, **Update** says so and offers **Update all** instead of running
a command that would skip the package and report success.

Versions-behind counts from the first component that actually differs, and
calendar versions are not counted at all: `20181225 → 20261002` is a date, not
79,777 releases.

Install and removal run **inside the app**. Removal still shows you the full
`pacman -Rs` preview, including the dependencies it would take with it, before
anything happens — the confirmation moved somewhere readable, it was not
dropped.

### AI — the estate no package manager tracks

<img src="docs/screenshots/ai.png" alt="AI" width="820">

Agent CLIs (Claude Code, Codex, Gemini, opencode, Copilot, Cursor, Crush,
Goose, Aider, Qwen, Amp, Droid), MCP servers with live health, skills, plugins,
marketplaces and hooks — with each vendor's own mark, fetched from that vendor.

How each agent updates is **detected, not assumed**: a `mise`-managed binary
updates through `mise`, one with its own `update` subcommand uses that, and one
with neither says so instead of pretending.

### Health check — what six gigabytes of nothing looks like

<img src="docs/screenshots/health.png" alt="Health check" width="820">

Sixteen read-only checks, each offering the single command that acts on what it
found: package cache, journal size, AUR build cache, orphans (with build
tooling deliberately kept), unmerged `.pacnew` files, failed services, broken
launcher entries, dead symlinks, AppImage rollback copies, AUR packages that
vanished upstream, package file integrity, session logs, trash, disk
headroom, settings reset to defaults, and whether the last update can be
undone.

A cleanup is judged by what it would free, not by how big the thing is. A
trimmed package cache is still gigabytes — two versions of everything
installed is what makes a downgrade possible — so it is rated on paccache's own
dry run, and clears once there is nothing left to trim.

---

## What it watches for

### Every action is checked, not assumed

An exit status says a command ran, not that it did what was asked. `yay -S
--needed` skips a package the system database does not know is newer;
`journalctl --vacuum-size` deletes nothing without root; `paccache` can have
nothing left to trim. Each exits 0. So every update, install and cleanup is
bracketed by the same question asked before and after — *which version is
installed? what does this check find?* — and you are told the difference.
"The update ran, but 26.04.5 is still installed" is a result, and it is shown
as one.

### Settings put back to their defaults

Some repair commands replace a config file with the shipped default and keep
yours as `<file>.bak.<time>` — without saying so. Almanac recognises that from
the files alone (the file now matches the default, the backup does not),
notices it the moment it happens while the window is open, says what was lost
("9 bar widgets removed"), and restores your version in one click. The
defaults it replaces are kept, so the restore can be undone too.

### Updates you can undo

On systems with [Snapper](http://snapper.io), every package Almanac installs
or updates is snapshotted first, and the Health check says whether your last
update — however it was run — can be rolled back. Booted into a snapshot from
the boot menu, it offers to make that snapshot permanent. Snapshot listings
are root-only, so this is read from pacman's log and the system journal
rather than from the snapshots themselves.

### Files an install took over

Installing an app can quietly make it what opens your `.docx` or `.pdf`
files. Almanac reads every file type's default app before and after each
install, and if anything changed hands it says so — *"WordCraft now opens
.docx, .odt — before: LibreOffice Writer"* — and can give them back, through
the standard `xdg-mime`.

### Install from a link

Paste a GitHub link (☰ → Install from a link, or straight into the search
box). Almanac lays out every way the project reaches your machine — the
official repositories, the project's own AppImage, AUR packages that build
from source or repackage a binary — and recommends one, with the reasons and
the caveats. An AppImage is checked against the SHA-256 GitHub publishes for
it before it is made runnable; an AUR package whose upstream is a different
project is flagged.

### Major releases and local changes

A new major version of an agent CLI is held back from "update all" and from
scheduled runs, and shown with its release notes when you update it on its
own. A plugin or checkout you have patched is weighed against the update
before pulling: if the update already contains your change it is dropped, if
it still applies it is carried forward, and if the update rewrote the same
lines your change is set aside as a git stash — never discarded.

---

## Install

**As a package** (recommended — Almanac then appears in its own Updates list
as a package, and defers its updates to `pacman`). It is not on the AUR yet;
the `PKGBUILD` in this repository builds it, and runs the tests while doing so:

```bash
git clone https://github.com/twodxme/almanac && cd almanac
makepkg -si
```

**From source:**

```bash
git clone https://github.com/twodxme/almanac ~/.local/share/almanac
cd ~/.local/share/almanac
ln -sf "$PWD/almanac" ~/.local/bin/almanac
install -Dm644 docs/almanac.desktop ~/.local/share/applications/almanac.desktop
install -Dm644 docs/io.github.twodxme.Almanac.svg \
  ~/.local/share/icons/hicolor/scalable/apps/io.github.twodxme.Almanac.svg
gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor
```

Then run `almanac`, or launch it from your application menu.

Runtime data lives in `~/.local/state/almanac/`, never inside the checkout, so
Almanac can update itself with a fast-forward pull without fighting its own
state.

### Updating

Almanac appears in its own Updates list. **☰ → Update Almanac…** does a
`git pull --ff-only` and offers to restart. It cannot overwrite local work: if
you have uncommitted changes, the pull refuses and says so rather than creating
a conflict inside a running application.

---

## Requirements

| | |
|---|---|
| **Required** | Python 3.11+, GTK 4, libadwaita 1.9+ |
| **Full experience** | Arch or Omarchy — `pacman`, `yay`, `checkupdates` |
| **Optional** | `omarchy` (plugins, themes, system updates) · `flatpak` · `mise` · `gh` · `snapper` (undoable updates) · `xdg-utils` · `xdg-terminal-exec` |

`gh` is worth having: Almanac uses your existing `gh auth token` to raise the
GitHub API ceiling from 60 requests an hour to 5,000. It is never written to
disk and never logged.

Almanac degrades rather than breaks. Without `omarchy` you lose the plugin and
theme catalogues, and full upgrades run through `yay`, `paru` or `pacman`
instead; without `flatpak` that source simply does not appear; without
`snapper` updates are not snapshotted. Nothing is required for the app to open
and be useful.

It follows the desktop's conventions rather than assuming one desktop: base
directories come from the XDG Base Directory variables, a terminal is opened
with `xdg-terminal-exec` (or `$TERMINAL`, or whichever known terminal is
installed), file-type choices go through `xdg-mime`, and anything needing root
asks through polkit.

---

## Theming

Almanac wears whatever Omarchy is wearing. It reads
`~/.local/state/omarchy/current/theme/colors.toml` and restates it as
libadwaita's named colours, so every widget follows — including ones added
later, because nothing is themed individually.

`omarchy theme set <name>` re-skins the window within a frame or two; the theme
is watched, not sampled at startup. On a machine without Omarchy, your GTK
theme shows through unmodified.

---

## Keyboard

| | |
|---|---|
| <kbd>Ctrl</kbd>+<kbd>1</kbd>…<kbd>7</kbd> | Jump to a destination |
| <kbd>Ctrl</kbd>+<kbd>F</kbd> | Search |
| <kbd>Esc</kbd> | Clear search |
| <kbd>Ctrl</kbd>+<kbd>R</kbd> / <kbd>F5</kbd> | Refresh the current page |
| <kbd>Ctrl</kbd>+<kbd>U</kbd> | Update all |
| <kbd>Ctrl</kbd>+<kbd>W</kbd> | Close |

`almanac --page store|updates|ai|health|featured` opens straight to a page,
which makes it scriptable from a keybinding or a bar widget.

---

## What leaves your machine

Stated plainly, because an app that reads your whole system should be explicit
about this:

- **Marketplace catalogues** from `plugins.omarchy.org`, the GitHub API and
  `getartcraft.com`. Cached for six hours — or ten minutes, when GitHub did not
  answer for some entry, which is listed as *Not checked* rather than
  guessed at.
- **Vendor icons**, fetched from each vendor's own domain — `stripe.com`'s
  favicon comes from `stripe.com`. Deliberately *not* a favicon proxy, which
  would mean sending one request containing the full list of everything you
  have installed to a single third party.
- **GitHub release checks** for AppImages and agent CLIs, conditional on ETags
  so an unchanged release costs no quota.
- **Install from a link**: the repository and its latest release from the
  GitHub API, and the matching package names from the AUR's public API —
  only for the link you pasted, only when you ask.

No analytics, no telemetry, no accounts. Nothing is sent about what you have
installed; icon requests reveal only that *somebody* asked that host for its
favicon.

---

## Contributing

Issues and pull requests are welcome — bug reports, new sources, new health
checks, packaging for other distributions, or a theme that makes it look
better than I can.

```bash
python3 test_app.py
```

118 tests, weighted toward the destructive paths — removal, orphan cleanup,
AppImage replacement — because those are the ones that cannot be undone by
clicking again. If you touch removal, orphan cleanup or install, the tests that
assert *nothing runs before confirmation* must still pass.

### Translating

Interface text is marked for GNU gettext and collected in
[`po/almanac.pot`](po/almanac.pot). To add a language:

```bash
msginit -i po/almanac.pot -o po/<lang>.po -l <lang>   # e.g. de, pt_BR
# translate po/<lang>.po, then:
po/update.sh                                            # merges and compiles
```

Run from a checkout, Almanac picks up `./locale` straight away; the package
build installs every `po/*.po` into the system locale directory. The window,
menus, dialogs and Health check headings are translatable today; sentences
built around a value (versions, counts, names) are still being converted, and
pull requests that rewrite them as whole translatable sentences are welcome.

There is a measured [UI/UX audit and gap analysis](docs/audit.md) covering the
conventions the interface follows and the gaps still open. New UI should match
what is already there: one primary action in the header, toasts for events, the
header subtitle for standing state, full-width boxed lists on secondary pages.

---

## Credits

Built for [**Omarchy**](https://omarchy.org), [DHH](https://github.com/dhh)'s
opinionated Arch + Hyprland distribution. Almanac exists because Omarchy made a
desktop worth curating — and because a system that good deserves to know what
is on it. The `omarchy` CLI does the real work behind every plugin, theme and
system action here; this is a window onto it.

- [Omarchy Plugin Marketplace](https://github.com/omacom/omarchy-plugin-marketplace)
  for the catalogue, the verification pipeline, and for publishing it in a form
  anyone can read.
- [ArtCraft](https://getartcraft.com) for the Craft apps.
- Every theme author in the `omarchy-theme` topic.

## License

[MIT](LICENSE).

---

## Screenshots

Every screenshot in this README is generated with `ALMANAC_DEMO=1`, which
replaces the local inventory with the fixtures in `demo.py`. The marketplace
catalogues stay real — those are public listings — but nothing in an image
comes from the machine that took it.

```bash
ALMANAC_DEMO=1 almanac --page featured
```
