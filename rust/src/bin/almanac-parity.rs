//! Parity runner: reads `{"module": ..., "cases": [[func, args], ...]}` on
//! stdin, answers each with the Rust port's result, as JSON on stdout.
//! Each answer is `{"v": result}`; a bare `null` means "not implemented in
//! Rust" and fails the parity run. A Python exception is matched by a Rust
//! `{"error": "..."}` result.
use std::io::Read;

fn call(module: &str, func: &str, args: &serde_json::Value) -> Option<serde_json::Value> {
    match module {
        "agents" => almanac::agents::parity(func, args),
        "ai" => almanac::ai::parity(func, args),
        "appimage" => almanac::appimage::parity(func, args),
        "autorun" => almanac::autorun::parity(func, args),
        "autostart" => almanac::autostart::parity(func, args),
        "autoupdate" => almanac::autoupdate::parity(func, args),
        "demo" => almanac::demo::parity(func, args),
        "featured" => almanac::featured::parity(func, args),
        "ghcache" => almanac::ghcache::parity(func, args),
        "guard" => almanac::guard::parity(func, args),
        "history" => almanac::history::parity(func, args),
        "i18n" => almanac::i18n::parity(func, args),
        "icons" => almanac::icons::parity(func, args),
        "inventory" => almanac::inventory::parity(func, args),
        "maintenance" => almanac::maintenance::parity(func, args),
        "mimeguard" => almanac::mimeguard::parity(func, args),
        "patches" => almanac::patches::parity(func, args),
        "picks" => almanac::picks::parity(func, args),
        "plugin_setup" => almanac::plugin_setup::parity(func, args),
        "plugins" => almanac::plugins::parity(func, args),
        "security" => almanac::security::parity(func, args),
        "selfupdate" => almanac::selfupdate::parity(func, args),
        "services" => almanac::services::parity(func, args),
        "settings" => almanac::settings::parity(func, args),
        "shortcuts" => almanac::shortcuts::parity(func, args),
        "snapshots" => almanac::snapshots::parity(func, args),
        "sources" => almanac::sources::parity(func, args),
        "store" => almanac::store::parity(func, args),
        "tasks" => almanac::tasks::parity(func, args),
        "themeart" => almanac::themeart::parity(func, args),
        "theming" => almanac::theming::parity(func, args),
        "verify" => almanac::verify::parity(func, args),
        "watcher" => almanac::watcher::parity(func, args),
        "xdg" => almanac::xdg::parity(func, args),
        _ => None,
    }
}

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("stdin");
    let req: serde_json::Value = serde_json::from_str(&input).expect("json");
    let module = req["module"].as_str().unwrap_or_default();
    let out: Vec<serde_json::Value> = req["cases"].as_array().map(|cases| {
        cases.iter().map(|c| {
            let func = c[0].as_str().unwrap_or_default();
            call(module, func, &c[1])
                .map(|v| serde_json::json!({ "v": v }))
                .unwrap_or(serde_json::Value::Null)
        }).collect()
    }).unwrap_or_default();
    println!("{}", serde_json::to_string(&out).unwrap());
}
