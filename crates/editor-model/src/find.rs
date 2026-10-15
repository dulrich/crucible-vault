//! Find in note (editor-usability-round WP-4): case-insensitive plain-substring matches over a
//! buffer's text, next/previous with wrap-around, and byte offset → (line, byte column).

use std::ops::Range;

/// Every non-overlapping case-insensitive match of `needle` in `hay`, as byte ranges in order.
/// An empty needle matches nothing.
pub fn matches(hay: &str, needle: &str) -> Vec<Range<usize>> {
    let pat: Vec<char> = needle.chars().flat_map(char::to_lowercase).collect();
    if pat.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut from = 0;
    for (start, _) in hay.char_indices() {
        if start < from {
            continue;
        }
        if let Some(end) = match_at(hay, start, &pat) {
            out.push(start..end);
            from = end;
        }
    }
    out
}

/// The end byte of a match of lowercased `pat` starting at `start`, if any.
fn match_at(hay: &str, start: usize, pat: &[char]) -> Option<usize> {
    let mut want = pat.iter();
    let mut pending = want.next();
    for (off, c) in hay[start..].char_indices() {
        for l in c.to_lowercase() {
            match pending {
                Some(&p) if p == l => pending = want.next(),
                _ => return None,
            }
        }
        if pending.is_none() {
            return Some(start + off + c.len_utf8());
        }
    }
    None
}

/// The match after `current` (wrapping); the first one when nothing is current.
pub fn next(current: Option<usize>, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match current {
        Some(i) if i + 1 < count => i + 1,
        _ => 0,
    })
}

/// The match before `current` (wrapping); the last one when nothing is current.
pub fn prev(current: Option<usize>, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    Some(match current {
        Some(i) if i > 0 && i <= count => i - 1,
        _ => count - 1,
    })
}

/// The first match at or after byte `at` (wrapping to the first), for re-anchoring after edits.
pub fn first_from(ms: &[Range<usize>], at: usize) -> Option<usize> {
    if ms.is_empty() {
        return None;
    }
    Some(ms.iter().position(|m| m.start >= at).unwrap_or(0))
}

/// Byte offset → 0-based (line, byte column) over `\n`-separated lines.
pub fn line_col(text: &str, byte: usize) -> (usize, usize) {
    let byte = byte.min(text.len());
    let before = &text[..byte];
    let line = before.matches('\n').count();
    let col = before.rfind('\n').map_or(byte, |i| byte - i - 1);
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_case_insensitive_and_non_overlapping() {
        assert_eq!(matches("Foo foo FOO", "foo"), vec![0..3, 4..7, 8..11]);
        assert_eq!(matches("aaaa", "aa"), vec![0..2, 2..4]);
        assert_eq!(matches("ÄBc äbc", "äB"), vec![0..3, 5..8]);
        assert!(matches("abc", "").is_empty());
        assert!(matches("abc", "abcd").is_empty());
    }

    #[test]
    fn next_prev_wrap() {
        assert_eq!(next(None, 3), Some(0));
        assert_eq!(next(Some(1), 3), Some(2));
        assert_eq!(next(Some(2), 3), Some(0));
        assert_eq!(prev(None, 3), Some(2));
        assert_eq!(prev(Some(0), 3), Some(2));
        assert_eq!(prev(Some(2), 3), Some(1));
        assert_eq!(next(Some(0), 0), None);
        assert_eq!(prev(None, 0), None);
        assert_eq!(first_from(&[0..1, 5..6], 3), Some(1));
        assert_eq!(first_from(&[0..1, 5..6], 9), Some(0));
    }

    #[test]
    fn line_col_counts_bytes() {
        let t = "ab\ncdé\nx";
        assert_eq!(line_col(t, 0), (0, 0));
        assert_eq!(line_col(t, 4), (1, 1));
        assert_eq!(line_col(t, 8), (2, 0));
        assert_eq!(line_col(t, 99), (2, 1));
    }
}
