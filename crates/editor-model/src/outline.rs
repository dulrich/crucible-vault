//! Outline extraction from the buffer's `ofm` parse (WP-5).
//!
//! Headings come from [`ofm::parse`] of the live buffer text (unsaved edits included), so fenced
//! code, frontmatter and other non-heading `#` lines are excluded by the shared tokenizer (D12).
//! Lines are 0-based (they feed `text_editor` cursor positions).

/// One outline entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineItem {
    /// 1..=6.
    pub level: u8,
    pub text: String,
    /// 0-based line of the heading's first source line.
    pub line: usize,
}

/// The 0-based line containing byte `offset` of `source` (clamped to the end).
pub fn line_of(source: &str, offset: usize) -> usize {
    let end = offset.min(source.len());
    source.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

/// Headings of an already-parsed note, in source order.
pub fn from_note(source: &str, note: &ofm::Note) -> Vec<OutlineItem> {
    let mut items: Vec<OutlineItem> = note
        .headings
        .iter()
        .map(|h| OutlineItem {
            level: h.level,
            text: h.text.clone(),
            line: line_of(source, h.span.start),
        })
        .collect();
    items.sort_by_key(|i| i.line);
    items
}

/// Parses `source` and returns its headings.
pub fn outline(source: &str) -> Vec<OutlineItem> {
    from_note(source, &ofm::parse(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "---\ntitle: x\ntags: [a]\n---\n# Top\n\ntext # not a heading\n\n```sh\n# comment in code\n```\n\n## Second ##\n\nSetext\n======\n\nSub setext\n----------\n\n###### Six\n";

    #[test]
    fn headings_levels_and_lines() {
        let o = outline(FIXTURE);
        let got: Vec<(u8, &str, usize)> = o
            .iter()
            .map(|i| (i.level, i.text.as_str(), i.line))
            .collect();
        assert_eq!(
            got,
            vec![
                (1, "Top", 4),
                (2, "Second", 12),
                (1, "Setext", 14),
                (2, "Sub setext", 17),
                (6, "Six", 20),
            ]
        );
    }

    #[test]
    fn reflects_edits() {
        let edited = FIXTURE.replace("# Top", "# Renamed\n\n### Inserted");
        let o = outline(&edited);
        assert_eq!(o[0].text, "Renamed");
        assert_eq!(
            (o[1].level, o[1].text.as_str(), o[1].line),
            (3, "Inserted", 6)
        );
    }

    #[test]
    fn line_of_clamps() {
        assert_eq!(line_of("a\nb\nc", 0), 0);
        assert_eq!(line_of("a\nb\nc", 2), 1);
        assert_eq!(line_of("a\nb", 999), 1);
    }
}
