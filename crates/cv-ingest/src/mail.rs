//! `.eml` parsing for the Maildir newsletter intake (plan `auto-intake-sprint` WP-4).
//!
//! [`parse_eml`] turns raw message bytes into a [`MailPost`] (over `mail-parser`: MIME multipart,
//! quoted-printable/base64 and non-UTF-8 charsets are decoded there). [`match_newsletter`] picks
//! the configured `[[newsletter]]` row for a message by `From` (exact address or `@domain`
//! suffix) or `List-Id`. Candidate post URLs (`MailPost.urls`) are read from the HTML: every href
//! goes through the mail-clean link resolver (Substack `/redirect/2/<b64>` decoded, `?url=`/`?u=`/
//! `?redirect=` wrappers unwrapped, tracking params stripped; an untrustworthy target is rejected),
//! and only post shapes survive, normalized without query or fragment: `<host>/p/<slug>`, and
//! `open.substack.com/pub/<pub>/p/<slug>` mapped to `https://<pub>.substack.com/p/<slug>`. The
//! message's own post (its title or "view in browser" link) comes first, then body order.
//! Nothing is ever fetched. [`canonical_post_url`] (plan intake-round-2-clean-dedupe-reading WP-2)
//! picks the identity: the first candidate with publication evidence (the mail's Substack host or
//! the matched blog row's host), rebased onto the blog row's host, and passed through the
//! [`is_clean_identity`] guard (no `token=`/`r=`, JWT segment or tracker host). An unproven
//! third-party link is never the identity; with none, the Message-ID is the identity.
//!
//! [`clean_body`] (plan `intake-followups-mail-youtube` WP-1) strips newsletter chrome from the
//! converted markdown.
//!
//! Pure: no I/O, no async.

#[cfg(test)]
use cv_text::url_canon::http_host as url_host;
use cv_text::url_canon::{bare_host_name, http_host, http_parts};
use mail_parser::MessageParser;

#[path = "mail_clean/mod.rs"]
mod clean;
pub use clean::{clean_body, clean_body_with, strip_html_chrome};

