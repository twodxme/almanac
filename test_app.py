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


class CatalogueLookups(unittest.TestCase):
    """A release lookup that failed is not a release without a Linux build.

    One rate-limited refresh listed VectorCraft -- installed, from its Linux
    AppImage -- and three others as "No Linux build" for six hours.
    """

    def _rows(self, releases):
        import io
        import ghcache
        html = ('<a href="/apps/wordcraft">WordCraft</a>'
                '<a href="/apps/gridcraft">GridCraft</a>')
        asset = {"name": "gridcraft-0.7.0-linux-x86_64.AppImage",
                 "browser_download_url": "https://example.invalid/g.AppImage",
                 "size": 1048576}
        found = {"tag_name": "v0.7.0", "assets": [asset]}
        with tempfile.TemporaryDirectory() as d, \
             mock.patch.object(store, "CACHE", Path(d)), \
             mock.patch.object(store.urllib.request, "urlopen",
                               return_value=io.BytesIO(html.encode())), \
             mock.patch.object(ghcache, "org_repos", return_value=[]), \
             mock.patch.object(ghcache, "latest_release",
                               side_effect=lambda r: releases(r, found)):
            return {r["id"]: r for r in store._artcraft_rows()}

    def test_a_failed_lookup_says_not_checked(self):
        rows = self._rows(lambda r, found: found if "gridcraft" in r else None)
        self.assertEqual(rows["gridcraft"]["status"], "Available")
        self.assertEqual(rows["wordcraft"]["status"], "Not checked")
        self.assertTrue(rows["wordcraft"]["unchecked"])

    def test_every_row_field_loads_into_a_plugin(self):
        # Rows become Plugin(**row); a key the dataclass lacks broke the whole
        # store, which the row-level tests above could not see.
        rows = self._rows(lambda r, found: found if "gridcraft" in r else None)
        for row in rows.values():
            store.Plugin(**{**row, "market": "artcraft"})

    def test_a_release_without_an_appimage_still_says_so(self):
        bare = {"tag_name": "v1.0.0", "assets": []}
        rows = self._rows(lambda r, found: found if "gridcraft" in r else bare)
        self.assertEqual(rows["wordcraft"]["status"], "No Linux build")
        self.assertFalse(rows["wordcraft"]["unchecked"])

    def test_an_incomplete_catalogue_is_refetched_before_the_ttl(self):
        import os
        import time
        with tempfile.TemporaryDirectory() as d, \
             mock.patch.object(store, "CACHE", Path(d)):
            m = store.Market(id="t", name="T", site="", loader=lambda: [
                {"id": "wordcraft", "unchecked": False}])
            m.cache.write_text(json.dumps({"schema": store.SCHEMA, "rows": [
                {"id": "wordcraft", "unchecked": True}]}))
            past = time.time() - store.RETRY - 1
            os.utime(m.cache, (past, past))
            rows = store._load_market(m, force=False)
        self.assertFalse(rows[0]["unchecked"], "the stale miss must be retried")


def _git_repo(d, name="up"):
    """A tiny upstream with one file, and a clone of it."""
    import subprocess

    def git(*a, cwd):
        subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
                       cwd=cwd, check=True, capture_output=True)
    up = Path(d, name)
    git("init", "-q", "-b", "main", str(up), cwd=d)
    (up / "f.txt").write_text("one\ntwo\nthree\nfour\nfive\nsix\n")
    git("add", ".", cwd=up)
    git("commit", "-q", "-m", "first", cwd=up)
    clone = Path(d, name + "-clone")
    git("clone", "-q", str(up), str(clone), cwd=d)
    return up, clone, git


class ActionsAreVerified(unittest.TestCase):
    """A command that exits 0 and changes nothing is reported as that."""

    def test_unchanged_version_is_not_success(self):
        import verify
        app = inventory.App(name="yaru", version="1.0", source=inventory.REPO,
                            update="2.0")
        out = verify.judge_app(app, "1.0", "1.0")
        self.assertIs(out.changed, False)
        self.assertIn("did not change", out.message)

    def test_changed_version_is_success(self):
        import verify
        app = inventory.App(name="yaru", version="1.0", source=inventory.REPO,
                            update="2.0")
        self.assertTrue(verify.judge_app(app, "1.0", "2.0").ok)

    def test_an_unanswerable_probe_is_not_called_success(self):
        import verify
        app = inventory.App(name="x", version="1", source=inventory.REPO)
        self.assertIsNone(verify.judge_app(app, None, "2").changed)

    def test_full_upgrade_reports_what_is_still_waiting(self):
        import verify
        out = verify.judge_pending({"a", "b", "c"}, {"c"})
        self.assertTrue(out.ok)
        self.assertIn("c", out.message)
        self.assertIs(verify.judge_pending({"a"}, {"a"}).changed, False)

    def test_a_fix_that_freed_nothing_says_so(self):
        import maintenance as m
        import verify
        before = m.Finding(id="journal", title="Journal", detail="1 GB",
                           severity=m.WARN, size_mb=500, fix=["true"])
        after = m.Finding(id="journal", title="Journal", detail="1 GB",
                          severity=m.WARN, size_mb=500, fix=["true"])
        self.assertIs(verify.judge_finding(before, after).changed, False)
        fixed = m.Finding(id="journal", title="Journal", detail="ok",
                          severity=m.OK)
        self.assertTrue(verify.judge_finding(before, fixed).ok)

    def test_every_check_can_be_asked_again(self):
        import maintenance as m
        self.assertEqual(set(m.CHECK_BY_ID.values()), set(m.CHECKS))

    def test_package_update_takes_a_snapshot_first(self):
        import app as app_mod
        import snapshots
        fake = _Confirmed()
        win = _Stub()
        win._after_pacman = mock.Mock()
        win._snapshot_first = lambda steps, what: \
            app_mod.AlmanacWindow._snapshot_first(win, steps, what)
        win._checked = lambda *a, **k: None
        pkg = inventory.App(name="somepkg", version="1", source=inventory.REPO,
                            update="2")
        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run), \
             mock.patch.object(inventory, "synced_version", return_value="2"), \
             mock.patch.object(snapshots, "snapper_configs", return_value=["root"]):
            app_mod.AlmanacWindow.on_update_one(win, None, pkg)
            fake.say_yes()
        self.assertEqual(fake.ran[0][0], "Taking a snapshot first")
        self.assertIn("yay -S", fake.commands[1])


