//! Fanfic chapter titles (plan `round-23-daemon-memory-mail-enrich-fanfic` WP-5): maps a clipped
//! note's title and leading body lines to `<story> - Chapter <n>`. Pure: callers rename.
//!
//! Title shapes come first, in order (the first ` - Chapter N` / `, Chapter N` whose N is a whole
//! decimal number wins; `Chapter` is case-insensitive):
//! 1. `Story - Chapter N - <anything>` → story is the text before ` - Chapter`;
//! 2. `Story (…) - Chapter N - …` → story is the text before ` (`;
//! 3. `Story, Chapter N…` → story is the text before `, Chapter`.
//!
//! WP-8: the body's chapter line wins over the title's N (sites number chapters by position, the
//! body carries the logical number): the first of the first [`BODY_LINES`] counted body lines
//! (non-blank, outside callout blocks; leading `#`/`>`/`*`/`_` markers stripped) that reads
//! `Chapter N` gives N; other `Chapter…` lines are skipped. Without a title shape the story is the
//! first ` - `/` | ` separated segment of the title (site, author and fandom suffixes dropped). A
//! caller-supplied publication (the serial row's) overrides the story ([`detect_with`]).
//!
//! N is a plain decimal integer: `Chapter 12.5`, a number that overflows `u32`, or one with a
//! leading zero is undetected, never rounded or normalised. Characters invalid in file names are
//! removed from the story and whitespace runs collapsed; an empty story is `None`.

use cv_text::vpath;

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

/// What the rule says about one note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Already `<story> - Chapter <n>`.
    Already,
    /// No shape detected.
    Undetected,
    /// Rename to this path (same folder).
    Rename(String),
}

/// The rule over plain inputs (plan `tn-review-2026-11-10-remediation` WP-R8): the note's vault
/// `path`, its `body` (frontmatter removed) and the story `publication` the caller resolved (the
/// serial row's, else the `publication` frontmatter) → what to do. `Already` only when the
/// basename equals the computed target; a rename keeps the note's folder.
#[must_use]
pub fn plan(path: &str, body: &str, publication: Option<&str>) -> Plan {
    let title = vpath::stem(path);
    match detect_with(title, body, publication) {
        None => Plan::Undetected,
        Some(t) => {
            let name = target_name(&t);
            if name == title {
                return Plan::Already;
            }
            let folder = vpath::parent(path).trim_matches('/');
            let file = format!("{name}.md");
            Plan::Rename(if folder.is_empty() {
                file
            } else {
                format!("{folder}/{file}")
            })
        }
    }
}

/// Detects the story and chapter of a note named `title` with body `body` (frontmatter already
/// removed); `detect_with(title, body, None)`.
#[must_use]
pub fn detect(title: &str, body: &str) -> Option<FanficTitle> {
    detect_with(title, body, None)
}

/// Detects the story and chapter (plan WP-8): the chapter is the body's first `Chapter N` line
/// (logical numbering) else the title shape's N; the story is `publication` (the work's serial
/// row / `publication` frontmatter) when non-empty after sanitising, else the title shape's head,
/// else the first ` - `/` | ` title segment. `None` when no chapter number or story is found.
#[must_use]
pub fn detect_with(title: &str, body: &str, publication: Option<&str>) -> Option<FanficTitle> {
    let shape = from_title(title);
    let chapter = match (body_chapter(body), &shape) {
        (Some(n), _) => n,
        (None, Some((_, Ok(n)))) => *n,
        _ => return None,
    };
    if let Some(story) = publication.map(sanitise).filter(|p| !p.is_empty()) {
        return Some(FanficTitle { story, chapter });
    }
    let head = match shape {
        Some((head, _)) => head,
        None => title
            .split(" - ")
            .next()
            .and_then(|s| s.split(" | ").next())?,
    };
    build(head, chapter)
}

/// Drops characters invalid in file names, collapses whitespace runs to one space, trims.
fn sanitise(s: &str) -> String {
    let kept: String = s.chars().filter(|c| !INVALID.contains(c)).collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build(story: &str, chapter: u32) -> Option<FanficTitle> {
    let story = sanitise(story);
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
        Some(c) => c.is_alphanumeric(),
    };
    let digits = &s[..end];
    if bad_tail || (digits.len() > 1 && digits.starts_with('0')) {
        return Some(Err(()));
    }
    Some(digits.parse().map_err(|_| ()))
}

/// The title shapes: `None` = no shape matched; `Some((head, n))` = the story head and the
/// shape's number (`Err` when not a plain integer: only a body chapter line can rescue it).
fn from_title(title: &str) -> Option<(&str, Result<u32, ()>)> {
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
            let mut head = &title[..at];
            if is_dash
                && head.trim_end().ends_with(')')
                && let Some(p) = head.find(" (")
            {
                head = &head[..p];
            }
            return Some((head, n));
        }
    }
    None
}

