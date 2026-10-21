//! Newsletter-chrome cleanup for email bodies (plan `intake-followups-mail-youtube` WP-1).
//!
//! [`clean_body`] is a pure, idempotent markdown pass over what [`crate::html_to_markdown`]
//! produces for a newsletter. It exists because captured mail notes carried wrapper chrome:
//! tracking redirects, button rows, subscribe prompts and the unsubscribe footer. Nothing here
//! has an Obsidian or Crucible ancestor (Crucible never ingested mail bodies); the structure was
//! read from public Substack/beehiiv/HubSpot newsletter mail.
//!
//! What it does: (1) cuts the unsubscribe footer (the last `unsubscribe` link's paragraph to the
//! end, only when a short tail follows); (2) drops chrome lines (like/comment/restack/share
//! buttons, "Forwarded this email?", "Upgrade to paid", "Read in app", "View in browser", …) and
//! tracking pixels; (3) rewrites every link target: a Substack `/redirect/<n>/<payload>` is
//! unwrapped to its target (the payload is `base64url(JSON {"e": <url>, …}).<sig>`, decodable
//! offline; an undecodable one keeps the anchor text and drops the link, never the tracker), an
//! `/app-link/…` is canonicalized to its post URL when the query carries one (else anchor text),
//! beehiiv/HubSpot wrappers unwrap only when the target is literally in the URL, and tracking
//! query params are stripped (`utm_*` everywhere; `r`, `s`, `triedRedirect`, `publication_id`,
//! `post_id`, `isFreemail`, `token` on Substack hosts). Prose paragraphs are never removed or
//! altered other than link-target rewriting.
//!
//! The paywall-marker verdict must be taken on the **pre-cleanup** body by the caller (removing an
//! "Upgrade to paid" line must never turn a partial message complete).
//!
//! Pure: no I/O, no async.

use super::{percent_decode, unwrap_redirect};

/// Cleans newsletter chrome out of `md` (see the module doc). Idempotent.
#[must_use]
pub fn clean_body(md: &str) -> String {
    let lines: Vec<&str> = md.lines().collect();
    let fenced = fence_mask(&lines);
    let end = footer_start(&lines, &fenced).unwrap_or(lines.len());
    let mut out: Vec<String> = Vec::with_capacity(end);
    for (i, line) in lines[..end].iter().enumerate() {
        if fenced[i] {
            out.push((*line).to_owned());
        } else if is_chrome_line(line) {
            out.push(String::new());
        } else {
            out.push(rewrite_line(line));
        }
    }
    collapse_blanks(&out, &fenced[..end])
}

/// Which lines sit inside (or on the delimiter of) a fenced code block.
fn fence_mask(lines: &[&str]) -> Vec<bool> {
    let mut open: Option<String> = None;
    lines
        .iter()
        .map(|l| {
            let t = l.trim_start();
            let fence = ["```", "~~~"].into_iter().find(|f| t.starts_with(f));
            match (&open, fence) {
                (Some(o), Some(f)) if f == o => {
                    open = None;
                    true
                }
                (None, Some(f)) => {
                    open = Some(f.to_owned());
                    true
                }
                (o, _) => o.is_some(),
            }
        })
        .collect()
}

