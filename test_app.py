#!/usr/bin/env python3
"""Tests for almanac.

Weighted toward the destructive paths -- removal, orphan cleanup, AppImage
replacement -- because those are the ones that cannot be undone by clicking
again, and the ones that had never been executed before this file existed.

Run: python3 test_app.py
"""
from __future__ import annotations

import json
import re
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).parent))

import appimage  # noqa: E402
import inventory  # noqa: E402
import store  # noqa: E402


class VersionsBehind(unittest.TestCase):
    def test_counts_first_differing_level_only(self):
        # 1.2.9 -> 3.0.0 is two majors; the lower components restart, so
        # differencing them as well would be meaningless.
        self.assertEqual(inventory.versions_behind("1.2.9", "3.0.0"), (2, "major"))
        self.assertEqual(inventory.versions_behind("0.6.0", "0.12.0"), (6, "minor"))
        self.assertEqual(inventory.versions_behind("1.2.3", "1.2.9"), (6, "patch"))

    def test_pkgrel_bump_is_a_rebuild_not_a_release(self):
        self.assertEqual(inventory.versions_behind("0.29.0-1", "0.29.0-2"), (0, "rel"))

    def test_epoch_is_ignored(self):
        # "1:1.2.96" -- the epoch is packaging metadata, not an upstream version.
        n, kind = inventory.versions_behind("1:1.2.96.518-2", "1:1.3.4.258-1")
        self.assertEqual((n, kind), (1, "minor"))

    def test_date_stamped_versions_are_not_counted_as_releases(self):
        # 20181225 -> 20261002 differenced as a release count gives 79,777,
        # which is a fact about calendars, not about software.
        self.assertEqual(
            inventory.versions_behind("20181225-1", "20261002.r63-3"), (0, ""))
        self.assertEqual(
            inventory.versions_behind("2026.10.16", "2026.10.17"), (0, ""))

    def test_unparseable_is_silent_rather_than_wrong(self):
        self.assertEqual(inventory.versions_behind("", "1.0"), (0, ""))
        self.assertEqual(inventory.versions_behind("git-abc123", "git-def456"), (0, ""))


class OrphanAdvice(unittest.TestCase):
    def test_build_tooling_is_kept(self):
        # Removing rust just means downloading 300MB again on the next AUR build.
        for name in ("rust", "python-build", "python-installer", "cmake"):
            self.assertEqual(inventory.orphan_advice(name), "keep", name)

    def test_debug_and_docs_are_kept(self):
        self.assertEqual(inventory.orphan_advice("logiops-debug"), "keep")
        self.assertEqual(inventory.orphan_advice("foo-docs"), "keep")

    def test_ordinary_orphan_is_removable(self):
        self.assertEqual(inventory.orphan_advice("libfoo"), "remove")


class Criticality(unittest.TestCase):
    def test_network_facing_is_security(self):
        self.assertEqual(inventory.criticality("google-chrome"), "security")
        self.assertEqual(inventory.criticality("openssh"), "security")

    def test_bin_suffix_is_stripped_before_matching(self):
        self.assertEqual(inventory.criticality("nordvpn-bin"), "security")

    def test_boot_critical_is_core(self):
        self.assertEqual(inventory.criticality("linux"), "core")
        self.assertEqual(inventory.criticality("mkinitcpio"), "core")

    def test_ordinary_package_is_neither(self):
        self.assertEqual(inventory.criticality("bat"), "")


class Duplicates(unittest.TestCase):
    def _app(self, name, source):
        return inventory.App(name=name, version="1", source=source)

    def test_bin_and_source_build_are_one_app(self):
        apps = [self._app("cursor", inventory.REPO),
                self._app("cursor-bin", inventory.AUR)]
        self.assertIn("cursor", inventory.duplicates(apps))

    def test_orphans_are_not_counted_as_duplicates(self):
        # An orphaned library is not a second copy of anything.
        apps = [self._app("foo", inventory.REPO),
                self._app("foo", inventory.ORPHAN)]
        self.assertEqual(inventory.duplicates(apps), {})

    def test_distinct_software_is_not_flagged(self):
        apps = [self._app("firefox", inventory.REPO),
                self._app("chromium", inventory.REPO)]
        self.assertEqual(inventory.duplicates(apps), {})


