"""Running things inside the app instead of handing them to a terminal.

Shelling out to a terminal window was the honest option while nothing here
could show progress or ask a question. It is not self-contained though: the
work happens somewhere the app cannot see, success is whatever the user
reports by closing the window, and the inventory has to be re-read on a timer
and hope.

A task here runs as a child process with its output streamed into a dialog, so
the app knows what happened and when it finished. Two things follow from that:

* Anything destructive is confirmed by a GTK dialog *first*, showing exactly
  what will be touched -- the preview that pacman's own y/N prompt gives. The
  prompt is not skipped, it is moved somewhere it can be read.
* Anything needing root runs under `pkexec`, which prompts through polkit.
  `sudo` is not usable here: it wants a TTY, and there is deliberately none.
"""
from __future__ import annotations

import os
import shlex
import signal
import subprocess
import threading

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, GLib, Gtk, Pango  # noqa: E402


def needs_root(argv: list[str]) -> bool:
    return bool(argv) and argv[0] in ("pacman", "paccache", "pacman-key")


def elevate(argv: list[str]) -> list[str]:
    """Wrap a command that needs root so polkit can ask for the password.

    --keep-cwd is deliberate: pkexec otherwise switches to root's home, which
    changes what a relative path means mid-task.
    """
    return ["pkexec", "--keep-cwd", *argv] if needs_root(argv) else list(argv)


class TaskDialog(Adw.Dialog):
    """Runs a list of (label, argv) steps, streaming output into the window."""

    def __init__(self, parent, title: str, steps: list, on_done=None):
        super().__init__(title=title)
        self.set_content_width(720)
        self.set_content_height(520)
        self.set_can_close(False)

        self._steps = steps
        self._on_done = on_done
        self._proc = None
        self._cancelled = False
        self._failed = False

        view = Adw.ToolbarView()
        header = Adw.HeaderBar()
        header.set_show_end_title_buttons(False)
        view.add_top_bar(header)

        body = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10,
                       margin_start=16, margin_end=16,
                       margin_top=12, margin_bottom=12)

        self.step_label = Gtk.Label(xalign=0, ellipsize=Pango.EllipsizeMode.END)
        self.step_label.add_css_class("heading")
        body.append(self.step_label)

        self.progress = Gtk.ProgressBar()
        self.progress.set_show_text(False)
        body.append(self.progress)

        self.buffer = Gtk.TextBuffer()
        text = Gtk.TextView(buffer=self.buffer, editable=False,
                            monospace=True, cursor_visible=False)
        text.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        text.add_css_class("card")
        text.set_left_margin(8)
        text.set_right_margin(8)
        text.set_top_margin(6)
        self.text = text

        scroll = Gtk.ScrolledWindow(vexpand=True)
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroll.set_child(text)
        self.scroll = scroll
        body.append(scroll)

        view.set_content(body)

        bottom = Gtk.Box(spacing=8, margin_start=16, margin_end=16,
                         margin_top=6, margin_bottom=12, halign=Gtk.Align.END)
        self.cancel_btn = Gtk.Button(label="Cancel")
        self.cancel_btn.connect("clicked", lambda _b: self.cancel())
        self.close_btn = Gtk.Button(label="Close")
        self.close_btn.add_css_class("suggested-action")
        self.close_btn.set_visible(False)
        self.close_btn.connect("clicked", lambda _b: self.force_close())
        bottom.append(self.cancel_btn)
        bottom.append(self.close_btn)
        view.add_bottom_bar(bottom)

        self.set_child(view)
        self._pulse = GLib.timeout_add(90, self._tick)
        self.present(parent)
        threading.Thread(target=self._work, daemon=True).start()

    # ---- ui helpers ----
    def _tick(self):
        self.progress.pulse()
        return True

    def _append(self, line: str):
        end = self.buffer.get_end_iter()
        self.buffer.insert(end, line)
        # Follow the tail, the way a terminal would.
        mark = self.buffer.create_mark(None, self.buffer.get_end_iter(), False)
        self.text.scroll_mark_onscreen(mark)
        self.buffer.delete_mark(mark)
        return False

    def log(self, line: str):
        GLib.idle_add(self._append, line if line.endswith("\n") else line + "\n")

    def cancel(self):
        self._cancelled = True
        self.log("\n— cancelled —")
        proc = self._proc
        if proc and proc.poll() is None:
            try:
                # The child may itself have children (git, makepkg); signal the
                # whole group so cancelling actually stops the work.
                os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                try:
                    proc.terminate()
                except ProcessLookupError:
                    pass

    def force_close(self):
        self.set_can_close(True)
        self.close()

    # ---- the work ----
    def _work(self):
        ok = True
        for label, argv in self._steps:
            if self._cancelled:
                ok = False
                break
            GLib.idle_add(self.step_label.set_text, label)

            # A step can be work done in this process rather than a command --
            # downloading and registering an AppImage, say. It takes the log
            # function and raises to fail.
            if callable(argv):
                try:
                    argv(self.log)
                except Exception as exc:                # noqa: BLE001
                    self.log(f"failed: {exc}")
                    ok = False
                    break
                self.log("")
                continue

            cmd = elevate(argv)
            self.log(f"$ {' '.join(shlex.quote(c) for c in cmd)}")
            try:
                self._proc = subprocess.Popen(
                    cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                    stdin=subprocess.DEVNULL, text=True, bufsize=1,
                    start_new_session=True)
            except OSError as exc:
                self.log(f"could not start: {exc}")
                ok = False
                break
            for line in self._proc.stdout:
                self.log(line.rstrip("\n"))
            code = self._proc.wait()
            self._proc = None
            if code != 0:
                self.log(f"— exited {code} —")
                ok = False
                break
            self.log("")
        GLib.idle_add(self._finish, ok and not self._cancelled)

    def _finish(self, ok: bool):
        if self._pulse:
            GLib.source_remove(self._pulse)
            self._pulse = None
        self.progress.set_fraction(1.0 if ok else 0.0)
        self.step_label.set_text(
            "Done" if ok else ("Cancelled" if self._cancelled else "Failed"))
        self.cancel_btn.set_visible(False)
        self.close_btn.set_visible(True)
        self.set_can_close(True)
        if self._on_done:
            self._on_done(ok)
        return False


