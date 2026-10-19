//! The pane header (plan editor-feedback-round-2 WP-6, Decisions locked "Pane header = tab bar +
//! note control bar"): a fixed-height **tab bar** (the pane's tab headers, each with a
//! fixed-width dirty/saving marker and a full-path tooltip) and, for note tabs, a fixed-height
//! **note control bar** (back/forward seg-group, centred path breadcrumb whose last segment is
//! the editable filename, view-mode control). Both bars clip; nothing in them can wrap, so the
//! editor below never moves when a note dirties, saves or switches.
//!
//! The tab title text is a pure function of the `editor.toml` `tab_title` mode ([`TabTitle`],
//! [`resolve`]).
use cv_text::vpath;

/// Height of each header bar (one `CONTROL` row).
pub const BAR_HEIGHT: f32 = crate::theme::CONTROL;
/// Width reserved for a tab header's dirty/saving marker (present even when clean).
pub const MARKER_WIDTH: f32 = 12.0;
/// A tab header's title is cut to this many characters (plus `…`).
pub const TITLE_MAX_CHARS: usize = 28;

/// `editor.toml` `quick_switch_open`: what Quick Switch does with a note already open (plan
/// editor-feedback-round-2 WP-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuickSwitchOpen {
    /// Focus the existing tab wherever it is.
    Switch,
    /// Move the existing tab into the active pane and focus it.
    #[default]
    Move,
    /// Open a second tab of the note in the active pane.
    Duplicate,
}

impl QuickSwitchOpen {
    /// Every mode, in settings order.
    pub const ALL: [Self; 3] = [Self::Switch, Self::Move, Self::Duplicate];

    /// The `editor.toml` spelling.
    pub fn key(self) -> &'static str {
        match self {
            Self::Switch => "switch",
            Self::Move => "move",
            Self::Duplicate => "duplicate",
        }
    }

    /// Parses the `editor.toml` spelling.
    pub fn of(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == s)
    }

    /// The Settings page label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Switch => "Switch",
            Self::Move => "Move",
            Self::Duplicate => "Duplicate",
        }
    }
}

/// `editor.toml` `tab_title`: what a tab header shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabTitle {
    /// The frontmatter `title` property, falling back to the basename.
    #[default]
    Title,
    /// The file stem.
    Basename,
    /// The vault path.
    Path,
    /// The shortest path suffix that distinguishes the tab among the pane's tabs.
    Unambiguous,
}

impl TabTitle {
    /// Every mode, in settings order.
    pub const ALL: [Self; 4] = [Self::Title, Self::Basename, Self::Path, Self::Unambiguous];

    /// The `editor.toml` spelling.
    pub fn key(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::Basename => "basename",
            Self::Path => "path",
            Self::Unambiguous => "unambiguous",
        }
    }

    /// Parses the `editor.toml` spelling.
    pub fn of(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.key() == s)
    }

    /// The Settings page label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Title => "Title",
            Self::Basename => "Basename",
            Self::Path => "Path",
            Self::Unambiguous => "Unambiguous",
        }
    }
}

/// The frontmatter `title` property of a note's text (a non-empty string scalar), if any.
pub fn frontmatter_title(text: &str) -> Option<String> {
    use ofm::frontmatter::{JsValue, parse_frontmatter, split_frontmatter};
    let (meta, _) = split_frontmatter(text);
    let Ok(JsValue::Obj(obj)) = parse_frontmatter(&meta?) else {
        return None;
    };
    obj.entries().into_iter().find_map(|(k, v)| match v {
        JsValue::Str(s) if k == "title" && !s.trim().is_empty() => Some(s.trim().to_owned()),
        _ => None,
    })
}

/// The last `n` segments of `path`, the leaf without `.md`.
fn suffix(path: &str, n: usize) -> String {
    let segs: Vec<&str> = path.split('/').collect();
    let from = segs.len().saturating_sub(n);
    let mut out = segs[from..].join("/");
    if let Some(s) = out.strip_suffix(".md") {
        out = s.to_owned();
    }
    out
}

