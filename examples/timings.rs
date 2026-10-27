//! Times the work behind the main pages, cold and warm, for before/after
//! comparisons: `cargo run --release --example timings`. Read-only.

use std::time::Instant;

fn time<T>(label: &str, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    println!("{label:32} {:>8.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    out
}

fn main() {
    let apps = time("inventory::collect (first)", almanac::inventory::collect);
    time("inventory::collect (again)", almanac::inventory::collect);
    println!("{:32} {:>8}", "  apps", apps.len());
    if std::env::var_os("BREAKDOWN").is_some() {
        use almanac::inventory as inv;
        let _ = time("  packages", inv::packages);
        let names: Vec<String> = String::from_utf8(std::process::Command::new("pacman").arg("-Qeq").output().unwrap().stdout).unwrap().lines().map(String::from).collect();
        let _ = time("    pending_both", inv::pending_both);
        let _ = time("    package_info", || inv::package_info(&names));
        let _ = time("    desktop_map", || inv::desktop_map(&names));
        let aur: Vec<String> = String::from_utf8(std::process::Command::new("pacman").arg("-Qmq").output().unwrap().stdout).unwrap().lines().map(String::from).collect();
        let _ = time("    try_aur_health", || inv::try_aur_health(&aur));
        let _ = time("    security::vulnerabilities", || almanac::security::vulnerabilities(None, None));
        let _ = time("  appimages", || inv::appimages(true));
        let _ = time("  flatpaks", inv::flatpaks);
        let _ = time("  local_builds", || inv::local_builds(true));
        let _ = time("  agents::try_detect", || almanac::agents::try_detect(true));
        let _ = time("  agents::try_mise_tools", || almanac::agents::try_mise_tools(true));
        let _ = time("  selfupdate::try_as_app", || almanac::selfupdate::try_as_app(true));
        let _ = time("  try_orphans", inv::try_orphans);
    }
    let found = time("maintenance::run_all (first)", almanac::maintenance::run_all);
    time("maintenance::run_all (again)", almanac::maintenance::run_all);
    println!("{:32} {:>8}", "  findings", found.len());
    for id in ["disk", "runtime-logs", "integrity", "pacman-cache", "journal", "yay-cache", "orphans", "aur-health", "failed-units", "pacnew", "dangling-desktop", "broken-symlinks", "rollbacks", "trash", "config-reset", "snapshot", "vulnerabilities"] {
        if let Some(check) = almanac::maintenance::check_by_id(id) {
            time(&format!("  check {id}"), check);
        }
    }
}