def run(parent, title: str, steps: list, on_done=None) -> TaskDialog:
    """Run steps in a dialog. `steps` is [(label, [argv...]), ...]."""
    return TaskDialog(parent, title, steps, on_done)


def confirm(parent, heading: str, body: str, detail: str,
            action: str, destructive: bool, on_yes) -> None:
    """Ask before doing something that cannot be undone by clicking again.

    `detail` is the preview -- the actual list of what will be removed or
    installed. Showing it is the whole point: a y/N prompt with no list is
    worse than this dialog, not better.
    """
    dialog = Adw.AlertDialog(heading=heading, body=body)
    if detail:
        buf = Gtk.TextBuffer()
        buf.set_text(detail)
        text = Gtk.TextView(buffer=buf, editable=False, monospace=True,
                            cursor_visible=False)
        text.set_wrap_mode(Gtk.WrapMode.WORD_CHAR)
        text.set_left_margin(8)
        text.set_right_margin(8)
        text.set_top_margin(6)
        text.set_bottom_margin(6)
        scroll = Gtk.ScrolledWindow()
        scroll.set_policy(Gtk.PolicyType.NEVER, Gtk.PolicyType.AUTOMATIC)
        scroll.set_min_content_height(170)
        scroll.set_max_content_height(300)
        scroll.set_child(text)
        scroll.add_css_class("card")
        dialog.set_extra_child(scroll)

    dialog.add_response("cancel", "Cancel")
    dialog.add_response("go", action)
    dialog.set_response_appearance(
        "go", Adw.ResponseAppearance.DESTRUCTIVE if destructive
        else Adw.ResponseAppearance.SUGGESTED)
    dialog.set_default_response("cancel")
    dialog.set_close_response("cancel")
    dialog.connect("response",
                   lambda _d, resp: on_yes() if resp == "go" else None)
    dialog.present(parent)


def preview_removal(names: list[str]) -> tuple[bool, str]:
    """What `pacman -Rs` would actually take out, without root.

    -Rns cannot be previewed (--nosave and --print are mutually exclusive), so
    the preview runs -Rs. The set of packages is the same; only the treatment
    of config files differs, and that is stated in the dialog.
    """
    try:
        out = subprocess.run(
            ["pacman", "-Rs", "--print", "--print-format", "%n %v", *names],
            capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError) as exc:
        return False, str(exc)
    if out.returncode != 0:
        return False, (out.stderr or out.stdout).strip()
    return True, out.stdout.strip()