class AppImageUpdateInfo(unittest.TestCase):
    def test_parses_github_release_spec(self):
        spec = appimage.parse_update_info(
            "gh-releases-zsync|storytold|vectorcraft|latest|"
            "vectorcraft-*-linux-x86_64.AppImage.zsync")
        self.assertEqual(spec["kind"], "github")
        self.assertEqual(spec["owner"], "storytold")
        self.assertEqual(spec["repo"], "vectorcraft")

    def test_unknown_scheme_yields_nothing(self):
        # zsync-over-http needs a zsync client we do not have; claiming support
        # would mean offering an update that cannot be applied.
        self.assertEqual(appimage.parse_update_info("zsync|https://example/x.zsync"), {})
        self.assertEqual(appimage.parse_update_info(""), {})

    def test_version_ordering_is_numeric(self):
        # The bug a string compare would introduce: 0.9.9 > 0.10.2 as text.
        self.assertLess(appimage._vkey("0.9.9"), appimage._vkey("0.10.2"))

    def test_same_or_older_release_is_not_an_update(self):
        with mock.patch.object(appimage, "latest_github",
                               return_value={"version": "1.0.0", "url": "u"}):
            rec = {"path": __file__, "version": "1.0.0",
                   "update_spec": {"kind": "github", "owner": "o", "repo": "r"}}
            self.assertEqual(appimage.check(rec), {})
            rec["version"] = "2.0.0"
            self.assertEqual(appimage.check(rec), {})

    def test_newer_release_is_reported(self):
        with mock.patch.object(appimage, "latest_github",
                               return_value={"version": "2.0.0", "url": "u"}):
            rec = {"path": __file__, "version": "1.0.0",
                   "update_spec": {"kind": "github", "owner": "o", "repo": "r"}}
            self.assertEqual(appimage.check(rec).get("version"), "2.0.0")


class AppImageDownloadSafety(unittest.TestCase):
    """The replacement must never leave the user without a working binary."""

    def test_previous_build_is_kept_for_rollback(self):
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "app.AppImage"
            dest.write_bytes(b"OLD")

            class FakeResponse:
                headers = {"Content-Length": "3"}
                def __init__(self): self._data = [b"NEW"]
                def read(self, _n): return self._data.pop(0) if self._data else b""
                def __enter__(self): return self
                def __exit__(self, *a): return False

            with mock.patch.object(appimage.urllib.request, "urlopen",
                                   return_value=FakeResponse()):
                appimage.download("https://example/x", dest)

            self.assertEqual(dest.read_bytes(), b"NEW")
            self.assertEqual(dest.with_suffix(".previous").read_bytes(), b"OLD",
                             "the old build must survive so a bad update is revertible")

    def test_failed_download_leaves_the_original_intact(self):
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "app.AppImage"
            dest.write_bytes(b"OLD")
            with mock.patch.object(appimage.urllib.request, "urlopen",
                                   side_effect=OSError("network died")):
                with self.assertRaises(OSError):
                    appimage.download("https://example/x", dest)
            self.assertEqual(dest.read_bytes(), b"OLD",
                             "a dropped connection must not destroy the installed build")