class ConfigResets(unittest.TestCase):
    """A config file put back to the shipped defaults, with the user's
    version sitting next to it as a backup nobody mentions."""

    def setUp(self):
        import time
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.home = root / "config"
        self.shipped = root / "omarchy"
        (self.home / "omarchy").mkdir(parents=True)
        (self.shipped / "config/omarchy").mkdir(parents=True)
        self.default = {"bar": [{"id": "omarchy.clock"}]}
        self.mine = {"bar": [{"id": "omarchy.clock"}, {"id": "my.widget"},
                             {"id": "my.tray"}]}
        (self.shipped / "config/omarchy/shell.json").write_text(
            json.dumps(self.default))
        self.cfg = self.home / "omarchy/shell.json"
        self.cfg.write_text(json.dumps(self.default, indent=2))
        self.bak = self.home / f"omarchy/shell.json.bak.{int(time.time()) - 60}"
        self.bak.write_text(json.dumps(self.mine))
        self.env = mock.patch.dict("os.environ", {
            "XDG_CONFIG_HOME": str(self.home), "OMARCHY_PATH": str(self.shipped)})
        self.env.start()

    def tearDown(self):
        self.env.stop()
        self.tmp.cleanup()

    def test_a_reset_is_found_with_what_it_removed(self):
        import guard
        [r] = guard.resets()
        self.assertEqual(r.path, self.cfg)
        self.assertEqual(r.lost, ["my.widget", "my.tray"])

    def test_restoring_brings_it_back_and_keeps_the_defaults(self):
        import guard
        [r] = guard.resets()
        guard.restore(r, log=lambda _m: None)
        self.assertEqual(json.loads(self.cfg.read_text()), self.mine)
        self.assertTrue(list(self.cfg.parent.glob("shell.json.defaults.*")))
        self.assertEqual(guard.resets(), [])

    def test_a_backup_that_was_itself_the_defaults_is_not_a_reset(self):
        import guard
        # Formatted differently, same content: nothing was lost.
        self.bak.write_text(json.dumps(self.default, indent=4))
        self.assertEqual(guard.resets(), [])

    def test_a_customised_file_is_not_a_reset(self):
        import guard
        self.cfg.write_text(json.dumps(self.mine))
        self.assertEqual(guard.resets(), [])

    def test_an_old_backup_is_let_go(self):
        import guard
        old = self.cfg.with_name("shell.json.bak.1000000000")
        self.bak.rename(old)
        self.assertEqual(guard.resets(), [])

    def test_without_shipped_defaults_nothing_is_claimed(self):
        import guard
        with mock.patch.dict("os.environ",
                             {"OMARCHY_PATH": str(Path(self.tmp.name) / "none")}):
            self.assertEqual(guard.resets(), [])

    def test_restoring_shell_json_restarts_never_refreshes(self):
        import guard
        with mock.patch.object(guard.shutil, "which", return_value="/bin/x"):
            cmds = guard.reload_for([self.cfg])
        self.assertEqual(cmds, [["omarchy-restart-shell"]])


class UpdateSnapshots(unittest.TestCase):
    LOG = (
        "[2026-10-16T22:52:33-0700] [PACMAN] starting full system upgrade\n"
        "[2026-10-16T22:52:40-0700] [ALPM] transaction started\n"
        "[2026-10-16T22:52:40-0700] [ALPM] running '05-snap-pac-pre.hook'...\n"
        "[2026-10-16T22:52:41-0700] [ALPM] upgraded a (1-1 -> 2-1)\n"
        "[2026-10-16T22:52:42-0700] [ALPM] transaction completed\n"
        "[2026-10-16T23:02:32-0700] [ALPM] transaction started\n"
        "[2026-10-16T23:02:33-0700] [ALPM] upgraded b (1-1 -> 2-1)\n"
        "[2026-10-16T23:02:34-0700] [ALPM] transaction completed\n")

    def test_the_last_transaction_is_read_with_its_changes(self):
        import snapshots
        tx = snapshots.last_transaction(self.LOG)
        self.assertEqual(tx.changes, ["upgraded b (1-1 -> 2-1)"])
        self.assertFalse(tx.hooks_snapshot)

    def test_a_snapshot_hook_inside_the_transaction_counts(self):
        import snapshots
        first = self.LOG.split("[2026-10-16T23:02:32")[0]
        self.assertTrue(snapshots.last_transaction(first).hooks_snapshot)

    def test_a_manual_snapper_create_in_the_journal_counts(self):
        import snapshots
        j = ("Oct 16 23:02:22 host sudo[1]: tim : COMMAND=/usr/bin/snapper "
             "-c root create -c number -d 4.0.4-1\n")
        self.assertTrue(snapshots.snapshot_before(0, j))
        self.assertFalse(snapshots.snapshot_before(0, "nothing\n"))

    def test_a_snapshot_boot_is_recognised(self):
        import snapshots
        line = "root=UUID=x rw rootflags=subvol=/@/.snapshots/142/snapshot quiet"
        self.assertEqual(snapshots.booted_snapshot(line), "142")
        self.assertEqual(snapshots.booted_snapshot("root=UUID=x rw"), "")

    def test_no_snapper_means_no_snapshot_step(self):
        import snapshots
        with mock.patch.object(snapshots, "snapper_configs", return_value=[]):
            self.assertIsNone(snapshots.create_step("x"))

    def test_a_refused_snapshot_does_not_stop_the_update(self):
        import snapshots
        failed = mock.Mock(returncode=1, stdout="", stderr="Request dismissed")
        with mock.patch.object(snapshots, "snapper_configs", return_value=["root"]), \
             mock.patch.object(snapshots.subprocess, "run", return_value=failed):
            _label, step = snapshots.create_step("x")
            lines = []
            step(lines.append)          # must not raise
        self.assertIn("continuing without one", lines[-1])


class DefaultApps(unittest.TestCase):
    def test_a_takeover_is_listed_with_what_it_replaced(self):
        import mimeguard
        before = {"application/vnd.oasis.opendocument.text": "writer.desktop",
                  "text/plain": "nvim.desktop"}
        after = {"application/vnd.oasis.opendocument.text": "wordcraft.desktop",
                 "text/plain": "nvim.desktop"}
        self.assertEqual(mimeguard.changes(before, after),
                         [("application/vnd.oasis.opendocument.text",
                           "writer.desktop", "wordcraft.desktop")])

    def test_no_snapshot_means_no_claim(self):
        import mimeguard
        self.assertEqual(mimeguard.changes(None, {"a/b": "x"}), [])

    def test_restoring_uses_the_standard_tool(self):
        import mimeguard
        with mock.patch.object(mimeguard.subprocess, "run") as run:
            n = mimeguard.restore([("text/rtf", "writer.desktop", "wc.desktop"),
                                   ("text/x-new", "", "wc.desktop")])
        self.assertEqual(n, 1)
        self.assertEqual(run.call_args[0][0],
                         ["xdg-mime", "default", "writer.desktop", "text/rtf"])


