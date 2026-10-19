//! Pure note-path helpers of the Reading pane (WP-R18, moved from the editor): rename targets,
//! missing-link note paths, and the shared vault-path stem / link-target decoding.

pub use cv_text::vpath::{percent_decode, stem};

/// The rename target for a submitted filename: `None` when unchanged or empty.
pub fn rename_target(path: &str, input: &str) -> Option<String> {
    let input = input.trim();
    let input = input.strip_suffix(".md").unwrap_or(input);
    if input.is_empty() || input == stem(path) {
        return None;
    }
    let ext = if path.ends_with(".md") { ".md" } else { "" };
    Some(match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{input}{ext}"),
        None => format!("{input}{ext}"),
    })
}

/// Plan round-6 WP-4: the vault path a missing link `target` creates — `#fragment` stripped and
/// percent-decoded (as the editor's `ReadingStore::resolve_link`); a target with a `/` is a vault path, a
/// bare one goes into `folder` (the explorer's New-note folder; `""` = vault root); `.md` is
/// appended when missing.
pub fn missing_note_path(target: &str, folder: &str) -> String {
    let bare = target.split('#').next().unwrap_or(target);
    let name = percent_decode(bare);
    let name = name.trim().trim_start_matches('/');
    let mut path = if name.contains('/') || folder.is_empty() {
        name.to_owned()
    } else {
        format!("{}/{name}", folder.trim_end_matches('/'))
    };
    if !path.to_ascii_lowercase().ends_with(".md") {
        path.push_str(".md");
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_every_escape() {
        assert_eq!(percent_decode("a%20b%5Fc"), "a b_c");
        assert_eq!(percent_decode("caf%C3%A9.png"), "café.png");
        assert_eq!(percent_decode("100%zz%2"), "100%zz%2");
        assert_eq!(percent_decode("bad%FF"), "bad%FF");
    }

    /// Plan round-6 WP-4: the missing-note path rule.
    #[test]
    fn missing_note_path_rule() {
        assert_eq!(missing_note_path("Nope", ""), "Nope.md");
        assert_eq!(missing_note_path("Nope", "inbox"), "inbox/Nope.md");
        assert_eq!(missing_note_path("sub/Deep", "inbox"), "sub/Deep.md");
        assert_eq!(missing_note_path("x/Done.md", "inbox"), "x/Done.md");
        assert_eq!(missing_note_path("My%20Note#sec", ""), "My Note.md");
    }
}
