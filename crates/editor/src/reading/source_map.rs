//! Source ↔ rendered mapping for find (plan editor-round-4 WP-3): what Reading never displays
//! of a block's source, and a source match's rendered counterpart.

use editor_model::render::RenderModel;

/// The byte offset where 0-based `line` starts in `text` (`text.len()` past the end).
fn line_start(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    text.match_indices('\n')
        .nth(line - 1)
        .map_or(text.len(), |(i, _)| i + 1)
}

/// plan editor-round-4 WP-3: byte ranges of a block's source that Reading never displays —
/// md-link destinations `](url "title")`, image `![alt](src)` wholes, wiki-link targets hidden
/// by an alias (`[[target|`), `[[`/`]]` brackets, embeds `![[…]]` and code-fence lines.
pub fn invisible_ranges(src: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    // Code-fence lines (the info string is not painted; the fence is markup).
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        let t = t.trim_start_matches(['>', ' ']);
        if t.starts_with("```") || t.starts_with("~~~") {
            out.push(at..at + line.len());
        }
        at += line.len();
    }
    // Wiki-links and embeds.
    let mut i = 0;
    while let Some(off) = src[i..].find("[[") {
        let s = i + off;
        let Some(len) = src[s..].find("]]") else {
            break;
        };
        let e = s + len + 2;
        let embed = s > 0 && bytes[s - 1] == b'!';
        if embed {
            out.push(s - 1..e);
        } else {
            match src[s..e].find('|') {
                Some(p) => out.push(s..s + p + 1),
                None => out.push(s..s + 2),
            }
            out.push(e - 2..e);
        }
        i = e;
    }
    // Md-link destinations (and whole images).
    let mut i = 0;
    while let Some(off) = src[i..].find("](") {
        let s = i + off;
        let mut depth = 0usize;
        let mut end = src.len();
        for (k, c) in src[s + 1..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = s + 1 + k + 1;
                        break;
                    }
                }
                '\n' => {
                    end = s + 1 + k;
                    break;
                }
                _ => {}
            }
        }
        let open = src[..s].rfind('[');
        match open {
            Some(o) if o > 0 && bytes[o - 1] == b'!' => out.push(o - 1..end),
            _ => out.push(s..end),
        }
        i = end.max(s + 2);
    }
    out
}

/// plan editor-round-4 WP-3: the rendered counterpart (top-level block, nth visible
/// occurrence) of the source match `r` of `q` in `text`; `None` = outside every block, or an
/// invisible match (no rendered counterpart).
pub fn source_counterpart(
    model: &RenderModel,
    text: &str,
    q: &str,
    r: &std::ops::Range<usize>,
) -> Option<(usize, usize)> {
    let (line, _) = editor_model::find::line_col(text, r.start);
    let b = model.block_at_line(line)?;
    let lines = &model.block_lines[b];
    let from = line_start(text, lines.start);
    let to = line_start(text, lines.end).max(from);
    let src = &text[from..to];
    let rel = r.start.checked_sub(from)?;
    let hidden = invisible_ranges(src);
    let visible =
        |m: &std::ops::Range<usize>| !hidden.iter().any(|h| m.start < h.end && h.start < m.end);
    let ms = editor_model::find::matches(src, q);
    let idx = ms.iter().position(|m| m.start >= rel)?;
    if !visible(&ms[idx]) {
        return None;
    }
    Some((b, ms[..idx].iter().filter(|m| visible(m)).count()))
}
