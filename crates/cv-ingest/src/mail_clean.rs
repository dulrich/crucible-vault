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
//! Round 2 (plan `intake-round-2-clean-dedupe-reading` WP-1, measured on 32 newsletter messages)
//! also removes the residue round 1 left: [`strip_html_chrome`] is an HTML-stage pre-pass that cuts
//! `<head>`/`<style>`/`<script>` elements (the CSS that leaked into the body), preheader spacer
//! elements (U+034F, soft hyphen, zero-width and NBSP runs of at least 4) and every `<img>` that is
//! a tracking pixel, an icon or an avatar, with its wrapping `<a>` when the anchor held only that
//! image. A pixel/icon/avatar is: a known tracker or icon URL (`substack.com/icon/…`, a filename
//! containing `icon`), an image whose every known dimension is **at most 64 px** (`width`/`height`
//! attributes, inline `style` `width`/`height`, or the `w_N`/`h_N` resize hints of an image-CDN
//! URL; this includes 1x1), or any image inside a link to a profile (`substack.com/@…`,
//! `/profile/…`, also behind a redirect wrapper). Avatars and icons are removed, never resized.
//! Publication logos (for example 183x40) and post-body images are kept.
//!
//! At the markdown stage [`clean_body`] additionally drops preheader spacer-only lines, the
//! referral block ("Invite your friends and earn rewards" heading, its paragraph and its
//! `Invite Friends`/`Subscribed` link lines), a CSS-shaped line (`@media`/`@font-face`/rule runs,
//! the backstop for style that leaked anyway), and the same pixel/icon/avatar images (by URL
//! shape, with the 64 px threshold above). [`clean_body_with`] takes a caller-supplied
//! `Fn(&str) -> Option<(u32, u32)>` giving the decoded size of an already-localized image, and
//! drops a markdown image (and its wrapping link) reported at 64x64 or smaller; the hook keeps
//! this crate free of I/O.
//!
//! The paywall-marker verdict must be taken on the **pre-cleanup** markdown by the caller (removing
//! an "Upgrade to paid" line must never turn a partial message complete). `strip_html_chrome` only
//! removes styling, spacers and small images, none of which can carry a paywall marker, so the
//! caller may take the verdict on the markdown of the stripped HTML.
//!
//! Pure: no I/O, no async.

use super::{percent_decode, unwrap_redirect};

/// Looks up the decoded `(width, height)` of an already-localized image by its markdown target.
pub type Dims<'a> = &'a dyn Fn(&str) -> Option<(u32, u32)>;

/// Images at most this many pixels in every known dimension are pixels, icons or avatars.
const SMALL_PX: u32 = 64;

/// Cleans newsletter chrome out of `md` (see the module doc). Idempotent.
#[must_use]
pub fn clean_body(md: &str) -> String {
    clean_body_with(md, &|_| None)
}

/// [`clean_body`] plus the image-dimension hook: a markdown image (and its wrapping link) whose
/// target `dims` reports at 64x64 or smaller is dropped. Idempotent.
#[must_use]
pub fn clean_body_with(md: &str, dims: Dims<'_>) -> String {
    let lines: Vec<&str> = md.lines().collect();
    let fenced = fence_mask(&lines);
    let end = footer_start(&lines, &fenced).unwrap_or(lines.len());
    let referral = referral_mask(&lines[..end], &fenced[..end]);
    let mut out: Vec<String> = Vec::with_capacity(end);
    for (i, line) in lines[..end].iter().enumerate() {
        if fenced[i] {
            out.push((*line).to_owned());
        } else if referral[i]
            || is_spacer_line(line)
            || is_css_line(line)
            || is_chrome_line(line, dims)
        {
            out.push(String::new());
        } else {
            out.push(rewrite_line(line, dims));
        }
    }
    collapse_blanks(&out, &fenced[..end])
}

/// Characters newsletters use as invisible preheader padding.
fn is_spacer_char(c: char) -> bool {
    matches!(
        c,
        '\u{034f}' | '\u{00ad}' | '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{00a0}'
    )
}

