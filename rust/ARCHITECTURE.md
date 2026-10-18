# Almanac in Rust: how the port is built

This crate is a **1:1 port** of the Python app in the directory above
(`../*.py`). The Python is the specification: same behaviour, same output,
same files on disk, same look. Where the Python has a comment or docstring
explaining *why*, the port keeps it as a Rust comment — those notes are the
design record and the user cares about them. Match the Python's comment
density and naming; do not invent new behaviour, do not "improve" logic.

## Layout

| Python | Rust |
|---|---|
| `xdg.py`, `inventory.py`, … (34 modules) | `src/xdg.rs`, `src/inventory.rs`, … same names |
| `app.py` | `src/ui/*.rs`, one file per section (see UI below) |
| `test_app.py` | `#[cfg(test)] mod tests` at the bottom of each ported file |
| `_CSS` in `app.py` | `data/style.css`, loaded with `include_str!` |
| — | `src/util.rs`: subprocess with timeout, Python rounding/title/repr, ISO dates, atomic JSON — **use it, do not re-implement** |
| — | `src/main.rs`: the `almanac-rs` binary (same CLI as `app.py`) |
| — | `src/bin/almanac-parity.rs`: the parity runner |

## Work-in-progress files (how parallel porting stays green)

Every module has two files: `x.rs` (last version that compiled and passed)
and `x.wip.rs` (being worked on). Cargo feature `wip-x` (underscores become
dashes: `wip-plugin-setup`, UI: `wip-ui-store-actions`) builds the crate with
the `.wip.rs` copy. So:

1. Edit **only** your own `src/<m>.wip.rs` (and your `parity/cases_<m>.py`).
2. Build and test with your feature: `--features wip-<m>`.
3. When it compiles, its tests pass, and parity passes: **promote** it with
   `cp src/<m>.wip.rs src/<m>.rs`. Promote early and often; other agents
   build against the promoted copy.
4. Never edit another module's files. If you need something from a module
   that is still a stub, code against the signature it has, and say so in
   your report. If a signature in another module is wrong, report it.

## Building (shared machine: 12 CPUs, 15 GB RAM)

Always build through the gate, with your own target directory:

```sh
cd /home/tim/.local/share/almanac/rust
export CARGO_TARGET_DIR=$PWD/target/agents/<your-module>
[ -d "$CARGO_TARGET_DIR" ] || { mkdir -p target/agents; cp -a --reflink=always target/seed "$CARGO_TARGET_DIR"; }
tools/cargo-slot cargo check --features wip-<m>
tools/cargo-slot cargo test  --features wip-<m> --lib <m>::
```

`tools/cargo-slot` allows three builds at once across all agents; it waits
for a free slot. `target/seed` has every dependency prebuilt. To add a crate
use `cargo add <crate>` (never hand-edit Cargo.toml) and say so in your report.

No `unsafe`. No panics in non-test code on anything that can fail at runtime
(I/O, parsing, subprocesses, network) — mirror the Python's `except` and
fall back the same way it does. `unwrap()` only where the Python itself
could not fail.

## Translating Python idioms

- **Names**: modules, functions, constants keep the Python names
  (`inventory::collect`, `theming::TEXT_TARGET`). Classes and dataclasses
  keep their names as structs (`inventory::App`, `store::Plugin`), fields
  keep their snake_case names, so JSON is identical.
- **Dataclasses** → `#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]`
  structs with `#[serde(default)]`. A Python `@property` becomes a method of
  the same name, and is **also** included when the parity hook serialises the
  struct if the Python tests or UI read it (add it via a wrapper or
  `serde_json::json!`).
- **dict with a fixed shape** → struct. **dict with dynamic keys** →
  `BTreeMap<String, T>` (or `serde_json::Value` when values are mixed).
- **`None` vs `""`**: keep exactly what Python returns. `str | None` →
  `Option<String>`; a function returning `""` for "nothing" returns `String`.
- **Exceptions**: a Python function that raises to its caller returns
  `Result<T, String>`; the parity hook reports an error as
  `{"error": "<message>"}`. Functions that catch and fall back stay infallible.
- **Rounding**: Python `round()` is round-half-even → `util::py_round`.
  `f"{x:.1f}"` → `format!("{x:.1}")` (both round the exact value correctly;
  parity will tell you if not). `str(float)`/`repr` → `util::py_float_repr`.
- **subprocess.run(..., timeout=n)** → `util::run(&argv, n)` /
  `util::run_with`. `shutil.which` → `util::which`. `Path.home()` →
  `util::home()`. `time.time()` → `util::now()`.
- **JSON state files** → `util::read_json` / `util::write_json` (atomic,
  Python-identical formatting: pass the same `indent`/`sort_keys` the Python
  passes). The Rust app reads and writes the **same files** the Python does
  (`~/.local/state/almanac`, `~/.cache/almanac`): same names, same schema,
  so a user switching from Python keeps their settings and history.
- **Threads** in non-UI code (`ThreadPoolExecutor`) → `std::thread::scope`.
- **Network** (`urllib`) → `ureq` 3.x (already a dependency). GitHub goes
  through `ghcache`, as in the Python.
- **Demo mode** (`ALMANAC_DEMO=1`): every `if demo.active(): return demo.x()`
  branch is ported exactly; screenshots depend on it.