class LocalChanges(unittest.TestCase):
    """A patched checkout, weighed against the update without touching it."""

    def test_a_change_the_update_already_has_is_absorbed(self):
        import patches
        with tempfile.TemporaryDirectory() as d:
            up, clone, git = _git_repo(d)
            (up / "f.txt").write_text("one\nTWO\nthree\nfour\nfive\nsix\n")
            git("commit", "-qam", "fix", cwd=up)
            git("fetch", "-q", cwd=clone)
            (clone / "f.txt").write_text("one\nTWO\nthree\nfour\nfive\nsix\n")
            self.assertEqual(patches.analyze(str(clone)).kind, patches.ABSORBED)

    def test_an_unrelated_change_still_applies(self):
        import patches
        with tempfile.TemporaryDirectory() as d:
            up, clone, git = _git_repo(d)
            (up / "f.txt").write_text("ONE\ntwo\nthree\nfour\nfive\nsix\n")
            git("commit", "-qam", "upstream", cwd=up)
            git("fetch", "-q", cwd=clone)
            (clone / "f.txt").write_text("one\ntwo\nthree\nfour\nfive\nSIX\n")
            state = patches.analyze(str(clone))
            self.assertEqual(state.kind, patches.APPLIES)
            labels = [l for l, _a in patches.steps(str(clone), state, "x")]
            self.assertEqual(labels[-1], "Putting your change back")
            # Analysing must not have touched the working tree.
            self.assertIn("SIX", (clone / "f.txt").read_text())

    def test_the_same_lines_changed_differently_conflict(self):
        import patches
        with tempfile.TemporaryDirectory() as d:
            up, clone, git = _git_repo(d)
            (up / "f.txt").write_text("one\nUPSTREAM\nthree\nfour\nfive\nsix\n")
            git("commit", "-qam", "upstream", cwd=up)
            git("fetch", "-q", cwd=clone)
            (clone / "f.txt").write_text("one\nMINE\nthree\nfour\nfive\nsix\n")
            state = patches.analyze(str(clone))
            self.assertEqual(state.kind, patches.CONFLICTS)
            self.assertEqual(len(state.upstream_commits), 1)
            # Set aside, never discarded.
            cmds = [" ".join(a) for _l, a in patches.steps(str(clone), state, "x")]
            self.assertTrue(any("stash push" in c for c in cmds))
            self.assertFalse(any("checkout" in c or "reset" in c for c in cmds))

    def test_a_clean_checkout_is_a_plain_pull(self):
        import patches
        with tempfile.TemporaryDirectory() as d:
            _up, clone, _git = _git_repo(d)
            state = patches.analyze(str(clone))
            self.assertEqual(state.kind, patches.CLEAN)
            self.assertEqual(len(patches.steps(str(clone), state, "x")), 1)


class InstallFromLink(unittest.TestCase):
    def test_links_are_recognised(self):
        import sources
        for link in ("https://github.com/storytold/wordcraft",
                     "github.com/storytold/wordcraft/releases",
                     "https://github.com/storytold/wordcraft.git"):
            self.assertEqual(sources.parse(link), ("storytold", "wordcraft"))
        self.assertIsNone(sources.parse("https://gitlab.com/a/b"))
        self.assertIsNone(sources.parse("wordcraft"))

    def test_the_x86_appimage_is_chosen_and_its_digest_used(self):
        import sources
        rel = {"tag_name": "v1.2.0", "assets": [
            {"name": "app-1.2.0-aarch64.AppImage", "size": 1,
             "browser_download_url": "u1"},
            {"name": "app-1.2.0-x86_64.AppImage", "size": 1048576,
             "browser_download_url": "u2", "digest": "sha256:" + "a" * 64}]}
        o = sources._appimage(rel)
        self.assertEqual(o.name, "app-1.2.0-x86_64.AppImage")
        self.assertEqual(o.sha256, "a" * 64)
        self.assertEqual(o.version, "1.2.0")

    def test_an_unverifiable_appimage_ranks_below_a_verified_one(self):
        import sources
        rel = {"tag_name": "v1", "assets": [
            {"name": "app-1-x86_64.AppImage", "size": 1,
             "browser_download_url": "u"}]}
        unverified = sources._appimage(rel)
        self.assertTrue(unverified.warnings)
        self.assertGreater(unverified.rank, 5)

    def test_an_aur_package_for_another_project_is_flagged(self):
        import sources
        data = json.dumps({"results": [
            {"Name": "app", "Version": "1", "NumVotes": 50, "Maintainer": "m",
             "URL": "https://github.com/someone-else/app"},
            {"Name": "app-bin", "Version": "1", "NumVotes": 50, "Maintainer": "m",
             "URL": "https://app.example.org"}]}).encode()
        with mock.patch.object(sources, "_get", return_value=data):
            opts = {o.name: o for o in sources._aur(
                ["app", "app-bin"],
                ["https://github.com/owner/app", "https://app.example.org/"])}
        self.assertTrue(any("not this project" in w for w in opts["app"].warnings))
        self.assertIn("Points at this project", opts["app-bin"].reasons)

    def test_a_checksum_mismatch_deletes_the_download(self):
        import appimage
        import sources
        with tempfile.TemporaryDirectory() as d, \
             mock.patch.object(inventory, "REGISTRY", Path(d, "appimages.json")):
            def fake_download(_url, dest, *_a):
                Path(dest).write_bytes(b"not what was published")
                return Path(dest)
            res = sources.Resolution("o", "app")
            opt = sources.Option(sources.APPIMAGE, "app-1-x86_64.AppImage", "1",
                                 "AppImage", url="u", sha256="0" * 64)
            with mock.patch.object(appimage, "download", side_effect=fake_download):
                with self.assertRaises(RuntimeError):
                    sources.install_appimage(res, opt, log=lambda _m: None)
            self.assertEqual(list(Path(d, "apps").iterdir()), [])


class MajorReleases(unittest.TestCase):
    def test_major_jumps_are_recognised(self):
        import agents
        self.assertTrue(agents.major_jump("1.19.3", "2.0.30"))
        self.assertFalse(agents.major_jump("2.1.265", "2.1.301"))
        self.assertFalse(agents.major_jump("1.0", ""))

    def test_scheduled_runs_hold_major_releases(self):
        import agents
        import autorun
        a = inventory.App(name="opencode", version="1.19.3", source="agent",
                          update="2.0.30", extra={"update_cmd": ["true"]})
        with mock.patch.object(agents, "detect", return_value=[a]), \
             mock.patch.object(autorun, "_run") as run, \
             mock.patch.object(autorun, "log"):
            self.assertEqual(autorun.update_agents(), 0)
        run.assert_not_called()


