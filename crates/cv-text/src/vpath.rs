//! Vault-path helpers (`/`-separated, vault-relative): one copy of the basename / stem / parent
//! rules and URL percent-decoding the daemon steps, the index, the search prep and the editor
//! share (plan tn-review-baseline-remediation WP-R10, finding B11).

/// The last path segment (`a/b/Note.md` → `Note.md`).
#[must_use]
pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The basename with a trailing `.md` stripped (`a/b/Note.md` → `Note`; `a/x.png` → `x.png`).
#[must_use]
pub fn stem(path: &str) -> &str {
    let base = basename(path);
    base.strip_suffix(".md").unwrap_or(base)
}

/// Obsidian `TFile.basename`: the basename without its *last* extension, whatever it is
/// (`a/x.tar.gz` → `x.tar`; a leading-dot name keeps its dot).
#[must_use]
pub fn stem_last_ext(path: &str) -> &str {
    let name = basename(path);
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

/// The parent folder (`""` at the root).
#[must_use]
pub fn parent(path: &str) -> &str {
    split(path).0
}

/// `(parent, basename)`; the parent is `""` at the root.
#[must_use]
pub fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Decodes `%XX` escapes; input whose decoded bytes are not UTF-8 is returned unchanged.
#[must_use]
pub fn percent_decode(s: &str) -> String {
    match percent_encoding::percent_decode_str(s).decode_utf8() {
        Ok(d) => d.into_owned(),
        Err(_) => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments() {
        assert_eq!(basename("a/b/Note.md"), "Note.md");
        assert_eq!(basename("Note.md"), "Note.md");
        assert_eq!(stem("a/b/Note.md"), "Note");
        assert_eq!(stem("a/x.png"), "x.png");
        assert_eq!(stem_last_ext("a/x.tar.gz"), "x.tar");
        assert_eq!(stem_last_ext(".hidden"), ".hidden");
        assert_eq!(parent("a/b/Note.md"), "a/b");
        assert_eq!(parent("Note.md"), "");
        assert_eq!(split("a/b"), ("a", "b"));
        assert_eq!(split("b"), ("", "b"));
    }

    #[test]
    fn decodes() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%FF"), "%FF");
    }
}