/// A non-blank line made only of whitespace and spacer characters.
fn is_spacer_line(line: &str) -> bool {
    !line.trim().is_empty() && line.chars().all(|c| c.is_whitespace() || is_spacer_char(c))
}

/// A long line that is structurally CSS: at least 200 bytes, no backtick or markdown link, and
/// either an at-rule (`@media`/`@font-face`/`@import`) with a rule body, or at least 3
/// `selector { prop: val; }` groups. Prose quoting inline code is never matched.
fn is_css_line(line: &str) -> bool {
    let t = line.trim();
    if t.len() < 200 || t.contains('`') || t.contains("](") {
        return false;
    }
    let lower = t.to_ascii_lowercase();
    let at_rule = ["@media", "@font-face", "@import"]
        .iter()
        .any(|a| lower.contains(a));
    let groups = t
        .split('}')
        .filter(|g| {
            g.split_once('{')
                .is_some_and(|(_, b)| (b.contains(':') && b.contains(';')) || has_declaration(b))
        })
        .count();
    t.contains('{') && ((at_rule && groups >= 1) || groups >= 3)
}

/// Whether a block body holds a `prop: value` declaration (a `:` directly after a word
/// character, followed by a value), so minified CSS without a `;` still counts.
fn has_declaration(body: &str) -> bool {
    let b = body.as_bytes();
    b.iter().enumerate().any(|(i, &c)| {
        c == b':'
            && i > 0
            && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'-')
            && body[i + 1..]
                .trim_start()
                .chars()
                .next()
                .is_some_and(|n| !matches!(n, '}' | '{'))
    })
}

/// The lines of the referral block: an "Invite your friends" heading, then the paragraph and
/// link-only lines that follow it.
fn referral_mask(lines: &[&str], fenced: &[bool]) -> Vec<bool> {
    let mut mask = vec![false; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let t = lines[i].trim_start();
        let heading = t.starts_with('#') || t.starts_with("**");
        if fenced[i]
            || !heading
            || !norm_label(t.trim_start_matches('#')).starts_with("invite your friends")
        {
            i += 1;
            continue;
        }
        mask[i] = true;
        let mut j = i + 1;
        loop {
            while j < lines.len() && lines[j].trim().is_empty() {
                j += 1;
            }
            let start = j;
            while j < lines.len() && !lines[j].trim().is_empty() && !fenced[j] {
                j += 1;
            }
            if start == j || !referral_block(&lines[start..j]) {
                break;
            }
            mask[start..j].iter_mut().for_each(|m| *m = true);
        }
        i = j.max(i + 1);
    }
    mask
}