class RegistryWrites(unittest.TestCase):
    def test_save_is_atomic(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "appimages.json"
            with mock.patch.object(inventory, "REGISTRY", target):
                inventory.save_registry({"a": {"name": "a"}})
                self.assertEqual(json.loads(target.read_text())["a"]["name"], "a")
                # The temp file used for the swap must not be left behind.
                self.assertEqual(list(Path(tmp).glob("*.tmp")), [])

    def test_corrupt_registry_does_not_raise(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "appimages.json"
            target.write_text("{ this is not json")
            with mock.patch.object(inventory, "REGISTRY", target):
                self.assertEqual(inventory._load_registry(), {},
                                 "a damaged registry must not take the window down")


class _Stub:
    """Stand-in for the window.

    GObject subclasses reject object.__new__, and constructing a real window
    needs a display. The destructive methods only touch a handful of
    attributes, so calling them unbound against this exercises the real logic
    without a GTK main loop.

    The two feedback methods are here rather than in each test because every
    code path can report something, and a missing mock would fail a test for
    a reason that has nothing to do with what it is checking.
    """

    def __init__(self):
        self._say = mock.Mock()
        self._summarize = mock.Mock()
        self.reload = mock.Mock()
        self._store_refresh = mock.Mock()
        self._plugins = []


class _Confirmed:
    """Stands in for tasks.confirm/tasks.run.

    Removal no longer happens in a terminal where pacman asks y/N; it happens
    in-process after a GTK confirmation that shows the full list. The property
    being protected is the same -- nothing is removed until a human has seen
    what would go -- so the tests assert the ordering, not the old transport.
    """

    def __init__(self):
        self.confirm_args = None
        self.ran = []
        self._on_yes = None

    def confirm(self, _parent, heading, body, detail, action, destructive, on_yes):
        self.confirm_args = dict(heading=heading, body=body, detail=detail,
                                 action=action, destructive=destructive)
        self._on_yes = on_yes

    def run(self, _parent, _title, steps, _done=None):
        self.ran.extend(steps)

    def say_yes(self):
        assert self._on_yes is not None, "nothing asked for confirmation"
        self._on_yes()

    @property
    def commands(self):
        out = []
        for _label, argv in self.ran:
            out.append(" ".join(argv) if isinstance(argv, list) else "<callable>")
        return out


class RemovalCommands(unittest.TestCase):
    """Verify what the destructive actions would run, without running them."""

    def test_package_removal_shows_the_list_before_removing_anything(self):
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        win._after_pacman = mock.Mock()
        pkg = inventory.App(name="somepkg", version="1", source=inventory.REPO)

        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run), \
             mock.patch.object(app_mod.tasks, "preview_removal",
                               return_value=(True, "somepkg 1\nleftover-dep 2")):
            app_mod.AlmanacWindow.on_remove(win, None, pkg)

            self.assertIsNotNone(fake.confirm_args, "removal must ask first")
            self.assertEqual(fake.ran, [],
                             "nothing may run before the user confirms")
            # The preview is the whole point: -Rns takes dependencies too, and
            # the user has to be able to see which ones.
            self.assertIn("leftover-dep", fake.confirm_args["detail"])
            self.assertTrue(fake.confirm_args["destructive"])

            fake.say_yes()
            self.assertIn("pacman -Rns --noconfirm somepkg", fake.commands)

    def test_removal_is_abandoned_when_the_preview_fails(self):
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        pkg = inventory.App(name="somepkg", version="1", source=inventory.REPO)

        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run), \
             mock.patch.object(app_mod.tasks, "preview_removal",
                               return_value=(False, "target not found")):
            app_mod.AlmanacWindow.on_remove(win, None, pkg)

        self.assertIsNone(fake.confirm_args,
                          "must not offer to remove what it cannot describe")
        self.assertEqual(fake.ran, [])

    def test_orphan_cleanup_excludes_the_keep_list(self):
        import app as app_mod
        fake = _Confirmed()

        class FakeStore:
            def __init__(self, items): self.items = items
            def get_n_items(self): return len(self.items)
            def get_item(self, i): return self.items[i]

        class FakeItem:
            def __init__(self, name, advice):
                self.name, self.advice = name, advice
                self.source = inventory.ORPHAN

        win = _Stub()
        win.store = FakeStore([FakeItem("rust", "keep"), FakeItem("libfoo", "remove")])
        win._after_pacman = mock.Mock()

        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run), \
             mock.patch.object(app_mod.tasks, "preview_removal",
                               return_value=(True, "libfoo 1")) as prev:
            app_mod.AlmanacWindow.on_clean_orphans(win, None)
            # rust must not even reach the preview, let alone the removal.
            self.assertEqual(prev.call_args.args[0], ["libfoo"])
            fake.say_yes()

        joined = " ".join(fake.commands)
        self.assertIn("libfoo", joined)
        self.assertNotIn("rust", joined,
                         "build tooling must not be swept up by a cleanup")

    def test_plugin_install_warns_that_plugins_are_unsandboxed(self):
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        win._store_after = mock.Mock()
        plugin = store.Plugin(
            id="x.y", name="Thing", repo="https://github.com/someone/thing",
            install_cmd="omarchy plugin add https://github.com/someone/thing --enable")

        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run):
            app_mod.AlmanacWindow.on_plugin_install(win, None, plugin)
            self.assertIn("sandbox", fake.confirm_args["body"].lower())
            # Provenance has to be in front of the decision, not a click away.
            self.assertIn("github.com/someone/thing", fake.confirm_args["detail"])
            fake.say_yes()

        self.assertIn("--yes", " ".join(fake.commands),
                      "the prompt moved to the dialog, so the CLI must not "
                      "block waiting for one")

    def test_appimage_removal_deletes_only_its_own_files(self):
        import app as app_mod
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "registry.json"
            live = Path(tmp) / "thing.AppImage"
            prev = Path(tmp) / "thing.previous"
            other = Path(tmp) / "unrelated.AppImage"
            for f in (live, prev, other):
                f.write_bytes(b"x")

            with mock.patch.object(inventory, "REGISTRY", target):
                inventory.save_registry({"thing": {"name": "thing", "path": str(live)}})
                win = _Stub()
                win.reload = mock.Mock()
                entry = inventory.App(name="thing", version="1",
                                      source=inventory.APPIMAGE,
                                      exec_path=str(live), extra={"key": "thing"})
                with mock.patch.object(app_mod.appimage_mod, "remove_desktop_entry"):
                    app_mod.AlmanacWindow._remove_appimage(win, entry)

                self.assertFalse(live.exists())
                self.assertFalse(prev.exists())
                self.assertTrue(other.exists(), "removal must not touch other files")
                self.assertEqual(inventory._load_registry(), {})