/// Joins the lines, dropping whitespace-only runs down to one blank line (outside code fences)
/// and the leading/trailing blank lines.
fn collapse_blanks(lines: &[String], fenced: &[bool]) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(lines.len());
    let mut prev_blank = true;
    for (l, f) in lines.iter().zip(fenced) {
        let blank = !f && l.trim().is_empty();
        if blank && prev_blank {
            continue;
        }
        out.push(if blank { "" } else { l });
        prev_blank = blank;
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// Most non-blank lines allowed after the unsubscribe paragraph for it to count as a footer.
const FOOTER_TAIL_MAX: usize = 15;

/// Index of the first line of the footer: the paragraph holding the last `unsubscribe` link
/// (plus a directly preceding `©` paragraph), when only a short tail follows it.
fn footer_start(lines: &[&str], fenced: &[bool]) -> Option<usize> {
    let hit = (0..lines.len()).rev().find(|&i| {
        !fenced[i]
            && lines[i].contains("](")
            && lines[i].to_ascii_lowercase().contains("unsubscribe")
    })?;
    let tail = lines[hit + 1..]
        .iter()
        .filter(|l| !l.trim().is_empty())
        .count();
    if tail > FOOTER_TAIL_MAX {
        return None;
    }
    let mut start = hit;
    while start > 0 && !lines[start - 1].trim().is_empty() {
        start -= 1;
    }
    // A copyright paragraph directly above (separate from the unsubscribe paragraph).
    let mut p = start;
    while p > 0 && lines[p - 1].trim().is_empty() {
        p -= 1;
    }
    if p < start && p > 0 {
        let mut q = p;
        while q > 0 && !lines[q - 1].trim().is_empty() {
            q -= 1;
        }
        if lines[q].trim_start().starts_with('©') {
            start = q;
        }
    }
    Some(start)
}

// ---- Markdown link/image scanning ----

/// One `[inner](dest)` (or `![alt](src)`) located in a line; byte offsets.
struct Span {
    /// Start of the item (the `!` of an image).
    start: usize,
    /// One past the closing `)`.
    end: usize,
    image: bool,
    inner: (usize, usize),
    dest: (usize, usize),
}

/// The link/image whose `[` (or `![`) sits at byte `at` of `s`.
fn scan(s: &str, at: usize) -> Option<Span> {
    let b = s.as_bytes();
    let image = b.get(at) == Some(&b'!');
    let open = if image { at + 1 } else { at };
    if b.get(open) != Some(&b'[') {
        return None;
    }
    let mut depth = 0usize;
    let mut i = open;
    let close = loop {
        match *b.get(i)? {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            _ => {}
        }
        i += 1;
    };
    if b.get(close + 1) != Some(&b'(') {
        return None;
    }
    let mut depth = 0usize;
    let mut j = close + 1;
    let end = loop {
        match *b.get(j)? {
            b'\\' => j += 1,
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    break j;
                }
            }
            _ => {}
        }
        j += 1;
    };
    Some(Span {
        start: at,
        end: end + 1,
        image,
        inner: (open + 1, close),
        dest: (close + 2, end),
    })
}

/// The items of a line made only of links/images separated by whitespace (else `None`).
fn only_items(line: &str) -> Option<Vec<Span>> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < line.len() {
        let c = line[i..].chars().next()?;
        if c.is_whitespace() || c == '\u{200b}' || c == '*' {
            i += c.len_utf8();
            continue;
        }
        let sp = scan(line, i)?;
        i = sp.end;
        items.push(sp);
    }
    (!items.is_empty()).then_some(items)
}

/// `s` with every image removed.
fn strip_images(s: &str) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with("![")
            && let Some(sp) = scan(s, i)
        {
            i = sp.end;
            continue;
        }
        let c = s[i..].chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    out
}

/// Lowercased label text: images and emphasis marks removed, whitespace collapsed, trailing
/// punctuation trimmed.
fn norm_label(s: &str) -> String {
    let t = strip_images(s).replace(['*', '_', '\u{200b}'], "");
    let t = t
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    t.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_owned()
}

/// Button/CTA labels that make a link-only line chrome.
const CHROME_LABELS: [&str; 17] = [
    "like",
    "comment",
    "comments",
    "restack",
    "share",
    "read in app",
    "subscribe now",
    "subscribe",
    "upgrade to paid",
    "share this post",
    "pledge your support",
    "view in browser",
    "view online",
    "view this post on the web",
    "leave a comment",
    "start writing",
    "get the app",
];

/// Substack's button icons are `substackcdn.com/image/fetch/…substack.com%2Ficon%2F…`.
fn is_icon(src: &str) -> bool {
    let l = src.to_ascii_lowercase();
    l.contains("substack.com%2ficon%2f") || l.contains("substack.com/icon/")
}