/// Whether a paragraph belongs to the referral block: the "earn rewards" blurb, or link-only
/// lines labelled `Invite Friends`/`Subscribed`.
fn referral_block(block: &[&str]) -> bool {
    let flat = norm_label(&block.join(" "));
    if flat.contains("earn rewards") || flat.contains("share it with your friends") {
        return true;
    }
    block.iter().all(|l| {
        only_items(l).is_some_and(|items| {
            items.iter().all(|sp| {
                !sp.image
                    && matches!(
                        norm_label(&l[sp.inner.0..sp.inner.1]).as_str(),
                        "invite friends" | "subscribed" | "invite" | "refer a friend"
                    )
            })
        })
    })
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

/// The `w_N`/`h_N` resize hints of an image-CDN URL (only the part before the embedded origin).
fn resize_hints(src: &str) -> Vec<u32> {
    let url = src.split_whitespace().next().unwrap_or("");
    let lower = url.to_ascii_lowercase();
    let cut = ["https%3a", "http%3a", "/https:", "/http:"]
        .iter()
        .filter_map(|m| lower.find(m))
        .min()
        .unwrap_or(lower.len());
    lower[..cut]
        .split([',', '/'])
        .filter_map(|tok| {
            let n = tok.strip_prefix("w_").or_else(|| tok.strip_prefix("h_"))?;
            n.parse::<u32>().ok()
        })
        .collect()
}

/// Icons and avatars by URL shape: Substack's button icons (`…substack.com%2Ficon%2F…`), any
/// `/icon/` path or `…icon….<ext>` filename, or an image-CDN resize hint of at most 64 px (`w_36`,
/// `w_40`, `w_48`, `h_…`).
fn is_icon(src: &str) -> bool {
    let url = src.split_whitespace().next().unwrap_or("");
    let l = url.to_ascii_lowercase();
    if l.contains("substack.com%2ficon%2f")
        || l.contains("substack.com/icon/")
        || l.contains("%2ficon%2f")
    {
        return true;
    }
    let path = l.split(['?', '#']).next().unwrap_or("");
    if path.contains("/icon/") || path.rsplit('/').next().is_some_and(|f| f.contains("icon")) {
        return true;
    }
    let hints = resize_hints(url);
    !hints.is_empty() && hints.iter().all(|n| *n <= SMALL_PX)
}

/// A markdown image to drop: a pixel, icon or avatar by URL shape, or reported small by `dims`.
fn is_small_image(dest: &str, dims: Dims<'_>) -> bool {
    let url = dest.split_whitespace().next().unwrap_or("");
    is_pixel(url) || is_icon(url) || dims_small(url, dims)
}

/// Whether the `dims` hook reports the image at 64x64 or smaller (the one threshold shared by
/// [`rewrite_line`] and [`is_chrome_line`], so a localized icon reads the same in both).
fn dims_small(src: &str, dims: Dims<'_>) -> bool {
    let url = src.split_whitespace().next().unwrap_or("");
    dims(url).is_some_and(|(w, h)| w <= SMALL_PX && h <= SMALL_PX)
}

/// Whether a whole line is newsletter chrome (button row, CTA, or a "forwarded" prompt).
fn is_chrome_line(line: &str, dims: Dims<'_>) -> bool {
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
            if is_small_image(&line[sp.dest.0..sp.dest.1], dims) {
                continue;
            }
            return false;
        }
        let inner = &line[sp.inner.0..sp.inner.1];
        let label = norm_label(inner);
        let is_icon_like = |s: &str| is_icon(s) || dims_small(s, dims);
        let has_icon = inner_images(inner).iter().any(|s| is_icon_like(s));
        let has_content_img = inner_images(inner)
            .iter()
            .any(|s| !is_icon_like(s) && !is_pixel(s));
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
pub(crate) enum Target {
    Keep(String),
    /// The target cannot be trusted or derived: keep the anchor text only.
    Drop,
}

/// The resolver seam (plan `intake-round-2-clean-dedupe-reading` WP-1, audit M3): `Some(url)` is
/// the trusted target of `url` with tracking params stripped (a redirect wrapper unwrapped), `None`
/// means the target cannot be trusted or derived and the link is dropped. WP-2 calls this from
/// `crate::mail` to derive a post URL.
pub(crate) fn resolve_link(url: &str) -> Option<String> {
    match resolve(url) {
        Target::Keep(k) => Some(k),
        Target::Drop => None,
    }
}

/// Whether `url` is a profile page (`substack.com/@handle`, `/profile/…`).
fn is_profile_url(url: &str) -> bool {
    split_url(url).is_some_and(|(host, path, _)| {
        is_substack_host(&host) && (path.starts_with("/@") || path.starts_with("/profile/"))
    })
}

pub(crate) fn resolve(url: &str) -> Target {
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

/// Rewrites the links of one line (pixels, icons, avatars and `dims`-small images removed,
/// redirects unwrapped, tracking params dropped).
fn rewrite_line(line: &str, dims: Dims<'_>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        let c = line[i..].chars().next().unwrap_or(' ');
        if (c == '[' || c == '!')
            && let Some(sp) = scan(line, i)
        {
            let dest = &line[sp.dest.0..sp.dest.1];
            if sp.image {
                if !is_small_image(dest, dims) {
                    out.push_str(&line[sp.start..sp.end]);
                }
            } else {
                let inner_src = &line[sp.inner.0..sp.inner.1];
                let mut inner = rewrite_line(inner_src, dims);
                let (url, title) = dest
                    .split_once(char::is_whitespace)
                    .map_or((dest, ""), |(u, t)| (u, t));
                match resolve(url) {
                    Target::Keep(k) => {
                        if is_profile_url(&k) {
                            inner = strip_images(&inner);
                        }
                        // A link that held only dropped images leaves nothing behind.
                        if !(inner.trim().is_empty() && !inner_src.trim().is_empty()) {
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

// ---- HTML-stage pre-pass ----

/// Strips newsletter chrome from message HTML before conversion (see the module doc): `<head>`,
/// `<style>` and `<script>` elements, preheader spacer elements, and pixel/icon/avatar `<img>`s
/// (with their wrapping `<a>` when it held only that image). Idempotent; never touches text.
#[must_use]
pub fn strip_html_chrome(html: &str) -> String {
    let mut s = html.to_owned();
    for tag in ["head", "style", "script"] {
        s = remove_elements(&s, tag);
    }
    for tag in ["span", "p", "div"] {
        s = remove_spacer_elements(&s, tag);
    }
    strip_small_images(&s, false)
}

/// Whether the byte after `<name` ends the tag name (so `<head` does not match `<header`).
fn tag_boundary(lower: &str, after: usize) -> bool {
    matches!(
        lower.as_bytes().get(after),
        Some(b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
    )
}

/// Byte offset of the next `<name` tag at or after `from` (name boundary respected).
fn find_open(lower: &str, name: &str, from: usize) -> Option<usize> {
    let open = format!("<{name}");
    let mut i = from;
    while let Some(rel) = lower.get(i..)?.find(&open) {
        let at = i + rel;
        if tag_boundary(lower, at + open.len()) {
            return Some(at);
        }
        i = at + open.len();
    }
    None
}

/// Offset one past the `>` of the `</name …>` closing tag at or after `from`.
fn find_close_end(lower: &str, name: &str, from: usize) -> Option<usize> {
    let close = format!("</{name}");
    let mut i = from;
    while let Some(rel) = lower.get(i..)?.find(&close) {
        let at = i + rel;
        if tag_boundary(lower, at + close.len()) {
            return lower[at..].find('>').map(|g| at + g + 1);
        }
        i = at + close.len();
    }
    None
}

/// Removes every `<name>…</name>` element (an unclosed one is left alone).
fn remove_elements(s: &str, name: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(at) = find_open(&lower, name, i) {
        let Some(end) = find_close_end(&lower, name, at) else {
            break;
        };
        out.push_str(&s[i..at]);
        i = end;
    }
    out.push_str(&s[i..]);
    out
}

/// Number of spacer units (spacer chars and spacer entities) in `inner`, or `None` when it holds
/// anything else than whitespace and spacers.
fn spacer_units(inner: &str) -> Option<usize> {
    let mut units = 0;
    let mut it = inner.char_indices();
    while let Some((i, c)) = it.next() {
        if c == '&' {
            let rest = &inner[i + 1..];
            let semi = rest.find(';').filter(|&e| e <= 8)?;
            let ent = rest[..semi].to_ascii_lowercase();
            let ok = matches!(
                ent.as_str(),
                "nbsp"
                    | "shy"
                    | "zwnj"
                    | "zwj"
                    | "#847"
                    | "#x34f"
                    | "#173"
                    | "#xad"
                    | "#160"
                    | "#xa0"
                    | "#8203"
                    | "#8204"
                    | "#8205"
                    | "#x200b"
                    | "#x200c"
                    | "#x200d"
                    | "#65279"
                    | "#xfeff"
            );
            if !ok {
                return None;
            }
            units += 1;
            for _ in 0..semi + 1 {
                it.next();
            }
        } else if is_spacer_char(c) {
            units += 1;
        } else if !c.is_whitespace() {
            return None;
        }
    }
    Some(units)
}

/// Removes `<name>` elements holding only a run (at least 4) of preheader spacers.
fn remove_spacer_elements(s: &str, name: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(at) = find_open(&lower, name, i) {
        let Some(gt) = lower[at..].find('>').map(|g| at + g + 1) else {
            break;
        };
        let inner_end = lower[gt..].find('<').map_or(lower.len(), |r| gt + r);
        let removable = spacer_units(&s[gt..inner_end]).is_some_and(|n| n >= 4)
            && lower[inner_end..].starts_with(&format!("</{name}"));
        if let (true, Some(end)) = (removable, find_close_end(&lower, name, inner_end)) {
            out.push_str(&s[i..at]);
            i = end;
        } else {
            out.push_str(&s[i..gt]);
            i = gt;
        }
    }
    out.push_str(&s[i..]);
    out
}

/// The value of attribute `name` in the open tag `tag` (quotes handled, entities left as is).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let pat = format!("{name}=");
    let mut from = 0;
    while let Some(rel) = lower[from..].find(&pat) {
        let at = from + rel;
        from = at + pat.len();
        let before = lower[..at].chars().next_back();
        if !before.is_some_and(|c| c.is_whitespace() || c == '"' || c == '\'') {
            continue;
        }
        let rest = &tag[from..];
        return Some(match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next().unwrap_or("").to_owned(),
            _ => rest
                .split(|c: char| c.is_whitespace() || c == '>' || c == '/')
                .next()
                .unwrap_or("")
                .to_owned(),
        });
    }
    None
}

/// A pixel size: digits with an optional `px` (a percentage or `auto` is unknown).
fn parse_px(v: &str) -> Option<u32> {
    let v = v.trim().to_ascii_lowercase();
    let v = v.trim_end_matches("!important").trim();
    v.strip_suffix("px").unwrap_or(v).trim().parse().ok()
}

/// The pixel `width`/`height` an `<img>` tag declares (attribute and inline style).
fn declared_dims(tag: &str) -> Vec<u32> {
    let style = attr(tag, "style").unwrap_or_default();
    let mut out = Vec::new();
    for prop in ["width", "height"] {
        if let Some(px) = attr(tag, prop).as_deref().and_then(parse_px) {
            out.push(px);
        }
        for decl in style.split(';') {
            if let Some((k, v)) = decl.split_once(':')
                && k.trim().eq_ignore_ascii_case(prop)
                && let Some(px) = parse_px(v)
            {
                out.push(px);
            }
        }
    }
    out
}

/// Whether an `<img>` open tag is a pixel, icon or avatar.
fn is_chrome_img(tag: &str) -> bool {
    let src = attr(tag, "src").unwrap_or_default().replace("&amp;", "&");
    if is_pixel(&src) || is_icon(&src) {
        return true;
    }
    let dims = declared_dims(tag);
    !dims.is_empty() && dims.iter().all(|d| *d <= SMALL_PX)
}

/// `s` without its tags (rough: for the "anchor held nothing" test).
fn text_of(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Drops chrome `<img>`s (all of them when `force`, inside a profile link); an anchor left with
/// no text and no image is dropped with them.
fn strip_small_images(s: &str, force: bool) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    loop {
        let img = find_open(&lower, "img", i);
        let a = find_open(&lower, "a", i);
        let (at, is_img) = match (img, a) {
            (Some(x), Some(y)) if y < x => (y, false),
            (Some(x), _) => (x, true),
            (None, Some(y)) => (y, false),
            (None, None) => break,
        };
        let Some(gt) = lower[at..].find('>').map(|g| at + g + 1) else {
            break;
        };
        out.push_str(&s[i..at]);
        let tag = &s[at..gt];
        if is_img {
            if !(force || is_chrome_img(tag)) {
                out.push_str(tag);
            }
            i = gt;
        } else if let Some(end) = find_close_end(&lower, "a", gt) {
            let close_at = lower[gt..end].rfind("</a").map_or(end, |r| gt + r);
            let inner = &s[gt..close_at];
            let href = attr(tag, "href").unwrap_or_default().replace("&amp;", "&");
            let profile = resolve_link(&href).is_some_and(|t| is_profile_url(&t));
            let kept = strip_small_images(inner, force || profile);
            if kept == inner || !text_of(&kept).trim().is_empty() || lower_has_img(&kept) {
                out.push_str(tag);
                out.push_str(&kept);
                out.push_str(&s[close_at..end]);
            }
            i = end;
        } else {
            out.push_str(tag);
            i = gt;
        }
    }
    out.push_str(&s[i..]);
    out
}

fn lower_has_img(s: &str) -> bool {
    find_open(&s.to_ascii_lowercase(), "img", 0).is_some()
}

#[cfg(test)]
#[path = "mail_clean_tests.rs"]
mod tests;
