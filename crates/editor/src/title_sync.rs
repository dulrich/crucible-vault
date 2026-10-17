//! Plan usability-sprint-captures-links WP-5: frontmatter `title` → filename sync (one
//! direction only — renaming a file never rewrites `title`). The plan placed the title diff in
//! `editor-model`, but that crate has no `cv-text` edge and the Cargo edges are frozen, so the
//! pure decision lives here, next to the shared [`cv_text::template::title_filename`].

use cv_text::template::title_filename;

/// The rename target when a save changed the note's `title` from `old` to `new`: the same
/// folder, `title_filename(new).md` — only for `.md` notes whose current stem is exactly
/// `title_filename(old)`, when both titles are non-empty and differ and the new form is
/// non-empty and differs from the stem. Else `None`.
pub fn title_sync_target(path: &str, old: Option<&str>, new: Option<&str>) -> Option<String> {
    let old = old.map(str::trim).filter(|s| !s.is_empty())?;
    let new = new.map(str::trim).filter(|s| !s.is_empty())?;
    if old == new || !path.ends_with(".md") {
        return None;
    }
    let stem = crate::reading::stem(path);
    let form = title_filename(new);
    if form.is_empty() || form == stem || stem != title_filename(old) {
        return None;
    }
    Some(match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{form}.md"),
        None => format!("{form}.md"),
    })
}

#[cfg(test)]
mod tests {
    use super::title_sync_target as t;

    #[test]
    fn fires_only_when_the_stem_is_the_old_title_form() {
        assert_eq!(
            t("a/b/Old: Name.md", Some("Old: Name"), Some("New?")),
            None,
            "stem is not the form of A (the colon is dropped)"
        );
        assert_eq!(
            t("a/b/Old Name.md", Some("Old: Name"), Some("New: Name")).as_deref(),
            Some("a/b/New Name.md"),
            "nested folder kept"
        );
        assert_eq!(
            t("Old.md", Some("Old"), Some("New")).as_deref(),
            Some("New.md")
        );
        assert_eq!(
            t("Other.md", Some("Old"), Some("New")),
            None,
            "stem differs"
        );
    }

    #[test]
    fn no_fire_on_empty_unchanged_or_non_md() {
        assert_eq!(t("Old.md", None, Some("New")), None);
        assert_eq!(t("Old.md", Some("Old"), None), None);
        assert_eq!(t("Old.md", Some("Old"), Some("  ")), None);
        assert_eq!(t("Old.md", Some("Old"), Some("???")), None, "form(B) empty");
        assert_eq!(t("Old.md", Some("Old"), Some("Old")), None);
        assert_eq!(
            t("Old.md", Some("Old"), Some("Old?")),
            None,
            "form(B) == stem"
        );
        assert_eq!(t("Old.canvas", Some("Old"), Some("New")), None);
    }
}
