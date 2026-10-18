//! `autorun.py` run as a script (`raise SystemExit(main())`): the
//! non-pacman half of the scheduled update, for omarchy-auto-update to call
//! in place of `python3 autorun.py`.

fn main() {
    std::process::exit(almanac::autorun::main(None));
}