/// Whether a whole line is newsletter chrome (button row, CTA, or a "forwarded" prompt).
fn is_chrome_line(line: &str) -> bool {
    let flat = norm_label(line);
    if flat.starts_with("forwarded this email") || flat.starts_with("view this post on the web") {
        return true;
    }
    let Some(items) = only_items(line) else {
        return false;
    };
    let mut any_chrome = false;
    for sp in &items {
        if sp.image {
            // A bare tracking pixel / icon image on an otherwise chrome line.
            if is_pixel(&line[sp.dest.0..sp.dest.1]) || is_icon(&line[sp.dest.0..sp.dest.1]) {
                continue;
            }
            return false;
        }
        let inner = &line[sp.inner.0..sp.inner.1];
        let label = norm_label(inner);
        let has_icon = inner_images(inner).iter().any(|s| is_icon(s));
        let has_content_img = inner_images(inner)
            .iter()
            .any(|s| !is_icon(s) && !is_pixel(s));
        if has_content_img {
            return false;
        }
        let chrome = if label.is_empty() {
            has_icon
        } else {
            CHROME_LABELS.contains(&label.as_str())
        };
        if !chrome {
            return false;
        }
        any_chrome = true;
    }
    any_chrome || items.iter().all(|s| s.image)
}

/// The source URLs of the images inside `s`.
fn inner_images(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = s[i..].find("![") {
        let at = i + rel;
        if let Some(sp) = scan(s, at) {
            out.push(s[sp.dest.0..sp.dest.1].to_owned());
            i = sp.end;
        } else {
            i = at + 2;
        }
    }
    out
}

// ---- Tracking pixels ----

/// Whether an image URL is an open/pixel tracker (known tracker hosts and paths, or 1x1 hints).
fn is_pixel(src: &str) -> bool {
    let url = src.split_whitespace().next().unwrap_or("");
    let Some((host, path, query)) = split_url(url) else {
        return false;
    };
    let q = query.to_ascii_lowercase();
    host.starts_with("eotrx.")
        || (host.starts_with("email.mg") && host.ends_with(".substack.com"))
        || path.starts_with("/o/")
        || path.starts_with("/ss/o/")
        || path.starts_with("/e2t/o/")
        || path.starts_with("/wf/open")
        || path.to_ascii_lowercase().contains("pixel")
        || path.to_ascii_lowercase().contains("/track/open")
        || ["width=1&", "w=1&", "height=1&"]
            .iter()
            .any(|h| format!("{q}&").contains(h))
        || path.to_ascii_lowercase().contains("1x1")
}

// ---- Links ----

/// `(host lowercased, path, query)` of an http(s) URL (fragment dropped).
fn split_url(url: &str) -> Option<(String, String, String)> {
    let lower = url.to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return None;
    };
    let rest = rest.split('#').next().unwrap_or(rest);
    let (auth_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (host, path) = auth_path
        .find('/')
        .map_or((auth_path, ""), |i| (&auth_path[..i], &auth_path[i..]));
    let host = host.rsplit('@').next().unwrap_or(host);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    Some((host, path.to_owned(), query.to_owned()))
}

fn is_substack_host(host: &str) -> bool {
    host == "substack.com" || host.ends_with(".substack.com")
}

