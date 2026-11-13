//! Source-editing assists (plan `editor-usability-round` WP-1): Obsidian-style list continuation
//! on Enter and the smart Home/End decision table. Pure: the editor feeds it a line's text and
//! byte columns and applies the result to its iced buffer as one undoable edit.

use std::ops::Range;

/// What Enter does on a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEnter {
    /// Insert this text at the cursor (a newline plus the continued prefix).
    Continue(String),
    /// Empty item: delete this byte range of the line (the marker), and insert no newline.
    Exit(Range<usize>),
}

/// The leading whitespace + blockquote prefix (`>` with optional following spaces, repeated).
fn lead_len(line: &str) -> (usize, bool) {
    let b = line.as_bytes();
    let mut i = 0;
    let mut quote = false;
    loop {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        if i < b.len() && b[i] == b'>' {
            quote = true;
            i += 1;
        } else {
            return (i, quote);
        }
    }
}

/// The list marker at the start of `s` (its byte length, including the trailing space) and the
/// marker to put on the next line. `None` when `s` does not start with a list marker.
fn marker(s: &str) -> Option<(usize, String)> {
    let b = s.as_bytes();
    let first = *b.first()?;
    if matches!(first, b'-' | b'*' | b'+') {
        if b.get(1) != Some(&b' ') {
            return None;
        }
        // Task: `- [?] ` (or `- [?]` at the end of the line).
        if b.get(2) == Some(&b'[')
            && let Some(close) = s.get(3..).and_then(|r| r.char_indices().nth(1))
            && close.1 == ']'
        {
            let end = 3 + close.0 + 1;
            match b.get(end) {
                Some(b' ') => return Some((end + 1, format!("{} [ ] ", first as char))),
                None => return Some((end, format!("{} [ ] ", first as char))),
                _ => {}
            }
        }
        return Some((2, format!("{} ", first as char)));
    }
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let delim = *b.get(digits)?;
    if !matches!(delim, b'.' | b')') || b.get(digits + 1) != Some(&b' ') {
        return None;
    }
    let n: u64 = s[..digits].parse().ok()?;
    Some((digits + 2, format!("{}{} ", n + 1, delim as char)))
}

/// Enter at byte column `col` of `line`: `None` means a plain newline.
///
/// Lists (`-`/`*`/`+`, tasks → `- [ ]`, `N.`/`N)` → N+1) continue at the same indent and
/// blockquote prefix; a blockquote line without a marker continues its `>` prefix. On an empty
/// item (only the marker after the cursor-free prefix) the marker is removed instead (for a bare
/// quote line, the quote prefix). A cursor inside the prefix gives a plain newline.
pub fn list_enter(line: &str, col: usize) -> Option<ListEnter> {
    let (lead, quote) = lead_len(line);
    let after = &line[lead..];
    let (mlen, next) = match marker(after) {
        Some(m) => m,
        None if quote => (0, String::new()),
        None => return None,
    };
    let body = lead + mlen;
    if col < body || col > line.len() {
        return None;
    }
    if line[body..].trim().is_empty() {
        let start = if mlen == 0 {
            // Bare quote: drop the whole `>` prefix, keeping leading indent.
            line.len() - line.trim_start().len()
        } else {
            lead
        };
        return Some(ListEnter::Exit(start..line.len()));
    }
    Some(ListEnter::Continue(format!("\n{}{next}", &line[..lead])))
}

