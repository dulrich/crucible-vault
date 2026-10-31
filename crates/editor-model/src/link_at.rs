//! Plan usability-sprint-captures-links WP-3: the link under a source-mode Ctrl+click.
//!
//! Independent of the highlighter on purpose (audit item E1): bare URLs emit no highlight span
//! (so `#frag` is never a tag), so the URL under the click is extracted from the line text here.

/// What a click position lands on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkAt {
    /// A `[[wikilink]]` / `![[embed]]`: its target (no `#fragment`, no `|alias`; empty for a
    /// same-note `[[#Heading]]`) and its `#fragment`, trimmed (plan round-19-yt-cleanup-intake
    /// WP-1).
    Wiki {
        target: String,
        fragment: Option<String>,
    },
    /// A `[label](url)`: the URL (remote or a vault path).
    Md(String),
    /// A bare `http(s)://…` / `mailto:…` run.
    Url(String),
}

const SCHEMES: [&str; 3] = ["https://", "http://", "mailto:"];

fn floor_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn wiki_at(line: &str, col: usize) -> Option<LinkAt> {
    let mut from = 0;
    while let Some(o) = line[from..].find("[[") {
        let open = from + o;
        let close = line[open + 2..].find("]]")? + open + 2;
        let end = close + 2;
        let start = if open > 0 && line.as_bytes()[open - 1] == b'!' {
            open - 1
        } else {
            open
        };
        if (start..end).contains(&col) {
            let inner = &line[open + 2..close];
            let target = inner.split('|').next().unwrap_or("");
            let (target, fragment) = match target.split_once('#') {
                Some((t, f)) => (t.trim(), Some(f.trim()).filter(|f| !f.is_empty())),
                None => (target.trim(), None),
            };
            return (!target.is_empty() || fragment.is_some()).then(|| LinkAt::Wiki {
                target: target.to_owned(),
                fragment: fragment.map(str::to_owned),
            });
        }
        from = end;
    }
    None
}

fn md_at(line: &str, col: usize) -> Option<LinkAt> {
    let mut from = 0;
    while let Some(o) = line[from..].find("](") {
        let mid = from + o;
        let Some(open) = line[..mid].rfind('[') else {
            from = mid + 2;
            continue;
        };
        let p = line[mid + 2..].find(')')?;
        let end = mid + 2 + p + 1;
        let start = if open > 0 && line.as_bytes()[open - 1] == b'!' {
            open - 1
        } else {
            open
        };
        if (start..end).contains(&col) {
            let raw = line[mid + 2..end - 1].trim();
            let url = if let Some(r) = raw.strip_prefix('<') {
                r.split('>').next().unwrap_or("")
            } else {
                raw.split_whitespace().next().unwrap_or("")
            };
            return (!url.is_empty()).then(|| LinkAt::Md(url.to_owned()));
        }
        from = end;
    }
    None
}

fn url_at(line: &str, col: usize) -> Option<LinkAt> {
    let tok_start = line[..col].rfind(char::is_whitespace).map_or(0, |i| {
        i + line[i..].chars().next().map_or(1, char::len_utf8)
    });
    let tok_end = line[col..]
        .find(char::is_whitespace)
        .map_or(line.len(), |i| col + i);
    let tok = &line[tok_start..tok_end];
    let lower = tok.to_ascii_lowercase();
    let start = SCHEMES
        .iter()
        .filter_map(|s| lower.find(s))
        .filter(|&i| tok_start + i <= col)
        .min()?;
    let url = tok[start..].trim_end_matches([')', ']', '>', '.', ',', ';', '!', '?', '"', '\'']);
    let scheme_len = SCHEMES
        .iter()
        .find(|s| lower[start..].starts_with(*s))
        .map_or(0, |s| s.len());
    let end = tok_start + start + url.len();
    (url.len() > scheme_len && col < end).then(|| LinkAt::Url(url.to_owned()))
}

/// The link under byte column `col` of `line` (clamped to a char boundary): a wikilink, an
/// md-link or a bare URL, in that precedence.
pub fn link_at(line: &str, col: usize) -> Option<LinkAt> {
    let col = floor_boundary(line, col);
    if col >= line.len() {
        return None;
    }
    wiki_at(line, col)
        .or_else(|| md_at(line, col))
        .or_else(|| url_at(line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wiki(t: &str, f: Option<&str>) -> Option<LinkAt> {
        Some(LinkAt::Wiki {
            target: t.to_owned(),
            fragment: f.map(str::to_owned),
        })
    }

    fn url(s: &str) -> Option<LinkAt> {
        Some(LinkAt::Url(s.to_owned()))
    }

    #[test]
    fn bare_url_with_fragment_and_trailing_punctuation() {
        let line = "see (https://ex.com/a?b=1#frag), then";
        let at = line.find("#frag").expect("frag");
        assert_eq!(link_at(line, at), url("https://ex.com/a?b=1#frag"));
        assert_eq!(
            link_at(line, line.find("https").expect("h")),
            url("https://ex.com/a?b=1#frag")
        );
        assert_eq!(link_at(line, line.find("then").expect("t")), None);
        assert_eq!(link_at(line, 0), None);
        assert_eq!(link_at("<mailto:me@x.y>.", 3), url("mailto:me@x.y"));
        assert_eq!(link_at("end https://x.y.", 15), None, "the trimmed dot");
        assert_eq!(link_at("http://", 2), None, "scheme only");
    }

    #[test]
    fn wiki_and_md_links() {
        let line = "a [[Note#H|alias]] b [lbl](https://x.y/z \"t\") c ![i](img.png)";
        assert_eq!(link_at(line, 5), wiki("Note", Some("H")));
        let at = line.find("lbl").expect("lbl");
        assert_eq!(link_at(line, at), Some(LinkAt::Md("https://x.y/z".into())));
        let at = line.find("img").expect("img");
        assert_eq!(link_at(line, at), Some(LinkAt::Md("img.png".into())));
        assert_eq!(link_at(line, 1), None);
        assert_eq!(link_at("x ![[pic.png]]", 2), wiki("pic.png", None));
        assert_eq!(link_at("[[N]]", 2), wiki("N", None));
        assert_eq!(link_at("[[#Head er]]", 2), wiki("", Some("Head er")));
        assert_eq!(link_at("[[]]", 2), None);
    }
}