class AnyonesMachine(unittest.TestCase):
    """Almanac is published; nothing may assume the machine it was written on."""

    def test_base_directories_follow_xdg(self):
        import xdg
        with mock.patch.dict("os.environ", {"XDG_DATA_HOME": "/srv/data",
                                            "XDG_CONFIG_HOME": "relative/ignored"}):
            self.assertEqual(xdg.data_home(), Path("/srv/data"))
            self.assertEqual(xdg.config_home(), Path.home() / ".config")

    def test_the_desktops_terminal_launcher_wins(self):
        import xdg
        with mock.patch.object(xdg.shutil, "which",
                               side_effect=lambda n: "/usr/bin/" + n):
            self.assertEqual(xdg.terminal_argv(["true"]),
                             ["xdg-terminal-exec", "true"])

    def test_each_terminal_gets_its_own_flags(self):
        import xdg
        have = {"gnome-terminal"}
        with mock.patch.object(xdg.shutil, "which",
                               side_effect=lambda n: n if n in have else None), \
             mock.patch.dict("os.environ", {"TERMINAL": ""}):
            self.assertEqual(xdg.terminal_argv(["true"]),
                             ["gnome-terminal", "--wait", "--", "true"])

    def test_no_omarchy_means_the_plain_updater(self):
        import xdg
        with mock.patch.object(xdg.shutil, "which",
                               side_effect=lambda n: n if n == "yay" else None):
            self.assertEqual(xdg.system_update_command(), "yay -Syu")

    def test_rollback_copies_are_looked_for_where_they_are_kept(self):
        import maintenance
        with tempfile.TemporaryDirectory() as d, \
             mock.patch.object(inventory, "REGISTRY", Path(d, "appimages.json")):
            Path(d, "apps").mkdir()
            Path(d, "apps", "tool.previous").write_bytes(b"x" * 2048)
            f = maintenance.rollbacks()
        self.assertEqual(f.items, ["tool.previous"])

    def test_local_builds_are_found_by_being_checkouts_not_by_name(self):
        import subprocess
        with tempfile.TemporaryDirectory() as d:
            repo = Path(d, "some-driver")
            subprocess.run(["git", "init", "-q", str(repo)], check=True)
            with mock.patch.dict("os.environ", {"XDG_DATA_HOME": d,
                                                "XDG_CONFIG_HOME": d}):
                names = [a.name for a in inventory.local_builds(check_updates=False)]
        self.assertIn("some-driver", names)

    def test_no_personal_paths_in_the_source(self):
        here = Path(__file__).parent
        for f in here.glob("*.py"):
            if f.name == "test_app.py":
                continue
            text = f.read_text()
            # Whoever runs the tests: their home must never be baked in.
            self.assertNotIn(str(Path.home()) + "/", text, f.name)
            self.assertNotIn("streamdock", text.lower(), f.name)


class AboutWindow(unittest.TestCase):
    def test_whats_new_comes_from_the_shipped_metainfo(self):
        import selfupdate
        version, markup = selfupdate.release_notes()
        self.assertEqual(version, selfupdate.VERSION)
        self.assertTrue(markup.startswith("<p>"))


class KnownVulnerabilities(unittest.TestCase):
    """Matched against the Arch Security Tracker with pacman's own vercmp."""
    GROUPS = [
        {"name": "AVG-1", "packages": ["openssl"], "status": "Fixed",
         "severity": "High", "type": "arbitrary code execution",
         "affected": "3.0.0-1", "fixed": "3.0.2-1", "issues": ["CVE-1"]},
        {"name": "AVG-2", "packages": ["libxml2"], "status": "Vulnerable",
         "severity": "Medium", "type": "denial of service",
         "affected": "2.14.0-1", "fixed": None, "issues": ["CVE-2"]},
        {"name": "AVG-3", "packages": ["curl"], "status": "Not affected",
         "severity": "Low", "type": "", "affected": "1-1", "fixed": None,
         "issues": []},
    ]

    def test_older_than_the_fix_is_vulnerable(self):
        import security
        v = security.vulnerabilities({"openssl": "3.0.1-1"}, self.GROUPS)
        self.assertEqual([x.group for x in v], ["AVG-1"])

    def test_the_fixed_version_is_not(self):
        import security
        self.assertEqual(security.vulnerabilities({"openssl": "3.0.2-1"},
                                                  self.GROUPS), [])
        # pkgrel and epoch are compared the way pacman compares them.
        self.assertEqual(security.vulnerabilities({"openssl": "1:2.0-1"},
                                                  self.GROUPS), [])

    def test_no_fix_yet_counts_from_the_first_affected_version(self):
        import security
        self.assertEqual(len(security.vulnerabilities({"libxml2": "2.15.0-1"},
                                                      self.GROUPS)), 1)
        self.assertEqual(security.vulnerabilities({"libxml2": "2.13.0-1"},
                                                  self.GROUPS), [])

    def test_not_affected_is_ignored(self):
        import security
        self.assertEqual(security.vulnerabilities({"curl": "8-1"}, self.GROUPS), [])

    def test_a_pending_update_that_fixes_it_is_a_security_update(self):
        import security
        v = security.vulnerabilities({"openssl": "3.0.1-1"}, self.GROUPS)
        self.assertTrue(security.fixed_by(v, "3.0.2-1"))
        self.assertFalse(security.fixed_by(v, "3.0.1-2"))

    def test_unfixable_advisories_are_not_a_warning(self):
        import maintenance as m
        import security
        v = security.vulnerabilities({"libxml2": "2.15.0-1"}, self.GROUPS)
        with mock.patch.object(security, "vulnerabilities", return_value=v):
            self.assertEqual(security.finding().severity, m.INFO)
        v = security.vulnerabilities({"openssl": "3.0.1-1"}, self.GROUPS)
        with mock.patch.object(security, "vulnerabilities", return_value=v):
            self.assertEqual(security.finding().severity, m.CRIT)

    def test_an_unreachable_tracker_is_not_a_clean_bill(self):
        import maintenance as m
        import security
        with mock.patch.object(security, "vulnerabilities", return_value=None):
            f = security.finding()
        self.assertNotEqual(f.severity, m.OK)


