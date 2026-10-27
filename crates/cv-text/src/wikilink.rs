//! The one wikilink parse and format pair: [`target`] reads a link entry, [`link_to`] writes one.

/// A wikilink (or bare path) entry's target: `[[a/b|alias]]` → `a/b`, `[[a/b#h]]` → `a/b`.
///
/// Trims, strips one `[[`/`]]` wrapper, then drops everything from the first `|` and the first `#`.
#[must_use]
pub fn target(s: &str) -> &str {
    let t = s.trim();
    let t = t.strip_prefix("[[").unwrap_or(t);
    let t = t.strip_suffix("]]").unwrap_or(t);
    let t = t.split('|').next().unwrap_or(t);
    t.split('#').next().unwrap_or(t).trim()
}

/// `[[path-without-.md]]`.
#[must_use]
pub fn link_to(path: &str) -> String {
    format!("[[{}]]", path.strip_suffix(".md").unwrap_or(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_strips_wrapper_alias_and_heading() {
        assert_eq!(target("[[a/b]]"), "a/b");
        assert_eq!(target("  [[a/b|alias]] "), "a/b");
        assert_eq!(target("[[a/b#h]]"), "a/b");
        assert_eq!(target("[[a#b|c]]"), "a");
        assert_eq!(target("a/b"), "a/b");
        assert_eq!(target("[[ ]]"), "");
    }

    #[test]
    fn link_to_drops_md_suffix() {
        assert_eq!(link_to("a/b.md"), "[[a/b]]");
        assert_eq!(link_to("a/b"), "[[a/b]]");
    }
}
