//! `.eml` parsing for the Maildir newsletter intake (plan `auto-intake-sprint` WP-4).
//!
//! [`parse_eml`] turns raw message bytes into a [`MailPost`] (over `mail-parser`: MIME multipart,
//! quoted-printable/base64 and non-UTF-8 charsets are decoded there). [`match_newsletter`] picks
//! the configured `[[newsletter]]` row for a message by `From` (exact address or `@domain`
//! suffix) or `List-Id`. Candidate post URLs are read from the HTML: Substack `/p/<slug>`,
//! `open.substack.com/pub/<pub>/p/<slug>` and a "view in browser/online" anchor. A tracking
//! redirect is unwrapped **only** when the target URL is literally in its query
//! (`?url=`/`?u=`/`?redirect=`); nothing is ever fetched, so an ESP redirect with an opaque token
//! yields no URL and the supersede engine's containment pass is the dedup path.
//!
//! [`clean_body`] (plan `intake-followups-mail-youtube` WP-1) strips newsletter chrome from the
//! converted markdown.
//!
//! Pure: no I/O, no async.

use mail_parser::MessageParser;

#[path = "mail_clean.rs"]
mod clean;
pub use clean::clean_body;

/// One parsed newsletter message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailPost {
    /// `Message-ID` (without angle brackets), or `sha:<hash of the raw bytes>` when absent.
    pub message_id: String,
    /// The sender address, lowercased (empty when absent).
    pub from_addr: String,
    /// The sender display name (empty when absent).
    pub from_name: String,
    /// `List-Id` (the id inside `<>`, lowercased), when present.
    pub list_id: Option<String>,
    /// Subject (empty when absent).
    pub subject: String,
    /// `Date` as RFC 3339, when present.
    pub date: Option<String>,
    /// The decoded HTML body, when the message has one.
    pub html: Option<String>,
    /// The decoded plain-text body, when the message has one.
    pub text: Option<String>,
    /// Candidate post URLs, document order, deduplicated.
    pub urls: Vec<String>,
}

/// A stable 64-bit FNV-1a hex digest of `bytes` (the synthetic message id of a message without a
/// `Message-ID`; wide enough that two distinct messages do not collide in practice).
fn digest(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Parses `.eml` bytes. `None` when the bytes are not a message (no headers at all).
#[must_use]
pub fn parse_eml(bytes: &[u8]) -> Option<MailPost> {
    let msg = MessageParser::new().parse(bytes)?;
    // A non-message (no header parsed) still yields a Message from mail-parser; require at least
    // one of the headers a newsletter always carries.
    if msg.from().is_none() && msg.subject().is_none() && msg.message_id().is_none() {
        return None;
    }
    let (from_name, from_addr) = msg
        .from()
        .and_then(|a| a.first())
        .map(|a| {
            (
                a.name().unwrap_or("").trim().to_owned(),
                a.address().unwrap_or("").trim().to_lowercase(),
            )
        })
        .unwrap_or_default();
    let list_id = {
        let v = msg.list_id();
        v.as_text()
            .map(str::to_owned)
            .or_else(|| {
                v.as_address()
                    .and_then(|a| a.first())
                    .and_then(|a| a.address().map(str::to_owned))
            })
            .map(|s| normalize_list_id(&s))
            .filter(|s| !s.is_empty())
    };
    let message_id = msg
        .message_id()
        .map(|m| m.trim().trim_matches(['<', '>']).to_owned())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("sha:{}", digest(bytes)));
    let html = msg.body_html(0).map(|c| c.into_owned());
    let text = msg.body_text(0).map(|c| c.into_owned());
    let urls = html.as_deref().map(extract_post_urls).unwrap_or_default();
    Some(MailPost {
        message_id,
        from_addr,
        from_name,
        list_id,
        subject: msg.subject().unwrap_or("").trim().to_owned(),
        date: msg.date().map(mail_parser::DateTime::to_rfc3339),
        html,
        text,
        urls,
    })
}

/// `"Name" <id.example>` / `<id.example>` / `id.example` → `id.example` (lowercase).
fn normalize_list_id(s: &str) -> String {
    let inner = match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => &s[a + 1..b],
        _ => s,
    };
    inner.trim().to_lowercase()
}

/// Whether a row's matchers (`from` entries, `list_id`) match `post`: a `from` entry is an exact
/// address, or an `@domain` suffix; both case-insensitive. Empty matchers never match.
#[must_use]
pub fn row_matches(post: &MailPost, from: &[String], list_id: Option<&str>) -> bool {
    let addr = post.from_addr.to_lowercase();
    let from_hit = !addr.is_empty()
        && from.iter().any(|f| {
            let f = f.trim().to_lowercase();
            if f.is_empty() {
                false
            } else if f.starts_with('@') {
                addr.ends_with(&f)
            } else {
                addr == f
            }
        });
    let list_hit = match (list_id.map(normalize_list_id), post.list_id.as_deref()) {
        (Some(want), Some(got)) => !want.is_empty() && want == got,
        _ => false,
    };
    from_hit || list_hit
}

