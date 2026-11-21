# Update all inside the Almanac window — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Update all runs `omarchy update -y` in a hidden PTY inside Almanac, shows a step timeline with per-app rows, asks for the sudo password and any Resolve question in-window, keeps running when the window is closed, and hands its result to the existing post-update report.

**Architecture:** Pure, GTK-free core in `src/inwindow/` (PTY runner, process-tree step tracker, prompt detector, askpass socket) unit-tested with fake `/proc` trees and fake updater scripts; a thin GTK layer in `src/ui/update_view.rs` renders the tracker's `Timeline` and is wired into `update_all.rs` in place of the terminal path when Omarchy is present.

**Tech Stack:** Rust, GTK4 0.11 / libadwaita 0.9 (`v1_5`), `rustix` (add feature `pty`), existing `postupdate.rs` report pipeline.

**Spec:** `docs/superpowers/specs/2026-11-21-in-window-update-design.md` (mockup: `docs/superpowers/specs/assets/2026-11-21-update-view.html`)

## Global Constraints

- Run exactly `omarchy update -y`; never set `OMARCHY_ALLOW_DIRECT_PACMAN`; the normal run leaves `OMARCHY_UPDATE_UNATTENDED` unset (so `-y` sets it to 1); only Resolve sets `OMARCHY_UPDATE_UNATTENDED=0`.
- The password is never written to disk or logs, and is dropped when the run ends.
- No step is ever shown done unless a later step's process has started or the run exited 0; when unsure, "Updating…".
- Almanac never answers "yes" to a reboot prompt; reboots show as a "Restart to finish" note.
- Steps, in order (labels verbatim): `Snapshot`, `Signing keys`, `System packages`, `Omarchy migrations`, `AUR and tools`, `Checking everything works`, `Other apps` (7th only when the batch has non-system items).
- Header copy: title `Updating your system`, subtitle `Step {n} of {total}`, footer `Safe to keep working`, expander `Details`.
- The demo gate refuses this run (`ALMANAC_DEMO`), like every task.
- One visual system: reuse `Win::note_list`/`note_row`, theme colours only; compare against the Updates and Health pages after UI work.
- Builds: `export CARGO_TARGET_DIR=~/.cache/claude-builds/almanac` (never `/tmp`). Location-dependent tests (`root_is_the_resolved_checkout…`, `the_checkout_is_found…`) are run once with the project's own `target/` at the end.
- Fallback: not Omarchy, PTY or askpass setup fails → today's terminal path, unchanged.

## Review Focus

1. **Wrong password.** sudo re-invokes askpass up to 3 times; the dialog must say `Wrong password — try again` on the 2nd/3rd ask, and after the third failure the `Snapshot` step (first sudo use) fails with `Password not accepted. Nothing was changed.` → test in Task 4 (`askpass_counts_attempts`) and Task 3 (`sudo_failure_fails_first_step`).
2. **Almanac killed mid-update.** Closing the PTY master must end the update (SIGHUP), never leave an orphan holding the Omarchy lock → test in Task 1 (`dropping_the_master_ends_the_child`).
3. **Packages not in the Updates list** (new dependencies, AUR deps) appear in pacman's `(i/n) installing x` lines → rows are added on the fly, not dropped or panicking → test in Task 2 (`unknown_packages_get_rows`).
4. **Carriage-return progress bars / huge output.** pacman redraws with `\r`; the tracker sees each segment and the log tail stays bounded (last 2 000 lines in memory; full log in the run file) → test in Task 2 (`carriage_returns_split_segments`) and Task 5 (`log_tail_is_bounded`).
5. **Network drop during download.** `error: failed retrieving file` / `failed to commit transaction` while in `System packages` → that step fails with the pacman error as detail and `Nothing was installed.` when no `upgraded` line was seen → test in Task 2 (`download_failure_fails_system_packages`).

---

## File Structure

