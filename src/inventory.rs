//! Port of `inventory.py`.
//!
//! Enumerate installed software across every source on this machine.
//!
//! Deliberately read-only. Nothing here mutates package state -- the GUI delegates
//! every install/update to the tool that actually owns it (pacman, yay, omarchy
//! update). The one exception is AppImages, which no package manager tracks, so
//! this module reads a registry that the app itself owns.
//!
//! Naming: Python's "private" helpers that other modules call
//! (`_load_registry`, `_pending_repo`, `_git_cache`, ...) drop the leading
//! underscore here (`load_registry`, `pending_repo`, `git_cache`), the same
//! rule the UI sections use for methods.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::util;
use crate::util::pyjson::{self, Build, Num};

mod py;
mod health;
mod app;
mod pacman;
mod appimages;
mod flatpak;
mod local;
mod discover;
mod plugin_registry;
mod snapshot;

use py::*;
pub use health::*;
pub use app::*;
pub use pacman::*;
pub use appimages::*;
pub use flatpak::*;
pub use local::*;
pub use discover::*;
pub use plugin_registry::*;
pub use snapshot::*;

// Runtime data, deliberately not where the code lives.
//
// Almanac installs as a git checkout that updates itself, so anything it
// writes has to sit outside that checkout -- otherwise `git pull` is fighting
// the app's own state, and a fresh clone would have 180MB of someone else's
// AppImages in it. Everything else derives from this one path.

/// `STATE`: `$XDG_STATE_HOME/almanac` (default `~/.local/state/almanac`).
/// A function rather than a constant because it reads the environment; the
/// Python read it once at import, which is the same answer for a process that
/// does not change its own environment.
pub fn state() -> PathBuf {
    // os.environ.get(...) uses the variable whenever it is set, even empty.
    match std::env::var_os("XDG_STATE_HOME") {
        Some(v) => PathBuf::from(v).join("almanac"),
        None => crate::util::home().join(".local/state").join("almanac"),
    }
}

/// `REGISTRY`: the AppImage registry the app itself owns.
pub fn registry() -> PathBuf {
    state().join("appimages.json")
}

// Sources, in the order they should appear.
pub const REPO: &str = "repo";
pub const AUR: &str = "aur";
pub const APPIMAGE: &str = "appimage";
pub const LOCAL: &str = "local";
pub const FLATPAK: &str = "flatpak";
pub const ORPHAN: &str = "orphan";
pub const SYSTEM: &str = "system";
/// AI coding agents: installed by their own scripts or a version manager, so no
/// package manager reports them. See agents.py.
pub const AGENT: &str = "agent";
/// Version-managed developer tools: mise, asdf and friends own software that
/// no package manager reports on.
pub const TOOL: &str = "tool";

/// Updates worth pulling out of the crowd. Two kinds:
///   security   — network-facing or credential-handling; a stale one is exposure
///   core       — can stop the machine booting, or needs a reboot to take effect
///                (plus, by rule in [`criticality`]: DKMS modules, every kernel
///                flavour, firmware splits, NVIDIA drivers, CPU microcode)
///
/// A slice of pairs rather than a map so the Python dict's iteration order
/// (security first) is kept.
pub const CRITICAL: &[(&str, &[&str])] = &[
    (
        "security",
        &[
            "chromium", "google-chrome", "firefox", "brave", "vivaldi", "opera",
            "openssl", "openssh", "gnutls", "curl", "wget", "nss", "ca-certificates",
            "sudo", "polkit", "1password", "bitwarden", "keepassxc", "gnupg",
            "wpa_supplicant", "networkmanager", "dnsmasq", "nginx", "openvpn",
            "wireguard-tools", "tailscale", "nordvpn-bin",
        ],
    ),
    (
        "core",
        &[
            "linux", "linux-lts", "linux-zen", "linux-firmware", "glibc", "systemd",
            "mkinitcpio", "limine", "grub", "mesa", "pacman", "btrfs-progs", "dkms",
            // What builds the initramfs or unlocks and assembles the root
            // before it is mounted: a bad update here fails at the next boot.
            "dracut", "booster", "limine-mkinitcpio-hook", "plymouth", "cryptsetup",
            "lvm2", "mdadm", "efibootmgr", "refind",
            // Copied into every initramfs (busybox for the `base` hook, kmod to
            // load modules), what writes or signs the boot entries, and ZFS
            // roots. Not util-linux or the fsck tools: they update often and a
            // bad build of them does not stop the kernel reaching userspace.
            "mkinitcpio-busybox", "kmod", "limine-snapper-sync", "systemd-ukify", "sbctl",
            "shim-signed", "zfs-utils",
        ],
    ),
];

