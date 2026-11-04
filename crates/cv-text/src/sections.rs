//! Markdown section helpers used by capture targeting.
//!
//! Ported from `https://github.com/dulrich/obsidian-crucible/blob/master/src/sections.ts:21-97`. A section starts on a heading line
//! and runs until the next heading at the same or shallower level, a `---` rule, or EOF.
//! Nothing Obsidian-induced to drop: the TS module is already pure string logic.

/// The line range of a section within `content.split('\n')`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionRange {
    /// Index of the heading line itself.
    pub header_index: usize,
    /// Index of the first line after the section ends.
    pub end_index: usize,
    /// Heading level (count of leading `#`s); 0 if the anchor had none.
    pub level: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionInsertMode {
    Append,
    Prepend,
    Replace,
}

fn leading_hashes(s: &str) -> usize {
    s.chars().take_while(|&c| c == '#').count()
}

/// `^(#+)\s` on an already-trimmed line: the heading level, if it is a heading.
fn heading_level(trimmed: &str) -> Option<usize> {
    let n = leading_hashes(trimmed);
    if n == 0 {
        return None;
    }
    trimmed[n..]
        .chars()
        .next()
        .filter(|c| c.is_whitespace())
        .map(|_| n)
}

/// The title of an ATX heading line: leading whitespace trimmed, 1–6 `#`s, then a space or tab, then the text trimmed, closing `#`s stripped and trimmed again. `None` for a non-heading or an empty title.
#[must_use]
pub fn heading_text(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let hashes = leading_hashes(t);
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &t[hashes..];
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    (!text.is_empty()).then_some(text)
}

/// Locate the section anchored by `header` (matched against trimmed lines).
pub fn find_section_range(content: &str, header: &str) -> Option<SectionRange> {
    let header_trimmed = header.trim();
    if header_trimmed.is_empty() {
        return None;
    }
    let lines: Vec<&str> = content.split('\n').collect();
    let header_index = lines.iter().position(|l| l.trim() == header_trimmed)?;
    let level = leading_hashes(header_trimmed);
    let mut end_index = lines.len();
    for (i, line) in lines.iter().enumerate().skip(header_index + 1) {
        let trimmed = line.trim();
        if heading_level(trimmed).is_some_and(|l| level == 0 || l <= level) || trimmed == "---" {
            end_index = i;
            break;
        }
    }
    Some(SectionRange {
        header_index,
        end_index,
        level,
    })
}

/// True when the section is missing or holds only blank lines.
pub fn is_section_empty(content: &str, header: &str) -> bool {
    let Some(range) = find_section_range(content, header) else {
        return true;
    };
    content
        .split('\n')
        .skip(range.header_index + 1)
        .take(range.end_index - range.header_index - 1)
        .all(|l| l.trim().is_empty())
}

/// Insert `payload` into the section anchored by `header` as one entry (internal newlines kept).
/// A missing section is appended at the end of the document.
pub fn insert_into_section(
    content: &str,
    header: &str,
    payload: &str,
    mode: SectionInsertMode,
) -> String {
    let header_trimmed = header.trim();
    let Some(range) = find_section_range(content, header_trimmed) else {
        let separator = if content.trim().is_empty() {
            ""
        } else {
            "\n\n"
        };
        let header_part = if header_trimmed.is_empty() {
            String::new()
        } else {
            format!("{header_trimmed}\n\n")
        };
        return format!("{}{separator}{header_part}{payload}", content.trim_end());
    };
    let mut lines: Vec<&str> = content.split('\n').collect();
    let SectionRange {
        header_index,
        end_index,
        ..
    } = range;
    match mode {
        SectionInsertMode::Replace => {
            lines.splice(header_index + 1..end_index, [payload]);
        }
        SectionInsertMode::Prepend => lines.insert(header_index + 1, payload),
        SectionInsertMode::Append => {
            let insert_index = (header_index + 1..end_index)
                .rev()
                .find(|&i| !lines[i].trim().is_empty())
                .map_or(header_index + 1, |i| i + 1);
            lines.insert(insert_index, payload);
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "# Title\n## A\n- a1\n### A.sub\n- s1\n\n## B\n- b1\n---\nfooter";

    #[test]
    fn heading_text_truth_table() {
        let cases = [
            ("# A", Some("A")),
            ("  ## Spaced  ", Some("Spaced")),
            ("###\tTab", Some("Tab")),
            ("###### Six", Some("Six")),
            ("####### Seven", None),
            ("#NoSpace", None),
            ("## Closing ##", Some("Closing")),
            ("## Closing ## ", Some("Closing")),
            ("## #", None),
            ("##   ", None),
            ("#", None),
            ("plain", None),
            ("", None),
        ];
        for (line, want) in cases {
            assert_eq!(heading_text(line), want, "{line:?}");
        }
    }

    #[test]
    fn finds_range_with_subheadings_inside() {
        let r = find_section_range(DOC, "## A").unwrap();
        assert_eq!((r.header_index, r.end_index, r.level), (1, 6, 2));
        let b = find_section_range(DOC, "  ## B ").unwrap();
        assert_eq!(
            (b.header_index, b.end_index),
            (6, 8),
            "--- ends the section"
        );
        assert!(find_section_range(DOC, "## Missing").is_none());
        assert!(find_section_range(DOC, "   ").is_none());
    }

    #[test]
    fn non_heading_anchor_ends_at_any_heading() {
        let doc = "Tasks:\n- x\n#### deep\n- y";
        assert_eq!(find_section_range(doc, "Tasks:").unwrap().end_index, 2);
        // `#tag` is not a heading (no whitespace after hashes).
        let doc2 = "## A\n#tag line\n## B";
        assert_eq!(find_section_range(doc2, "## A").unwrap().end_index, 2);
    }

    #[test]
    fn empty_check() {
        assert!(is_section_empty("## A\n\n  \n## B\nx", "## A"));
        assert!(!is_section_empty(DOC, "## A"));
        assert!(is_section_empty(DOC, "## Nope"));
    }

    #[test]
    fn insert_modes() {
        let doc = "## A\n- one\n\n## B";
        assert_eq!(
            insert_into_section(doc, "## A", "- two", SectionInsertMode::Append),
            "## A\n- one\n- two\n\n## B"
        );
        assert_eq!(
            insert_into_section(doc, "## A", "- zero", SectionInsertMode::Prepend),
            "## A\n- zero\n- one\n\n## B"
        );
        assert_eq!(
            insert_into_section(doc, "## A", "new", SectionInsertMode::Replace),
            "## A\nnew\n## B"
        );
        assert_eq!(
            insert_into_section("## A\n\n## B", "## A", "x", SectionInsertMode::Append),
            "## A\nx\n\n## B"
        );
    }

    #[test]
    fn insert_missing_section_appends() {
        assert_eq!(
            insert_into_section("body\n\n", "## New", "p", SectionInsertMode::Append),
            "body\n\n## New\n\np"
        );
        assert_eq!(
            insert_into_section("", "## New", "p", SectionInsertMode::Append),
            "## New\n\np"
        );
        assert_eq!(
            insert_into_section("body", "", "p", SectionInsertMode::Append),
            "body\n\np"
        );
    }
}
