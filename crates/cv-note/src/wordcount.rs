//! Prose-only word count — Crucible `stripNonProseContent` / `calculateWordCount`
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/lint.ts:95-139`, quirk `src/AGENTS.md:18,39`).
//!
//! Markup (fenced/inline code, HTML comments, `<svg>`/`<script>`/`<style>` blocks, image embeds)
//! is deleted and `[text](url)` / `[[page|alias]]` links reduce to their visible text before
//! counting, so an inline chart's coordinate tokens never inflate `word-count`. **The strip
//! steps are order-sensitive** (outer/greedy constructs first) — keep the order.
//!
//! Deviations: the Rust `regex` crate has no back-references, so the fenced-code and HTML-block
//! patterns (`\1`) are matched by hand with the same leftmost/lazy semantics. `Intl.Segmenter`'s
//! `isWordLike` is approximated by UAX #29-style word runs (letters/digits/`_`, joined across a
//! single `'` `.` `:` between letters or `,` `.` `;` between digits; each CJK ideograph/kana is
//! one word). Nothing Obsidian-induced to drop.

use std::sync::LazyLock;

use regex::{Captures, Regex};

fn re(p: &str) -> Option<Regex> {
    Regex::new(p).ok()
}

static INLINE_CODE_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"`[^`]*`"));
static HTML_COMMENT_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"(?s)<!--.*?-->"));
static HTML_BLOCK_OPEN_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"(?i)<(svg|script|style)\b"));
static MD_IMAGE_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"!\[[^\]]*\]\([^)]*\)"));
static WIKI_EMBED_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"!\[\[[^\]]*\]\]"));
static MD_LINK_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"\[([^\]]*)\]\([^)]*\)"));
static WIKILINK_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"\[\[([^\]|]*)(?:\|([^\]]*))?\]\]"));
static HTML_TAG_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"</?[a-zA-Z][^>]*>"));

fn replace_all(r: &LazyLock<Option<Regex>>, s: &str, with: &str) -> String {
    match r.as_ref() {
        Some(r) => r.replace_all(s, with).into_owned(),
        None => s.to_owned(),
    }
}

/// `(`{3,}|~{3,})[\s\S]*?\1` → `' '`.
fn strip_fenced(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'`' || c == b'~' {
            let mut j = i;
            while j < b.len() && b[j] == c {
                j += 1;
            }
            let run = j - i;
            if run >= 3 {
                // Greedy `{3,}` with backtracking: try the longest opener first.
                let mut found = None;
                for n in (3..=run).rev() {
                    let fence = &s[i..i + n];
                    if let Some(k) = s[i + n..].find(fence) {
                        found = Some(i + n + k + n);
                        break;
                    }
                }
                if let Some(end) = found {
                    out.push_str(&s[copied..i]);
                    out.push(' ');
                    copied = end;
                    i = end;
                    continue;
                }
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    out.push_str(&s[copied..]);
    out
}

/// `<(svg|script|style)\b[\s\S]*?<\/\1>` (case-insensitive) → `' '`.
fn strip_html_blocks(s: &str) -> String {
    let Some(open) = HTML_BLOCK_OPEN_RE.as_ref() else {
        return s.to_owned();
    };
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut copied = 0;
    let mut at = 0;
    while let Some(caps) = open.captures_at(s, at) {
        let (Some(m), Some(name)) = (caps.get(0), caps.get(1)) else {
            break;
        };
        let close = format!("</{}>", name.as_str().to_ascii_lowercase());
        match lower[m.end()..].find(&close) {
            Some(k) => {
                let end = m.end() + k + close.len();
                out.push_str(&s[copied..m.start()]);
                out.push(' ');
                copied = end;
                at = end;
            }
            None => at = m.start() + 1,
        }
        if at >= s.len() {
            break;
        }
    }
    out.push_str(&s[copied..]);
    out
}

/// Reduce body markup to readable prose. Order matters — see the module doc.
pub fn strip_non_prose_content(body: &str) -> String {
    let s = strip_fenced(body);
    let s = replace_all(&INLINE_CODE_RE, &s, " ");
    let s = replace_all(&HTML_COMMENT_RE, &s, " ");
    let s = strip_html_blocks(&s);
    let s = replace_all(&MD_IMAGE_RE, &s, " ");
    let s = replace_all(&WIKI_EMBED_RE, &s, " ");
    let s = replace_all(&MD_LINK_RE, &s, "$1");
    let s = match WIKILINK_RE.as_ref() {
        Some(r) => r
            .replace_all(&s, |c: &Captures<'_>| {
                c.get(2)
                    .or_else(|| c.get(1))
                    .map_or(String::new(), |m| m.as_str().to_owned())
            })
            .into_owned(),
        None => s,
    };
    replace_all(&HTML_TAG_RE, &s, " ")
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF)
}

