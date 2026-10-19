//! Pure note-path helpers of the Reading pane: rename targets and missing-link note paths.

use super::{percent_decode, stem};

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
/// percent-decoded (as [`super::ReadingStore::resolve_link`]); a target with a `/` is a vault path, a
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