class EmptyStates(unittest.TestCase):
    """A blank screen is indistinguishable from a broken one.

    Each category's empty state has to say which kind of empty it is --
    "nothing to do" reads completely differently from "nothing matched" --
    so the mapping is asserted rather than left to whoever edits it next.
    """

    def test_every_source_category_explains_itself(self):
        import app as app_mod
        states = app_mod.AlmanacWindow.EMPTY_STATES
        for key in (inventory.ORPHAN, inventory.FLATPAK, inventory.APPIMAGE,
                    inventory.LOCAL, inventory.AUR, "updates"):
            self.assertIn(key, states, f"{key} has no empty state")
            icon, title, body = states[key]
            self.assertTrue(icon and title and body,
                            f"{key}'s empty state is incomplete")
            self.assertGreater(len(body), 20,
                               f"{key}'s empty state does not explain anything")

    def test_an_empty_updates_list_is_good_news_not_an_error(self):
        import app as app_mod
        icon, title, body = app_mod.AlmanacWindow.EMPTY_STATES["updates"]
        self.assertIn("ok", icon, "a finished state should not look like a fault")
        self.assertNotIn("error", icon)
        self.assertIn("up to date", title.lower())

    def test_orphan_empty_state_does_not_imply_something_is_wrong(self):
        import app as app_mod
        _icon, title, body = app_mod.AlmanacWindow.EMPTY_STATES[inventory.ORPHAN]
        self.assertNotIn("error", (title + body).lower())
        self.assertNotIn("fail", (title + body).lower())


class ThemeActions(unittest.TestCase):
    """Themes install, apply and remove with their own commands."""

    @staticmethod
    def _wire(win, app_mod, *names):
        """Bind the real methods the code under test dispatches into."""
        for name in names:
            setattr(win, name,
                    getattr(app_mod.AlmanacWindow, name).__get__(win))

    def test_removal_uses_the_theme_command_not_the_plugin_one(self):
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        self._wire(win, app_mod, "_remove_theme", "_store_after")
        theme = store.Plugin(id="theme.x.y", name="Lumon",
                             repo="https://github.com/x/omarchy-lumon-theme",
                             install_kind="theme", installed=True)

        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run):
            app_mod.AlmanacWindow.on_plugin_remove(win, None, theme)
            fake.say_yes()

        joined = " ".join(fake.commands)
        self.assertIn("omarchy theme remove Lumon", joined)
        self.assertNotIn("plugin remove", joined,
                         "a theme is not a plugin and the CLI knows it")

    def test_a_theme_shipped_with_omarchy_is_not_removable(self):
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        self._wire(win, app_mod, "_remove_theme", "_store_after")
        stock = store.Plugin(id="theme.o.s", name="Solitude",
                             repo="https://github.com/x/omarchy-solitude-theme",
                             install_kind="theme", installed=True,
                             built_in=True)

        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run):
            app_mod.AlmanacWindow.on_plugin_remove(win, None, stock)

        self.assertIsNone(fake.confirm_args,
                          "must not offer to remove what omarchy ships")
        self.assertEqual(fake.ran, [])
        win._say.assert_called()

    def test_installing_a_theme_does_not_also_apply_it(self):
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        self._wire(win, app_mod, "_install_theme", "_theme_installed",
                   "_apply_theme_now")
        theme = store.Plugin(
            id="theme.x.y", name="Lumon",
            repo="https://github.com/x/omarchy-lumon-theme",
            install_kind="theme", installable=True,
            install_cmd="omarchy theme install https://github.com/x/omarchy-lumon-theme")

        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run):
            app_mod.AlmanacWindow.on_plugin_install(win, None, theme)
            fake.say_yes()

        joined = " ".join(fake.commands)
        self.assertIn("theme install", joined)
        self.assertNotIn("theme set", joined,
                         "installing must not re-skin the desktop on its own")


