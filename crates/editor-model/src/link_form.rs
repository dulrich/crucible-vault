//! The wikilink text inserted for a picked note (plan round-5-reading-pickers-intake WP-9).
//!
//! Mirrors the daemon resolver's basename rule (`crates/cv-index/src/resolver.rs` header:
//! unique basename case-insensitively, else the shortest path among namesakes, ties by byte
//! order): the bare stem is inserted when it resolves to `path`, else the vault path. Notes drop
//! `.md`; other files keep their extension.

/// The case-folded basename key of `path` (`.md` stripped), as the resolver keys it.
fn key(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    match base.strip_suffix(".md") {
        Some(s) => s.to_owned(),
        None => base,
    }
}

/// `path` without a trailing `.md`.
fn strip_md(path: &str) -> &str {
    path.strip_suffix(".md").unwrap_or(path)
}

/// The link target (inside `[[ ]]`) for vault file `path`, given every vault path `paths`.
pub fn wikilink_target(path: &str, paths: &[String]) -> String {
    let k = key(path);
    let winner = paths
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(path))
        .filter(|p| key(p) == k)
        .min_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    let stem = strip_md(path.rsplit('/').next().unwrap_or(path));
    if winner == Some(path) {
        stem.to_owned()
    } else {
        strip_md(path).to_owned()
    }
}

/// `[[target]]` for `path`.
pub fn wikilink(path: &str, paths: &[String]) -> String {
    format!("[[{}]]", wikilink_target(path, paths))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn unique_stem_and_attachment_extension() {
        let ps = v(&["a/Foo.md", "b/Bar.md", "img/pic.png"]);
        assert_eq!(wikilink("a/Foo.md", &ps), "[[Foo]]");
        assert_eq!(wikilink("img/pic.png", &ps), "[[pic.png]]");
    }

    #[test]
    fn namesakes_shortest_wins_else_path() {
        let ps = v(&["deep/x/foo.md", "a/Foo.md", "b/foo.md"]);
        // "a/Foo.md" and "b/foo.md" tie on length; byte order picks "a/Foo.md".
        assert_eq!(wikilink("a/Foo.md", &ps), "[[Foo]]");
        assert_eq!(wikilink("b/foo.md", &ps), "[[b/foo]]");
        assert_eq!(wikilink("deep/x/foo.md", &ps), "[[deep/x/foo]]");
    }
}