// ------------------------------------------------------------------ module --

/// 'security', 'core', or '' -- empty for the ordinary majority.
pub fn criticality(name: &str) -> String {
    let n = name.strip_suffix("-bin").unwrap_or(name);
    let base = n.strip_suffix("-git").unwrap_or(n).to_lowercase();
    let lower = name.to_lowercase();
    for (kind, names) in CRITICAL {
        if names.contains(&base.as_str()) || names.contains(&lower.as_str()) {
            return kind.to_string();
        }
    }
    // A DKMS module (nvidia-dkms, zfs-dkms, virtualbox-host-dkms, ...) is
    // rebuilt against every kernel, and one that fails to build leaves the
    // next boot without its graphics or its filesystem: core, by suffix,
    // since there are too many to list.
    if base.ends_with("-dkms") || lower.ends_with("-dkms") {
        return "core".to_string();
    }
    // Kernels, the GPU drivers built for them and CPU microcode: all take
    // effect only at the next boot, and a bad one stops it. By rule, since
    // each has flavours and branches too many to list.
    if [base.as_str(), lower.as_str()].iter().any(|n| is_kernel(n) || is_nvidia_driver(n) || n.ends_with("-ucode")) {
        return "core".to_string();
    }
    String::new()
}

/// Kernel flavours, after `linux-`: Arch's (lts, zen, hardened, rt, rt-lts),
/// the AUR's and other repos' common ones, alone or one after another
/// (`linux-rt-lts`, `linux-vfio-lts`); `lts` also with the series it
/// tracks (`linux-lts66`, `linux-lts515`). A name is only a package name at
/// this point (nothing says whether it ships a vmlinuz), so this is an
/// allowlist: `linux-wifi-hotspot`, `linux-api-headers`, `linux-tools-meta`,
/// `linux-atm` and the `-docs` packages are userland and stay ordinary.
const KERNEL_FLAVOURS: &[&str] = &[
    "lts", "zen", "hardened", "rt", "mainline", "next", "libre", "lqx", "clear", "pf", "g14", "surface", "asahi",
    "vfio", "git",
];

/// Flavour families with their own variants after a dash
/// (`linux-cachyos-bore`, `linux-xanmod-edge`, `linux-tkg-pds`,
/// `linux-omarchy-eevdf`, `linux-amd-znver3`, `linux-ck-generic-v3`):
/// whatever follows is a build of the same kernel.
const KERNEL_FAMILIES: &[&str] = &["cachyos", "xanmod", "tkg", "omarchy", "amd", "ck"];

/// One flavour word: a listed one, or `lts` with its series' digits.
fn is_flavour(word: &str) -> bool {
    KERNEL_FLAVOURS.contains(&word)
        || word.strip_prefix("lts").is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
}

/// A kernel, its headers (DKMS builds against them, so they move with it)
/// or its firmware: `linux`, `linux-<flavour>[-headers]`, `linux-firmware`
/// and its splits (`linux-firmware-amdgpu`, ...: the firmware itself since
/// `linux-firmware` became a meta package). `-git` and `-bin` are already
/// stripped. Not `linux-firmware-whence`: the licence files, nothing a
/// boot reads.
fn is_kernel(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("linux-") else {
        return name == "linux";
    };
    if rest == "firmware-whence" {
        return false;
    }
    if rest == "headers" || rest == "firmware" || rest.starts_with("firmware-") {
        return true;
    }
    let flavour = rest.strip_suffix("-headers").unwrap_or(rest);
    if flavour.ends_with("-docs") || flavour.ends_with("-dbg") {
        return false;
    }
    let mut words = flavour.split('-');
    let first = words.next().unwrap_or("");
    KERNEL_FAMILIES.contains(&first) || (is_flavour(first) && words.all(is_flavour))
}

