//! HTML → Markdown for blog bodies (over `htmd`, plan `v0-automation-pipelines` dependency
//! posture) and the display word count.
//!
//! Replaces Crucible's use of Obsidian's `htmlToMarkdown` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/
//! orchestration/utils/blogsApi.ts:74`) — an Obsidian-provided API, so the dependency is the
//! constraint, not the behaviour. If the output starts needing repeated post-processing shims,
//! that is the own-vs-depend trigger (principle #7), not a reason to add more shims here.

/// Converts `html` to Markdown. A converter failure falls back to the tag-stripped text, so a
/// body is never silently dropped.
#[must_use]
pub fn html_to_markdown(html: &str) -> String {
    match htmd::convert(html) {
        Ok(md) => md.trim().to_owned(),
        Err(_) => strip_tags(html)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Word count of body HTML: drop `script`/`style` blocks, strip tags and entities, count
/// whitespace-separated tokens. A rough display metric only — never used to decide whether a post
/// has a body. Port of Crucible `countWords` (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/
/// blogs.ts:513-525`).
#[must_use]
pub fn word_count(html: &str) -> usize {
    strip_tags(html).split_whitespace().count()
}

/// Plain text of `html` on one line (tags dropped, the common entities decoded, whitespace
/// collapsed) — the captured blog note's `description` (plan `v0-automation-pipelines` WP-8).
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let pre = html
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    strip_tags(&pre)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Replaces `<script>…</script>`/`<style>…</style>` blocks, tags and `&name;`/`&#n;` entities with
/// spaces.
fn strip_tags(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let rest = &html[i..];
        let lrest = &lower[i..];
        if lrest.starts_with("<script") || lrest.starts_with("<style") {
            let tag = if lrest.starts_with("<script") {
                "</script>"
            } else {
                "</style>"
            };
            if let Some(end) = lrest.find(tag) {
                out.push(' ');
                i += end + tag.len();
                continue;
            }
        }
        if rest.starts_with('<')
            && let Some(end) = rest.find('>')
        {
            out.push(' ');
            i += end + 1;
            continue;
        }
        if rest.starts_with('&')
            && let Some(end) = rest[1..].find(';')
        {
            let name = &rest[1..=end];
            let is_named = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphabetic());
            let is_num = name.len() > 1
                && name.starts_with('#')
                && name[1..].chars().all(|c| c.is_ascii_digit());
            if is_named || is_num {
                out.push(' ');
                i += end + 2;
                continue;
            }
        }
        let ch = rest.chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_common_markup() {
        let md = html_to_markdown(
            "<h2>Title</h2><p>Some <strong>bold</strong> and <a href=\"https://x.example/\">a link</a>.</p><ul><li>one</li></ul>",
        );
        assert!(md.contains("## Title"), "{md}");
        assert!(md.contains("**bold**"), "{md}");
        assert!(md.contains("[a link](https://x.example/)"), "{md}");
        assert!(md.contains("one"), "{md}");
    }

    #[test]
    fn word_count_matches_crucible_rules() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("<p>Hello&nbsp;big <b>world</b></p>"), 3);
        assert_eq!(
            word_count("<style>p{}</style><p>one two</p><script>var a = 1;</script>"),
            2
        );
        assert_eq!(word_count("caf\u{e9} &#8217; ok"), 2);
    }
}