#[path = "mail_ready.rs"]
mod ready;
pub use ready::is_complete;

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
    let subject = msg.subject().unwrap_or("").trim().to_owned();
    let urls = html
        .as_deref()
        .map(|h| extract_post_urls_for(h, &subject))
        .unwrap_or_default();
    Some(MailPost {
        message_id,
        from_addr,
        from_name,
        list_id,
        subject,
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

/// The `<pub>.substack.com` host a message comes from (plan intake-followups-mail-youtube WP-2):
/// the `List-Id` when it is exactly `<pub>.substack.com` (one non-empty label), else
/// `<local>.substack.com` for a `<local>@substack.com` sender, else `None`.
#[must_use]
pub fn substack_host(post: &MailPost) -> Option<String> {
    const SUFFIX: &str = ".substack.com";
    let single_label = |h: &str| {
        h.strip_suffix(SUFFIX)
            .is_some_and(|l| !l.is_empty() && !l.contains('.'))
    };
    if let Some(l) = post.list_id.as_deref() {
        let l = l.trim().to_ascii_lowercase();
        if single_label(&l) {
            return Some(l);
        }
    }
    let (local, domain) = post.from_addr.trim().rsplit_once('@')?;
    if domain.eq_ignore_ascii_case("substack.com") && !local.is_empty() {
        let h = format!("{}{SUFFIX}", local.to_ascii_lowercase());
        return single_label(&h).then_some(h);
    }
    None
}

/// Query keys whose value is a literal target URL.
const REDIRECT_KEYS: [&str; 3] = ["url", "u", "redirect"];

/// A query-string value decoded: `cv_text::vpath::percent_decode` plus the form-encoding rule
/// that a literal `+` is a space (an encoded `%2B` stays `+`).
pub(crate) fn decode_query_value(s: &str) -> String {
    cv_text::vpath::percent_decode(&s.replace('+', " "))
}

/// The literal target of a redirect wrapper (`?url=`/`?u=`/`?redirect=` holding an http(s) URL),
/// else `href` itself.
pub(crate) fn unwrap_redirect(href: &str) -> String {
    let Some((_, query)) = href.split_once('?') else {
        return href.to_owned();
    };
    let query = query.split('#').next().unwrap_or(query);
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=')
            && REDIRECT_KEYS.contains(&k.to_ascii_lowercase().as_str())
        {
            let target = decode_query_value(v);
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
            if let Some(href) = href_attr(attrs) {
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

/// The `href` of an anchor's attribute text (`crate::html::attr`, then `&amp;` decoded and
/// trimmed).
fn href_attr(attrs: &str) -> Option<String> {
    crate::html::attr(attrs, "href", &[]).map(|v| v.replace("&amp;", "&").trim().to_owned())
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

/// `(host, slug)` of a post URL in any accepted shape, query and fragment ignored:
/// `https://<host>/p/<slug>`, `open.substack.com/pub/<pub>/p/<slug>` (host `<pub>.substack.com`)
/// and `substack.com/@<handle>/p/<slug>` (host `substack.com`: the handle does not name the
/// publication's subdomain, so this shape is kept but never proven). The host is the raw
/// lowercased host (`www.` kept, audit E2): a rebuilt URL keeps it; only comparisons strip it.
fn post_parts(url: &str) -> Option<(String, String)> {
    let parts = http_parts(url)?;
    let host = parts.host;
    if host.is_empty() {
        return None;
    }
    let segs: Vec<&str> = parts.path.split('/').filter(|s| !s.is_empty()).collect();
    let ok = |s: &str| !s.is_empty();
    match segs.as_slice() {
        ["pub", publ, "p", slug] if host == "open.substack.com" && ok(slug) => {
            let publ = publ.to_ascii_lowercase();
            (!publ.is_empty() && !publ.contains('.'))
                .then(|| (format!("{publ}.substack.com"), (*slug).to_owned()))
        }
        [h, "p", slug] if host == "substack.com" && h.starts_with('@') && ok(slug) => {
            Some((host, format!("{h}/p/{slug}")))
        }
        ["p", slug] if host != "open.substack.com" && ok(slug) => Some((host, (*slug).to_owned())),
        _ => None,
    }
}

/// The one clean form of a post URL: `https://<host>/p/<slug>` (no query, no fragment).
fn normalize_post_url(url: &str) -> Option<String> {
    let (host, slug) = post_parts(url)?;
    Some(if slug.starts_with('@') {
        format!("https://{host}/{slug}")
    } else {
        format!("https://{host}/p/{slug}")
    })
}

/// Candidate post URLs of an HTML body (see the module docs), the message's own post first,
/// deduplicated. Same as [`extract_post_urls_for`] with no subject.
#[must_use]
pub fn extract_post_urls(html: &str) -> Vec<String> {
    extract_post_urls_for(html, "")
}

/// Candidate post URLs of an HTML body: every href resolved through the mail-clean link resolver
/// (an untrustworthy target is rejected) and normalized to `https://<host>/p/<slug>`. The first
/// title link (anchor text equal to `subject`) or "view in browser" link that is a post comes
/// first, the rest follow in body order; deduplicated.
#[must_use]
pub fn extract_post_urls_for(html: &str, subject: &str) -> Vec<String> {
    let subject = subject.trim().to_lowercase();
    let mut own: Option<String> = None;
    let mut out: Vec<String> = Vec::new();
    for (href, text) in anchors(html) {
        let Some(url) = clean::resolve_link(&unwrap_redirect(&href)) else {
            continue;
        };
        let Some(url) = normalize_post_url(&url) else {
            continue;
        };
        let t = text.trim().to_lowercase();
        if own.is_none() && (is_view_in_browser(&t) || (!subject.is_empty() && t == subject)) {
            own = Some(url.clone());
        }
        if !out.contains(&url) {
            out.push(url);
        }
    }
    if let Some(o) = own
        && let Some(i) = out.iter().position(|u| *u == o)
    {
        let u = out.remove(i);
        out.insert(0, u);
    }
    out
}

/// The canonical post identity of a message (plan intake-round-2-clean-dedupe-reading WP-2): the
/// first candidate with **publication evidence** (its host is the mail's `substack_host`, which
/// the mapped `open.substack.com/pub/<pub>` form also meets, or the blog row's host), rebased onto
/// `https://<blog host>/p/<slug>` when a blog row is known. An unproven candidate is never the
/// identity. The result passes [`is_clean_identity`]; `None` when no candidate survives.
#[must_use]
pub fn canonical_post_url(post: &MailPost, blog_link: Option<&str>) -> Option<String> {
    canonical_post_url_in(post, blog_link, &[])
}

/// [`canonical_post_url`] where a candidate on any of `pub_hosts` (the resolved publication's
/// feed and `hosts` hosts, `www.` ignored) is also proven (plan sources-publication-model WP-3).
/// The rebase target stays the `blog_link` host.
#[must_use]
pub fn canonical_post_url_in(
    post: &MailPost,
    blog_link: Option<&str>,
    pub_hosts: &[String],
) -> Option<String> {
    let pub_bare: Vec<String> = pub_hosts.iter().filter_map(|h| bare_host_name(h)).collect();
    let sub = substack_host(post).and_then(|h| bare_host_name(&h));
    let blog_host = blog_link.and_then(http_host);
    let blog_bare = blog_host.as_deref().and_then(bare_host_name);
    post.urls.iter().find_map(|c| {
        let (host, slug) = post_parts(c)?;
        if slug.starts_with('@') {
            return None;
        }
        let h = bare_host_name(&host);
        let proven = h
            .as_ref()
            .is_some_and(|hh| sub == h || blog_bare == h || pub_bare.contains(hh));
        if !proven {
            return None;
        }
        let url = match &blog_host {
            Some(b) => format!("https://{b}/p/{slug}"),
            None => format!("https://{host}/p/{slug}"),
        };
        is_clean_identity(&url).then_some(url)
    })
}

/// Defense in depth for an identity URL: rejects a query or fragment carrying `token=`/`r=`, a
/// JWT-shaped path segment (three base64url segments joined by `.`), and tracker hosts and paths
/// (`/redirect/`, beehiiv `/ss/c/`, `/v2/c/`, `/view/`, `/app-link/`, `link.mail.beehiiv.com`,
/// hubspot and list-manage links).
#[must_use]
pub fn is_clean_identity(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some(parts) = http_parts(&lower).filter(|p| !p.host.is_empty()) else {
        return false;
    };
    let (host, path) = (parts.host.as_str(), parts.path);
    let query_bad = [parts.query, parts.fragment.unwrap_or("")]
        .iter()
        .flat_map(|q| q.split(['&', '?', '#', ';']))
        .any(|p| p.starts_with("token=") || p.starts_with("r="));
    let tracker_host = host == "link.mail.beehiiv.com"
        || host.contains("hubspot")
        || host.ends_with("list-manage.com")
        || host.starts_with("click.")
        || host.starts_with("links.");
    let tracker_path = ["/redirect/", "/ss/c/", "/v2/c/", "/app-link/"]
        .iter()
        .any(|p| path.contains(p))
        || path.starts_with("/view/");
    let jwt = path.split('/').any(|seg| {
        let parts: Vec<&str> = seg.split('.').collect();
        parts.len() == 3
            && parts.iter().all(|p| {
                p.len() >= 8
                    && p.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
    });
    !(lower.contains("token=") || query_bad || tracker_host || tracker_path || jwt)
}

#[cfg(test)]
#[path = "mail_tests.rs"]
mod tests;