/// The first configured row matching `post`. `matchers` projects a row to its `(from, list_id)`
/// (`cv-ingest` does not depend on the config crate, so the row type is the caller's).
#[must_use]
pub fn match_newsletter<'a, T>(
    post: &MailPost,
    rows: &'a [T],
    matchers: impl Fn(&'a T) -> (&'a [String], Option<&'a str>),
) -> Option<&'a T> {
    rows.iter().find(|r| {
        let (from, list_id) = matchers(r);
        row_matches(post, from, list_id)
    })
}

/// Query keys whose value is a literal target URL.
const REDIRECT_KEYS: [&str; 3] = ["url", "u", "redirect"];

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    char::from(c)
        .to_digit(16)
        .and_then(|d| u8::try_from(d).ok())
}

/// The literal target of a redirect wrapper (`?url=`/`?u=`/`?redirect=` holding an http(s) URL),
/// else `href` itself.
fn unwrap_redirect(href: &str) -> String {
    let Some((_, query)) = href.split_once('?') else {
        return href.to_owned();
    };
    let query = query.split('#').next().unwrap_or(query);
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && REDIRECT_KEYS.contains(&k.to_ascii_lowercase().as_str())
        {
            let target = percent_decode(v);
            let lower = target.to_ascii_lowercase();
            if lower.starts_with("https://") || lower.starts_with("http://") {
                return target;
            }
        }
    }
    href.to_owned()
}

/// `(href, anchor text)` of every `<a>` in `html`, document order.
fn anchors(html: &str) -> Vec<(String, String)> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("<a") {
        let start = from + rel;
        let after = start + 2;
        let Some(tag_end) = lower[after..].find('>').map(|e| after + e) else {
            break;
        };
        // `<a` must be the tag itself (`<abbr>` is not an anchor).
        let boundary = lower.as_bytes().get(after).copied();
        if boundary.is_some_and(|c| c.is_ascii_whitespace()) {
            let attrs = &html[after..tag_end];
            let close = lower[tag_end..]
                .find("</a")
                .map_or(html.len(), |c| tag_end + c);
            if let Some(href) = attr(attrs, "href") {
                let text = crate::html::html_to_text(&html[tag_end + 1..close]);
                out.push((href, text));
            }
            from = close.max(tag_end);
        } else {
            from = tag_end;
        }
    }
    out
}

/// The value of attribute `name` in a tag's attribute text.
fn attr(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find(name) {
        let at = from + rel;
        from = at + name.len();
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = attrs[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let rest = rest[1..].trim_start();
        let (val, _) = match rest.chars().next()? {
            q @ ('"' | '\'') => {
                let inner = &rest[1..];
                let end = inner.find(q)?;
                (&inner[..end], ())
            }
            _ => {
                let end = rest
                    .find(|c: char| c.is_ascii_whitespace())
                    .unwrap_or(rest.len());
                (&rest[..end], ())
            }
        };
        return Some(val.replace("&amp;", "&").trim().to_owned());
    }
    None
}

fn is_view_in_browser(text: &str) -> bool {
    let t = text.to_lowercase();
    [
        "view in browser",
        "view online",
        "view this email",
        "read online",
        "view in your browser",
    ]
    .iter()
    .any(|p| t.contains(p))
}

/// Whether `url` is a Substack-style post URL: `https://<host>/p/<slug>` (a host serving
/// `/p/<slug>` with nothing after the slug) or `open.substack.com/pub/<pub>/p/<slug>`.
fn is_post_url(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut parts = rest.split('/');
    let host = parts.next().unwrap_or("").to_ascii_lowercase();
    let segs: Vec<&str> = parts.filter(|s| !s.is_empty()).collect();
    if host == "open.substack.com" {
        return matches!(segs.as_slice(), ["pub", _, "p", slug] if !slug.is_empty());
    }
    matches!(segs.as_slice(), ["p", slug] if !slug.is_empty()) && !host.is_empty()
}

/// Candidate post URLs of an HTML body (see the module docs), document order, deduplicated.
#[must_use]
pub fn extract_post_urls(html: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (href, text) in anchors(html) {
        let url = unwrap_redirect(&href);
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("https://") || lower.starts_with("http://")) {
            continue;
        }
        if (is_post_url(&url) || is_view_in_browser(&text)) && !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
#[path = "mail_tests.rs"]
mod tests;
