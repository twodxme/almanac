//! `almanac`: one window for everything installed on this machine. See
//! `ui::shell::run` for the command line (`--page NAME`, `--guard`).

fn main() -> gtk::glib::ExitCode {
    // A child the app starts to read file-type defaults from a fresh GIO
    // (mimeguard::snapshot); it never opens a window.
    if std::env::args().nth(1).as_deref() == Some(almanac::mimeguard::SNAPSHOT_FLAG) {
        almanac::mimeguard::print_snapshot();
        return gtk::glib::ExitCode::SUCCESS;
    }
    almanac::ui::shell::run()
}