class ShelfShape(unittest.TestCase):
    """Featured and the AI page carry differently-shaped collections.

    Featured yields Shelf objects; the AI survey yields
    (title, description, entries) tuples. Unpacking one as the other raised
    only at runtime, on a page that had already rendered.
    """

    def test_featured_build_returns_shelf_objects(self):
        import featured
        shelves = featured.build([], [], [])
        self.assertTrue(shelves)
        for shelf in shelves:
            self.assertTrue(hasattr(shelf, "items"),
                            "a shelf must expose .items, not unpack as a tuple")
            self.assertTrue(hasattr(shelf, "title"))
            self.assertIsInstance(shelf.items, list)

    def test_ai_survey_returns_triples(self):
        import ai
        for group in ai.group_specs():
            self.assertEqual(len(group), 3,
                             "the AI page unpacks these as (title, desc, loader)")


class DiskChecks(unittest.TestCase):
    """Checking only / misses how a machine actually runs out of room.

    A full XDG runtime directory -- 1.6 GB of tmpfs that one runaway
    compositor log can fill -- stops system updates with "No space left on
    device" while `df /` still reports hundreds of gigabytes free.
    """

    def test_disk_check_looks_beyond_the_root_filesystem(self):
        import maintenance
        f = maintenance.disk_space()
        self.assertIn("/", f.detail)
        import os
        runtime = os.environ.get("XDG_RUNTIME_DIR") or f"/run/user/{os.getuid()}"
        if os.path.isdir(runtime):
            self.assertIn(runtime, f.detail + " ".join(f.items),
                          "the runtime directory must be watched")

    def test_a_nearly_full_small_tmpfs_is_critical(self):
        import maintenance
        # A 1.6 GB tmpfs with 40 MB left is 97% used: judged on headroom,
        # not on percentage alone, because small filesystems fill fast.
        with mock.patch.object(maintenance, "_usage") as usage:
            usage.return_value = (40, 1600, 97.5)
            f = maintenance.disk_space()
        self.assertEqual(f.severity, maintenance.CRIT)

    def test_runtime_log_cleanup_is_offered_only_when_there_is_something(self):
        import maintenance
        f = maintenance.runtime_logs()
        if f.size_mb < 1:
            self.assertIsNone(f.fix, "nothing to clear, so nothing offered")
        else:
            self.assertTrue(f.fix_label)


