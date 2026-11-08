//! Fanfic chapter titles (plan `round-23-daemon-memory-mail-enrich-fanfic` WP-5): maps a clipped
//! note's title and leading body lines to `<story> - Chapter <n>`. Pure: callers rename.
//!
//! Title shapes come first, in order (the first ` - Chapter N` / `, Chapter N` whose N is a whole
//! decimal number wins; `Chapter` is case-insensitive):
//! 1. `Story - Chapter N - <anything>` → story is the text before ` - Chapter`;
//! 2. `Story (…) - Chapter N - …` → story is the text before ` (`;
//! 3. `Story, Chapter N…` → story is the text before `, Chapter`.
//!
//! Otherwise the body fallback: the first of the first [`BODY_LINES`] non-blank body lines
//! (heading `#` markers skipped) that starts with `Chapter N` gives N, and the story is the first
//! ` - `/` | ` separated segment of the title (site, author and fandom suffixes dropped).
//!
//! N is a plain decimal integer: `Chapter 12.5`, a number that overflows `u32`, or one with a
//! leading zero is undetected (`None`), never rounded or normalised. Characters invalid in file
//! names are removed from the story; an empty story is `None`.

/// Body lines (non-blank, after the frontmatter) the fallback looks at.
pub const BODY_LINES: usize = 15;

/// A detected story name and chapter number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanficTitle {
    /// The story name, file-name safe and trimmed.
    pub story: String,
    /// The chapter number.
    pub chapter: u32,
}

const INVALID: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// `<story> - Chapter <n>`.
#[must_use]
pub fn target_name(t: &FanficTitle) -> String {
    format!("{} - Chapter {}", t.story, t.chapter)
}

/// Whether `title` already reads `^.+ - Chapter \d+$`.
#[must_use]
pub fn is_target_shape(title: &str) -> bool {
    title.rfind(" - Chapter ").is_some_and(|i| {
        let (head, num) = (&title[..i], &title[i + " - Chapter ".len()..]);
        !head.is_empty() && !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit())
    })
}

/// Detects the story and chapter of a note named `title` with body `body` (frontmatter already
/// removed). `None` when undetectable; callers check [`is_target_shape`] first.
#[must_use]
pub fn detect(title: &str, body: &str) -> Option<FanficTitle> {
    if let Some(found) = from_title(title) {
        return found;
    }
    let chapter = body_chapter(body)?;
    let first = title
        .split(" - ")
        .next()
        .and_then(|s| s.split(" | ").next())?;
    build(first, chapter)
}

fn build(story: &str, chapter: u32) -> Option<FanficTitle> {
    let story: String = story.chars().filter(|c| !INVALID.contains(c)).collect();
    let story = story.trim().to_owned();
    (!story.is_empty()).then_some(FanficTitle { story, chapter })
}

/// Parses `digits` at the start of `s` (after `Chapter `): `Ok(n)`, or `Err(())` when the number is
/// not a plain integer (decimal part, leading zero, overflow) or runs into letters.
fn number(s: &str) -> Option<Result<u32, ()>> {
    let end = s.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return None;
    }
    let rest = &s[end..];
    let mut chars = rest.chars();
    let bad_tail = match chars.next() {
        None => false,
        Some('.') => chars.next().is_some_and(|c| c.is_ascii_digit()),
        Some(c) => c.is_alphanumeric() || c == '_',
    };
    let digits = &s[..end];
    if bad_tail || (digits.len() > 1 && digits.starts_with('0')) {
        return Some(Err(()));
    }
    Some(digits.parse().map_err(|_| ()))
}

/// The title shapes: `None` = no shape matched (try the body); `Some(None)` = a shape matched
/// but is not detectable (no fallback: the title names a chapter we cannot read).
fn from_title(title: &str) -> Option<Option<FanficTitle>> {
    let lower = title.to_ascii_lowercase();
    for (sep, is_dash) in [(" - chapter ", true), (", chapter ", false)] {
        let mut from = 0;
        while let Some(i) = lower[from..].find(sep) {
            let at = from + i;
            let after = at + sep.len();
            from = at + 1;
            let Some(n) = number(&title[after..]) else {
                continue;
            };
            let Ok(n) = n else {
                return Some(None);
            };
            let mut head = &title[..at];
            if is_dash
                && head.trim_end().ends_with(')')
                && let Some(p) = head.find(" (")
            {
                head = &head[..p];
            }
            return Some(build(head, n));
        }
    }
    None
}

