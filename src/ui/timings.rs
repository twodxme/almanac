//! `ALMANAC_TIMINGS=1`: how long from launch to each step of a usable
//! window, on stderr, each step once. For measuring start-up; silent
//! otherwise.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

fn start() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var_os("ALMANAC_TIMINGS").is_some_and(|v| !v.is_empty() && v != "0")
    })
}

/// The clock starts here: call it first thing.
pub fn launch() {
    start();
    mark("launch");
}

/// `what` happened now; printed the first time only.
pub fn mark(what: &str) {
    if !enabled() {
        return;
    }
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.iter().any(|s| s == what) {
        return;
    }
    seen.push(what.to_string());
    eprintln!(
        "almanac: {:>7.1} ms  {what}",
        start().elapsed().as_secs_f64() * 1000.0
    );
}
