//! Stateful markdown line highlighter (plan `v0-editor.md` WP-2).
//!
//! A hand-written line scanner — no syntect/tree-sitter and no second markdown parser (D12). It
//! matches iced's line-at-a-time `Highlighter` contract: [`scan_line`] takes the [`LineState`]
//! at the start of a line and returns the spans plus the state for the next line. The iced
//! adapter (`crates/editor/src/source`) caches one state per line so `change_line` re-entry
//! resumes from the saved state instead of rescanning the document.
//!
//! Spans are byte ranges into the line, sorted and non-overlapping.

use std::ops::Range;

/// What a span is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    FrontmatterKey,
    FrontmatterValue,
    /// Fence lines (```` ``` ````/`~~~`) and the frontmatter `---` delimiters.
    FenceMarker,
    /// Fenced-code lines and inline code.
    Code,
    Heading(u8),
    Wikilink,
    Embed,
    MdLink,
    Tag,
    Emphasis,
    Strong,
    /// `==text==`.
    Highlight,
    /// The `[x]` of a task item, carrying the status char (incl. `>`).
    TaskMarker(char),
    /// `> [!type] title`.
    CalloutHeader,
    /// `[^id]` references and definitions.
    Footnote,
}

/// Block context carried between lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Block {
    /// Before the first line (frontmatter may open here only).
    #[default]
    DocStart,
    Body,
    Frontmatter,
    /// Inside a fenced code block opened by `len` × `ch`.
    Fence {
        ch: u8,
        len: usize,
    },
}

/// State at the start of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineState {
    pub block: Block,
    /// The previous line was part of a callout (`> [!…]` header or its `>` continuation).
    pub in_callout: bool,
}

/// One span.
pub type Span = (Range<usize>, Kind);

fn fence_open(line: &str) -> Option<(u8, usize)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let ch = *rest.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = rest.bytes().take_while(|&b| b == ch).count();
    if len < 3 || (ch == b'`' && rest[len..].contains('`')) {
        return None;
    }
    Some((ch, len))
}

fn fence_closes(line: &str, ch: u8, len: usize) -> bool {
    let t = line.trim();
    let n = t.bytes().take_while(|&b| b == ch).count();
    n >= len && n == t.len() && line.len() - line.trim_start_matches(' ').len() <= 3
}

/// Scans one line (without its line ending) from `state`.
pub fn scan_line(state: LineState, line: &str) -> (Vec<Span>, LineState) {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let full = 0..line.len();
    match state.block {
        Block::DocStart if line.trim_end() == "---" => (
            vec![(full, Kind::FenceMarker)],
            LineState {
                block: Block::Frontmatter,
                in_callout: false,
            },
        ),
        Block::Frontmatter => {
            let t = line.trim_end();
            if t == "---" || t == "..." {
                return (
                    vec![(full, Kind::FenceMarker)],
                    LineState {
                        block: Block::Body,
                        in_callout: false,
                    },
                );
            }
            (frontmatter_line(line), state)
        }
        Block::Fence { ch, len } => {
            if fence_closes(line, ch, len) {
                (
                    vec![(full, Kind::FenceMarker)],
                    LineState {
                        block: Block::Body,
                        in_callout: false,
                    },
                )
            } else if line.is_empty() {
                (Vec::new(), state)
            } else {
                (vec![(full, Kind::Code)], state)
            }
        }
        Block::DocStart | Block::Body => {
            if let Some((ch, len)) = fence_open(line) {
                return (
                    vec![(full, Kind::FenceMarker)],
                    LineState {
                        block: Block::Fence { ch, len },
                        in_callout: false,
                    },
                );
            }
            let (spans, in_callout) = body_line(line, state.in_callout);
            (
                spans,
                LineState {
                    block: Block::Body,
                    in_callout,
                },
            )
        }
    }
}

/// Scans a whole document (convenience for tests and one-shot callers).
pub fn scan_document(text: &str) -> Vec<Vec<Span>> {
    let mut st = LineState::default();
    text.split('\n')
        .map(|l| {
            let (s, next) = scan_line(st, l);
            st = next;
            s
        })
        .collect()
}

fn frontmatter_line(line: &str) -> Vec<Span> {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent..];
    if let Some(stripped) = rest.strip_prefix("- ") {
        let start = line.len() - stripped.len();
        return if stripped.trim().is_empty() {
            Vec::new()
        } else {
            vec![(start..line.len(), Kind::FrontmatterValue)]
        };
    }
    let Some(colon) = rest.find(':') else {
        return Vec::new();
    };
    let mut out = vec![(indent..indent + colon, Kind::FrontmatterKey)];
    let after = indent + colon + 1;
    let vstart = after + (line[after..].len() - line[after..].trim_start().len());
    if vstart < line.len() {
        out.push((vstart..line.len(), Kind::FrontmatterValue));
    }
    out
}