- Create `src/inwindow/mod.rs` — module root, re-exports.
- Create `src/inwindow/pty.rs` — spawn a command on a PTY in its own session; read/write; wait.
- Create `src/inwindow/tracker.rs` — `Timeline` model; maps process names and output lines to step/app states.
- Create `src/inwindow/procs.rs` — list the command names in a session's process tree from `/proc`.
- Create `src/inwindow/prompt.rs` — recognise a waiting question in the output tail.
- Create `src/inwindow/askpass.rs` — Unix-socket server (in Almanac) and client (`almanac --askpass`).
- Create `src/inwindow/run.rs` — headless driver tying pty + procs + tracker + prompt + askpass; emits events.
- Create `src/ui/update_view.rs` — GTK timeline view, password and question UI, failure card, report hand-off.
- Modify `src/lib.rs` (add `pub mod inwindow;`), `src/main.rs` (askpass flag), `Cargo.toml` (rustix `pty`).
- Modify `src/ui/update_all.rs` — choose in-window path; flatpak out of `system_command`; `Other apps` step.
- Modify `src/ui/shell/app.rs:173` — close-request hides while a run is on; Quit guard.
- Modify `src/ui/update_report.rs` — `Run` gains `in_window: bool`; report replaces the view's content.

---

### Task 1: PTY runner

**Files:** Create `src/inwindow/mod.rs`, `src/inwindow/pty.rs`; Modify `src/lib.rs`, `Cargo.toml` (rustix features += `"pty"`).

**Interfaces:**
- Produces: `pub struct Pty { pub pid: u32, .. }`; `Pty::spawn(argv: &[String], env: &[(String, String)]) -> Result<Pty, String>`; `fn master_fd(&self) -> BorrowedFd<'_>`; `fn write_all(&self, bytes: &[u8]) -> Result<(), String>`; `fn read_available(&self, buf: &mut Vec<u8>) -> Result<usize, String>` (non-blocking; `Ok(0)` = nothing now); `fn try_wait(&mut self) -> Option<i32>` (exit code; signal n → 128+n); `Drop` closes the master.

- [ ] **Step 1: Write failing tests** in `pty.rs`:
  - `the_child_runs_on_a_terminal`: spawn `["sh","-c","test -t 0 && test -t 1 && echo tty-ok; read x; echo got:$x; exit 3"]`, write `b"hi\n"`, read until exit; assert output contains `tty-ok` and `got:hi`, `try_wait() == Some(3)`.
  - `the_child_leads_its_own_session`: spawn `["sh","-c","ps -o sid= -p $$"]`; assert the printed sid equals `pty.pid`.
  - `dropping_the_master_ends_the_child`: spawn `["sh","-c","sleep 30"]`, note `pid`, drop the `Pty`; assert `kill(pid, 0)` returns `ESRCH` (Drop reaps it).
- [ ] **Step 2:** `cargo test --lib inwindow::pty` → fails (module missing).
- [ ] **Step 3: Implement** with `rustix::pty::{openpt, grantpt, unlockpt, ptsname}`; `std::process::Command` with `pre_exec` doing `setsid()` and `ioctl_tiocsctty(slave)`; slave as stdin/stdout/stderr; master `O_NONBLOCK | O_CLOEXEC`; set window size 120×40 (`tcsetwinsize`). `Drop`: close master, then `waitpid` up to 2 s, then SIGKILL the session leader.
- [ ] **Step 4:** tests pass.
- [ ] **Step 5:** commit `inwindow: run a command on a hidden terminal`.

### Task 2: Step tracker

**Files:** Create `src/inwindow/tracker.rs`, `src/inwindow/procs.rs`.

