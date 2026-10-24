//! "What's New": the newest release's notes, from the shipped AppStream file.

use super::*;

/// (version, AppStream description markup) of the newest release.
///
/// Read from the same AppStream file software centres show, so the About
/// window's "What's New" never disagrees with them. A checkout has it under
/// docs/; a package installs it to the system metainfo directory.
/// The exceptions ElementTree lets through are `Err`.
pub fn try_release_notes() -> Result<Option<(String, String)>, String> {
    release_notes_in(&ROOT)
}

/// `release_notes` with `ROOT` given (the parity cases point it at a temp dir).
///
/// On a thread of its own: `tostring` recurses once per level of the
/// description, to Python's recursion limit (about 1000), which a small
/// caller's stack cannot be trusted to hold.
pub fn release_notes_in(root: &Path) -> Result<Option<(String, String)>, String> {
    util::with_stack(util::DEEP_STACK, || release_notes_here(root))
}

pub(super) fn release_notes_here(root: &Path) -> Result<Option<(String, String)>, String> {
    let paths = [
        py_path_join(root.as_os_str(), &format!("docs/{METAINFO}")),
        OsString::from(format!("/usr/share/metainfo/{METAINFO}")),
    ];
    for path in &paths {
        // except (OSError, ET.ParseError): continue
        let Ok(bytes) = std::fs::read(path) else { continue };
        let doc = match et::parse(&bytes) {
            Ok(doc) => doc,
            Err(et::Error::Parse) => continue,
            Err(et::Error::Raise(e)) => return Err(e),
        };
        let Some(release) = doc.find2("releases", "release") else { continue };
        let Some(desc) = release.find("description") else {
            return Ok(None);
        };
        // libadwaita takes the description's children as AppStream markup.
        let mut body = String::new();
        for child in &desc.children {
            body.push_str(&et::tostring(child)?);
        }
        let version = release.get("version").unwrap_or_default().to_string();
        return Ok(Some((version, py_split(&body).join(" "))));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // AboutWindow.test_whats_new_comes_from_the_shipped_metainfo
    #[test]
    fn test_whats_new_comes_from_the_shipped_metainfo() {
        let (version, markup) = try_release_notes().unwrap().expect("release notes");
        assert_eq!(version, VERSION);
        assert!(markup.starts_with("<p>"));
    }
    #[test]
    fn release_notes_nested_past_the_recursion_limit_do_not_overflow_a_small_stack() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        let deep = |n: usize| "<p>".repeat(n) + "x" + &"</p>".repeat(n);
        for (n, ok) in [(10, true), (5000, false)] {
            let xml = format!(
                "<component><releases><release version=\"1.0\"><description>{}</description></release></releases></component>",
                deep(n)
            );
            std::fs::write(dir.path().join("docs").join(METAINFO), xml).unwrap();
            let root = dir.path().to_path_buf();
            let got = std::thread::Builder::new()
                .stack_size(128 << 10)
                .spawn(move || release_notes_in(&root))
                .unwrap()
                .join()
                .unwrap();
            assert_eq!(got.is_ok(), ok, "{n}: {got:?}");
        }
    }
}