fn body_line(line: &str, prev_callout: bool) -> (Vec<Span>, bool) {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();

    // Headings own the whole line.
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if (1..=6).contains(&hashes)
        && indent <= 3
        && (trimmed.len() == hashes || trimmed.as_bytes().get(hashes) == Some(&b' '))
    {
        // `hashes` ≤ 6 so the cast cannot truncate.
        return (vec![(0..line.len(), Kind::Heading(hashes as u8))], false);
    }

    // Callouts: `> [!type] title` header; `>` lines continue it.
    if let Some(q) = trimmed.strip_prefix('>') {
        let q_off = line.len() - q.len();
        let inner = q.trim_start();
        let inner_off = line.len() - inner.len();
        if inner.starts_with("[!")
            && let Some(close) = inner.find(']')
            && close > 2
        {
            return (vec![(inner_off..line.len(), Kind::CalloutHeader)], true);
        }
        return (inline(line, q_off), prev_callout);
    }

    let mut out = Vec::new();
    let mut from = indent;
    // Task marker: `- [x] ` / `* [ ] ` / `+ [>] ` / `1. [x] `.
    let bullet = match trimmed.as_bytes().first() {
        Some(b'-' | b'*' | b'+') => Some(1),
        Some(b) if b.is_ascii_digit() => {
            let d = trimmed.bytes().take_while(u8::is_ascii_digit).count();
            matches!(trimmed.as_bytes().get(d), Some(b'.' | b')')).then_some(d + 1)
        }
        _ => None,
    };
    if let Some(b) = bullet
        && let Some(after) = trimmed[b..].strip_prefix(' ')
        && let Some(body) = after.strip_prefix('[')
    {
        let mut chars = body.chars();
        if let (Some(c), Some(']')) = (chars.next(), chars.next()) {
            let start = line.len() - after.len();
            let end = start + 2 + c.len_utf8();
            out.push((start..end, Kind::TaskMarker(c)));
            from = end;
        }
    }
    out.extend(inline(line, from));
    (out, false)
}

fn is_tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}

/// Inline spans of `line[from..]`.
fn inline(line: &str, from: usize) -> Vec<Span> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = from;
    while i < b.len() {
        let rest = &line[i..];
        // Inline code: a run of N backticks closed by the same run.
        if b[i] == b'`' {
            let n = rest.bytes().take_while(|&c| c == b'`').count();
            let tick = &rest[..n];
            if let Some(close) = rest[n..].find(tick) {
                let end = i + n + close + n;
                out.push((i..end, Kind::Code));
                i = end;
            } else {
                i += n;
            }
            continue;
        }
        // Bare URLs: skip to whitespace so `#frag` is never a tag.
        if rest.starts_with("http://") || rest.starts_with("https://") {
            i += rest.find(char::is_whitespace).unwrap_or(rest.len());
            continue;
        }
        if rest.starts_with("![[")
            && let Some(close) = rest[3..].find("]]")
        {
            let end = i + 3 + close + 2;
            out.push((i..end, Kind::Embed));
            i = end;
            continue;
        }
        if rest.starts_with("[[")
            && let Some(close) = rest[2..].find("]]")
        {
            let end = i + 2 + close + 2;
            out.push((i..end, Kind::Wikilink));
            i = end;
            continue;
        }
        if rest.starts_with("[^")
            && let Some(close) = rest.find(']')
            && close > 2
        {
            let mut end = i + close + 1;
            if b.get(end) == Some(&b':') {
                end += 1;
            }
            out.push((i..end, Kind::Footnote));
            i = end;
            continue;
        }
        if b[i] == b'[' || rest.starts_with("![") {
            let open = if b[i] == b'!' { 1 } else { 0 };
            if let Some(close) = rest.find("](")
                && close > open
                && let Some(paren) = rest[close + 2..].find(')')
            {
                let end = i + close + 2 + paren + 1;
                out.push((i..end, Kind::MdLink));
                i = end;
                continue;
            }
        }
        if rest.starts_with("==")
            && let Some(close) = rest[2..].find("==")
            && close > 0
        {
            let end = i + 2 + close + 2;
            out.push((i..end, Kind::Highlight));
            i = end;
            continue;
        }
        if (rest.starts_with("**") || rest.starts_with("__"))
            && let Some(close) = rest[2..].find(&rest[..2])
            && close > 0
        {
            let end = i + 2 + close + 2;
            out.push((i..end, Kind::Strong));
            i = end;
            continue;
        }
        if (b[i] == b'*' || b[i] == b'_')
            && b.get(i + 1)
                .is_some_and(|c| !c.is_ascii_whitespace() && *c != b[i])
            && (b[i] == b'*' || i == 0 || !b[i - 1].is_ascii_alphanumeric())
            && let Some(close) = rest[1..].find(b[i] as char)
            && close > 0
        {
            let end = i + 1 + close + 1;
            out.push((i..end, Kind::Emphasis));
            i = end;
            continue;
        }
        if b[i] == b'#' && (i == 0 || b[i - 1].is_ascii_whitespace()) {
            let len: usize = rest[1..]
                .chars()
                .take_while(|&c| is_tag_char(c))
                .map(char::len_utf8)
                .sum();
            let body = &rest[1..1 + len];
            if len > 0 && !body.chars().all(|c| c.is_ascii_digit()) {
                out.push((i..i + 1 + len, Kind::Tag));
                i += 1 + len;
                continue;
            }
        }
        // Advance one char.
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    out
}

#[cfg(test)]
mod tests;
