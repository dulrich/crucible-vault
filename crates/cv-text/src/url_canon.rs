//! URL canonicalization, YouTube video-id extraction, X status parsing and blog post-id
//! derivation — the single home A2 reuses (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/AGENTS.md:36,79`:
//! one YT-id regex set; platform-aware post-id canonicalization, never a blind param strip).
//!
//! Sources: `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/urlCanonicalize.ts` (`canonicalizeUrl`,
//! `slugForCanonical`, `shortHash`), `utils/youtube.ts:22-35` (`URL_PATTERNS`,
//! `extractVideoIdFromUrl`), `utils/xPost.ts` (`extractXStatusFromUrl`, `canonicalXStatusUrl`),
//! `utils/blogs.ts:64-130,383-398` (`CANON_RULES`, `TRACKING_PARAM_RE`, `postIdFromUrl`).
//! Nothing Obsidian-induced to drop: all were pure functions over the WHATWG URL type, which the
//! `url` crate implements.

use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;
use url::Url;
use url::form_urlencoded;

fn re(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

fn captures1(re: &Option<Regex>, hay: &str) -> Option<String> {
    re.as_ref()?
        .captures(hay)?
        .get(1)
        .map(|m| m.as_str().to_owned())
}

// ---------------------------------------------------------------- YouTube

static YT_URL_PATTERNS: LazyLock<Vec<Option<Regex>>> = LazyLock::new(|| {
    vec![
        re(r#"(?:youtube\.com/watch\?[^"\s]*\bv=)([A-Za-z0-9_-]{11})"#),
        re(r"(?:youtu\.be/)([A-Za-z0-9_-]{11})"),
        re(r"(?:youtube\.com/shorts/)([A-Za-z0-9_-]{11})"),
        re(r"(?:youtube\.com/embed/)([A-Za-z0-9_-]{11})"),
    ]
});

/// The one YouTube video-id extractor (`watch?v=`, `youtu.be/`, `/shorts/`, `/embed/`).
/// Shorts and watch URLs yield the same id — the dedup invariant.
pub fn extract_video_id_from_url(value: &str) -> Option<String> {
    YT_URL_PATTERNS.iter().find_map(|r| captures1(r, value))
}

// ---------------------------------------------------------------- X / Twitter

const X_HOSTS: &[&str] = &[
    "x.com",
    "www.x.com",
    "mobile.x.com",
    "twitter.com",
    "www.twitter.com",
    "mobile.twitter.com",
];

static HANDLE_STATUS_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^/([A-Za-z0-9_]{1,15})/status/([0-9]+)(?:/|$)"));
static I_WEB_STATUS_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^/i/web/status/([0-9]+)(?:/|$)"));

/// An X status reference; `status_id` stays a string (64-bit ids, never parse to a number).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XStatusRef {
    pub handle: Option<String>,
    pub status_id: String,
}

/// Parse an X/Twitter status URL. Query params never affect the result.
pub fn extract_x_status_from_url(raw: &str) -> Option<XStatusRef> {
    let parsed = Url::parse(raw).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") || !X_HOSTS.contains(&parsed.host_str()?) {
        return None;
    }
    let path = parsed.path();
    if let Some(c) = HANDLE_STATUS_RE.as_ref().and_then(|r| r.captures(path)) {
        return Some(XStatusRef {
            handle: c.get(1).map(|m| m.as_str().to_owned()),
            status_id: c.get(2)?.as_str().to_owned(),
        });
    }
    captures1(&I_WEB_STATUS_RE, path).map(|status_id| XStatusRef {
        handle: None,
        status_id,
    })
}

pub fn canonical_x_status_url(handle: Option<&str>, status_id: &str) -> String {
    match handle {
        Some(h) => format!("https://x.com/{h}/status/{status_id}"),
        None => format!("https://x.com/i/web/status/{status_id}"),
    }
}