/// Query params removed from every URL (`utm_*`) and, on Substack hosts, the share/session ones.
fn strip_tracking(url: &str) -> String {
    let Some((host, _, _)) = split_url(url) else {
        return url.to_owned();
    };
    let (base, frag) = url
        .split_once('#')
        .map_or((url, None), |(b, f)| (b, Some(f)));
    let Some((head, query)) = base.split_once('?') else {
        return url.to_owned();
    };
    let sub = is_substack_host(&host);
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| {
            let k = p.split('=').next().unwrap_or("").to_ascii_lowercase();
            !(k.starts_with("utm_")
                || (sub
                    && [
                        "r",
                        "s",
                        "triedredirect",
                        "publication_id",
                        "post_id",
                        "isfreemail",
                        "token",
                    ]
                    .contains(&k.as_str())))
        })
        .collect();
    let mut out = head.to_owned();
    if !kept.is_empty() {
        out.push('?');
        out.push_str(&kept.join("&"));
    }
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Decodes unpadded/padded base64url (also tolerates `+`/`/`); `None` on a bad character.
fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// The string value of the `"e"` key of a small JSON object (`\/`, `\"`, `\\`, `\uXXXX` handled).
fn json_e(json: &str) -> Option<String> {
    let at = json.find("\"e\":\"")? + 5;
    let mut out = String::new();
    let mut it = json[at..].chars();
    while let Some(c) = it.next() {
        match c {
            '"' => return Some(out),
            '\\' => match it.next()? {
                'u' => {
                    let hex: String = it.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                'n' | 't' | 'r' => return None,
                e => out.push(e),
            },
            c => out.push(c),
        }
    }
    None
}

/// What a link target becomes.
enum Target {
    Keep(String),
    /// The target cannot be trusted or derived: keep the anchor text only.
    Drop,
}

fn resolve(url: &str) -> Target {
    let Some((host, path, query)) = split_url(url) else {
        return Target::Keep(url.to_owned());
    };
    if host == "substack.com" || host == "www.substack.com" {
        if let Some(rest) = path.strip_prefix("/redirect/") {
            let payload = rest.rsplit('/').next().unwrap_or("");
            let payload = payload.split('.').next().unwrap_or("");
            let target = b64url_decode(payload)
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|j| json_e(&j))
                .filter(|t| {
                    let l = t.to_ascii_lowercase();
                    l.starts_with("https://") || l.starts_with("http://")
                });
            return target.map_or(Target::Drop, |t| resolve(&t));
        }
        if path.starts_with("/app-link/") {
            return query
                .split('&')
                .filter_map(|p| p.split_once('='))
                .find(|(k, _)| {
                    ["redirect", "url", "u", "post", "post_url"]
                        .contains(&k.to_ascii_lowercase().as_str())
                })
                .map(|(_, v)| percent_decode(v))
                .filter(|t| {
                    let l = t.to_ascii_lowercase();
                    l.starts_with("https://") || l.starts_with("http://")
                })
                .map_or(Target::Drop, |t| Target::Keep(strip_tracking(&t)));
        }
    }
    if host == "link.mail.beehiiv.com" || host.ends_with(".hubspotlinks.com") {
        let t = unwrap_redirect(url);
        if t != url {
            return resolve(&t);
        }
    }
    Target::Keep(strip_tracking(url))
}

/// Rewrites the links of one line (pixels removed, redirects unwrapped, tracking params dropped).
fn rewrite_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        let c = line[i..].chars().next().unwrap_or(' ');
        if (c == '[' || c == '!')
            && let Some(sp) = scan(line, i)
        {
            let dest = &line[sp.dest.0..sp.dest.1];
            if sp.image {
                if !is_pixel(dest) {
                    out.push_str(&line[sp.start..sp.end]);
                }
            } else {
                let inner = rewrite_line(&line[sp.inner.0..sp.inner.1]);
                let (url, title) = dest
                    .split_once(char::is_whitespace)
                    .map_or((dest, ""), |(u, t)| (u, t));
                match resolve(url) {
                    Target::Keep(k) => {
                        out.push('[');
                        out.push_str(&inner);
                        out.push_str("](");
                        out.push_str(&k);
                        if !title.is_empty() {
                            out.push(' ');
                            out.push_str(title);
                        }
                        out.push(')');
                    }
                    Target::Drop => out.push_str(&inner),
                }
            }
            i = sp.end;
            continue;
        }
        out.push(c);
        i += c.len_utf8();
    }
    out
}

#[cfg(test)]
#[path = "mail_clean_tests.rs"]
mod tests;
