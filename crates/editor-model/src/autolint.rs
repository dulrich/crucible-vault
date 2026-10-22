//! Auto-lint at autosave (plan round-9-feedback-todos WP-6, D26): the pure decisions of "lint the
//! open buffer's frontmatter" — which paths are exempt, whether the cursor sits in the
//! frontmatter, and the frontmatter-only edit (as one undo [`Change`]) with the cursor shift it
//! implies. The lint engine and settings parser are `cv_note`'s, re-exported here so the editor
//! needs no direct `cv-note` edge.

use crate::undo::{Change, Cursor, Pos};

pub use cv_note::{LintContext, LintSettings, ParamValue, lint_note};

/// Vault folder whose notes are never auto-linted (templates, prompts, Bases, `Settings.md`).
pub const CUSTOM_FOLDER: &str = "_custom";

/// A planned auto-lint edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The frontmatter-only change (body untouched), in the pre-edit document's coordinates.
    pub change: Change,
    /// The cursor after the edit: body positions shifted by the frontmatter line delta.
    pub after: Cursor,
}

/// Whether `path` is exempt: under `_custom/`, a configured template itself, or inside a
/// template's folder (a root-level template exempts only itself).
pub fn excluded(path: &str, template_paths: &[String]) -> bool {
    if path
        .strip_prefix(CUSTOM_FOLDER)
        .is_some_and(|r| r.starts_with('/'))
    {
        return true;
    }
    template_paths.iter().any(|t| {
        let t = t.trim().trim_start_matches('/');
        if t.is_empty() {
            return false;
        }
        if path == t {
            return true;
        }
        t.rsplit_once('/')
            .is_some_and(|(dir, _)| !dir.is_empty() && path.starts_with(&format!("{dir}/")))
    })
}

/// The line index of the first body line (just past the closing fence), when `text` has
/// frontmatter.
pub fn body_line(text: &str) -> Option<usize> {
    let r = cv_note::span(text)?;
    Some(text[..r.end].matches('\n').count())
}

/// Plans the auto-lint of `text`: the frontmatter-only edit, or `None` when the note has no
/// frontmatter, the cursor or selection touches the frontmatter, lint fails, or lint changes
/// nothing in the frontmatter (a body-only difference is ignored).
pub fn plan(
    text: &str,
    cursor: Cursor,
    ctx: &LintContext,
    settings: &LintSettings,
) -> Option<Plan> {
    let body = body_line(text)?;
    if cursor.pos.line < body || cursor.anchor.is_some_and(|a| a.line < body) {
        return None;
    }
    let out = lint_note(text, ctx, settings).ok()?;
    let old_end = cv_note::span(text)?.end;
    let new_end = cv_note::span(&out.text)?.end;
    let change = Change::diff(0, &text[..old_end], &out.text[..new_end])?;
    let delta = |c: &Change| {
        let n = |s: &str| isize::try_from(s.matches('\n').count()).unwrap_or(0);
        n(&c.inserted) - n(&c.removed)
    };
    let d = delta(&change);
    let shift = |p: Pos| Pos::new(p.line.saturating_add_signed(d), p.col);
    Some(Plan {
        after: Cursor {
            pos: shift(cursor.pos),
            anchor: cursor.anchor.map(shift),
        },
        change,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> LintContext {
        LintContext {
            basename: "n".into(),
            today: "2026-10-22".into(),
            created: "2026-10-22".into(),
        }
    }

    fn settings() -> LintSettings {
        // Only the `updated` / `word-count` steps matter here.
        LintSettings::default()
    }

    const NOTE: &str = "---\ntitle: T\n---\n\nbody one two\n";

    #[test]
    fn plan_edits_the_frontmatter_only_and_shifts_the_cursor() {
        let cur = Cursor::caret(4, 3);
        let p = plan(NOTE, cur, &ctx(), &settings()).expect("lint changes the note");
        let mut out = NOTE.to_owned();
        p.change.apply(&mut out).unwrap();
        assert!(out.ends_with("\n\nbody one two\n"), "body kept: {out:?}");
        assert!(out.contains("updated: 2026-10-22"));
        let added = out.matches('\n').count() - NOTE.matches('\n').count();
        assert!(added > 0);
        assert_eq!(p.after, Cursor::caret(4 + added, 3));
        // The cursor still sits on the same text.
        let line = out.split('\n').nth(p.after.pos.line).unwrap();
        assert_eq!(line, "body one two");
    }

    #[test]
    fn plan_skips_cursor_or_anchor_in_frontmatter_and_missing_frontmatter() {
        assert!(plan(NOTE, Cursor::caret(1, 0), &ctx(), &settings()).is_none());
        assert!(plan(NOTE, Cursor::caret(2, 0), &ctx(), &settings()).is_none());
        let sel = Cursor {
            pos: Pos::new(4, 2),
            anchor: Some(Pos::new(1, 0)),
        };
        assert!(plan(NOTE, sel, &ctx(), &settings()).is_none());
        assert!(plan("just body\n", Cursor::caret(0, 0), &ctx(), &settings()).is_none());
    }

    #[test]
    fn plan_is_none_when_lint_changes_nothing_in_the_frontmatter() {
        let p = plan(NOTE, Cursor::caret(4, 0), &ctx(), &settings()).unwrap();
        let mut once = NOTE.to_owned();
        p.change.apply(&mut once).unwrap();
        assert!(plan(&once, Cursor::caret(5, 0), &ctx(), &settings()).is_none());
    }

    #[test]
    fn excluded_covers_custom_and_template_folders() {
        let t = vec!["templates/daily/day.md".to_owned(), "root.md".to_owned()];
        assert!(excluded("_custom/Settings.md", &t));
        assert!(!excluded("_customer/x.md", &t));
        assert!(excluded("templates/daily/day.md", &t));
        assert!(excluded("templates/daily/other.md", &t));
        assert!(!excluded("templates/other.md", &t));
        assert!(excluded("root.md", &t));
        assert!(
            !excluded("sibling.md", &t),
            "a root template exempts only itself"
        );
        assert!(!excluded("notes/a.md", &[]));
    }
}