/// NVIDIA's kernel module packages and the userspace that must match them
/// (`nvidia-utils` pins `nvidia`'s exact version; a mismatch leaves
/// graphics broken until a reboot): `nvidia`, `nvidia-open`, `nvidia-lts`,
/// `nvidia-utils`, `lib32-nvidia-utils`, legacy branches (`nvidia-580xx`,
/// `nvidia-580xx-utils`) and `-beta`. Not the tools around them
/// (`nvidia-settings`, `nvidia-prime`, `nvidia-container-toolkit`,
/// `opencl-nvidia`).
fn is_nvidia_driver(name: &str) -> bool {
    let (lib32, n) = match name.strip_prefix("lib32-") {
        Some(n) => (true, n),
        None => (false, name),
    };
    let Some(rest) = n.strip_prefix("nvidia") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('-').skip(1).collect();
    let legacy = |p: &str| p.strip_suffix("xx").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()));
    (rest.is_empty() || rest.starts_with('-'))
        && parts.iter().all(|p| ["open", "lts", "beta", "utils"].contains(p) || legacy(p))
        && (!lib32 || parts.last() == Some(&"utils"))
}

/// Same software present twice -- an AppImage plus its packaged build, or
/// a -bin and a source package side by side.
///
/// A Vec of pairs, not a map: the Python dict keeps first-seen order and the
/// UI shows the first five.
pub fn duplicates(apps: &[App]) -> Vec<(String, Vec<String>)> {
    let mut seen: Vec<(String, Vec<&App>)> = Vec::new();
    for a in apps {
        if a.source == ORPHAN {
            continue;
        }
        let mut base = a.name.to_lowercase();
        for suffix in ["-bin", "-git", "-appimage", "-desktop"] {
            if let Some(s) = base.strip_suffix(suffix) {
                base = s.to_string();
            }
        }
        match seen.iter_mut().find(|(k, _)| *k == base) {
            Some((_, v)) => v.push(a),
            None => seen.push((base, vec![a])),
        }
    }
    seen.into_iter()
        .filter(|(_, v)| v.len() > 1)
        .map(|(k, v)| (k, v.iter().map(|a| format!("{} ({})", a.name, a.source)).collect()))
        .collect()
}


/// Stack for each [`collect`] thread: the 8 MiB of `ui::shell::BG_STACK`,
/// Python's own thread stack. It was `util::JSON_STACK` while a collector
/// could free a value nested as deep as Python's json reads; none can now.
/// GitHub answers stop at `ghcache::VALUE_DEPTH` (512) levels, the registry
/// and git cache at [`py_loads`]' 900, AUR answers at serde's 128, and agents
/// drop their raw cache values on their own deep threads (`strip_raw`) and
/// keep a cached release url only when it is a str (`agents::url_value`),
/// so the deepest thing a collector clones, writes or drops here is 900
/// levels of `serde_json::Value` (`collector_stack_holds_the_deepest_value`).
/// Reserved, not used, like every stack.
const COLLECTOR_STACK: usize = 8 << 20;

/// Everything, in one list.
///
/// The six collectors are independent and each is dominated by waiting --
/// checkupdates and yay on the network, git on disk, agents on process
/// start-up. Run together the total is the slowest one rather than the sum.
pub fn collect() -> Vec<App> {
    if crate::demo::active() {
        return crate::demo::collect();
    }

    // Almanac lists itself. An app whose job is telling you what is stale
    // should not be the one thing that silently is.
    // An Err is a collector that raised, like a panic.
    type Collector = fn() -> Result<Vec<App>, String>;
    let sources: Vec<Collector> = vec![
        packages,
        || appimages(true),
        flatpaks,
        || local_builds(true),
        || crate::agents::try_detect(true),
        || crate::agents::try_mise_tools(true),
        // `lambda: [selfupdate.as_app()]`: where as_app raises, the row is
        // dropped with the rest of that collector.
        || crate::selfupdate::try_as_app(true).map(|a| vec![a]),
        try_orphans,
    ];
    let mut out = Vec::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = sources
            .iter()
            .map(|f| std::thread::Builder::new().stack_size(COLLECTOR_STACK).spawn_scoped(s, f))
            .collect();
        for h in handles {
            let Ok(h) = h else { continue };
            match h.join() {
                Ok(Ok(apps)) => out.extend(apps),
                // One collector failing must not empty the whole window; the
                // others still describe most of the system.
                Ok(Err(_)) | Err(_) => continue,
            }
        }
    });
    out
}

