//! The application and the CLI.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::inventory;

use super::window::Win;

/// Opening straight to a page makes Almanac scriptable -- a bar widget or a
/// keybinding can land on Updates without a click -- and makes a screenshot
/// of a given page reproducible.
pub const PAGE_ALIASES: &[(&str, &str)] = &[
    ("featured", "featured"), ("home", "featured"), ("today", "featured"),
    ("store", "discover"), ("app-store", "discover"), ("discover", "discover"),
    ("themes", "themes"), ("theme", "themes"),
    ("updates", "updates"), ("update", "updates"),
    ("ai", inventory::AGENT), ("agents", inventory::AGENT),
    ("health", "maintenance"), ("maintenance", "maintenance"),
    ("history", "history"), ("timeline", "history"),
    ("plugins", "plugins"), ("plugin", "plugins"),
    ("switchboard", "switchboard"), ("switches", "switchboard"),
    ("apps", "apps"), ("applications", "apps"),
    ("all", "all"), ("everything", "all"),
    ("orphans", inventory::ORPHAN),
];

/// `PAGE_ALIASES.get(wanted)`.
pub fn resolve_page(wanted: &str) -> Option<&'static str> {
    PAGE_ALIASES.iter().find(|(k, _)| *k == wanted).map(|(_, v)| *v)
}

/// The D-Bus name, which the desktop file and metadata use too.
pub fn application_id() -> &'static str {
    "io.github.twodxme.Almanac"
}

/// Almanac's own glyphs in the checkout this binary was built in, when that
/// is still there and can be trusted (`util::build_checkout`). A package
/// installs them into hicolor instead.
pub fn icons_dir() -> Option<PathBuf> {
    crate::util::build_checkout().map(|c| c.join("icons"))
}

/// `AlmanacApp(Adw.Application)`.
#[derive(Clone)]
pub struct AlmanacApp {
    pub app: adw::Application,
    pub start_page: Rc<RefCell<String>>,
    /// The one window, kept alive here (`self.props.active_window`).
    pub win: Rc<RefCell<Option<Win>>>,
}

impl AlmanacApp {
    pub fn new() -> AlmanacApp {
        let app = adw::Application::builder()
            .application_id(application_id())
            .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
            .build();
        app.add_main_option(
            "page",
            glib::Char::from(b'p'),
            glib::OptionFlags::NONE,
            glib::OptionArg::String,
            "Open on a specific page: store, updates, ai, health, apps, all",
            Some("PAGE"),
        );
        let this = AlmanacApp {
            app,
            start_page: Rc::new(RefCell::new(String::new())),
            win: Rc::new(RefCell::new(None)),
        };
        let t = this.clone();
        this.app.connect_handle_local_options(move |_app, opts| t.local_options(opts));
        let t = this.clone();
        this.app.connect_command_line(move |_app, cmd| t.command_line(cmd));
        let t = this.clone();
        this.app.connect_startup(move |_app| t.startup());
        let t = this.clone();
        this.app.connect_activate(move |_app| t.activate());
        this
    }

    /// `do_command_line`: `--page NAME` picks the first page.
    pub fn command_line(&self, cmd: &gio::ApplicationCommandLine) -> glib::ExitCode {
        let wanted = wanted_page(&cmd.options_dict());
        if !wanted.is_empty() {
            match resolve_page(&wanted) {
                None => {
                    // Normally caught by `local_options` first; kept for a
                    // command line that reaches the primary instance some
                    // other way.
                    eprint!("{}", unknown_page_message(&wanted));
                    return glib::ExitCode::from(2);
                }
                Some(resolved) => *self.start_page.borrow_mut() = resolved.to_string(),
            }
        }
        self.app.activate();
        glib::ExitCode::SUCCESS
    }