class Timeline(unittest.TestCase):
    LOG = (
        "[2026-10-16T23:02:32-0700] [ALPM] transaction started\n"
        "[2026-10-16T23:02:33-0700] [ALPM] upgraded a (1-1 -> 2-1)\n"
        "[2026-10-16T23:02:33-0700] [ALPM] upgraded b (1-1 -> 2-1)\n"
        "[2026-10-16T23:02:33-0700] [ALPM] installed c (1-1)\n"
        "[2026-10-16T23:02:34-0700] [ALPM] transaction completed\n"
        "[2026-10-16T23:05:00-0700] [ALPM] transaction started\n"
        "[2026-10-16T23:05:01-0700] [ALPM] transaction completed\n")

    def test_a_transaction_is_one_event_with_a_plain_summary(self):
        import history
        [e] = history.package_events(0, self.LOG)
        self.assertEqual(e.title, "Updated 2 packages · installed c")
        self.assertEqual(len(e.items), 3)

    def test_snapshots_are_read_from_the_journal(self):
        import history
        rec = {"MESSAGE": "tim : TTY=pts/4 ; COMMAND=/usr/bin/snapper -c root "
                          "create -c number -d 4.0.4-1",
               "__REALTIME_TIMESTAMP": "1792216942000000"}
        [e] = history.snapshot_events(0, json.dumps(rec) + "\n")
        self.assertEqual(e.kind, history.SNAPSHOT)
        self.assertIn("4.0.4-1", e.detail)

    def test_almanacs_own_actions_round_trip(self):
        import history
        with tempfile.TemporaryDirectory() as d, \
             mock.patch.dict("os.environ", {"XDG_STATE_HOME": d}):
            history.record(history.ALMANAC, "yaru did not change",
                           severity="warn")
            [e] = history.recorded(0)
        self.assertEqual((e.title, e.severity), ("yaru did not change", "warn"))

    def test_days_with_a_snapshot_are_marked(self):
        import history
        snap = history.Event(1792216942, history.SNAPSHOT, "Snapshot taken")
        pkg = history.Event(1792216952, history.PACKAGES, "Updated a")
        with mock.patch.object(history, "package_events", return_value=[pkg]), \
             mock.patch.object(history, "snapshot_events", return_value=[snap]), \
             mock.patch.object(history, "reset_events", return_value=[]), \
             mock.patch.object(history, "recorded", return_value=[]), \
             mock.patch.object(history.time, "time", return_value=1792220000):
            [day] = history.timeline()
        self.assertTrue(day.snapshot)
        self.assertEqual(day.events[0], pkg)          # newest first


class BackgroundGuard(unittest.TestCase):
    def test_enabling_writes_a_standard_autostart_entry(self):
        import watcher
        with tempfile.TemporaryDirectory() as d, \
             mock.patch.dict("os.environ", {"XDG_CONFIG_HOME": d}):
            watcher.enable(start_now=False)
            entry = Path(d, "autostart", watcher.ENTRY_NAME).read_text()
            self.assertTrue(watcher.enabled())
            watcher.disable()
            self.assertFalse(watcher.enabled())
        self.assertIn("--guard", entry)
        self.assertIn("NoDisplay=true", entry)

    def test_exec_arguments_are_quoted_per_the_spec(self):
        import watcher
        self.assertEqual(watcher._quote("/usr/bin/almanac"), "/usr/bin/almanac")
        self.assertEqual(watcher._quote("/path with space/app.py"),
                         '"/path with space/app.py"')

    def test_a_reset_is_announced_once(self):
        import guard
        import watcher
        r = guard.Reset(Path("/c/shell.json"), Path("/c/shell.json.bak.1"), 1,
                        ["x"])
        with mock.patch.object(watcher, "_notify") as notify:
            seen = watcher.announce([r], set())
            watcher.announce([r], seen)
        self.assertEqual(notify.call_count, 1)
        self.assertIn("1 of your entries was removed", notify.call_args[0][1])


class Accessibility(unittest.TestCase):
    def test_every_icon_only_button_is_named(self):
        """A button that is only an icon is announced as just "button"
        unless it carries an accessible name -- _name() sets it together
        with the tooltip, so each one must reach _name or set LABEL itself
        within a few lines of being created."""
        lines = (Path(__file__).parent / "app.py").read_text().splitlines()
        made = re.compile(r"(\w+(?:\.\w+)?) = \(?Gtk\.(?:Menu|Toggle)?Button\(icon_name=")
        unnamed = []
        for i, line in enumerate(lines):
            m = made.search(line)
            if not m:
                continue
            var = m.group(1)
            window = "\n".join(lines[i:i + 12])
            if (f"_name({var}" not in window
                    and "AccessibleProperty.LABEL" not in window
                    and var not in ("upd", "play", "menu")):   # named on bind
                unnamed.append(f"app.py:{i + 1} {var}")
        self.assertEqual(unnamed, [])


class Translations(unittest.TestCase):
    def test_the_template_is_current(self):
        """po/almanac.pot must hold every marked string, or translators work
        from a stale list. Run po/update.sh after changing UI text."""
        import shutil
        import subprocess
        if not shutil.which("xgettext"):
            self.skipTest("gettext is not installed")
        here = Path(__file__).parent
        with tempfile.TemporaryDirectory() as d:
            subprocess.run(["xgettext", "--language=Python", "--keyword=_",
                            "--keyword=ngettext:1,2", "--from-code=UTF-8",
                            "-o", f"{d}/now.pot", *sorted(map(str, here.glob("*.py")))],
                           check=True, capture_output=True)
            ids = lambda f: set(re.findall(r'^msgid "(.+)"$', Path(f).read_text(), re.M))
            missing = ids(f"{d}/now.pot") - ids(here / "po/almanac.pot")
        self.assertEqual(missing, set(), "run po/update.sh")

    def test_a_compiled_translation_is_used(self):
        import gettext
        import shutil
        import subprocess
        import i18n
        if not shutil.which("msgfmt"):
            self.skipTest("gettext is not installed")
        with tempfile.TemporaryDirectory() as d:
            po = Path(d, "de.po")
            po.write_text('msgid ""\nmsgstr "Content-Type: text/plain; charset=UTF-8\\n"\n\n'
                          'msgid "History"\nmsgstr "Verlauf"\n')
            Path(d, "de/LC_MESSAGES").mkdir(parents=True)
            subprocess.run(["msgfmt", "-o", f"{d}/de/LC_MESSAGES/almanac.mo", str(po)],
                           check=True)
            t = gettext.translation(i18n.DOMAIN, localedir=d, languages=["de"])
        self.assertEqual(t.gettext("History"), "Verlauf")

    def test_untranslated_falls_back_to_english(self):
        import i18n
        self.assertEqual(i18n._("No string anyone has translated"),
                         "No string anyone has translated")


