//! Link targets: redirect unwrapping, the tracking-param strip and tracking-pixel detection. URL
//! parts, the Substack host test and the tracking-param policy come from `cv_text::url_canon`.

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use cv_text::url_canon::{http_parts, is_link_tracking_param, is_substack_host};

use crate::mail::{decode_query_value, unwrap_redirect};

// ---- Tracking pixels ----

/// Whether an image URL is an open/pixel tracker (known tracker hosts and paths, or 1x1 hints).
pub(super) fn is_pixel(src: &str) -> bool {
    let url = src.split_whitespace().next().unwrap_or("");
    let Some(p) = http_parts(url) else {
        return false;
    };
    let (host, path) = (p.host.as_str(), p.path);
    let q = p.query.to_ascii_lowercase();
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

/// Query params removed per `url_canon::is_link_tracking_param` (`utm_*` everywhere, the
/// share/session ones on Substack hosts); the kept params keep their order, the fragment stays.
fn strip_tracking(url: &str) -> String {
    let Some(p) = http_parts(url) else {
        return url.to_owned();
    };
    let (base, frag) = url
        .split_once('#')
        .map_or((url, None), |(b, f)| (b, Some(f)));
    let Some((head, query)) = base.split_once('?') else {
        return url.to_owned();
    };
    let sub = is_substack_host(&p.host);
    let kept: Vec<&str> = query
        .split('&')
        .filter(|p| !is_link_tracking_param(p.split('=').next().unwrap_or(""), sub))
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

/// base64url with optional padding and non-canonical trailing bits tolerated.
const B64URL_LENIENT: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Decodes a Substack redirect payload over the `base64` crate: base64url or standard alphabet
/// (`+`/`/` mapped onto `-`/`_`), padded or not, read up to the first `=`; a lone trailing
/// sextet (which carries no full byte) is ignored. `None` on a bad character.
pub(super) fn decode_redirect_payload(s: &str) -> Option<Vec<u8>> {
    let s = s
        .split('=')
        .next()
        .unwrap_or("")
        .replace('+', "-")
        .replace('/', "_");
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    let usable = s.len() - usize::from(s.len() % 4 == 1);
    B64URL_LENIENT.decode(&s[..usable]).ok()
}

/// The `"e"` string of a redirect payload's JSON object, over `serde_json`. A value holding a
/// newline, tab or carriage return is rejected (as the hand-rolled reader it replaces did).
pub(super) fn redirect_payload_target(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let e = v.get("e")?.as_str()?;
    (!e.contains(['\n', '\t', '\r'])).then(|| e.to_owned())
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
pub(super) fn is_profile_url(url: &str) -> bool {
    http_parts(url).is_some_and(|p| {
        is_substack_host(&p.host) && (p.path.starts_with("/@") || p.path.starts_with("/profile/"))
    })
}

pub(crate) fn resolve(url: &str) -> Target {
    let Some(p) = http_parts(url) else {
        return Target::Keep(url.to_owned());
    };
    let (host, path, query) = (p.host.as_str(), p.path, p.query);
    if host == "substack.com" || host == "www.substack.com" {
        if let Some(rest) = path.strip_prefix("/redirect/") {
            let payload = rest.rsplit('/').next().unwrap_or("");
            let payload = payload.split('.').next().unwrap_or("");
            let target = decode_redirect_payload(payload)
                .and_then(|b| String::from_utf8(b).ok())
                .and_then(|j| redirect_payload_target(&j))
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
                .map(|(_, v)| decode_query_value(v))
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