    /// The `--page` check of `do_command_line`, made in the invoking process.
    ///
    /// The Python reports an unknown page with `printerr_literal`, which
    /// prints on the stderr of whoever ran the command -- also when another
    /// Almanac is already running and receives the command line. That call
    /// needs gio's v2_80 feature, which the crate does not enable, and a plain
    /// `eprint!` in `command_line` lands on the *running* instance's stderr
    /// instead. Checking here, before the command line is handed on, prints
    /// in the right terminal with the same text and exit status 2 -- and,
    /// like the Python, still does so where no display can be opened (GTK is
    /// not started yet).
    pub fn local_options(&self, opts: &glib::VariantDict) -> std::ops::ControlFlow<glib::ExitCode> {
        let wanted = wanted_page(opts);
        if !wanted.is_empty() && resolve_page(&wanted).is_none() {
            eprint!("{}", unknown_page_message(&wanted));
            return std::ops::ControlFlow::Break(glib::ExitCode::from(2));
        }
        std::ops::ControlFlow::Continue(())
    }

    /// `do_startup`: Almanac's own glyphs, shipped beside the code so they
    /// look the same under any icon theme. (The Adw.Application default
    /// handler runs first: "startup" is a run-first signal.)
    pub fn startup(&self) {
        crate::ui::timings::mark("startup");
        if let (Some(display), Some(icons)) = (gdk::Display::default(), icons_dir()) {
            gtk::IconTheme::for_display(&display).add_search_path(icons);
        }
    }

    /// `do_activate`: create the window once, land on the requested page,
    /// present it.
    pub fn activate(&self) {
        let existing = self.win.borrow().clone();
        let win = match existing {
            Some(w) => w,
            None => {
                let w = Win::new(&self.app);
                crate::ui::timings::mark("window built");
                w.window.connect_map(|_| crate::ui::timings::mark("window mapped"));
                w.window.add_tick_callback(|_, _| {
                    crate::ui::timings::mark("first frame");
                    glib::ControlFlow::Break
                });
                let weak = w.downgrade();
                w.window.connect_close_request(move |_| match weak.upgrade() {
                    Some(w) => AlmanacApp::on_close(&w),
                    None => glib::Propagation::Proceed,
                });
                *self.win.borrow_mut() = Some(w.clone());
                // Old thumbnails and cache files, orphaned AppImage copies:
                // at most once a day, never on the main thread.
                let _ = std::thread::Builder::new()
                    .name("housekeeping".into())
                    .spawn(crate::housekeeping::run_if_due);
                // Almanac's own log keeps 30 days; a window opened after a
                // quiet month should not show what has expired. Never in
                // demo mode, which must not touch the machine's files.
                if !crate::demo::active() {
                    let _ = std::thread::Builder::new()
                        .name("history-expiry".into())
                        .spawn(crate::history::expire);
                    // Timer units an older Almanac put in ~/.config override
                    // the packaged ones (and run an older script) until
                    // they are replaced.
                    let _ = std::thread::Builder::new().name("timer-migration".into()).spawn(|| {
                        if let Err(e) = crate::autoupdate::migrate_legacy_units() {
                            eprintln!("almanac: {e}");
                        }
                    });
                }
                w
            }
        };
        let start = self.start_page.borrow().clone();
        if !start.is_empty() {
            // After the restored category and the first inventory pass, so a
            // late callback cannot pull the window off the requested page.
            let weak = win.downgrade();
            glib::idle_add_local_full(glib::Priority::LOW, move || {
                if let Some(w) = weak.upgrade() {
                    w.select_category(&start);
                }
                glib::ControlFlow::Break
            });
        }
        win.window.present();
    }

    /// `_on_close`: save the window state; let the close proceed.
    pub fn on_close(win: &Win) -> glib::Propagation {
        win.on_close()
    }
}

impl Default for AlmanacApp {
    fn default() -> Self {
        AlmanacApp::new()
    }
}

/// `(opts.get("page") or "").strip().lower()`.
fn wanted_page(opts: &glib::VariantDict) -> String {
    let page = opts.lookup::<String>("page").ok().flatten().unwrap_or_default();
    py_strip(&page).to_lowercase()
}