**Interfaces:**
- Produces:
  - `pub enum StepId { Snapshot, SigningKeys, SystemPackages, Migrations, AurAndTools, Checking, OtherApps }` with `label(&self) -> &'static str` (Global Constraints labels).
  - `pub enum StepState { Waiting, Running { detail: String }, Done { detail: String }, Failed { reason: String, changed: Changed, fix: Option<Fix> } }`; `pub enum Changed { Nothing, Some, Unknown }`; `pub enum Fix { StopStuckUpdate(Vec<u32>), Resolve, RetryUpdate }`.
  - `pub enum AppState { Waiting, Downloading, Installing, Done { version: String }, Failed }`; `pub struct AppRow { pub name: String, pub to: String, pub step: StepId, pub state: AppState }`.
  - `pub struct Timeline { pub steps: Vec<(StepId, StepState)>, pub apps: Vec<AppRow>, pub restart_note: Option<String>, pub generic: bool }`; `Timeline::new(apps: &[(String, String, bool /*aur*/)], other_apps: bool) -> Timeline`; `fn current(&self) -> Option<StepId>`; `fn position(&self) -> (usize, usize)` (`Step n of total`).
  - `pub struct Tracker`; `Tracker::new(t: Timeline)`; `fn on_procs(&mut self, names: &[String])`; `fn on_output(&mut self, chunk: &str)`; `fn on_exit(&mut self, code: i32)`; `fn timeline(&self) -> &Timeline`.
  - `procs::session_commands(proc_root: &Path, sid: u32) -> Vec<String>` — script names via `program_names`-style resolution (make `postupdate::program_names` `pub(crate)`).
- Mapping (process → step): `omarchy-update-pkg-prune`, `omarchy-snapshot` → Snapshot; `omarchy-update-keyring` → SigningKeys; `omarchy-update-system-pkgs`, `omarchy-update-pacman`, `omarchy-update-system-pkgs-when-conflicted` → SystemPackages; `omarchy-migrate`, `omarchy-update-orphan-pkgs`, `omarchy-update-restart`, `omarchy-hook` → Migrations; `omarchy-update-mise`, `omarchy-update-aur-pkgs` → AurAndTools; `omarchy-update-boot` → Checking. Earlier steps become Done only when a later step's process is seen.
- Output rules (per `\r`/`\n` segment, ANSI stripped via `postupdate::clean_output`): `:: Retrieving packages` → SystemPackages detail `Downloading`; `(i/n) upgrading|installing|reinstalling NAME` → that row Installing (unknown NAME → new row on current step); `upgraded NAME (a -> b)`-style lines or the next row starting → previous row Done{b}; `An Omarchy update is already running` → Snapshot Failed{`Another Omarchy update is running.`, Nothing, StopStuckUpdate(holders)}; `This upgrade needs an answer` → SystemPackages Failed{…, Nothing, Resolve}; `error: failed retrieving file`/`failed to commit transaction` → SystemPackages Failed{pacman line, Nothing if no row Done else Some}; `Run omarchy-system reboot when ready` → `restart_note`; nonzero exit with no failure set → current step Failed{`It stopped with an error (exit status N).`, Unknown, RetryUpdate}; exit 0 → all remaining Running → Done.
- No process match for 5 s while running and no current step → `generic = true` (view shows `Updating…`).

- [ ] **Step 1: Write failing tests** (tracker.rs): `processes_tick_steps_in_order`, `a_step_is_never_done_early` (AUR process seen → SystemPackages Done but AurAndTools Running; then `omarchy-update-boot` → AurAndTools Done), `pacman_lines_move_app_rows` (five rows from the screenshot; `(3/5) upgrading slack-desktop` → slack Installing, cursor/mise Done), `unknown_packages_get_rows`, `carriage_returns_split_segments` (`"(1/5) upgrading a\r(2/5) upgrading b\n"` → a Done, b Installing), `already_running_fails_before_anything`, `conflict_offers_resolve`, `download_failure_fails_system_packages`, `sudo_failure_fails_first_step` (exit 1 with only `omarchy-update-pkg-prune` never seen and output `sudo: 3 incorrect password attempts` → Snapshot Failed{`Password not accepted. Nothing was changed.`, Nothing, None}), `exit_zero_completes`, `restart_note_is_kept`. procs.rs: `session_commands_reads_a_fake_tree` (reuse the `fake_proc` pattern from `postupdate.rs` tests, with `stat` sid field).
- [ ] **Step 2:** run, expect compile failure.
- [ ] **Step 3:** implement.
- [ ] **Step 4:** `cargo test --lib inwindow::` passes.
- [ ] **Step 5:** commit `inwindow: map omarchy update's processes and output to a step timeline`.