/// A tab's header text. `siblings` are the paths of the pane's tabs (may include `path`);
/// `fm_title` is the note's frontmatter `title` (only consulted in [`TabTitle::Title`]).
pub fn resolve(mode: TabTitle, path: &str, siblings: &[&str], fm_title: Option<&str>) -> String {
    match mode {
        TabTitle::Title => fm_title
            .filter(|t| !t.trim().is_empty())
            .map_or_else(|| vpath::stem(path).to_owned(), str::to_owned),
        TabTitle::Basename => vpath::stem(path).to_owned(),
        TabTitle::Path => path.to_owned(),
        TabTitle::Unambiguous => {
            let depth = path.split('/').count();
            (1..=depth)
                .map(|n| suffix(path, n))
                .find(|s| {
                    !siblings
                        .iter()
                        .filter(|o| **o != path)
                        .any(|o| suffix(o, s.split('/').count()) == *s)
                })
                .unwrap_or_else(|| suffix(path, depth))
        }
    }
}

/// `s` cut to `max` characters with a trailing `…` (headers never grow or wrap).
pub fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A tab header's marker glyph: `•` unsaved, `◌` saving, blank when clean.
pub fn marker(dirty: bool, saving: bool) -> &'static str {
    if saving {
        "◌"
    } else if dirty {
        "•"
    } else {
        ""
    }
}

/// The breadcrumb's folder segments (`a/b/Note.md` → `["a", "b"]`).
pub fn folders(path: &str) -> Vec<&str> {
    match path.rsplit_once('/') {
        Some((dir, _)) => dir.split('/').collect(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_mode_uses_frontmatter_then_basename() {
        assert_eq!(
            resolve(TabTitle::Title, "a/b/Note.md", &[], Some("My Note")),
            "My Note"
        );
        assert_eq!(resolve(TabTitle::Title, "a/b/Note.md", &[], None), "Note");
        assert_eq!(
            resolve(TabTitle::Title, "a/b/Note.md", &[], Some("  ")),
            "Note"
        );
    }

    #[test]
    fn basename_and_path_modes() {
        assert_eq!(
            resolve(TabTitle::Basename, "a/b/Note.md", &[], Some("x")),
            "Note"
        );
        assert_eq!(
            resolve(TabTitle::Path, "a/b/Note.md", &[], Some("x")),
            "a/b/Note.md"
        );
    }

    #[test]
    fn unambiguous_mode_is_the_shortest_distinguishing_suffix() {
        let sibs = ["a/x/Note.md", "a/y/Note.md", "b/x/Note.md", "c/Other.md"];
        assert_eq!(
            resolve(TabTitle::Unambiguous, "c/Other.md", &sibs, None),
            "Other"
        );
        assert_eq!(
            resolve(TabTitle::Unambiguous, "a/y/Note.md", &sibs, None),
            "y/Note"
        );
        assert_eq!(
            resolve(TabTitle::Unambiguous, "a/x/Note.md", &sibs, None),
            "a/x/Note"
        );
        // Alone in the pane (or duplicated): the basename.
        assert_eq!(
            resolve(TabTitle::Unambiguous, "a/x/Note.md", &["a/x/Note.md"], None),
            "Note"
        );
    }

    #[test]
    fn frontmatter_title_parses_a_string_scalar() {
        assert_eq!(
            frontmatter_title("---\ntitle: Hello There\ntags: [a]\n---\nbody"),
            Some("Hello There".into())
        );
        assert_eq!(frontmatter_title("---\ntags: [a]\n---\nbody"), None);
        assert_eq!(frontmatter_title("no frontmatter"), None);
    }

    #[test]
    fn modes_round_trip_and_helpers() {
        for m in TabTitle::ALL {
            assert_eq!(TabTitle::of(m.key()), Some(m));
        }
        assert_eq!(TabTitle::of("stem"), None);
        assert_eq!(TabTitle::default(), TabTitle::Title);
        assert_eq!(ellipsize("abcdef", 4), "abc…");
        assert_eq!(ellipsize("abc", 4), "abc");
        assert_eq!(marker(true, false), "•");
        assert_eq!(marker(true, true), "◌");
        assert_eq!(marker(false, false), "");
        assert_eq!(folders("a/b/N.md"), ["a", "b"]);
        assert!(folders("N.md").is_empty());
    }
}