/// The text `do_command_line` prints for a page it does not know.
fn unknown_page_message(wanted: &str) -> String {
    let keys: BTreeSet<&str> = PAGE_ALIASES.iter().map(|(k, _)| *k).collect();
    format!(
        "almanac: unknown page {}. Try one of: {}\n",
        py_repr_str(wanted),
        keys.into_iter().collect::<Vec<_>>().join(", ")
    )
}

/// `str.isspace()` for one character: Unicode White_Space plus the ASCII
/// separators U+001C..U+001F, which Python counts as whitespace.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

/// Python's `str.isprintable()` for one character: everything but the
/// Unicode categories Other (Cc, Cf, Cs, Co, Cn) and Separator (Zl, Zp, Zs),
/// with the ASCII space let through.
fn py_printable(c: char) -> bool {
    static NON_PRINTABLE: std::sync::OnceLock<Option<regex::Regex>> = std::sync::OnceLock::new();
    if c == ' ' {
        return true;
    }
    match NON_PRINTABLE.get_or_init(|| regex::Regex::new(r"^[\p{C}\p{Z}]$").ok()) {
        Some(re) => !re.is_match(c.encode_utf8(&mut [0; 4])),
        None => !c.is_control(),
    }
}

/// `repr(str)` for the error message: single quotes unless the text holds one
/// and no double quote; what is not printable escaped as Python escapes it.
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        let n = c as u32;
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if py_printable(c) => out.push(c),
            _ if n < 0x100 => out.push_str(&format!("\\x{n:02x}")),
            _ if n < 0x10000 => out.push_str(&format!("\\u{n:04x}")),
            _ => out.push_str(&format!("\\U{n:08x}")),
        }
    }
    out.push(quote);
    out
}

/// The entry point for `main.rs`: `sys.exit(AlmanacApp().run(sys.argv))`.
///
/// `almanac --guard` is the background settings watcher: no window, so it is
/// dispatched before any GTK work.
pub fn run() -> glib::ExitCode {
    // `sys.argv` holds an argument that is not UTF-8 too (surrogate-escaped);
    // `env::args` would panic on it, so the bytes are compared.
    if std::env::args_os().skip(1).any(|a| a == "--guard") {
        let code = crate::watcher::main();
        crate::history::flush();
        return glib::ExitCode::from(u8::try_from(code).unwrap_or(1));
    }
    crate::ui::timings::launch();
    let code = AlmanacApp::new().app.run();
    // History lines the window recorded are written by a thread of their
    // own: let it finish before the process ends.
    crate::history::flush();
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_aliases_resolve() {
        assert_eq!(resolve_page("health"), Some("maintenance"));
        assert_eq!(resolve_page("store"), Some("discover"));
        assert_eq!(resolve_page("orphans"), Some(inventory::ORPHAN));
        assert_eq!(resolve_page("nope"), None);
    }

    /// `str.strip().lower()` as `do_command_line` reads `--page`.
    #[test]
    fn page_name_is_stripped_like_python() {
        assert_eq!(py_strip("\x1c updates\u{a0}\x1f"), "updates");
        assert_eq!(py_strip("\u{200b}x"), "\u{200b}x"); // not whitespace to Python either
        let msg = unknown_page_message("nope");
        assert!(msg.starts_with("almanac: unknown page 'nope'. Try one of: agents, ai, all, app-store,"));
        assert!(msg.ends_with("today, update, updates\n"));
    }

    #[test]
    fn py_repr_quotes() {
        assert_eq!(py_repr_str("abc"), "'abc'");
        assert_eq!(py_repr_str("it's"), "\"it's\"");
        assert_eq!(py_repr_str("a\u{200b}\u{a0}\x1c\u{e000}\u{10ffff}é日"), "'a\\u200b\\xa0\\x1c\\ue000\\U0010ffffé日'");
        assert_eq!(py_repr_str("a\"b'c"), "'a\"b\\'c'");
    }
}