### Task 3: Prompt detector

**Files:** Create `src/inwindow/prompt.rs`.

**Interfaces:**
- Produces: `pub enum Prompt { Password { prompt: String }, Question { text: String, answers: Vec<Answer> } }`; `pub struct Answer { pub label: String, pub send: Vec<u8>, pub is_reboot_yes: bool }`; `pub fn detect(tail: &str) -> Option<Prompt>` — looks only at the last non-empty segment after `clean_output`.

- [ ] **Step 1: Write failing tests:** `yes_no_defaults` (`":: Proceed with installation? [Y/n] "` → answers `Yes`→`b"y\n"`, `No`→`b"n\n"`), `replace_conflict` (`":: python-x and python-y are in conflict. Remove python-y? [y/N] "`), `selection` (`"Enter a selection (default=all): "` → single answer `Default`→`b"\n"`), `sudo_password` (`"[sudo] password for tim: "` → Password), `gum_confirm` (recorded gum line ending in `Yes  No` → answers sending `b"y"`/`b"n"`), `reboot_yes_is_marked` (gum line containing `reboot` → Yes has `is_reboot_yes = true`), `plain_output_is_not_a_prompt`.
- [ ] **Step 2–4:** fail, implement, pass.
- [ ] **Step 5:** commit `inwindow: recognise a question the update is waiting on`.

### Task 4: Askpass

**Files:** Create `src/inwindow/askpass.rs`; Modify `src/main.rs` (if `args[1] == "--askpass"` → `std::process::exit(askpass::client_main(args[2..]))` before GTK starts).

**Interfaces:**
- Produces: `pub const SOCKET_ENV: &str = "ALMANAC_ASKPASS_SOCKET"`; `pub struct Server { pub path: PathBuf, .. }`; `Server::bind(dir: &Path) -> Result<Server, String>` (random name, mode 0600, removed on Drop); `fn accept(&self) -> Option<Request>` (non-blocking); `pub struct Request { pub prompt: String, pub attempt: u32 }`; `Request::answer(self, secret: Option<&str>)` (None = cancel); `pub fn client_main(args: &[String]) -> i32` (connects to `$SOCKET_ENV`, sends the prompt line, prints the secret + `\n` to stdout, returns 0; cancel/any error → 1).
- Peer check: `SO_PEERCRED` uid must equal `getuid()`, else the connection is dropped.
- Secret held as `Vec<u8>` zeroed on drop; never logged.

- [ ] **Step 1: Write failing tests:** `client_gets_the_answer` (bind in tempdir; thread runs `client_main` with env set; main accepts, answers `"pw"`; client stdout `pw\n`, exit 0), `cancel_exits_one`, `socket_is_private` (mode `0o600`), `askpass_counts_attempts` (three requests in one server lifetime → attempts 1,2,3).
- [ ] **Step 2–4:** fail, implement, pass. Then **verify sudo for real once by hand** (not a test): `script -qc 'SUDO_ASKPASS=/bin/false sudo -A -k true; SUDO_ASKPASS=/bin/false sudo -k true' /dev/null` and note whether plain `sudo` on a PTY prompts on the PTY (`[sudo] password for`) or calls askpass. Record the answer in the spec's Password paragraph. Either way Task 5 handles both (PTY prompt → `Prompt::Password`, answered from the same dialog). Do not run more than one failed attempt (faillock).
- [ ] **Step 5:** commit `inwindow: ask for the sudo password in Almanac`.