class DemoModeHidesTheMachine(unittest.TestCase):
    """Screenshots are made in demo mode and published; nothing in them may
    describe the machine they were taken on."""

    def test_sidebar_stats_are_synthetic(self):
        import os
        import app as app_mod
        with mock.patch.dict("os.environ", {"ALMANAC_DEMO": "1"}):
            kernel = app_mod.AlmanacWindow._kernel_version()
            uptime = app_mod.AlmanacWindow._uptime()
        self.assertNotIn(os.uname().release.split("-")[0], kernel)
        self.assertEqual(uptime, "6d 2h")

    def test_plugins_are_synthetic(self):
        import plugins
        with mock.patch.dict("os.environ", {"ALMANAC_DEMO": "1"}), \
             mock.patch.object(plugins.subprocess, "run",
                               side_effect=AssertionError("asked omarchy")):
            self.assertTrue(plugins.installed())

    def test_history_is_synthetic(self):
        import history
        with mock.patch.dict("os.environ", {"ALMANAC_DEMO": "1"}), \
             mock.patch.object(history, "package_events",
                               side_effect=AssertionError("read the real log")):
            self.assertTrue(history.timeline())


class SetupAfterInstall(unittest.TestCase):
    """Plugins the marketplace marks "requires additional setup" are installed
    like any other, and their README's setup is walked through after."""

    def entry(self, **kw):
        e = {"id": "x", "repo": "https://github.com/a/x", "installAvailable": False,
             "repositoryLayout": "root-plugin", "installCommand": "",
             "installNote": "This plugin requires additional setup before it "
                            "can be enabled. Follow the upstream installation "
                            "instructions."}
        e.update(kw)
        return store._omarchy_entry(e)

    def test_a_setup_plugin_becomes_installable(self):
        row = self.entry()
        self.assertTrue(row["installable"])
        self.assertTrue(row["needs_setup"])
        self.assertEqual(row["install_cmd"],
                         "omarchy plugin add https://github.com/a/x --enable")
        self.assertEqual(row["status"], "Needs setup")

    def test_what_cannot_be_installed_safely_stays_manual(self):
        for kw in ({"repositoryLayout": "monorepo"},
                   {"repositoryLayout": "suite"},
                   {"installNote": "Omarchy clones the current upstream repository, "
                                   "validates it locally…"},
                   {"installNote": "Nearby requires a version-matched native helper."},
                   {"repo": "ssh://evil/x"}):
            row = self.entry(**kw)
            self.assertFalse(row["installable"], kw)
            self.assertFalse(row["needs_setup"], kw)

    def test_a_repo_url_is_quoted_into_the_command(self):
        import shlex
        row = self.entry(repo="https://github.com/a/x;rm -rf ~")
        self.assertEqual(shlex.split(row["install_cmd"])[3],
                         "https://github.com/a/x;rm -rf ~")

    README = """# Plugin
## Requirements
You need an API key from the service.
```bash
sudo pacman -S --needed jq socat
yay -S some-helper-bin
```
## Installation
```bash
omarchy plugin add https://github.com/a/x --enable
make install PREFIX=~/.local
./scripts/setup.sh
curl -fsSL https://example.com/i.sh | sh
```
## Development
```bash
cargo test
make build
```
## Uninstall
```bash
./uninstall.sh
```
"""

    def plugin(self, d):
        root = Path(d, "x")
        (root / "scripts").mkdir(parents=True)
        (root / "README.md").write_text(self.README)
        (root / "scripts/setup.sh").write_text("echo configuring\n")
        (root / "uninstall.sh").write_text("rm -rf everything\n")
        return root

    def test_setup_is_read_only_from_setup_sections(self):
        import plugin_setup
        with tempfile.TemporaryDirectory() as d:
            plan = plugin_setup.plan(self.plugin(d))
        self.assertEqual(plan.packages, ["jq", "socat", "some-helper-bin"])
        self.assertEqual(plan.commands, ["make install PREFIX=~/.local",
                                         "./scripts/setup.sh"])
        self.assertIn("scripts/setup.sh", plan.scripts)
        self.assertEqual(plan.by_hand, [("Credentials or a sign-in", "Requirements")])

    def test_never_offered(self):
        import plugin_setup
        with tempfile.TemporaryDirectory() as d:
            cmds = " ".join(plugin_setup.plan(self.plugin(d)).commands)
        for bad in ("curl", "uninstall", "cargo test", "make build",
                    "omarchy plugin add"):
            self.assertNotIn(bad, cmds)

    def test_the_terminal_script_stops_at_the_first_failure(self):
        import plugin_setup
        with tempfile.TemporaryDirectory() as d:
            script = plugin_setup.terminal_script(plugin_setup.plan(self.plugin(d)))
        lines = script.splitlines()
        self.assertTrue(lines[0].startswith("cd "))
        self.assertEqual(lines[1], "set -e")

    def test_installing_says_setup_follows_and_then_offers_it(self):
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        win._store_after = mock.Mock()
        win._offer_setup = mock.Mock()
        p = store.Plugin(id="x", name="X", description="", author="a",
                         install_cmd="omarchy plugin add https://github.com/a/x --enable",
                         installable=True, needs_setup=True, market="omarchy")
        done = {}
        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run",
                               lambda _w, _t, steps, cb: done.update(cb=cb)):
            app_mod.AlmanacWindow.on_plugin_install(win, None, p)
            self.assertIn("setup", fake.confirm_args["body"])
            fake.say_yes()
        done["cb"](True)
        win._offer_setup.assert_called_once_with(p)


class StorePages(unittest.TestCase):
    """Themes have their own page; the App Store does not list them again."""

    def test_each_page_draws_from_its_own_catalogue(self):
        import app as app_mod
        theme = store.Plugin(id="t", name="T", description="", author="",
                             install_kind="theme", market="themes")
        plugin = store.Plugin(id="p", name="P", description="", author="",
                              market="omarchy")
        win = _Stub()
        win._plugins = [theme, plugin]
        win._page = "discover"
        self.assertEqual(app_mod.AlmanacWindow._store_pool(win), [plugin])
        self.assertEqual(app_mod.AlmanacWindow._store_pool(win, "themes"), [theme])


