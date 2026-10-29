//! The shapes every page is built from, in one place so the pages cannot
//! drift apart: the scrolling page body and its margins, and the severity
//! pill. (The titled list, `Win::section`, lives with the AI page, where the
//! Python had it.)

use adw::prelude::*;

/// The space every page keeps from the window's edges and from the header
/// bar (or the search bar, on the pages that have one).
pub const PAGE_MARGIN: i32 = 12;

/// Under the last card, a little more, so the end of a page reads as an end.
pub const PAGE_MARGIN_BOTTOM: i32 = 16;

/// Between the cards and sections of a page.
pub const PAGE_SPACING: i32 = 12;

/// Every scrolling page's frame: a vertical body on the page margins, in a
/// scroller that fills the page. Returns (page, body); the page goes in the
/// window's stack and the page's content goes in the body.
///
/// Health check, History and AI each built this by hand and left out the top
/// margin, so their first card sat flush under the header bar while Today's
/// sat 12px down. One builder, and that cannot happen again.
pub fn page_body() -> (gtk::Box, gtk::Box) {
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(PAGE_SPACING)
        .margin_start(PAGE_MARGIN)
        .margin_end(PAGE_MARGIN)
        .margin_top(PAGE_MARGIN)
        .margin_bottom(PAGE_MARGIN_BOTTOM)
        .build();
    body.add_css_class("page-body");
    let scroller = gtk::ScrolledWindow::builder().vexpand(true).build();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_child(Some(&body));
    let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    page.append(&scroller);
    (page, body)
}

/// What a section is, one dim line under its heading (wrapped when the window
/// is narrow). The one way a page explains a list, so no page hides it in a
/// tooltip and another shows it.
pub fn section_note(text: &str) -> gtk::Label {
    let note = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .use_markup(false)
        .build();
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    note
}

thread_local! {
    /// Every severity pill in the window, one width: the widest of them.
    static SEVERITY_WIDTH: gtk::SizeGroup = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
}

/// A severity pill ("Critical", "Attention", "Optional", "OK"), at the end
/// of its row like every other status pill (the Plugins page's), and as wide
/// as every other severity pill, whatever its word or language: Today, Health
/// check and AI each sized theirs to the word, so a list's pills were three
/// widths.
pub fn severity_pill(label: &str, css: &str) -> gtk::Label {
    let pill = gtk::Label::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .xalign(0.5)
        .build();
    pill.add_css_class(if css.is_empty() { "pill" } else { css });
    pill.add_css_class("severity-pill");
    SEVERITY_WIDTH.with(|group| group.add_widget(&pill));
    pill
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = include_str!("shape.rs");

    fn code() -> &'static str {
        SRC.split("#[cfg(test)]").next().unwrap_or(SRC)
    }

    /// The page shape is on the spacing scale, and its top margin is set: the
    /// one that was missing on three pages.
    #[test]
    fn the_page_body_keeps_the_margin_on_every_side() {
        let scale = [0, 2, 4, 6, 8, 12, 16, 20, 24];
        for v in [PAGE_MARGIN, PAGE_MARGIN_BOTTOM, PAGE_SPACING] {
            assert!(scale.contains(&v), "{v} is off the scale");
        }
        let body = code().split("pub fn page_body").nth(1).unwrap_or("");
        for side in [".margin_start(PAGE_MARGIN)", ".margin_end(PAGE_MARGIN)", ".margin_top(PAGE_MARGIN)"] {
            assert!(body.contains(side), "page_body lost {side}");
        }
    }

    /// Every scrolling page is built by `page_body`, none by hand: a page
    /// that builds its own frame is how three of them lost their top margin.
    #[test]
    fn every_scrolling_page_uses_the_page_body() {
        let pages = [
            ("today.rs", include_str!("../today.rs"), "pub fn build_featured"),
            ("ai.rs", include_str!("../ai.rs"), "pub fn build_ai"),
            ("history.rs", include_str!("../history.rs"), "pub fn build_maintenance"),
            ("history.rs", include_str!("../history.rs"), "pub fn build_history"),
            ("plugins/page.rs", include_str!("../plugins/page.rs"), "pub fn build_plugins_page"),
            ("switchboard.rs", include_str!("../switchboard.rs"), "pub fn build_switchboard"),
        ];
        for (file, src, builder) in pages {
            let body = src
                .split(builder)
                .nth(1)
                .and_then(|s| s.split("\n    }\n").next())
                .unwrap_or_else(|| panic!("{file}: {builder} not found"));
            assert!(body.contains("page_body()"), "{file}: {builder} builds its own frame");
            assert!(!body.contains("ScrolledWindow"), "{file}: {builder} has a scroller of its own");
        }
    }

    /// The severity pills are one helper, and no page makes its own.
    #[test]
    fn severity_pills_come_from_one_helper() {
        for (file, src) in [
            ("today.rs", include_str!("../today.rs")),
            ("ai.rs", include_str!("../ai.rs")),
            ("history.rs", include_str!("../history.rs")),
        ] {
            let code = src.split("#[cfg(test)]").next().unwrap_or(src);
            assert!(code.contains("severity_pill("), "{file} does not use severity_pill");
            // How each page made its own: a label given the severity's class.
            assert!(!code.contains(".add_css_class(css)"), "{file}: a severity pill made by hand");
        }
        let css = include_str!("../../../data/style.css");
        assert!(css.contains(".severity-pill"), "the severity pill is never styled");
    }
}