### Task 5: Headless run driver

**Files:** Create `src/inwindow/run.rs`.

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces: `pub enum Event { Changed, NeedsPassword { prompt: String, attempt: u32 }, Question(Prompt), Finished { code: i32 } }`; `pub struct UpdateRun`; `UpdateRun::start(argv: Vec<String>, env: Vec<(String,String)>, timeline: Timeline, log_path: PathBuf) -> Result<UpdateRun, String>` (binds askpass in `$XDG_RUNTIME_DIR`, sets `SUDO_ASKPASS=<current exe>` with arg routing via a wrapper env `ALMANAC_ASKPASS=1`, `SOCKET_ENV`); `fn poll(&mut self) -> Vec<Event>` (read PTY → full log file + bounded tail (2 000 lines) → tracker; every call refresh `session_commands`; detect prompts after 400 ms output idle; accept askpass); `fn answer_password(&mut self, secret: Option<&str>)`; `fn answer(&mut self, a: &Answer)` (refuses `is_reboot_yes`); `fn timeline(&self) -> &Timeline`; `fn log_tail(&self) -> String`; `fn sid(&self) -> u32`.
- Resolve: `UpdateRun::resolve(...)` = `start` with `OMARCHY_UPDATE_UNATTENDED=0` and the existing timeline; reboot questions auto-answered with the `No` answer.

- [ ] **Step 1: Write failing tests** using a fake updater script written to a tempdir (bash; spawns children named via `exec -a omarchy-snapshot sleep 0.2` etc., prints recorded pacman lines, optionally asks `Proceed? [Y/n]`, optionally calls `$SUDO_ASKPASS`): `a_clean_fake_update_completes` (all steps Done, rows Done, Finished{0}), `a_question_is_raised_and_answered`, `a_password_is_asked_via_askpass`, `reboot_yes_is_never_sent` (fake gum reboot prompt → driver answers No itself, `restart_note` set), `log_tail_is_bounded` (fake prints 10 000 lines → tail ≤ 2 000, log file has all).
- [ ] **Step 2–4:** fail, implement, pass.
- [ ] **Step 5:** commit `inwindow: drive an update headlessly`.

### Task 6: The timeline view

**Files:** Create `src/ui/update_view.rs`; Modify `src/ui/mod.rs`.

**Interfaces:**
- Consumes: `Timeline`, `Event`, `UpdateRun` (Task 5).
- Produces: `pub struct UpdateView`; `UpdateView::new(win: &Win) -> UpdateView`; `fn present(&self)`; `fn render(&self, t: &Timeline)`; `fn ask_password(&self, prompt: &str, attempt: u32, reply: impl Fn(Option<String>) + 'static)`; `fn ask(&self, p: &Prompt, reply: impl Fn(Answer) + 'static)`; `fn show_failure_fix(&self, fix: &Fix, on_fix: impl Fn() + 'static)`; `fn replace_with_report(&self, content: gtk::Widget)`; `fn append_log(&self, text: &str)`.
- Look (from the mockup): `adw::Dialog` (content width 560), header title/subtitle per Global Constraints, `gtk::ProgressBar` (fraction = done steps / total), step rows via `Win::note_row` with icons `object-select-symbolic` (done), spinner (running), `dialog-error-symbolic` (failed), dim label (waiting); the step whose rows are active shows an inset `gtk::ListBox` `.card` with app icons from the inventory icon lookup the Updates page uses and `version ✓` suffixes; `Safe to keep working` label (`.success` style); `gtk::Expander` `Details` with the monospace log tail. Password: in-dialog `adw::PasswordEntryRow` + Cancel/Update, subtitle `Wrong password — try again` when `attempt > 1`. Failure: inset card under the failed step with the reason, `Nothing was changed.`/`Some changes were made.` line, and the fix button (`Stop it and update`, `Resolve`, `Try again`).
- [ ] **Step 1:** add a GTK-free test `labels_and_positions` in `update_view.rs` for the pure helpers it uses (`subtitle(&Timeline) -> String` → `Step 4 of 6`; `changed_line(Changed) -> &str`).
- [ ] **Step 2–4:** fail, implement view + helpers, pass; then run Almanac in demo mode with a fake run (`ALMANAC_DEMO=1`, driver fed the Task 5 fake script) and screenshot it next to the Updates and Health pages (`omarchy-cmd-screenshot` or `grim`) — compare spacing, fonts, colours against the mockup.
- [ ] **Step 5:** commit `ui: in-window update timeline`.

