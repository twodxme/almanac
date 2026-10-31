//! `almanac-autorun`: the non-pacman half of the scheduled update, for
//! omarchy-auto-update to call.
//!
//! `--core-holds` answers the script's other question: which pending
//! updates Almanac counts as core (the kernel, boot, firmware, DKMS
//! modules -- what can stop the machine booting). It prints one name per line, so the script
//! holds exactly what the app's own Updates page marks critical, rather
//! than keeping a second list that drifts from it.
//!
//! `--core-holds --stdin` classifies the lines the script passes in (its
//! `checkupdates` and `yay -Qua` output alike, `name old -> new`), so AUR
//! updates are held too. Without `--stdin` it runs `checkupdates` itself,
//! and fails closed: a check that did not finish exits 3 with the reason
//! on stderr, never 0 with an empty list the script would read as "nothing
//! to hold".
//!
//! `--record-hold TITLE DETAIL` puts a run that stopped short -- held for a
//! core package or Arch news, skipped on battery, failed -- on the History
//! page, so a notification that went unseen is not the only record of why
//! nothing was installed.

use std::io::Read;

/// The exit status for a `--core-holds` that could not decide.
const UNDECIDED: i32 = 3;

fn core_holds(rest: &[String]) -> i32 {
    let names = match rest {
        [] => almanac::autorun::core_holds_checked(&almanac::util::run(&["checkupdates"], 120)),
        [flag] if flag == "--stdin" => {
            let mut raw = Vec::new();
            std::io::stdin()
                .read_to_end(&mut raw)
                .map_err(|e| format!("reading stdin: {e}"))
                .and_then(|_| almanac::autorun::core_holds(&String::from_utf8_lossy(&raw)))
        }
        _ => Err(format!(
            "unexpected arguments after --core-holds: {}",
            rest.join(" ")
        )),
    };
    match names {
        Ok(names) => {
            for name in names {
                println!("{name}");
            }
            0
        }
        Err(e) => {
            eprintln!("almanac-autorun --core-holds: {e}");
            UNDECIDED
        }
    }
}

fn record_hold(rest: &[String]) -> i32 {
    let [title, detail] = rest else {
        eprintln!("usage: almanac-autorun --record-hold TITLE DETAIL");
        return 2;
    };
    let history =
        almanac::history::record_checked(almanac::history::AUTOUPDATE, title, detail, "warn", &[]);
    match history {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("almanac-autorun --record-hold: {e}");
            1
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--core-holds") => std::process::exit(core_holds(&args[1..])),
        Some("--record-hold") => {
            let code = record_hold(&args[1..]);
            almanac::history::flush();
            std::process::exit(code)
        }
        _ => {}
    }
    let code = almanac::autorun::main(None);
    // process::exit does not wait for History's writer thread.
    almanac::history::flush();
    std::process::exit(code);
}
