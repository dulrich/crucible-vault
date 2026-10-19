//! Source-text edits shared by Source and Reading (moved out of `render`, plan
//! tn-review-baseline-remediation WP-R22 / F9): each returns a minimal splice of the note text.

/// Plan round-8-intake-views-anchor WP-3: the edit that sets top-level frontmatter boolean
/// `key` in `source` to `to` (`None` = toggle: `true` ↔ `false`, null/missing → `true`), as one
/// minimal splice `(byte range of source, replacement)` from the shared writer
/// `cv_note::FrontmatterDoc::set_edit` (B10; the daemon's `NoteSetBool` uses it too). Upserts a
/// null / missing key or a frontmatter-less note. `None` when nothing changes (already `to`),
/// the key holds a non-boolean value, or the block is unterminated / unsupported.
pub fn frontmatter_bool_edit(
    source: &str,
    key: &str,
    to: Option<bool>,
) -> Option<(std::ops::Range<usize>, String)> {
    use cv_note::{FrontmatterDoc, JsValue};
    let doc = FrontmatterDoc::new(source);
    let cur = doc.get(key).ok()?;
    if !matches!(cur, None | Some(JsValue::Null | JsValue::Bool(_))) {
        return None;
    }
    let next = to.unwrap_or(cur != Some(JsValue::Bool(true)));
    doc.set_edit(key, &JsValue::Bool(next)).ok()?
}

/// The toggle for the task at `line` of `source`: `[x]`/`[X]` → `[ ]`; `[ ]` and custom
/// statuses (`[/]`, `[-]`, …) → `[x]`. Returns the status char's byte-column range within
/// that line and its replacement, or `None` if the line holds no task box.
pub fn task_toggle(source: &str, line: usize) -> Option<(std::ops::Range<usize>, char)> {
    let (cols, status) = task_status_at(source, line)?;
    let next = if matches!(status, 'x' | 'X') {
        ' '
    } else {
        'x'
    };
    Some((cols, next))
}

/// plan editor-ui-round-3 WP-4: set the task at `line` to status `to` (the Reading context
/// menu). Returns the status char's byte-column range within that line and `to`, or `None`
/// if the line holds no task box or already has that status.
pub fn task_set_status(
    source: &str,
    line: usize,
    to: char,
) -> Option<(std::ops::Range<usize>, char)> {
    let (cols, status) = task_status_at(source, line)?;
    (status != to).then_some((cols, to))
}

/// The status char of the task box at `line` and its byte-column range within that line.
fn task_status_at(source: &str, line: usize) -> Option<(std::ops::Range<usize>, char)> {
    let start: usize = source.split_inclusive('\n').take(line).map(str::len).sum();
    let text = source[start..].split('\n').next()?;
    let open = text.find('[')?;
    let prefix = text[..open].trim_start();
    // Only a list marker (and quote/callout `>` prefixes) may precede the box.
    let marker_ok = prefix
        .trim_start_matches(|c: char| c == '>' || c.is_whitespace())
        .trim_end()
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '-' | '*' | '+' | '.' | ')'));
    let rest = &text[open + 1..];
    let status = rest.chars().next()?;
    if !marker_ok || !rest[status.len_utf8()..].starts_with(']') {
        return None;
    }
    let at = open + 1;
    Some((at..at + status.len_utf8(), status))
}