/// Everything worth showing in a detail view, fetched on demand.
///
/// Deliberately not part of collect(): dependency lists mean a pacman call
/// per package, which would turn a 2-second scan into a minute for data that
/// is only read one row at a time.
///
/// Err: pacman's output is not UTF-8 (`_run`'s UnicodeDecodeError).
pub fn try_details(app: &App) -> Result<Extra, String> {
    let mut out = app.extra_dict();
    if [REPO, AUR, SYSTEM, ORPHAN].contains(&app.source.as_str()) {
        let text = run(&["pacman", "-Qi", &app.name], 20)?;
        let mut field: Option<String> = None;
        let mut acc: HashMap<String, String> = HashMap::new();
        for line in py_splitlines(&text) {
            // `if line.startswith(" ") and field`: a field named "" is
            // falsy, so the line is parsed as a field of its own.
            if line.starts_with(' ') {
                if let Some(f) = field.as_ref().filter(|f| !f.is_empty()) {
                    if let Some(v) = acc.get_mut(f) {
                        v.push(' ');
                        v.push_str(py_strip(line));
                    }
                    continue;
                }
            }
            let Some((k, v)) = line.split_once(':') else { continue };
            let k = py_strip(k).to_string();
            acc.insert(k.clone(), py_strip(v).to_string());
            field = Some(k);
        }
        for key in ["Depends On", "Required By", "Optional Deps", "Provides",
                    "Conflicts With", "Packager", "Architecture", "Validated By"] {
            if let Some(v) = acc.get(key) {
                if !v.is_empty() && v != "None" {
                    out.insert(key.to_string(), json!(v));
                }
            }
        }
        let files = py_splitlines(&run(&["pacman", "-Qlq", &app.name], 20)?).len();
        out.insert("files".into(), json!(files));
    }
    Ok(out)
}

