//! Markdown link/image scanning, chrome-line classification and the per-line link rewrite.

use super::links::{Target, is_pixel, is_profile_url, resolve};
use super::{Dims, SMALL_PX};

/// One `[inner](dest)` (or `![alt](src)`) located in a line; byte offsets.
pub(super) struct Span {
    /// Start of the item (the `!` of an image).
    pub(super) start: usize,
    /// One past the closing `)`.
    pub(super) end: usize,
    pub(super) image: bool,
    pub(super) inner: (usize, usize),
    pub(super) dest: (usize, usize),
}

/// The link/image whose `[` (or `![`) sits at byte `at` of `s`.
pub(super) fn scan(s: &str, at: usize) -> Option<Span> {
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
pub(super) fn only_items(line: &str) -> Option<Vec<Span>> {
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
pub(super) fn strip_images(s: &str) -> String {
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
pub(super) fn norm_label(s: &str) -> String {
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
pub(super) fn resize_hints(src: &str) -> Vec<u32> {
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
pub(super) fn is_icon(src: &str) -> bool {
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
pub(super) fn is_small_image(dest: &str, dims: Dims<'_>) -> bool {
    let url = dest.split_whitespace().next().unwrap_or("");
    is_pixel(url) || is_icon(url) || dims_small(url, dims)
}

/// Whether the `dims` hook reports the image at 64x64 or smaller (the one threshold shared by
/// [`rewrite_line`] and [`is_chrome_line`], so a localized icon reads the same in both).
pub(super) fn dims_small(src: &str, dims: Dims<'_>) -> bool {
    let url = src.split_whitespace().next().unwrap_or("");
    dims(url).is_some_and(|(w, h)| w <= SMALL_PX && h <= SMALL_PX)
}

/// Whether a whole line is newsletter chrome (button row, CTA, or a "forwarded" prompt).
pub(super) fn is_chrome_line(line: &str, dims: Dims<'_>) -> bool {
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
pub(super) fn inner_images(s: &str) -> Vec<String> {
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

/// Rewrites the links of one line (pixels, icons, avatars and `dims`-small images removed,
/// redirects unwrapped, tracking params dropped).
pub(super) fn rewrite_line(line: &str, dims: Dims<'_>) -> String {
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
