//! `almanac`: one window for everything installed on this machine. See
//! `ui::shell::run` for the command line (`--page NAME`, `--guard`).

fn main() -> gtk::glib::ExitCode {
    // A child the app starts to read file-type defaults from a fresh GIO
    // (mimeguard::snapshot); it never opens a window.
    if std::env::args().nth(1).as_deref() == Some(almanac::mimeguard::SNAPSHOT_FLAG) {
        almanac::mimeguard::print_snapshot();
        return gtk::glib::ExitCode::SUCCESS;
    }
    // Debug builds: `--preview-update-view[=running|failure|password|question|none]`
    // shows the in-window update with a fixed timeline, for screenshots; `none`
    // opens only the window (with `--page NAME`), as a separate instance.
    #[cfg(debug_assertions)]
    if let Some(which) = std::env::args_os().skip(1).find_map(|a| {
        a.to_str()?
            .strip_prefix("--preview-update-view")
            .map(|rest| rest.trim_start_matches('=').to_string())
    }) {
        return almanac::ui::shell::run_preview(which);
    }
    almanac::ui::shell::run()
}