class PluginStates(unittest.TestCase):
    """Classified before anything is offered -- especially development links."""

    def setUp(self):
        import subprocess
        self.tmp = tempfile.TemporaryDirectory()
        self.d = Path(self.tmp.name)
        self.git = lambda *a, cwd: subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
            cwd=cwd, check=True, capture_output=True)
        self.up = self.d / "up"
        self.git("init", "-q", "-b", "main", str(self.up), cwd=self.d)
        (self.up / "manifest.json").write_text("{}")
        self.git("add", ".", cwd=self.up)
        self.git("commit", "-qm", "one", cwd=self.up)
        self.plug = self.d / "plugins" / "a.plugin"
        self.git("clone", "-q", str(self.up), str(self.plug), cwd=self.d)

    def tearDown(self):
        self.tmp.cleanup()

    def advance_upstream(self):
        (self.up / "f").write_text("x")
        self.git("add", ".", cwd=self.up)
        self.git("commit", "-qm", "two", cwd=self.up)

    def state(self, d=None):
        import plugins
        return plugins.classify(d or self.plug, {})

    def test_current_and_behind(self):
        import plugins
        self.assertEqual(self.state().kind, plugins.CURRENT)
        self.advance_upstream()
        st = self.state()
        self.assertEqual((st.kind, st.behind), (plugins.UPDATE, 1))

    def test_a_symlinked_development_plugin_is_never_updatable(self):
        import plugins
        self.advance_upstream()
        link = self.d / "plugins" / "dev.plugin"
        link.symlink_to(self.plug)
        st = self.state(link)
        self.assertEqual((st.kind, st.detail), (plugins.LOCAL, "symlink"))
        self.assertFalse(st.updatable)

    def test_edits_ahead_and_diverged_are_local_changes(self):
        import plugins
        (self.plug / "manifest.json").write_text('{"x": 1}')
        self.assertEqual(self.state().detail, "modified")
        self.git("commit", "-qam", "mine", cwd=self.plug)
        self.assertEqual(self.state().detail, "ahead")
        self.advance_upstream()
        st = self.state()
        self.assertEqual((st.kind, st.detail), (plugins.LOCAL_CHANGES, "diverged"))

    def test_plain_folders_are_local(self):
        import plugins
        plain = self.d / "plugins" / "mine"
        plain.mkdir()
        self.assertEqual(self.state(plain).detail, "not-git")
        self.assertFalse(self.state(plain).updatable)

    def test_bulk_update_scope(self):
        import plugins
        self.assertTrue(plugins.in_scope("verified", "verified"))
        self.assertFalse(plugins.in_scope("reviewed-stale", "verified"))
        self.assertTrue(plugins.in_scope("reviewed-stale", "changed"))
        self.assertFalse(plugins.in_scope(None, "changed"))     # unlisted
        self.assertTrue(plugins.in_scope(None, "all"))

    def test_restarting_never_refreshes(self):
        import plugins
        argv = " ".join(plugins.restart_argv())
        self.assertIn("omarchy-restart-shell", argv)
        self.assertNotIn("refresh", argv)

    def test_scheduled_runs_skip_links_and_out_of_scope_plugins(self):
        import autorun
        import plugins
        self.advance_upstream()
        (self.d / "plugins" / "dev.plugin").symlink_to(self.plug)
        with mock.patch.object(plugins, "plugins_dir",
                               return_value=self.d / "plugins"), \
             mock.patch.object(plugins, "scope", return_value="verified"), \
             mock.patch.object(store, "catalog", return_value=[]), \
             mock.patch.object(autorun, "_run", return_value=True) as run, \
             mock.patch.object(autorun, "log"):
            autorun.update_plugins()
        run.assert_not_called()             # unlisted, so outside "verified"
        with mock.patch.object(plugins, "plugins_dir",
                               return_value=self.d / "plugins"), \
             mock.patch.object(plugins, "scope", return_value="all"), \
             mock.patch.object(store, "catalog", return_value=[]), \
             mock.patch.object(autorun, "_run", return_value=True) as run, \
             mock.patch.object(autorun, "log"):
            autorun.update_plugins()
        updated = [c.args[0][3] for c in run.call_args_list]
        self.assertEqual(updated, ["a.plugin"])     # never the symlink


class PluginShortcuts(unittest.TestCase):
    BINDS = [{"modmask": 64, "key": "SPACE", "description": "Omarchy menu",
              "keycode": 0, "mouse": False, "catch_all": False, "submap": ""}]

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        hypr = Path(self.tmp.name, "hypr")
        hypr.mkdir()
        self.path = hypr / "bindings.lua"
        self.original = '-- mine\no.bind("SUPER + SPACE", "Omarchy menu", "x")\n'
        self.path.write_text(self.original)
        self.env = mock.patch.dict("os.environ", {"XDG_CONFIG_HOME": self.tmp.name})
        self.env.start()

    def tearDown(self):
        self.env.stop()
        self.tmp.cleanup()

    def hyprctl(self, errors_after_reload=""):
        state = {"reloaded": False}

        def run(*args):
            if args[:2] == ("-j", "binds"):
                return json.dumps(self.BINDS)
            if args == ("reload",):
                state["reloaded"] = True
                return "ok"
            if args == ("configerrors",):
                return errors_after_reload if state["reloaded"] else ""
            return ""
        return run

    def test_combinations_are_normalised(self):
        import shortcuts
        self.assertEqual(shortcuts.normalise("alt+super+v")[0], "SUPER + ALT + V")
        with self.assertRaises(ValueError):
            shortcuts.normalise("v")                    # no modifier

    def test_a_shortcut_is_written_in_its_own_block(self):
        import shortcuts
        with mock.patch.object(shortcuts, "_hyprctl", side_effect=self.hyprctl()):
            shortcuts.set_shortcut("lxp.vitals", "super+alt+v")
        raw = self.path.read_text()
        self.assertTrue(raw.startswith(self.original.rstrip("\n")))
        self.assertEqual(shortcuts.current("lxp.vitals"), "SUPER + ALT + V")
        self.assertIn("omarchy-shell shell toggle lxp.vitals", raw)
        with mock.patch.object(shortcuts, "_hyprctl", side_effect=self.hyprctl()):
            shortcuts.set_shortcut("lxp.vitals", None)
        self.assertEqual(self.path.read_text().strip(), self.original.strip())

    def test_a_taken_combination_is_refused_and_nothing_written(self):
        import shortcuts
        with mock.patch.object(shortcuts, "_hyprctl", side_effect=self.hyprctl()):
            with self.assertRaises(ValueError) as e:
                shortcuts.set_shortcut("lxp.vitals", "super+space")
        self.assertIn("Omarchy menu", str(e.exception))
        self.assertEqual(self.path.read_text(), self.original)

    def test_a_rejected_change_is_rolled_back(self):
        import shortcuts
        with mock.patch.object(shortcuts, "_hyprctl",
                               side_effect=self.hyprctl("line 3: bad bind")):
            with self.assertRaises(ValueError):
                shortcuts.set_shortcut("lxp.vitals", "super+alt+v")
        self.assertEqual(self.path.read_text(), self.original)

    def test_a_hand_edited_block_is_not_overwritten(self):
        import shortcuts
        with mock.patch.object(shortcuts, "_hyprctl", side_effect=self.hyprctl()):
            shortcuts.set_shortcut("lxp.vitals", "super+alt+v")
        self.path.write_text(self.path.read_text().replace("toggle", "summon"))
        with self.assertRaises(ValueError):
            shortcuts.current("lxp.vitals")


