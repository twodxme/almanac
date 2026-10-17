# Almanac

A catalogue of everything installed on this machine: where each thing came
from, how old it is, whether it still has a maintainer, and what needs
attention. Built for [Omarchy](https://omarchy.org/) and Arch.

It covers what no single package manager does:

- **Repo, AUR, Flatpak, AppImage and local git builds** in one list, with
  versions-behind counts and per-item actions.
- **An app store** backed by the official
  [Omarchy plugin marketplace](https://plugins.omarchy.org/) and ArtCraft,
  with provenance on every card — the repository the code actually comes
  from, and whether what you would install is still what was reviewed.
- **AI tooling**: agent CLIs, MCP servers, skills, plugins, marketplaces and
  hooks, none of which any package manager tracks.
- **System health**: orphans, package cache, journal size, failed units,
  unmerged configs, broken launchers and dead symlinks, each with the one
  command that acts on it.

Everything that installs or removes runs inside the app, with the full list
shown before anything destructive happens.

## Install

```bash
git clone https://github.com/twodxme/almanac ~/.local/share/almanac
ln -s ~/.local/share/almanac/almanac ~/.local/bin/almanac
```

Runtime data lives in `~/.local/state/almanac/`, never in the checkout, so
Almanac can update itself with a fast-forward pull.

## Requirements

Python 3, GTK 4, libadwaita 1.9+, and `pacman`. `gh` is optional but raises
the GitHub API ceiling from 60 requests an hour to 5,000.

## Tests

```bash
python3 test_app.py
```

The suite is weighted toward the destructive paths — removal, orphan cleanup,
AppImage replacement — because those are the ones that cannot be undone by
clicking again.