### Task 7: Wire into Update all, background, notification

**Files:** Modify `src/ui/update_all.rs`, `src/ui/update_report.rs`, `src/ui/shell/app.rs:173`.

**Interfaces:**
- Consumes: Tasks 5–6; `postupdate::{probe_before, probe_after, build_report, scan_log, scan_output}`.
- Produces: `fn start_in_window(&self, run: SharedRun, rest: Vec<App>, busy: Busy)` in `update_all.rs`; `Run.in_window: bool`; `pub fn update_running() -> bool` (for the close guard).
- Behaviour: `start_update_all` takes the in-window path when `xdg::system_update_command() == "omarchy update"`, not demo, and `UpdateRun::start` succeeds; else today's terminal path. `system_command` no longer appends `flatpak update -y` on the in-window path (flatpak moves to the Other apps step). A `glib::timeout_add_local(150 ms)` pumps `poll()` → view. On `Finished{code}`: `probe_after` → `build_report` (log = run file text via `scan_output`), then run `update_rest_then` with its outcomes rendered as the `Other apps` step rows, then `replace_with_report` (no stacked dialog). Fix buttons: StopStuckUpdate → existing `Repair::StopUpdate` steps then restart run; Resolve → `UpdateRun::resolve`; RetryUpdate → `on_update_all`.
- Background: while `update_running()`, the window's close-request hides the window and `app.hold()` keeps the process; on finish, `gio::Notification` (`Update finished — {n} updated` / `Update needs a look`) and release the hold; a password/question while hidden calls `window.present()`. App Quit during a run → confirm dialog; during `SystemPackages` Running → refuse with `Quitting now would stop pacman part-way.`
- [ ] **Step 1: Write failing tests:** `system_command_leaves_flatpak_out_in_window`, `in_window_is_chosen_only_for_omarchy` (pure chooser fn `use_in_window(cmd: &str, demo: bool) -> bool`), `quit_is_refused_during_system_packages` (pure `quit_decision(&Timeline) -> QuitDecision { Allow, Confirm, Refuse }`).
- [ ] **Step 2–4:** fail, implement, pass.
- [ ] **Step 5: End-to-end on this machine.** Five real updates are waiting (cursor-bin, mise-bin, omarchy-speedtest, slack-desktop, voxtype-bin). Run the dev build, click Update all, enter the password in-window, and confirm: no terminal opens; steps tick in order; the five rows tick with their new versions; closing the window mid-run keeps it going and a notification arrives; the report shows every check passed; `pacman -Q` shows the new versions; `checkupdates` is empty; nothing holds `$XDG_RUNTIME_DIR/omarchy-update.lock`.
- [ ] **Step 6:** commit `Update all runs inside the window`.

### Task 8: Whole-suite verification

- [ ] `cargo clippy --all-targets` clean; `cargo test` all pass (+ the two location-dependent tests with the project `target/`); `cargo test --lib ui::shell::guard` (demo-gate guard) passes with the new code paths routed through `run_task` where they start tasks.
- [ ] Side-by-side screenshots of the update view, Updates, Health and Today pages; fix any visual drift.
- [ ] Commit any fixes.