// ---------------------------------------------------------------- canonicalizeUrl

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedSourceHint {
    /// Always `youtube-channel` today.
    pub kind: String,
    pub canonical: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalizedUrl {
    /// The trimmed input.
    pub url: String,
    pub canonical: String,
    pub domain: String,
    pub filename: String,
    pub youtube_video_id: Option<String>,
    pub x_status_id: Option<String>,
    pub tracked_source: Option<TrackedSourceHint>,
}

const TRACKING_PARAMS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "fbclid",
    "gclid",
    "ref",
    "ref_src",
    "mc_cid",
    "mc_eid",
];
const YT_HOSTS: &[&str] = &["youtube.com", "www.youtube.com", "m.youtube.com"];
const YOUTU_BE_HOSTS: &[&str] = &["youtu.be", "www.youtu.be"];
const ARXIV_HOSTS: &[&str] = &["arxiv.org", "www.arxiv.org"];

static ARXIV_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"^/(?:abs|pdf)/([^/]+?)(?:\.pdf)?$"));

fn set_youtube_watch(u: &mut Url, id: &str) -> Option<()> {
    u.set_host(Some("www.youtube.com")).ok()?;
    u.set_path("/watch");
    u.set_query(Some(&format!("v={id}")));
    Some(())
}

/// Forum post identity (XenForo shape, host-agnostic): a thread-page URL whose fragment is
/// exactly `post-<digits>`, or a `/posts/<digits>` path, keys as `/posts/<digits>`. Call before
/// the fragment is dropped; returns the replacement path.
fn forum_post_path(u: &Url) -> Option<String> {
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let path = u.path();
    if let Some(id) = path.strip_prefix("/posts/") {
        let id = id.strip_suffix('/').unwrap_or(id);
        return all_digits(id).then(|| format!("/posts/{id}"));
    }
    let id = u.fragment()?.strip_prefix("post-")?;
    (all_digits(id) && path.split('/').any(|seg| seg == "threads")).then(|| format!("/posts/{id}"))
}

/// Canonicalize an http(s) URL: lowercase host, drop fragment, YouTube → `www.youtube.com/watch?v=`,
/// arXiv pdf → abs, X status → `x.com`, tracking params stripped, trailing slashes trimmed.
pub fn canonicalize_url(raw: &str) -> Option<CanonicalizedUrl> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parsed = Url::parse(trimmed).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let forum_path = forum_post_path(&parsed);
    parsed.set_fragment(None);
    if let Some(p) = forum_path {
        parsed.set_path(&p);
    }
    let host = parsed.host_str()?.to_owned();
    let mut youtube_video_id = None;
    let mut x_status_id = None;
    let mut tracked_source = None;

    if YT_HOSTS.contains(&host.as_str()) {
        if let Some(id) = extract_video_id_from_url(parsed.as_str()) {
            set_youtube_watch(&mut parsed, &id)?;
            youtube_video_id = Some(id);
        } else if let Some(c) = detect_youtube_channel_hint(parsed.path()) {
            tracked_source = Some(TrackedSourceHint {
                kind: "youtube-channel".into(),
                canonical: c,
            });
        }
    } else if YOUTU_BE_HOSTS.contains(&host.as_str()) {
        if let Some(id) = extract_video_id_from_url(parsed.as_str()) {
            set_youtube_watch(&mut parsed, &id)?;
            youtube_video_id = Some(id);
        }
    } else if ARXIV_HOSTS.contains(&host.as_str()) {
        if let Some(id) = captures1(&ARXIV_RE, parsed.path()) {
            parsed.set_host(Some("arxiv.org")).ok()?;
            parsed.set_path(&format!("/abs/{id}"));
            parsed.set_query(None);
        }
    } else if X_HOSTS.contains(&host.as_str())
        && let Some(r) = extract_x_status_from_url(parsed.as_str())
    {
        let cx = Url::parse(&canonical_x_status_url(r.handle.as_deref(), &r.status_id)).ok()?;
        parsed.set_host(cx.host_str()).ok()?;
        parsed.set_path(cx.path());
        parsed.set_query(cx.query());
        x_status_id = Some(r.status_id);
    }

    if youtube_video_id.is_none()
        && let Some(q) = parsed.query().filter(|q| !q.is_empty()).map(str::to_owned)
    {
        let mut ser = form_urlencoded::Serializer::new(String::new());
        for (k, v) in form_urlencoded::parse(q.as_bytes()) {
            if !TRACKING_PARAMS.contains(&k.to_lowercase().as_str()) {
                ser.append_pair(&k, &v);
            }
        }
        let out = ser.finish();
        parsed.set_query(if out.is_empty() { None } else { Some(&out) });
    }

    let path = parsed.path();
    if path.len() > 1 && path.ends_with('/') {
        let new_path = path.trim_end_matches('/').to_owned();
        parsed.set_path(&new_path);
    }

    let canonical = parsed.as_str().to_owned();
    Some(CanonicalizedUrl {
        url: trimmed.to_owned(),
        filename: slug_for_canonical(&canonical),
        domain: parsed.host_str().unwrap_or_default().to_owned(),
        canonical,
        youtube_video_id,
        x_status_id,
        tracked_source,
    })
}

