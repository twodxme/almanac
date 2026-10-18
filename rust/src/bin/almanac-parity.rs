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
        // The icon choices the window makes (`ui/ai.rs`, `ui/plugins.rs`).
        "ui" => almanac::ui::ai::parity(func, args),
        _ => None,
    }
}

/// The answers' writer: a thread of its own, with a stack sized for them.
///
/// An answer can be as deep as a document a case feeds in (guard parses a
/// file nested 100000 levels when Python's limit allows it, and that limit
/// grows with `ulimit -s`), and serde_json writes and drops a `Value`
/// recursively. That is the harness's own recursion, so it runs here, not
/// on the main thread: the cases themselves stay on the main thread, whose
/// stack is whatever `ulimit -s` gives, as the app's code runs on its
/// callers' threads -- a port function that recursed as deep as its input
/// would still overflow there and be caught.
fn writer_stack() -> usize {
    const GIB: usize = 1 << 30;
    match rustix::process::getrlimit(rustix::process::Resource::Stack).current {
        // ~160 bytes of Python's C stack per level; a few hundred of serde's.
        Some(n) => (n as usize).saturating_mul(8).clamp(GIB, 8 * GIB),
        None => GIB,
    }
}

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).expect("stdin");
    let req: serde_json::Value = serde_json::from_str(&input).expect("json");
    let module = req["module"].as_str().unwrap_or_default();
    let (tx, rx) = std::sync::mpsc::channel::<Option<serde_json::Value>>();
    // The receiver goes to whichever spawn succeeds.
    let rx = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
    let spawn = |size: usize| {
        let rx = rx.clone();
        std::thread::Builder::new().name("parity-out".into()).stack_size(size).spawn(move || {
            let mut out = String::from("[");
            let Some(rx) = rx.lock().ok().and_then(|mut r| r.take()) else { return out };
            for (i, v) in rx.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match v {
                    Some(v) => out.push_str(&serde_json::to_string(&serde_json::json!({ "v": v })).unwrap()),
                    None => out.push_str("null"),
                }
            }
            out.push(']');
            out
        })
    };
    // Reserved, not committed: only the pages a deep answer touches are used.
    let writer = spawn(writer_stack()).or_else(|_| spawn(1 << 30)).expect("writer thread");
    for c in req["cases"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let func = c[0].as_str().unwrap_or_default();
        tx.send(call(module, func, &c[1])).expect("writer");
    }
    drop(tx);
    println!("{}", writer.join().expect("writer"));
}