/// The body lines the fallback counts: blank lines and callout blocks (a `> [!` line and the
/// `>` lines after it — the enrich abstract, `[!removed]-` folds) are skipped.
fn counted_lines(body: &str) -> impl Iterator<Item = &str> {
    let mut in_callout = false;
    body.lines().filter_map(move |l| {
        let t = l.trim();
        if t.is_empty() {
            in_callout = false;
            return None;
        }
        if t.starts_with("> [!") || (in_callout && t.starts_with('>')) {
            in_callout = true;
            return None;
        }
        in_callout = false;
        Some(t)
    })
}

/// The first counted line (of [`BODY_LINES`]) reading `Chapter N` once leading whitespace, `#`,
/// `>`, `*` and `_` markers are stripped; other `Chapter…` lines are skipped.
fn body_chapter(body: &str) -> Option<u32> {
    counted_lines(body).take(BODY_LINES).find_map(|line| {
        let line = line
            .trim_start_matches(|c: char| matches!(c, '#' | '>' | '*' | '_') || c.is_whitespace());
        let head = line.get(..7)?;
        if !head.eq_ignore_ascii_case("chapter") {
            return None;
        }
        let rest = &line[7..];
        let trimmed = rest.trim_start();
        if trimmed.len() == rest.len() {
            return None;
        }
        number(trimmed).and_then(Result::ok)
    })
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
    fn plan_keeps_the_folder_and_flags_the_target_shape() {
        assert_eq!(plan("F/A - Chapter 2.md", "x", None), Plan::Already);
        assert_eq!(
            plan("F/A - Chapter 2 - W.md", "x", None),
            Plan::Rename("F/A - Chapter 2.md".into())
        );
        assert_eq!(
            plan("A - Chapter 2 - W.md", "x", Some("Story")),
            Plan::Rename("Story - Chapter 2.md".into())
        );
        assert_eq!(plan("F/Nothing.md", "x", None), Plan::Undetected);
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
        assert_eq!(d("Alpha - Chapter 12.5 - X", "prose\n"), None);
        assert_eq!(
            d("Alpha - Chapter 12.5 - X", "Chapter 3\n"),
            some("Alpha", 3)
        );
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

    #[test]
    fn body_markers_are_stripped() {
        assert_eq!(d("Alpha", "**Chapter 9 \n"), some("Alpha", 9));
        assert_eq!(d("Alpha", "***Chapter 4:*** Name\n"), some("Alpha", 4));
        assert_eq!(d("Alpha", "> **Chapter 2**\n"), some("Alpha", 2));
        assert_eq!(d("Alpha", "## Chapter 5\n"), some("Alpha", 5));
        assert_eq!(d("Alpha", "\u{a0}_Chapter 6: x_\n"), some("Alpha", 6));
        assert_eq!(d("Alpha", "_Chapter 8_\n"), some("Alpha", 8));
    }

    #[test]
    fn non_numbered_chapter_lines_are_skipped() {
        assert_eq!(
            d("Alpha", "Chapter Text\nChapters: 3/?\nChapter 7: X\n"),
            some("Alpha", 7)
        );
    }

    #[test]
    fn callout_blocks_are_not_counted() {
        let abs = format!(
            "> [!abstract]-\n{}\n> [!removed]- Removed\n> gone\n",
            "> summary line\n".repeat(15)
        );
        let body = format!("{abs}{}Chapter 157: Of Things\n", "meta\n".repeat(3));
        assert_eq!(d("Gamma Tale", &body), some("Gamma Tale", 157));
    }

    #[test]
    fn body_number_beats_title_number() {
        assert_eq!(
            d("Alpha - Chapter 12 - W", "Rating: T\nChapter 10\n"),
            some("Alpha", 10)
        );
        assert_eq!(
            d("Alpha - Chapter 12 - W", "no chapter line\n"),
            some("Alpha", 12)
        );
    }

    #[test]
    fn publication_overrides_story_and_is_cleaned() {
        let t = detect_with(
            "Site Title - Chapter 3 - W",
            "",
            Some(" Delta:  The  Tale "),
        )
        .unwrap();
        assert_eq!((t.story.as_str(), t.chapter), ("Delta The Tale", 3));
        let t = detect_with("Site Title - Chapter 3 - W", "", Some(" : ")).unwrap();
        assert_eq!(t.story, "Site Title");
        assert_eq!(detect_with("Delta", "prose\n", Some("Delta")), None);
    }
}
