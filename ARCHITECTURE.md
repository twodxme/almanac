# How Almanac is built

Almanac is a GTK 4 / libadwaita app in Rust. Up to 1.7 it was Python; 2.0 is
a line-for-line port of that app, held to it case by case until the port was
complete, and the Python was then retired (it is in the git history up to
v2.0.0). The comments carried over from it are the design record: where a
comment explains *why*, keep it when you change the code beside it.

## Layout

| | |
|---|---|
| `src/<module>.rs` | the logic: `inventory`, `store`, `maintenance`, `agents`, `plugins`, `theming`, … one module per concern |
| `src/inventory.rs` + `src/inventory/` | the inventory: `collect`, criticality and duplicates at the top; one file per source (`pacman`, `appimages`, `flatpak`, `local`), the row type (`app`), staleness (`health`), search (`discover`, `plugin_registry`) and Python's str/json (`py`). Everything public is re-exported, so callers say `inventory::x` |
| `src/selfupdate.rs` + `src/selfupdate/` | Almanac updating Almanac: the inventory row at the top; `checkout` (install kind, git state), `check` (what an update would be), `restart`, `notes` (What's New, through `et`, a port of ElementTree on expat), and what Python gave it (`process`, `pystr`, `tables`). Re-exported the same way |
| `src/appimage.rs` + `src/appimage/` | AppImage tracking: `release` (the update source an AppImage carries, version keys, its newest GitHub release), `download` (every URL scheme, length/ELF/digest checks, the swap that keeps a `.previous` copy, the download lock), `desktop` (launcher entry and icon), `urllib` (the HTTP/TLS client, redirects, proxies), `pyisms` |
| `src/ui/*.rs` | the window, one file per section (see UI below) |
| `src/util.rs` | subprocess with timeout, rounding and title-casing that match what users saw in 1.x, ISO dates, atomic JSON — **use it, do not re-implement** |
| `src/main.rs` | the `almanac` binary |
| `src/bin/almanac-autorun.rs` | the non-pacman half of the scheduled update, and `--core-holds [--stdin]` for omarchy-auto-update (which pending repo/AUR updates are core; exits 3, never an empty list, when it cannot tell) |
| `data/style.css` | the stylesheet, loaded with `include_str!` |
| `tests/ported.rs` | cross-module checks (translation template, icons, sources) |

## Conventions

- **Errors**: a function whose failure the caller must see returns
  `Result<T, String>`; one that has a sensible fallback stays infallible and
  falls back. No panics in non-test code on anything that can fail at run
  time (I/O, parsing, subprocesses, network). Several modules keep a lenient
  function beside a `try_` / `_checked` one; UI code uses the fallible one.
  `Err` texts keep Python's spelling (`TypeError: ...`, `[Errno 2] ...`),
  which the tests pin; the display points (`say`, `notify`, `Win::plain`
  subtitles, the task log) pass them through `util::human_error`, which
  puts them in plain English. Show errors through those, not around them.
- **No `unsafe`**, with one documented exception: `icons.rs` turns on TLS
  post-handshake authentication through `openssl-sys`, because some vendors'
  sites (openai.com, gnome.org) refuse a TLS hello without it.
- **Files on disk**: `~/.local/state/almanac`, `~/.cache/almanac`,
  `~/.config/almanac`, written through `util::write_json` (atomic, with the
  same formatting 1.x used), so users keep their settings and history across
  versions. Change a format only with a migration. `housekeeping` prunes
  what only grows (month-old thumbnails and GitHub answers, rollback copies
  of AppImages no longer registered) once a day in the background.
- **Subprocesses** go through `util::run` / `util::run_with` (timeouts
  that cover the output, the whole process group killed, never panic), or
  `util::run_bytes` where the bytes matter (strict decoding as Python's
  `text=True` does, OS-string arguments). `RunOpts` covers stdin (null,
  inherited, input, a file), env, cwd, argv[0] and discarding stderr; a
  failed start keeps its errno, an exit its signal. Do not write another
  runner: add the option. GitHub goes through `ghcache` (token, ETags, budget guard).
- **Demo mode** (`ALMANAC_DEMO=1`) swaps every machine reader for fixtures in
  `demo.rs`; screenshots depend on it, and it must never show anything from
  the real machine.
- **i18n**: user-facing strings go through `crate::i18n::tr("text")`;
  `po/update.sh` refreshes the template. Catalogs are read as UTF-8 (what
  msgfmt writes); one in another charset is skipped, untranslated.
- **Never change the machine from a test.** No package installs or removals,
  no `systemctl enable/disable`, no `git pull` in real checkouts, no writes to
  the real `~/.config`, `~/.local/state`, `~/.cache`: point `XDG_*` / `HOME`
  at a temp dir, and test the parts that decide (the argv, the plan, the
  parse) rather than running them.

## Deep input

The app reads caches and state files a user (or a bug) could have nested tens
of thousands of levels deep. Code must never recurse once per level of such
input on a stack it does not size: a stack overflow aborts the whole process,
on any thread.

- Value types that mirror JSON (`PyVal`, `PyValue`, ...) get `Clone`,
  `PartialEq` and `Drop` without recursion: `util::tree_clone_eq!` and a
  hand-written `Drop` that takes children out flat. Conversions use
  `util::tree::fold` / `fold_ids` (arenas); a deep `serde_json::Value` is
  cloned, compared and dropped with `util::tree::{clone_value, eq_value, drop_value}`.
- Python's `json.loads` is `util::pyjson::loads` (or `loads_into` for an
  arena): serde_json first, an iterative CPython-compatible scanner for what
  serde refuses, straight into the module's own value type. It never
  recurses; give it the module's depth limit (`Limit`) and error text.