/// `try_details` for callers that need a plain map: where the Python
/// raises, this answers the row's own `extra`.
pub fn details(app: &App) -> Extra {
    try_details(app).unwrap_or_else(|_| app.extra_dict())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Criticality
    #[test]
    fn test_network_facing_is_security() {
        assert_eq!(criticality("google-chrome"), "security");
        assert_eq!(criticality("openssh"), "security");
    }

    #[test]
    fn test_bin_suffix_is_stripped_before_matching() {
        assert_eq!(criticality("nordvpn-bin"), "security");
    }

    #[test]
    fn test_boot_critical_is_core() {
        assert_eq!(criticality("linux"), "core");
        assert_eq!(criticality("mkinitcpio"), "core");
        for name in [
            "dracut", "booster", "cryptsetup", "lvm2", "mdadm", "plymouth", "limine-mkinitcpio-hook",
            "mkinitcpio-busybox", "kmod", "limine-snapper-sync", "systemd-ukify", "sbctl", "zfs-utils",
            "linux-git-headers",
        ] {
            assert_eq!(criticality(name), "core", "{name}");
        }
    }

    #[test]
    fn test_dkms_modules_are_core() {
        for name in ["dkms", "nvidia-dkms", "virtualbox-host-dkms", "zfs-dkms", "zfs-dkms-git", "Broadcom-WL-DKMS"] {
            assert_eq!(criticality(name), "core", "{name}");
        }
        // only the suffix: a name that merely mentions dkms is not a module
        assert_eq!(criticality("dkms-tools-doc"), "");
    }

    #[test]
    fn kernels_drivers_and_microcode_are_core_by_rule() {
        for name in [
            "linux", "linux-lts", "linux-zen", "linux-hardened", "linux-rt", "linux-rt-lts", "linux-rt-lts-headers",
            "linux-headers", "linux-zen-headers", "linux-cachyos", "linux-cachyos-bore", "linux-cachyos-lts-headers",
            "linux-xanmod", "linux-xanmod-edge", "linux-xanmod-lts-bin", "linux-tkg-pds", "linux-mainline",
            "linux-mainline-git", "linux-git", "linux-omarchy", "linux-omarchy-eevdf-headers", "linux-firmware",
            "linux-firmware-amdgpu", "linux-firmware-git", "nvidia", "nvidia-open", "nvidia-lts", "nvidia-open-lts",
            "nvidia-utils", "lib32-nvidia-utils", "nvidia-580xx-utils", "lib32-nvidia-580xx-utils",
            "nvidia-open-beta", "nvidia-open-dkms", "intel-ucode", "amd-ucode", "linux-vfio-lts",
            "linux-vfio-lts-headers", "linux-lts66", "linux-lts515", "linux-lts515-headers", "linux-amd",
            "linux-amd-znver3", "linux-amd-znver2-headers", "linux-ck", "linux-ck-generic-v3",
            "linux-ck-generic-v3-headers", "linux-vfio", "linux-zen-lts", "linux-firmware-cirrus-dx13260",
            "linux-firmware-other",
        ] {
            assert_eq!(criticality(name), "core", "{name}");
        }
        // linux-* userland, and the tools around a driver: ordinary.
        for name in [
            "linux-wifi-hotspot", "linux-api-headers", "linux-docs", "linux-zen-docs", "linux-cachyos-docs",
            "linux-tools-meta", "linux-atm", "linux-pam", "linuxqq", "util-linux", "nvidia-settings", "nvidia-prime",
            "nvidia-container-toolkit", "nvidia-cg-toolkit", "libnvidia-container", "opencl-nvidia",
            "lib32-opencl-nvidia", "libva-nvidia-driver", "python-nvidia-ml-py", "nvidiax", "lib32-nvidia",
            "iucode-tool", "linux-firmware-whence", "linux-lts-docs", "linux-lts66-docs", "linux-amd-docs",
            "linux-ck-generic-v3-docs", "linux-lts-foo", "linux-ltsx", "linux-lts6x", "linux-vfio-hotspot",
            "linux-amdgpu-tools", "linux-cka", "linux-tools", "linux-lts-", "linux-",
        ] {
            assert_eq!(criticality(name), "", "{name}");
        }
    }

    #[test]
    fn test_ordinary_package_is_neither() {
        assert_eq!(criticality("bat"), "");
    }

    // ---- Duplicates
    fn app(name: &str, source: &str) -> App {
        App::new(name, "1", source)
    }

    #[test]
    fn test_bin_and_source_build_are_one_app() {
        let apps = [app("cursor", REPO), app("cursor-bin", AUR)];
        assert!(duplicates(&apps).iter().any(|(k, _)| k == "cursor"));
    }

    #[test]
    fn test_orphans_are_not_counted_as_duplicates() {
        // An orphaned library is not a second copy of anything.
        let apps = [app("foo", REPO), app("foo", ORPHAN)];
        assert!(duplicates(&apps).is_empty());
    }

    #[test]
    fn test_distinct_software_is_not_flagged() {
        let apps = [app("firefox", REPO), app("chromium", REPO)];
        assert!(duplicates(&apps).is_empty());
    }

    #[test]
    fn collector_stack_holds_the_deepest_value() {
        // The deepest a collector reads (py_loads' limit), cloned, written
        // and dropped on a collector's stack, as local_builds does with the
        // git cache; one level more is refused before anything recurses.
        let deep = |n: usize| format!("{}{}", "{\"k\": [".repeat(n / 2), "]}".repeat(n / 2));
        assert!(py_loads(&deep(902)).is_err());
        let text = deep(900);
        let ok = std::thread::Builder::new()
            .stack_size(COLLECTOR_STACK)
            .spawn(move || {
                let v = py_loads(&text).unwrap();
                let copy = v.clone();
                let out = py_dumps(&Value::Object(Map::from_iter([("c".to_string(), copy)])), Some(2));
                drop(v);
                out.len() > 900
            })
            .unwrap()
            .join()
            .unwrap();
        assert!(ok);
    }
}