/// Byte width of the line's leading whitespace.
pub fn indent_width(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

/// plan editor-round-4 WP-2: whether `prefix` (the text before the cursor) is only indentation
/// plus a list marker (`-`/`*`/`+`/`1.`/`1)`, optional task box) and optional spaces — the
/// case where Tab indents the whole line instead of typing `\t`.
pub fn is_list_prefix(prefix: &str) -> bool {
    let rest = &prefix[indent_width(prefix)..];
    marker(rest).is_some_and(|(n, _)| rest[n..].bytes().all(|b| b == b' '))
}

/// Byte column where the content starts on a list line: after the indent, the list marker and
/// an optional task box (`[ ] `/`[x] `, for ordered markers too). `None` for a non-list line.
pub fn list_content_start(line: &str) -> Option<usize> {
    let indent = indent_width(line);
    let rest = &line[indent..];
    let (n, _) = marker(rest)?;
    let mut end = indent + n;
    // `marker` takes the task box for unordered markers only; accept it after `1. ` here.
    let b = line.as_bytes();
    if b.get(end) == Some(&b'[')
        && let Some(close) = line.get(end + 1..).and_then(|r| r.char_indices().nth(1))
        && close.1 == ']'
    {
        let after = end + 1 + close.0 + 1;
        match b.get(after) {
            Some(b' ') => end = after + 1,
            None => end = after,
            _ => {}
        }
    }
    Some(end)
}

/// Smart Home. `col`: logical byte column before; `indent`: [`indent_width`]; `visual_home`: the
/// column iced's visual-line `Home` motion lands on from `col`. A visual home past column 0 means
/// the cursor is on a wrapped continuation row.
///
/// - continuation row, not at its start → its start; at its start → the indent end;
/// - first row: past the indent → indent end; inside the indent or at its end (indent > 0) →
///   column 0; at column 0 → the indent end;
/// - first row of a list line (`content_start`, see [`list_content_start`]): past the content
///   start → the content start; from there the rule above applies.
pub fn home_target(
    col: usize,
    indent: usize,
    visual_home: usize,
    content_start: Option<usize>,
) -> usize {
    if visual_home > 0 {
        return if col != visual_home {
            visual_home
        } else {
            indent
        };
    }
    if let Some(cs) = content_start
        && col > cs
    {
        return cs;
    }
    if col > indent {
        indent
    } else if col > 0 {
        0
    } else {
        indent
    }
}

/// Smart End. `visual_end`: where iced's visual-line `End` lands from `col`; `line_len`: the
/// logical line length. End goes to the visual row end; pressed at a visual row end it goes to
/// the logical line end.
pub fn end_target(col: usize, visual_end: usize, line_len: usize) -> usize {
    if visual_end != col {
        visual_end
    } else {
        line_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_prefix_detection() {
        assert!(is_list_prefix("- "));
        assert!(is_list_prefix("  - [ ] "));
        assert!(is_list_prefix("\t1. "));
        assert!(is_list_prefix("3)  "));
        assert!(is_list_prefix("* [x]"));
        assert!(!is_list_prefix("- a"));
        assert!(!is_list_prefix("-"));
        assert!(!is_list_prefix("text"));
        assert!(!is_list_prefix(""));
    }

    fn cont(line: &str) -> Option<ListEnter> {
        list_enter(line, line.len())
    }

    #[test]
    fn bullets_continue_with_indent() {
        assert_eq!(cont("- a"), Some(ListEnter::Continue("\n- ".into())));
        assert_eq!(cont("  * a"), Some(ListEnter::Continue("\n  * ".into())));
        assert_eq!(cont("\t+ a"), Some(ListEnter::Continue("\n\t+ ".into())));
    }

    #[test]
    fn tasks_continue_unchecked() {
        assert_eq!(
            cont("- [x] done"),
            Some(ListEnter::Continue("\n- [ ] ".into()))
        );
        assert_eq!(
            cont("    - [ ] t"),
            Some(ListEnter::Continue("\n    - [ ] ".into()))
        );
        assert_eq!(
            cont("- [/] t"),
            Some(ListEnter::Continue("\n- [ ] ".into()))
        );
    }

    #[test]
    fn numbered_increment() {
        assert_eq!(cont("1. a"), Some(ListEnter::Continue("\n2. ".into())));
        assert_eq!(cont("  9) a"), Some(ListEnter::Continue("\n  10) ".into())));
    }

    #[test]
    fn quote_prefix_kept() {
        assert_eq!(cont("> - a"), Some(ListEnter::Continue("\n> - ".into())));
        assert_eq!(cont("> text"), Some(ListEnter::Continue("\n> ".into())));
        assert_eq!(
            cont("> > 3. x"),
            Some(ListEnter::Continue("\n> > 4. ".into()))
        );
    }

    #[test]
    fn empty_item_exits() {
        assert_eq!(cont("- "), Some(ListEnter::Exit(0..2)));
        assert_eq!(cont("  - [ ] "), Some(ListEnter::Exit(2..8)));
        assert_eq!(cont("3. "), Some(ListEnter::Exit(0..3)));
        assert_eq!(cont("> - "), Some(ListEnter::Exit(2..4)));
        assert_eq!(cont("> "), Some(ListEnter::Exit(0..2)));
    }

    #[test]
    fn non_lists_and_prefix_cursor_are_plain() {
        assert_eq!(cont("text"), None);
        assert_eq!(cont("-text"), None);
        assert_eq!(cont("1.5 kg"), None);
        assert_eq!(cont("  indented"), None);
        assert_eq!(list_enter("- abc", 1), None);
        assert_eq!(
            list_enter("- abc", 3),
            Some(ListEnter::Continue("\n- ".into()))
        );
    }

    #[test]
    fn home_table() {
        // First row (visual home 0), indent 4.
        assert_eq!(home_target(10, 4, 0, None), 4);
        assert_eq!(home_target(4, 4, 0, None), 0);
        assert_eq!(home_target(2, 4, 0, None), 0);
        assert_eq!(home_target(0, 4, 0, None), 4);
        assert_eq!(home_target(0, 0, 0, None), 0);
        assert_eq!(home_target(5, 0, 0, None), 0);
        // Continuation row starting at 40.
        assert_eq!(home_target(50, 4, 40, None), 40);
        assert_eq!(home_target(40, 4, 40, None), 4);
        // List lines: `- abc` (content 2), `- [ ] abc` (6), `1. abc` (3), `1) [x] abc` (7).
        for (line, indent, cs) in [
            ("- abc", 0, 2),
            ("- [ ] abc", 0, 6),
            ("1. abc", 0, 3),
            ("1) [x] abc", 0, 7),
            ("  - abc", 2, 4),
        ] {
            assert_eq!(list_content_start(line), Some(cs), "{line}");
            let c = Some(cs);
            assert_eq!(home_target(cs + 2, indent, 0, c), cs, "{line} past");
            assert_eq!(home_target(cs, indent, 0, c), indent, "{line} at");
            assert_eq!(home_target(indent, indent, 0, c), 0, "{line} indent end");
        }
        // Wrapped continuation row of a list line is unchanged.
        assert_eq!(home_target(50, 0, 40, Some(2)), 40);
        // Marker-less line.
        assert_eq!(list_content_start("plain text"), None);
        assert_eq!(list_content_start("-x"), None);
        assert_eq!(home_target(5, 0, 0, list_content_start("plain")), 0);
    }

    #[test]
    fn end_table() {
        assert_eq!(end_target(3, 40, 90), 40);
        assert_eq!(end_target(40, 40, 90), 90);
        assert_eq!(end_target(90, 90, 90), 90);
        assert_eq!(indent_width("  \tx "), 3);
    }
}