- Where recursion is simpler and bounded, run it on
  `util::with_stack(util::DEEP_STACK, ...)`; walks that may go very deep
  through serde use `util::JSON_STACK` (`util::with_stack_named` when the
  thread must not take the caller's name).
- A value that leaves a module for code that walks it recursively is bounded
  at the boundary: `ghcache::try_get_json` and its siblings refuse a body
  nested past `ghcache::VALUE_DEPTH` (512) with Python's RecursionError, so
  store.rs, sources.rs, agents.rs and picks.rs never see a deep one;
  verify.rs converts an app's `extra` only `EXTRA_DEPTH` (64) levels down.
- `ui::shell::spawn_bg` runs background work on an 8 MiB thread
  (`BG_STACK`): Python's own thread stack, which the depth limits copied from
  it (sources 2000, security 1000, inventory 900) were measured on.
  `util::JSON_STACK` is left where code still recurses as deep as Python's
  json reads: ghcache's cache writer and conversions (`ghcache::deep`),
  agents (`agents::deep`, its pool workers) and picks (`picks::_deep`). New
  code that needs it starts its own. The inventory collector threads are
  back on 8 MiB (`inventory::COLLECTOR_STACK`): nothing they hold is deeper
  than 900 levels, since agents drop their raw cache values on their own
  deep threads.

## UI (`src/ui/`)

| file | what |
|---|---|
| `shell.rs` + `shell/` | the shared frame, re-exported as `shell::…`: `helpers` (labels, formatting, the stylesheet, `spawn_bg`), `items` (list items), `tables` (sidebar groups, pages, sorts), `window` (`Win` / `Inner` / `Shared`, the texture cache), `build` (`Win::new`: CSS and theme providers, sidebar, header, pages), `columns`, `state` (shortcuts, persisted state), `watch` (change detection), `app` (the application and its command line) |
| `packages.rs` + `packages/` | data loading and filtering: Updates, Applications, Everything, Orphans tables, the details sheet — `extra` (reading the `extra` dict as Python did), `decisions` (pure matching, grouping, sorting), `loading`, `details`, `counts` (badges, stats, sidebar counts), `filtering` (sort menu, filter, search, sidebar selection), `chrome` (page chrome, primary action, theme, status line) |
| `store.rs` | App Store and Themes pages: grid, cards, marketplace data |
| `store_actions.rs` | install/remove from the store, setup after install, themes, AppImages |
| `plugins.rs` + `plugins/` | Plugins page; on/off, shortcuts, bulk updates, the shell — `page` (pills, filter, rows, logos, menus, switches), `updates` (bulk update planning and its confirmation), `controls` (on/off, shortcuts, restarting the shell), `removal` (install, remove, orphan cleanup), `pyisms` |
| `switchboard.rs` | Switchboard |
| `history.rs` | Health check glue and History |
| `ai.rs` | AI page |
| `preferences.rs` | Preferences dialog |
| `today.rs` | Today page, hero, picks |
| `actions.rs` + `actions/` | row actions, verification, file takeovers, install from a link, update one, AppImage add — `rows` (row actions, self-update, About), `verification`, `takeovers`, `link`, `update` (update one, AppImage update and removal), `appimage_add`, `pyisms` |
| `update_all.rs` | Update all: the system update in a terminal, then every other updater as one batch |

- **The window** is `ui::shell::Win`: `#[derive(Clone)] pub struct Win(Rc<Inner>)`
  with `Deref<Target = Inner>`. `Inner` holds the `adw::ApplicationWindow`,
  the widgets shared across sections, `shared: RefCell<shell::Shared>` (state
  read by more than one section, e.g. the plugin list), and one
  `RefCell<<section>::State>` per section. Each section file defines
  `#[derive(Default)] pub struct State` holding its own widgets and data.
- **Methods** live in `impl Win { … }` blocks in their section's file; a
  section split into a directory (`shell/`, …) spreads them across its files.
- **Callbacks** hold a weak handle: `let w = self.downgrade();
  btn.connect_clicked(move |_| if let Some(w) = w.upgrade() { w.on_x(); });`
- **Background work** → `shell::spawn_bg(move || work(), move |result| on_main_thread(result))`.
- **Never hold a `RefCell` borrow across a call into GTK** — a signal handler
  firing synchronously will borrow again and panic. Clone what you need, drop
  the borrow, then call.
- **List models**: rows are `glib::BoxedAnyObject` wrapping the Rust value
  (`AppItem`, `PluginItem`).
- **One visual system**: every page uses the same CSS classes, spacing scale,
  pills and boxed lists. After any visual change, regenerate the screenshots
  (`tools/screenshots.sh`) and compare every page, not just the one changed.

## Command line

`almanac [--page NAME] [--guard]`: `--page` opens a page (`today`, `store`,
`updates`, `ai`, `health`, `history`, `plugins`, `switchboard`, … — an
unknown name lists them), `--guard` runs the background settings watcher with
no window. The application id is `io.github.twodxme.Almanac`.