static YT_HANDLE_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^/(@[^/]+)$"));
static YT_CHANNEL_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^/channel/(UC[^/]+)$"));
static YT_C_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^/c/([^/]+)$"));
static YT_USER_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^/user/([^/]+)$"));

fn detect_youtube_channel_hint(path: &str) -> Option<String> {
    let p = path.trim_end_matches('/');
    if let Some(h) = captures1(&YT_HANDLE_RE, p) {
        return Some(format!("https://www.youtube.com/{h}"));
    }
    if let Some(c) = captures1(&YT_CHANNEL_RE, p) {
        return Some(format!("https://www.youtube.com/channel/{c}"));
    }
    if let Some(c) = captures1(&YT_C_RE, p) {
        return Some(format!("https://www.youtube.com/c/{c}"));
    }
    captures1(&YT_USER_RE, p).map(|u| format!("https://www.youtube.com/user/{u}"))
}

/// FNV-1a over UTF-16 code units, first 4 hex chars (matches Crucible's `shortHash`).
pub fn short_hash(input: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for unit in input.encode_utf16() {
        h ^= u32::from(unit);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")[..4].to_owned()
}

fn clean_slug(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let keep = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@');
        let ch = if keep { c } else { '_' };
        if ch == '_' && out.ends_with('_') {
            continue;
        }
        out.push(ch);
    }
    out.trim_matches('_').to_owned()
}

fn fallback_slug(input: &str) -> String {
    let cleaned = clean_slug(input);
    let s = if cleaned.is_empty() {
        "link".to_owned()
    } else {
        cleaned.to_lowercase()
    };
    s.chars().take(100).collect()
}

/// A filesystem-safe note filename for a canonical URL.
pub fn slug_for_canonical(canonical: &str) -> String {
    let Ok(parsed) = Url::parse(canonical) else {
        return fallback_slug(canonical);
    };
    let host = parsed.host_str().unwrap_or_default();
    let path = parsed.path().trim_start_matches('/');
    let query = parsed.query().unwrap_or_default();
    let joined = [host, path, query]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("__");
    let separated: String = joined
        .chars()
        .map(|c| {
            if matches!(c, '/' | '?' | '&' | '=') {
                "__".to_owned()
            } else {
                c.to_string()
            }
        })
        .collect();
    let mut slug = clean_slug(&separated);
    if slug.is_empty() {
        slug = fallback_slug(canonical);
    }
    if slug.len() > 100 {
        // `clean_slug` output is pure ASCII, so byte slicing is char-safe.
        slug = format!("{}-{}", &slug[..96], short_hash(canonical));
    }
    slug.to_lowercase()
}

// ---------------------------------------------------------------- hosts, URL parts, link tracking

/// Host, path, query and fragment of an `http(s)` URL, split on the raw (trimmed) text with no
/// normalization: the host is lowercased with userinfo and port dropped (`www.` kept); path,
/// query and fragment keep their case and encoding. The one splitter for callers that rebuild or
/// pattern-match the original text (`url::Url` would re-encode it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpParts<'a> {
    pub host: String,
    /// From the first `/` after the host (empty when there is none).
    pub path: &'a str,
    /// After the first `?` (empty when absent).
    pub query: &'a str,
    /// After the first `#`.
    pub fragment: Option<&'a str>,
}

