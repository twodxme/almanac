//! `almanac-rs`: the Rust build of Almanac, with the same command line as
//! `app.py` (`--page NAME`, `--guard`). See `ui::shell::run`.

fn main() -> gtk::glib::ExitCode {
    almanac::ui::shell::run()
}