class UpdatesClearAfterUpdating(unittest.TestCase):
    """Something just updated must leave the updates list on the next scan.

    Both of these were cached as a whole -- the installed side along with the
    upstream side -- so after updating everything, ten rows stayed listed for
    hours: mise tools at versions already replaced, and plugins "901 commits
    behind" that had just been pulled.
    """

    def test_mise_compares_the_live_version_not_the_cached_one(self):
        import agents
        stale = {"uv": {"current": "0.12.13", "latest": "0.13.1"}}
        with mock.patch.object(agents, "_mise_installed",
                               return_value={"uv": "0.13.1"}), \
             mock.patch.object(agents, "_mise_outdated", return_value=stale):
            [uv] = agents.mise_tools()
        self.assertEqual(uv.version, "0.13.1")
        self.assertFalse(uv.has_update)

    def test_commit_count_is_recounted_while_the_fetch_is_cached(self):
        import subprocess
        with tempfile.TemporaryDirectory() as d:
            def git(*a, cwd=d):
                subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t",
                                *a], cwd=cwd, check=True, capture_output=True)
            up, clone = Path(d, "up"), Path(d, "clone")
            git("init", "-q", "-b", "main", str(up))
            git("commit", "-q", "--allow-empty", "-m", "one", cwd=up)
            git("clone", "-q", str(up), str(clone))
            git("commit", "-q", "--allow-empty", "-m", "two", cwd=up)

            cache = {}
            self.assertEqual(inventory._git_behind(clone, cache)["behind"], 1)
            git("pull", "-q", "--ff-only", cwd=clone)
            # Inside the fetch window: no network, but the count is fresh.
            self.assertEqual(inventory._git_behind(clone, cache)["behind"], 0)

    def test_a_release_the_system_db_lacks_is_sent_to_update_all(self):
        # checkupdates saw 2.0 in a fresh private db; the system db still
        # offers 1.0, so `yay -S --needed` would skip it and report success.
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        win.on_update_all = mock.Mock()
        pkg = inventory.App(name="somepkg", version="1.0", source=inventory.REPO,
                            update="2.0")
        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run), \
             mock.patch.object(inventory, "synced_version", return_value="1.0"):
            app_mod.AlmanacWindow.on_update_one(win, None, pkg)
            fake.say_yes()
        self.assertEqual(fake.ran, [], "a no-op install must not be run")
        win.on_update_all.assert_called_once()


class CleanupsThatCannotHelp(unittest.TestCase):
    """A finding must clear once its fix has done everything it can."""

    def test_a_trimmed_package_cache_is_not_a_finding(self):
        import maintenance
        # 4.4 GB, but every byte is the two newest versions of something
        # installed: paccache has no candidates and the button would do nothing.
        with mock.patch.object(maintenance, "_du_mb", return_value=(4500, 522)), \
             mock.patch.object(maintenance, "_run",
                               return_value="==> no candidate packages found for pruning"):
            f = maintenance.pacman_cache()
        self.assertEqual(f.severity, maintenance.OK)
        self.assertFalse(f.actionable)

    def test_cache_severity_follows_what_a_trim_would_free(self):
        import maintenance
        dry = "==> finished dry run: 26 candidates (disk space saved: 1.23 GiB)"
        with mock.patch.object(maintenance, "_du_mb", return_value=(4500, 522)), \
             mock.patch.object(maintenance, "_run", return_value=dry):
            f = maintenance.pacman_cache()
        # Two dry runs (-k2 and -uk0), each 1.23 GiB here.
        self.assertAlmostEqual(f.size_mb, 2 * 1.23 * 1024, places=0)
        self.assertEqual(f.severity, maintenance.WARN)
        self.assertTrue(f.actionable)

    def test_journal_vacuum_runs_as_root(self):
        import tasks
        # As the user it deletes nothing, prints "freed 0B" and exits 0.
        self.assertEqual(tasks.elevate(["journalctl", "--vacuum-size=500M"])[0],
                         "pkexec")
        self.assertFalse(tasks.needs_root(["journalctl", "--disk-usage"]))