- **i18n**: `_("text")` → `crate::i18n::tr("text")` (returns `String`).
- **Dates**: `datetime` → `chrono`; ISO strings → `util::iso_ts`.

## Parity: proving the port matches

Every module exposes

```rust
pub fn parity(func: &str, args: &serde_json::Value) -> Option<serde_json::Value>
```

mapping a function name to a call of the real function and its result as
JSON (`serde_json::to_value`), `None` for names it does not know. Its
counterpart `parity/cases_<m>.py` yields cases:

```python
def cases():
    yield "contrast", {"a": "#fff", "b": "#000"}, lambda: theming.contrast("#fff", "#000")
```

Run: `RUST_FEATURES=wip-<m> python3 parity/run.py <m>` (and again with
`--demo`). Cover **every public function**, with ordinary inputs, edge cases
(empty, malformed, unicode), and — for modules that read the machine — the
real machine as it is. Python's output is the answer; any difference is a
Rust bug unless the Python result is nondeterministic (time, random), in
which case pin the input (pass `now` explicitly, use fixed files in a temp
dir).

**Safety: parity and tests must never change this machine.** No package
installs/removals, no `systemctl enable/disable`, no `git pull` in real
checkouts, no writes to `~/.config`, `~/.local/state`, `~/.cache` of the real
user. For functions with side effects, test the parts that decide (the argv
they would run, the plan, the parse) and point file-writing functions at a
temp dir via `XDG_STATE_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` / `HOME`
set in both the Python case and the Rust args.

## Tests

Port every test in `../test_app.py` that exercises your module, into your
file's `#[cfg(test)] mod tests`, same name in snake_case, same assertions.
`grep -n "<module>\." ../test_app.py` finds them. Tests that need a display
(GTK widgets) belong to the UI sections.

## UI (`src/ui/`)

`app.py` is split into sections, each a file under `src/ui/`:

| file | `app.py` lines | what |
|---|---|---|
| `shell.rs` | 1–1454, 6481–end | helpers, CSS, list items, the window, sidebar, header, columns, persisted state, change detection, `AlmanacApp` and the CLI |
| `packages.rs` | 1454–2481 | data loading and filtering: Updates, Applications, Everything, Orphans tables, detail |
| `store.rs` | 2481–3153 | App Store and Themes pages: grid, cards, marketplace data |
| `store_actions.rs` | 3153–3675 | install/remove from the store, setup after install, themes, AppImages |
| `plugins.rs` | 3695–3993, 5636–5999 | Plugins page; on/off, shortcuts, bulk updates, the shell |
| `switchboard.rs` | 3993–4221 | Switchboard |
| `history.rs` | 3675–3695, 4221–4486 | maintenance glue and History |
| `ai.rs` | 4486–4876 | AI page |
| `preferences.rs` | 4876–5096 | Preferences dialog |
| `today.rs` | 5096–5530 | Today page, hero, picks |
| `actions.rs` | 5530–5636, 5999–6481 | actions, verification, file takeovers, install from link, update one/all, AppImage add |

Conventions (the shell defines these; every section uses them):

- **The window** is `ui::shell::Win`: `#[derive(Clone)] pub struct Win(Rc<Inner>)`
  with `Deref<Target = Inner>`. `Inner` holds the `adw::ApplicationWindow`,
  the widgets shared across sections, `shared: RefCell<shell::Shared>` (state
  read by more than one section, e.g. the plugin list), and one
  `RefCell<<section>::State>` per section (`store: RefCell<store::State>`).
  Each section file defines `#[derive(Default)] pub struct State` holding its
  own widgets and data — a Python `self._x` used only by that section lives
  there.
- **Methods**: every Python method becomes `impl Win { pub fn name(&self, …) }`
  in its section's file, same name (leading underscore dropped:
  `_feat_reload` → `feat_reload`). Rust allows `impl Win` blocks in many files.
- **Callbacks** hold a weak handle: `let w = self.downgrade();
  btn.connect_clicked(move |_| if let Some(w) = w.upgrade() { w.on_x(); });`
- **Background work** (`threading.Thread` + `GLib.idle_add`) →
  `shell::spawn_bg(move || work(), move |result| on_main_thread(result))`.
- **Never hold a `RefCell` borrow across a call into GTK** — a signal handler
  firing synchronously will borrow again and panic. Clone what you need, drop
  the borrow, then call.
- **List models**: rows are `glib::BoxedAnyObject` wrapping the Rust value
  (`AppItem`, `PluginItem`). Card/row parts that Python stashed on widgets
  (`item._parts`) go in a small struct you can reach without `unsafe`
  (e.g. a tiny `glib::wrapper!` widget subclass, or a side table).
- **CSS** is `data/style.css`, verbatim from `_CSS`. Do not restyle anything.
- **Look**: the same widgets, classes, sizes and spacing as the Python, so
  the pages are pixel-comparable. Check with
  `tools/shot.sh py <page> a.png; tools/shot.sh rs <page> b.png` and look at
  both images.

## CLI

`almanac-rs` accepts exactly what `app.py` accepts (`--page …`, `--guard`,
`--autorun`, …). Its application id is `io.github.twodxme.Almanac`, except
when run as `almanac-rs`, where it is `io.github.twodxme.Almanac.Rs` so it
can run beside the Python app without the two fighting over one D-Bus name.