class PluginsPage(unittest.TestCase):
    def test_every_plugin_is_listed_with_what_it_is(self):
        import plugins
        listed = [
            {"id": "omarchy.clock", "name": "Clock", "kinds": ["bar-widget"],
             "enabled": True, "canDisable": True, "firstParty": True},
            {"id": "omarchy.bar", "name": "Bar", "kinds": ["bar"],
             "enabled": True, "canDisable": False, "firstParty": True},
            {"id": "me.thing", "name": "Thing", "kinds": ["panel"],
             "enabled": False, "canDisable": True, "firstParty": False},
        ]
        with tempfile.TemporaryDirectory() as d:
            shipped = Path(d, "omarchy/shell/plugins/clock")
            shipped.mkdir(parents=True)
            (shipped / "manifest.json").write_text(json.dumps(
                {"id": "omarchy.clock", "description": "The time"}))
            mine = Path(d, "config/omarchy/plugins/me.thing")
            mine.mkdir(parents=True)
            (mine / "manifest.json").write_text(json.dumps(
                {"id": "me.thing", "version": "1.0"}))
            with mock.patch.dict("os.environ", {"OMARCHY_PATH": f"{d}/omarchy",
                                                "XDG_CONFIG_HOME": f"{d}/config"}), \
                 mock.patch.object(plugins.subprocess, "run",
                                   return_value=mock.Mock(stdout=json.dumps(listed))):
                found = {i.id: i for i in plugins.installed(fetch=False)}
        self.assertEqual(list(found)[0], "me.thing")         # yours first
        self.assertFalse(found["me.thing"].built_in)
        self.assertEqual(found["me.thing"].state.kind, plugins.LOCAL)   # no .git
        self.assertTrue(found["omarchy.clock"].built_in)
        self.assertEqual(found["omarchy.clock"].description, "The time")
        self.assertFalse(found["omarchy.bar"].can_disable)
        self.assertTrue(found["me.thing"].has_panel)

    def test_plugins_leave_applications_and_local_builds(self):
        import app as app_mod
        win = _Stub()
        win.query = ""
        plugin = mock.Mock(source=inventory.LOCAL, has_update=True, haystack="",
                           app=inventory.App(name="p", version="1",
                                             source=inventory.LOCAL,
                                             extra={"plugin_id": "p"}))
        build = mock.Mock(source=inventory.LOCAL, has_update=False, haystack="",
                          app=inventory.App(name="b", version="1",
                                            source=inventory.LOCAL, extra={}))
        for key, want in (("apps", False), (inventory.LOCAL, False),
                          ("all", True), ("updates", True)):
            win.filter_key = key
            self.assertEqual(app_mod.AlmanacWindow._match(win, plugin, None), want, key)
        win.filter_key = inventory.LOCAL
        self.assertTrue(app_mod.AlmanacWindow._match(win, build, None))


class WindowStarts(unittest.TestCase):
    def test_every_page_shortcut_is_a_valid_accelerator(self):
        """Ctrl+10 does not exist; an invalid accelerator stops the window
        from being constructed at all."""
        import app as app_mod
        self.assertLessEqual(len(app_mod.AlmanacWindow.PAGE_KEYS), 10)
        for i in range(1, len(app_mod.AlmanacWindow.PAGE_KEYS) + 1):
            self.assertIsNotNone(
                app_mod.Gtk.ShortcutTrigger.parse_string(f"<Control>{i % 10}"))


class PluginLogos(unittest.TestCase):
    def plugin(self, pid, name, kinds=("bar-widget",)):
        import plugins
        return plugins.Installed(id=pid, name=name, kinds=kinds)

    def test_a_service_plugin_gets_its_vendor(self):
        import plugins
        self.assertEqual(plugins.vendor_for(self.plugin("tim.tailscale", "My Tailscale")),
                         "tailscale.com")

    def test_a_namespace_is_not_a_vendor(self):
        import plugins
        p = self.plugin("io.github.someone.lock-explorer", "Lock Screen Explorer")
        self.assertEqual(plugins.vendor_for(p), "")
        self.assertEqual(plugins.symbol_for(p), "system-lock-screen-symbolic")

    def test_the_kind_does_not_decide_the_icon(self):
        import plugins
        # "bar-widget" must not make every bar plugin look like the bar.
        self.assertEqual(plugins.symbol_for(self.plugin("x.usage", "AI Usage")),
                         plugins.GENERIC)

    def test_github_owner(self):
        import plugins
        self.assertEqual(plugins.github_owner("https://github.com/lxp/vitals.git"), "lxp")
        self.assertEqual(plugins.github_owner("git@github.com:lxp/vitals.git"), "lxp")
        self.assertEqual(plugins.github_owner("https://gitlab.com/a/b"), "")


class PluginRemovalAndRecheck(unittest.TestCase):
    def test_removing_a_plugin_removes_its_shortcut(self):
        import app as app_mod
        import shortcuts
        lines = []
        with mock.patch.object(shortcuts, "available", return_value=True), \
             mock.patch.object(shortcuts, "current", return_value="SUPER + ALT + V"), \
             mock.patch.object(shortcuts, "set_shortcut",
                               return_value="removed") as unset:
            _label, step = app_mod.AlmanacWindow._forget_shortcut("lxp.vitals")
            step(lines.append)
        unset.assert_called_once_with("lxp.vitals", None)

    def test_a_shortcut_that_cannot_be_removed_does_not_fail_the_removal(self):
        import app as app_mod
        import shortcuts
        lines = []
        with mock.patch.object(shortcuts, "available", return_value=True), \
             mock.patch.object(shortcuts, "current",
                               side_effect=ValueError("edited by hand")):
            _label, step = app_mod.AlmanacWindow._forget_shortcut("x")
            step(lines.append)                  # must not raise
        self.assertIn("left in bindings.lua", lines[-1])

    def test_store_removal_also_removes_the_shortcut(self):
        import app as app_mod
        fake = _Confirmed()
        win = _Stub()
        win._store_after = mock.Mock()
        win._forget_shortcut = app_mod.AlmanacWindow._forget_shortcut
        p = store.Plugin(id="x", name="X", install_kind="plugin", market="omarchy")
        with mock.patch.object(app_mod.tasks, "confirm", fake.confirm), \
             mock.patch.object(app_mod.tasks, "run", fake.run):
            app_mod.AlmanacWindow.on_plugin_remove(win, None, p)
            fake.say_yes()
        self.assertEqual(fake.ran[1][0], "Removing its keyboard shortcut")


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