class PackageIntegrity(unittest.TestCase):
    """"Permission denied" and "missing" are not the same answer.

    Most of /etc and all of /boot is root-only, so an unprivileged check
    cannot see a large share of what it inspects. Counting those as faults
    reports a healthy system as broken -- which is exactly the conclusion a
    hand-run check reached on this machine before the distinction existed.
    """

    DENIED = ("warning: omarchy-settings: /etc/sudoers.d/omarchy-dns "
              "(Permission denied)\n"
              "warning: intel-ucode: /boot/intel-ucode.img (Permission denied)\n"
              "omarchy-settings: 643 total files, 4 missing files\n")
    GONE = ("warning: somepkg: /usr/share/somepkg/data.bin "
            "(No such file or directory)\n"
            "somepkg: 12 total files, 1 missing file\n")

    @staticmethod
    def _run_with(output):
        import maintenance
        class Result:
            stdout = output
            stderr = ""
        with mock.patch.object(maintenance.subprocess, "run",
                               return_value=Result()):
            return maintenance.package_integrity()

    def test_unreadable_files_are_not_faults(self):
        import maintenance
        f = self._run_with(self.DENIED)
        self.assertEqual(f.severity, maintenance.OK,
                         "permission denied is not a broken package")
        self.assertEqual(f.items, [])
        self.assertIn("without root", f.detail,
                      "the check must disclose what it could not inspect")

    def test_genuinely_missing_files_are_reported(self):
        import maintenance
        f = self._run_with(self.GONE)
        self.assertEqual(f.severity, maintenance.WARN)
        self.assertEqual(len(f.items), 1)
        self.assertIn("data.bin", f.items[0])

    def test_both_at_once_counts_them_separately(self):
        f = self._run_with(self.DENIED + self.GONE)
        self.assertEqual(len(f.items), 1, "only the real miss is flagged")
        self.assertIn("2 file(s) could not be read", f.detail)

    def test_stderr_is_read_too(self):
        # pacman writes the per-file warnings to stderr; reading only stdout
        # reports full coverage of a check that saw none of them.
        import maintenance
        class Split:
            stdout = "somepkg: 12 total files, 1 missing file\n"
            stderr = ("warning: somepkg: /usr/share/somepkg/data.bin "
                      "(No such file or directory)\n")
        with mock.patch.object(maintenance.subprocess, "run",
                               return_value=Split()):
            f = maintenance.package_integrity()
        self.assertEqual(f.severity, maintenance.WARN,
                         "the warning only exists on stderr")


class VisualSystem(unittest.TestCase):
    """The two conventions an interface drifts away from first.

    Spacing and badge styling are individually invisible and cumulatively
    obvious: before this, the window used twenty distinct spacing values and
    nine badge classes across three font sizes, each one locally sensible.
    These assert the system rather than any particular number.
    """

    SCALE = {2, 4, 6, 8, 12, 16, 20, 24}
    PILLS = {"pill", "pill-crit", "pill-warn", "pill-ok", "pill-accent",
             "pill-quiet"}

    @staticmethod
    def _source():
        return (Path(__file__).parent / "app.py").read_text()

    def test_spacing_comes_from_the_scale(self):
        offenders = {}
        for prop, value in re.findall(
                r"\b(margin_(?:start|end|top|bottom)|spacing|child_spacing"
                r"|line_spacing)=(\d+)\b", self._source()):
            if int(value) not in self.SCALE:
                offenders.setdefault(int(value), []).append(prop)
        self.assertEqual(
            offenders, {},
            f"spacing off the scale {sorted(self.SCALE)}: {offenders}")

    def test_badges_use_one_pill_system(self):
        retired = ("crit-pill", "stale-pill", "crit-core", "store-chip",
                   "store-installed", "trust-official", "trust-ok",
                   "trust-warn", "trust-none")
        src = self._source() + (Path(__file__).parent / "store.py").read_text()
        found = [name for name in retired if f'"{name}"' in src]
        self.assertEqual(found, [],
                         f"retired badge classes still referenced: {found}")

    def test_every_pill_class_is_defined_in_the_stylesheet(self):
        src = self._source()
        css = src.split('_CSS = b"""')[1].split('"""')[0]
        used = set(re.findall(r'"(pill(?:-[a-z]+)?)"', src))
        self.assertTrue(used, "no pill classes found at all")
        for name in used:
            self.assertIn(f".{name}", css,
                          f"{name} is applied but never styled")
        self.assertTrue(used <= self.PILLS,
                        f"unknown pill variants: {sorted(used - self.PILLS)}")

    def test_pills_are_all_one_size(self):
        css = self._source().split('_CSS = b"""')[1].split('"""')[0]
        block = re.search(
            r"\.pill,[^{]*\{([^}]*)\}", css)
        self.assertIsNotNone(block, "the shared pill rule is missing")
        self.assertIn("font-size", block.group(1),
                      "the pill size must be set once, on the shared rule")
        # No variant may override the size; that is how three sizes happened.
        for variant in ("pill-crit", "pill-warn", "pill-ok", "pill-accent",
                        "pill-quiet"):
            # Anchored: the shared selector ends in ".pill-quiet {" too,
            # and matching that would test the wrong rule.
            rule = re.search(r"(?m)^\." + variant + r"\s*\{([^}]*)\}", css)
            if rule:
                self.assertNotIn("font-size", rule.group(1),
                                 f".{variant} must not set its own size")


if __name__ == "__main__":
    unittest.main(verbosity=2)