/// Splits an `http(s)` URL into [`HttpParts`] (`None` for any other scheme). The host may be
/// empty (`https:///x`); [`http_host`] rejects that.
#[must_use]
pub fn http_parts(url: &str) -> Option<HttpParts<'_>> {
    let url = url.trim();
    let scheme = |p: &str| {
        url.get(..p.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(p))
    };
    let rest = if scheme("https://") {
        &url[8..]
    } else if scheme("http://") {
        &url[7..]
    } else {
        return None;
    };
    let (rest, fragment) = rest
        .split_once('#')
        .map_or((rest, None), |(r, f)| (r, Some(f)));
    let (auth_path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let (auth, path) = auth_path
        .find('/')
        .map_or((auth_path, ""), |i| (&auth_path[..i], &auth_path[i..]));
    let host = auth.rsplit('@').next().unwrap_or(auth);
    let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
    Some(HttpParts {
        host,
        path,
        query,
        fragment,
    })
}

/// The lowercased host of an `http(s)` URL (userinfo, port, path, query and fragment dropped;
/// `www.` kept); `None` when the scheme is not http(s) or the host is empty.
#[must_use]
pub fn http_host(url: &str) -> Option<String> {
    http_parts(url).map(|p| p.host).filter(|h| !h.is_empty())
}

/// A host name in comparison form: trimmed, lowercased, leading `www.` stripped; `None` when
/// empty.
#[must_use]
pub fn bare_host_name(host: &str) -> Option<String> {
    let h = host.trim().to_lowercase();
    Some(h.trim_start_matches("www.").to_owned()).filter(|h| !h.is_empty())
}

/// The comparison host of a URL (any scheme with a host), parsed with `url::Url`: lowercased,
/// leading `www.` stripped. For comparison only, never for rebuilding a URL.
#[must_use]
pub fn bare_host(url: &str) -> Option<String> {
    bare_host_name(Url::parse(url.trim()).ok()?.host_str()?)
}

/// [`bare_host`] of the [`canonicalize_url`] form, so host aliases converge (`youtu.be` and
/// `www.youtube.com`, `twitter.com` and `x.com`, …).
#[must_use]
pub fn canonical_bare_host(url: &str) -> Option<String> {
    canonicalize_url(url).and_then(|c| bare_host_name(&c.domain))
}

/// Whether `host` (lowercased) is Substack's: `substack.com` or a subdomain.
#[must_use]
pub fn is_substack_host(host: &str) -> bool {
    host == "substack.com" || host.ends_with(".substack.com")
}

/// The share/session params Substack appends to its links (lowercased keys).
pub const SUBSTACK_SESSION_PARAMS: &[&str] = &[
    "r",
    "s",
    "triedredirect",
    "publication_id",
    "post_id",
    "isfreemail",
    "token",
];

/// The link-rewrite tracking policy (mail intake): any `utm_*` key, plus
/// [`SUBSTACK_SESSION_PARAMS`] when the link is on a Substack host. Case-insensitive. (The
/// identity form [`canonicalize_url`] strips its own explicit list, `TRACKING_PARAMS`.)
#[must_use]
pub fn is_link_tracking_param(key: &str, on_substack: bool) -> bool {
    let k = key.to_ascii_lowercase();
    k.starts_with("utm_") || (on_substack && SUBSTACK_SESSION_PARAMS.contains(&k.as_str()))
}

// ---------------------------------------------------------------- blog post-id

/// How a post URL is reduced to a stable post-id (per-blog Canon override).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CanonMethod {
    #[default]
    Auto,
    Substack,
    StripParams,
    KeepParams,
}