fn body_chapter(body: &str) -> Option<u32> {
    for line in body
        .lines()
        .map(|l| l.trim().trim_start_matches('#').trim_start())
        .filter(|l| !l.is_empty())
        .take(BODY_LINES)
    {
        let Some(head) = line.get(..7) else { continue };
        if !head.eq_ignore_ascii_case("chapter") {
            continue;
        }
        let rest = &line[7..];
        let trimmed = rest.trim_start();
        if trimmed.len() == rest.len() {
            continue;
        }
        return number(trimmed).and_then(Result::ok);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(title: &str, body: &str) -> Option<(String, u32)> {
        detect(title, body).map(|t| (t.story, t.chapter))
    }

    fn some(story: &str, n: u32) -> Option<(String, u32)> {
        Some((story.to_owned(), n))
    }

    #[test]
    fn dash_shape() {
        assert_eq!(
            d("Alpha Story - Chapter 12 - Writer - Some Fandom", ""),
            some("Alpha Story", 12)
        );
        assert_eq!(d("Alpha - chapter 3 - X", ""), some("Alpha", 3));
    }

    #[test]
    fn parenthesised_shape() {
        assert_eq!(
            d("Alpha Story (A Sequel) - Chapter 7 - Writer", ""),
            some("Alpha Story", 7)
        );
    }

    #[test]
    fn comma_shape() {
        assert_eq!(
            d("Alpha Story, Chapter 4: Title", ""),
            some("Alpha Story", 4)
        );
        assert_eq!(d("Alpha Story, Chapter 9", ""), some("Alpha Story", 9));
    }

    #[test]
    fn chapter_inside_story_name() {
        assert_eq!(
            d("Example Serial - Chapter 5 - Writer", ""),
            some("Example Serial", 5)
        );
        assert_eq!(
            d("A - Chapter Zero - Chapter 5 - Writer", ""),
            some("A - Chapter Zero", 5)
        );
    }

    #[test]
    fn body_fallback_strips_suffix_segments() {
        let body = "\n\n## Chapter 3\nText\n";
        assert_eq!(
            d("Alpha Story | Archive of Our Own", body),
            some("Alpha Story", 3)
        );
        assert_eq!(
            d("Alpha Story - Writer - Fandom", "Chapter 21: Name\n"),
            some("Alpha Story", 21)
        );
    }

    #[test]
    fn body_fallback_window_and_shape() {
        let far = format!("{}Chapter 2\n", "line\n".repeat(15));
        assert_eq!(d("Alpha", &far), None);
        let near = format!("{}Chapter 2\n", "line\n".repeat(14));
        assert_eq!(d("Alpha", &near), some("Alpha", 2));
        assert_eq!(d("Alpha", "Chapters of life\n"), None);
    }

    #[test]
    fn already_target_and_shape_predicate() {
        assert!(is_target_shape("Alpha - Chapter 12"));
        assert!(!is_target_shape("Alpha - Chapter 12 - X"));
        assert!(!is_target_shape("Alpha - Chapter x"));
        assert!(!is_target_shape(" - Chapter 1"));
    }

    #[test]
    fn undetectable() {
        assert_eq!(d("Alpha Story", "Some prose.\n"), None);
        assert_eq!(d("Alpha Story - Writer", ""), None);
    }

    #[test]
    fn integers_only() {
        assert_eq!(d("Alpha - Chapter 12.5 - X", "Chapter 3\n"), None);
        assert_eq!(d("Alpha - Chapter 007 - X", ""), None);
        assert_eq!(d("Alpha - Chapter 99999999999 - X", ""), None);
        assert_eq!(d("Alpha", "Chapter 1.5\n"), None);
    }

    #[test]
    fn story_is_file_name_safe() {
        assert_eq!(
            d("What: A \"Story\"? - Chapter 2 - X", ""),
            some("What A Story", 2)
        );
        assert_eq!(d(" : - Chapter 2 - X", ""), None);
        let t = detect("Alpha, Chapter 6", "").unwrap();
        assert_eq!(target_name(&t), "Alpha - Chapter 6");
    }
}