fn is_word_char(c: char) -> bool {
    (c.is_alphanumeric() || c == '_') && !is_cjk(c)
}

/// Count word-like segments (see the module doc for the approximation).
pub fn count_words(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let mut count = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if is_cjk(c) {
            count += 1;
            i += 1;
            continue;
        }
        if !is_word_char(c) {
            i += 1;
            continue;
        }
        count += 1;
        i += 1;
        loop {
            while i < chars.len() && is_word_char(chars[i]) {
                i += 1;
            }
            let (Some(&mid), Some(&next), Some(&prev)) =
                (chars.get(i), chars.get(i + 1), chars.get(i.wrapping_sub(1)))
            else {
                break;
            };
            if !is_word_char(next) {
                break;
            }
            let joins = match mid {
                '\'' | '\u{2019}' | '.' => true,
                ':' | '\u{00B7}' => prev.is_alphabetic() && next.is_alphabetic(),
                ',' | ';' => prev.is_numeric() && next.is_numeric(),
                _ => false,
            };
            if !joins {
                break;
            }
            i += 1;
        }
    }
    count
}

/// Crucible `calculateWordCount`: frontmatter removed, markup stripped, words counted.
pub fn calculate_word_count(content: &str) -> usize {
    let (_, body) = ofm::frontmatter::split_frontmatter(content);
    count_words(&strip_non_prose_content(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors ported from https://github.com/dulrich/obsidian-crucible/blob/master/tests/lint.wordcount.test.mjs.

    #[test]
    fn inline_svg_contributes_no_words() {
        let svg = r#"<svg version="1.1" width="620" height="500"><path d="M 0 98.75 L 2.15 92.38 L 4.3 92.80 L 6.45 105.86"></path><text>Index performance</text></svg>"#;
        assert_eq!(
            calculate_word_count(&format!("Before chart.\n\n{svg}\n\nAfter chart.")),
            4
        );
        assert_eq!(strip_non_prose_content(svg).trim(), "");
    }

    #[test]
    fn carnage_example_counts_far_below_inflated() {
        let mut path = String::from("M 0 98.75 ");
        let segs: Vec<String> = (0..200)
            .map(|i| format!("L {} {}", f64::from(i) * 2.15, 90 + i))
            .collect();
        path.push_str(&segs.join(" "));
        let body = [
            "Fears of rising interest rates collided with worries about AI spending on Wall Street.".to_owned(),
            String::new(),
            format!(r#"<svg width="620" height="500"><path d="{path}"></path><path d="{path}"></path></svg>"#),
            String::new(),
            "Stocks, bonds, oil, gold and bitcoin all tumbled.".to_owned(),
        ]
        .join("\n");
        let count = calculate_word_count(&body);
        assert!(count < 50, "expected prose-only count, got {count}");
    }

    #[test]
    fn fenced_and_inline_code_excluded() {
        let body = "Here is some prose.\n\n```js\nconst x = computeSomething(alpha, beta, gamma);\n```\n\nTrailing `inlineCodeToken` prose.";
        assert_eq!(calculate_word_count(body), 6);
    }

    #[test]
    fn embeds_drop_links_reduce() {
        assert_eq!(calculate_word_count("![](photos/img.png)"), 0);
        assert_eq!(calculate_word_count("![[some/embed.png]]"), 0);
        assert_eq!(
            calculate_word_count("See [the report](https://example.com/very/long/url) now."),
            4
        );
        assert_eq!(
            calculate_word_count("Read [[Some Page|the alias]] today."),
            4
        );
        assert_eq!(calculate_word_count("Read [[PlainTarget]] today."), 3);
    }

    #[test]
    fn plain_prose() {
        assert_eq!(
            calculate_word_count("The quick brown fox jumps over the lazy dog."),
            9
        );
    }

    #[test]
    fn frontmatter_excluded() {
        let body = "---\ntitle: Some Title\nword-count: 999\ntags:\n  - clippings\n---\n\nJust three words.";
        assert_eq!(calculate_word_count(body), 3);
    }

    #[test]
    fn segmenter_approximation() {
        assert_eq!(count_words("don't stop 3.14 1,000 e.g. prose-only"), 7);
        assert_eq!(count_words("日本語"), 3);
    }

    #[test]
    fn tilde_fence_and_case_insensitive_block() {
        assert_eq!(calculate_word_count("a ~~~\nx y z\n~~~ b"), 2);
        assert_eq!(calculate_word_count("a <SCRIPT>x y</script> b"), 2);
    }
}