impl CanonMethod {
    /// Lenient parse (`normalizeCanonMethod`): unknown or empty → `Auto`.
    pub fn normalize(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "substack" => Self::Substack,
            "strip-params" => Self::StripParams,
            "keep-params" => Self::KeepParams,
            _ => Self::Auto,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostIdOptions {
    /// Forced method; wins over `host_rules` unless `Auto`.
    pub method: Option<CanonMethod>,
    /// hostname → override, from the blogs registry.
    pub host_rules: BTreeMap<String, CanonMethod>,
}

const SUBSTACK_SIGNATURE_PARAMS: &[&str] =
    &["post_id", "publication_id", "isFreemail", "triedRedirect"];
static SUBSTACK_PATH_RE: LazyLock<Option<Regex>> = LazyLock::new(|| re(r"^/p/[^/]+/?$"));
static TRACKING_PARAM_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| re(r"(?i)^(utm_|fbclid$|gclid$|mc_cid$|mc_eid$|ref$|ref_src$)"));

fn strip_trailing_slash(s: &str) -> String {
    s.strip_suffix('/').unwrap_or(s).to_owned()
}

fn bare_slug(u: &Url) -> String {
    strip_trailing_slash(&format!("{}{}", u.origin().ascii_serialization(), u.path()))
}

fn substack_detect(u: &Url) -> bool {
    if !SUBSTACK_PATH_RE
        .as_ref()
        .is_some_and(|r| r.is_match(u.path()))
    {
        return false;
    }
    let pairs: Vec<(String, String)> = u
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if pairs
        .iter()
        .any(|(k, _)| SUBSTACK_SIGNATURE_PARAMS.contains(&k.as_str()))
    {
        return true;
    }
    pairs
        .iter()
        .find(|(k, _)| k == "utm_source")
        .is_some_and(|(_, v)| v == "substack")
}

/// Derive a stable blog post-id from a post URL. Unparseable input returns the trimmed input.
pub fn post_id_from_url(url: &str, opts: &PostIdOptions) -> String {
    let Ok(mut u) = Url::parse(url) else {
        return url.trim().to_owned();
    };
    let method = opts
        .method
        .filter(|m| *m != CanonMethod::Auto)
        .or_else(|| u.host_str().and_then(|h| opts.host_rules.get(h).copied()))
        .unwrap_or_default();
    let forum_path = forum_post_path(&u);
    u.set_fragment(None);
    if let Some(p) = forum_path {
        u.set_path(&p);
    }
    match method {
        CanonMethod::KeepParams => strip_trailing_slash(u.as_str()),
        CanonMethod::StripParams | CanonMethod::Substack => bare_slug(&u),
        CanonMethod::Auto => {
            if substack_detect(&u) {
                return bare_slug(&u);
            }
            let pairs: Vec<(String, String)> = u
                .query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            let is_tracking = |k: &str| TRACKING_PARAM_RE.as_ref().is_some_and(|r| r.is_match(k));
            if pairs.iter().any(|(k, _)| is_tracking(k)) {
                let mut ser = form_urlencoded::Serializer::new(String::new());
                for (k, v) in pairs.iter().filter(|(k, _)| !is_tracking(k)) {
                    ser.append_pair(k, v);
                }
                let out = ser.finish();
                u.set_query(if out.is_empty() { None } else { Some(&out) });
            }
            strip_trailing_slash(u.as_str())
        }
    }
}

/// Build hostname → method from configured blogs `(feed link, method)`, skipping `Auto` and
/// unparseable links (`buildBlogCanonHostMap`).
pub fn build_blog_canon_host_map<'a, I>(entries: I) -> BTreeMap<String, CanonMethod>
where
    I: IntoIterator<Item = (&'a str, CanonMethod)>,
{
    entries
        .into_iter()
        .filter(|(_, m)| *m != CanonMethod::Auto)
        .filter_map(|(link, m)| Some((Url::parse(link).ok()?.host_str()?.to_owned(), m)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDLE: &str = "PandaAshwinee";
    const STATUS_ID: &str = "2078296458122645635";

    fn x_ref(handle: Option<&str>) -> Option<XStatusRef> {
        Some(XStatusRef {
            handle: handle.map(str::to_owned),
            status_id: STATUS_ID.to_owned(),
        })
    }

    // ---- https://github.com/dulrich/obsidian-crucible/blob/master/tests/xPost.test.mjs
    #[test]
    fn x_host_variants_and_case() {
        for host in X_HOSTS.iter().chain(&["WWW.X.COM"]) {
            assert_eq!(
                extract_x_status_from_url(&format!("https://{host}/{HANDLE}/status/{STATUS_ID}")),
                x_ref(Some(HANDLE))
            );
        }
    }

    #[test]
    fn x_query_suffix_and_iweb() {
        assert_eq!(
            extract_x_status_from_url(&format!("https://x.com/{HANDLE}/status/{STATUS_ID}?s=20")),
            x_ref(Some(HANDLE))
        );
        assert_eq!(
            extract_x_status_from_url(&format!(
                "https://twitter.com/{HANDLE}/status/{STATUS_ID}?t=abc"
            )),
            x_ref(Some(HANDLE))
        );
        assert_eq!(
            extract_x_status_from_url(&format!("https://x.com/i/web/status/{STATUS_ID}")),
            x_ref(None)
        );
        assert_eq!(
            extract_x_status_from_url(&format!(
                "https://x.com/{HANDLE}/status/{STATUS_ID}/photo/1"
            )),
            x_ref(Some(HANDLE))
        );
    }

    #[test]
    fn x_invalid_inputs() {
        assert_eq!(
            extract_x_status_from_url(&format!("https://example.com/{HANDLE}/status/{STATUS_ID}")),
            None
        );
        assert_eq!(
            extract_x_status_from_url(&format!("https://x.com/{HANDLE}")),
            None
        );
        assert_eq!(
            extract_x_status_from_url(&format!("https://x.com/{HANDLE}/likes")),
            None
        );
        assert_eq!(
            extract_x_status_from_url(&format!("https://x.com/{HANDLE}/status/{STATUS_ID}abc")),
            None
        );
        assert_eq!(
            extract_x_status_from_url(&format!("ftp://x.com/{HANDLE}/status/{STATUS_ID}")),
            None
        );
        assert_eq!(extract_x_status_from_url("not a url"), None);
        let big = "9223372036854775807";
        assert_eq!(
            extract_x_status_from_url(&format!("https://x.com/{HANDLE}/status/{big}"))
                .unwrap()
                .status_id,
            big
        );
    }

    #[test]
    fn x_canonical_forms() {
        assert_eq!(
            canonical_x_status_url(Some(HANDLE), STATUS_ID),
            format!("https://x.com/{HANDLE}/status/{STATUS_ID}")
        );
        assert_eq!(
            canonical_x_status_url(None, STATUS_ID),
            format!("https://x.com/i/web/status/{STATUS_ID}")
        );
    }

    #[test]
    fn canonicalize_x_variants_converge() {
        let variants = [
            format!("https://x.com/{HANDLE}/status/{STATUS_ID}"),
            format!("https://www.x.com/{HANDLE}/status/{STATUS_ID}"),
            format!("https://mobile.x.com/{HANDLE}/status/{STATUS_ID}"),
            format!("https://twitter.com/{HANDLE}/status/{STATUS_ID}"),
            format!("https://www.twitter.com/{HANDLE}/status/{STATUS_ID}"),
            format!("https://mobile.twitter.com/{HANDLE}/status/{STATUS_ID}"),
            format!("https://x.com/{HANDLE}/status/{STATUS_ID}?s=20"),
            format!("https://x.com/{HANDLE}/status/{STATUS_ID}?t=someTrackingToken"),
            format!("https://twitter.com/{HANDLE}/status/{STATUS_ID}/photo/1"),
        ];
        let expected = format!("https://x.com/{HANDLE}/status/{STATUS_ID}");
        for v in &variants {
            let c = canonicalize_url(v).unwrap();
            assert_eq!(c.canonical, expected, "variant {v}");
            assert_eq!(c.x_status_id.as_deref(), Some(STATUS_ID));
        }
        let iweb = canonicalize_url(&format!("https://x.com/i/web/status/{STATUS_ID}")).unwrap();
        assert_eq!(
            iweb.canonical,
            format!("https://x.com/i/web/status/{STATUS_ID}")
        );
        let profile = canonicalize_url(&format!("https://x.com/{HANDLE}")).unwrap();
        assert_eq!(
            (profile.x_status_id, profile.youtube_video_id),
            (None, None)
        );
    }

    #[test]
    fn canonicalize_youtube_and_arxiv() {
        let c = canonicalize_url("https://youtu.be/dQw4w9WgXcQ").unwrap();
        assert_eq!(c.canonical, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(c.youtube_video_id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(c.x_status_id, None);
        let s =
            canonicalize_url("https://M.YouTube.com/shorts/dQw4w9WgXcQ?feature=share#t=1").unwrap();
        assert_eq!(s.canonical, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        let a = canonicalize_url("https://arxiv.org/abs/2301.12345").unwrap();
        assert_eq!(a.canonical, "https://arxiv.org/abs/2301.12345");
        assert_eq!((a.x_status_id, a.youtube_video_id), (None, None));
        let p = canonicalize_url("https://www.arxiv.org/pdf/2301.12345.pdf").unwrap();
        assert_eq!(p.canonical, "https://arxiv.org/abs/2301.12345");
    }

    #[test]
    fn canonicalize_generic_and_channel_hint() {
        let c = canonicalize_url("  https://Example.com/a/b/?utm_source=x&id=7&FBCLID=1#frag ")
            .unwrap();
        assert_eq!(c.canonical, "https://example.com/a/b?id=7");
        assert_eq!(c.domain, "example.com");
        assert_eq!(c.filename, "example.com_a_b_id_7");
        assert_eq!(
            canonicalize_url("https://example.com/?utm_medium=e")
                .unwrap()
                .canonical,
            "https://example.com/"
        );
        let ch = canonicalize_url("https://www.youtube.com/@example-creator/").unwrap();
        assert_eq!(
            ch.tracked_source.unwrap().canonical,
            "https://www.youtube.com/@example-creator"
        );
        assert!(canonicalize_url("ftp://example.com/x").is_none());
        assert!(canonicalize_url("   ").is_none());
        assert!(canonicalize_url("nope").is_none());
    }

    #[test]
    fn video_id_patterns() {
        for u in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?feature=x&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                extract_video_id_from_url(u).as_deref(),
                Some("dQw4w9WgXcQ"),
                "{u}"
            );
        }
        assert_eq!(
            extract_video_id_from_url("https://www.youtube.com/@example-creator"),
            None
        );
    }

    #[test]
    fn long_slug_is_hashed() {
        let long = format!("https://example.com/{}", "a".repeat(150));
        let s = slug_for_canonical(&long);
        assert_eq!(s.len(), 101);
        assert_eq!(&s[97..], short_hash(&long));
        assert_eq!(short_hash(""), "811c");
    }

    // ---- https://github.com/dulrich/obsidian-crucible/blob/master/tests/postId.test.mjs
    const SUBSTACK_EMAIL: &str = "https://www.blog.example.com/p/top-universities-and-national-intelligence?publication_id=1&post_id=200711891&isFreemail=false&r=52ktrf&triedRedirect=true&utm_source=substack&utm_medium=email";
    const SUBSTACK_SLUG: &str =
        "https://www.blog.example.com/p/top-universities-and-national-intelligence";

    fn pid(u: &str) -> String {
        post_id_from_url(u, &PostIdOptions::default())
    }

    fn pid_m(u: &str, m: CanonMethod) -> String {
        post_id_from_url(
            u,
            &PostIdOptions {
                method: Some(m),
                ..Default::default()
            },
        )
    }

    #[test]
    fn post_id_auto_vectors() {
        assert_eq!(pid(SUBSTACK_EMAIL), SUBSTACK_SLUG);
        assert_eq!(pid(SUBSTACK_SLUG), SUBSTACK_SLUG);
        assert_eq!(
            pid("https://example.com/p/some-slug?article_id=123"),
            "https://example.com/p/some-slug?article_id=123"
        );
        assert_eq!(
            pid("https://news.example.org/read?article_id=998877"),
            "https://news.example.org/read?article_id=998877"
        );
        assert_eq!(
            pid("https://blog.example.org/post-title?utm_source=x&utm_medium=email&fbclid=abc"),
            "https://blog.example.org/post-title"
        );
        assert_eq!(
            pid("https://blog.example.org/post/#section"),
            "https://blog.example.org/post"
        );
        assert_eq!(pid("  not a url "), "not a url");
    }

    const FORUM_A: &str = "https://Forums.Example.com/threads/some-story.123/page-16#post-1001";
    const FORUM_B: &str = "https://forums.example.com/threads/some-story.123/page-16#post-1002";

    #[test]
    fn forum_post_fragment_keys_per_post() {
        let canon = |u: &str| canonicalize_url(u).map(|c| c.canonical);
        assert_ne!(canon(FORUM_A), canon(FORUM_B));
        assert_ne!(pid(FORUM_A), pid(FORUM_B));
        assert_eq!(
            canon(FORUM_A).as_deref(),
            Some("https://forums.example.com/posts/1001")
        );
        assert_eq!(pid(FORUM_A), "https://forums.example.com/posts/1001");
        let posts = "https://forums.example.com/posts/1001/";
        assert_eq!(canon(posts), canon(FORUM_A));
        assert_eq!(pid(posts), pid(FORUM_A));
    }

    #[test]
    fn forum_post_rule_is_narrow() {
        let reply = "https://forums.example.com/threads/some-story.123/page-16#reply";
        assert_eq!(
            canonicalize_url(reply).map(|c| c.canonical).as_deref(),
            Some("https://forums.example.com/threads/some-story.123/page-16")
        );
        assert_eq!(
            pid(reply),
            "https://forums.example.com/threads/some-story.123/page-16"
        );
        let other = "https://blog.example.org/post/x#post-5";
        assert_eq!(
            canonicalize_url(other).map(|c| c.canonical).as_deref(),
            Some("https://blog.example.org/post/x")
        );
        assert_eq!(pid(other), "https://blog.example.org/post/x");
    }

    #[test]
    fn post_id_overrides() {
        let u = "https://example.com/read?article_id=1&utm_source=x";
        assert_eq!(pid_m(u, CanonMethod::KeepParams), u);
        assert_eq!(
            pid_m(u, CanonMethod::StripParams),
            "https://example.com/read"
        );
        assert_eq!(
            pid_m(
                "https://example.com/p/my-slug?whatever=1",
                CanonMethod::Substack
            ),
            "https://example.com/p/my-slug"
        );
        let rules = build_blog_canon_host_map([
            ("https://example.com/feed", CanonMethod::KeepParams),
            ("https://a.org/", CanonMethod::Auto),
            ("bad", CanonMethod::StripParams),
        ]);
        assert_eq!(rules.len(), 1);
        let opts = PostIdOptions {
            method: Some(CanonMethod::Auto),
            host_rules: rules,
        };
        assert_eq!(post_id_from_url(u, &opts), u);
    }

    #[test]
    fn canon_method_normalize() {
        assert_eq!(
            CanonMethod::normalize(" Strip-Params "),
            CanonMethod::StripParams
        );
        assert_eq!(CanonMethod::normalize("bogus"), CanonMethod::Auto);
    }

    #[test]
    fn http_parts_split_the_raw_text() {
        let p = http_parts(" HTTPS://User@Www.Ex.COM:8080/P/Slug?A=1&b=2#Frag ");
        assert_eq!(
            p,
            Some(HttpParts {
                host: "www.ex.com".into(),
                path: "/P/Slug",
                query: "A=1&b=2",
                fragment: Some("Frag"),
            })
        );
        assert_eq!(http_parts("ftp://a.example/"), None);
        assert_eq!(http_host("https:///x"), None);
        assert_eq!(http_host("http://a.example?x"), Some("a.example".into()));
    }

    #[test]
    fn bare_host_forms() {
        assert_eq!(bare_host("https://WWW.Ex.com/x"), Some("ex.com".into()));
        assert_eq!(bare_host("not a url"), None);
        assert_eq!(
            bare_host_name(" Www.Fanfiction.net "),
            Some("fanfiction.net".into())
        );
        assert_eq!(bare_host_name("www."), None);
        assert_eq!(
            canonical_bare_host("https://youtu.be/dQw4w9WgXcQ"),
            Some("youtube.com".into())
        );
        assert_eq!(
            canonical_bare_host("https://twitter.com/a/status/1"),
            Some("x.com".into())
        );
    }

    #[test]
    fn link_tracking_policy() {
        assert!(is_link_tracking_param("UTM_Source", false));
        assert!(!is_link_tracking_param("r", false));
        assert!(is_link_tracking_param("isFreemail", true));
        assert!(!is_link_tracking_param("keep", true));
        assert!(is_substack_host("a.substack.com") && !is_substack_host("notsubstack.com"));
    }
}
