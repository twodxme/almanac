//! `almanac-autorun`: the non-pacman half of the scheduled update, for
//! omarchy-auto-update to call.
//!
//! `--core-holds` answers the script's other question: which pending
//! repository updates Almanac counts as core (the kernel, boot, firmware --
//! what can stop the machine booting). It prints one name per line, so the
//! script holds exactly what the app's own Updates page marks critical,
//! rather than keeping a second list that drifts from it.

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--core-holds") {
        let out = almanac::util::run(&["checkupdates"], 120);
        for line in out.stdout.lines() {
            if let Some(name) = line.split_whitespace().next() {
                if almanac::inventory::criticality(name) == "core" {
                    println!("{name}");
                }
            }
        }
        return;
    }
    std::process::exit(almanac::autorun::main(None));
}
