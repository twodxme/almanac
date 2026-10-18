//! Translations, through GNU gettext.
//!
//! Strings shown to people are wrapped in `_()`; `xgettext` collects them into
//! po/almanac.pot, translators produce po/<language>.po, and the package build
//! compiles those to the system locale directory. A checkout run from source
//! uses ./locale instead, if `po/update.sh` has compiled it there.
//!
//! With no translation for the user's language, `_()` returns the English text
//! unchanged -- nothing here can stop the app from starting.
//!
//! Port of `i18n.py`. Python's `_` is `tr` here (ARCHITECTURE.md); the
//! import-time `setlocale` + `gettext.translation(..., fallback=True)` is done
//! once, lazily, on first use (or explicitly through `init`).

pub const DOMAIN: &str = "almanac";

/// ./locale in a checkout; None means the system's own locale directory.
pub fn localedir() -> Option<String> {
    todo!("port: i18n::localedir")
}

/// The Python module's import-time setup: `locale.setlocale(LC_ALL, "")`
/// (an unsupported locale is still English) and binding the text domain to
/// `localedir()`. Safe to call more than once; `tr` calls it on first use.
pub fn init() {
    todo!("port: i18n::init")
}

/// `_(text)`: the translation of `text`, or `text` itself when there is none.
pub fn tr(text: &str) -> String {
    let _ = text;
    todo!("port: i18n::_")
}

/// `ngettext(singular, plural, n)`.
pub fn ngettext(singular: &str, plural: &str, n: u64) -> String {
    let _ = (singular, plural, n);
    todo!("port: i18n::ngettext")
}

/// Parity hook: see ARCHITECTURE.md.
pub fn parity(_func: &str, _args: &serde_json::Value) -> Option<serde_json::Value> {
    None
}
