//! Substack post probe helpers (plan `auto-intake-sprint` WP-2): the `/api/v1/posts/<slug>` URL
//! for a `/p/<slug>` post and the two fields `track_blogs` reads from its JSON (`audience`,
//! `wordcount`). The completeness policy itself is pure, in [`cv_ingest::access`]; the JSON is
//! parsed here because `cv-ingest` carries no `serde_json`.

use serde::Deserialize;

/// `<scheme>://<host>/api/v1/posts/<slug>` for a post URL whose path is `/p/<slug>` (any host,
/// http or https); `None` otherwise.
#[must_use]
pub fn probe_url(post_url: &str) -> Option<String> {
    let u = url::Url::parse(post_url.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    let mut segs = u.path_segments()?.filter(|s| !s.is_empty());
    let (Some("p"), Some(slug), None) = (segs.next(), segs.next(), segs.next()) else {
        return None;
    };
    let host = u.host_str()?;
    let port = u.port().map(|p| format!(":{p}")).unwrap_or_default();
    Some(format!("{}://{host}{port}/api/v1/posts/{slug}", u.scheme()))
}

#[derive(Deserialize)]
struct PostMeta {
    audience: String,
    #[serde(default)]
    wordcount: Option<u32>,
}

/// `(audience, wordcount)` of a Substack post JSON; extra fields are ignored, malformed JSON (or
/// a missing `audience`) is `None`.
#[must_use]
pub fn parse_post_meta(json: &str) -> Option<(String, Option<u32>)> {
    let m: PostMeta = serde_json::from_str(json).ok()?;
    Some((m.audience, m.wordcount))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_url_only_for_p_slug_paths() {
        assert_eq!(
            probe_url("https://x.substack.com/p/hello?utm_source=rss").as_deref(),
            Some("https://x.substack.com/api/v1/posts/hello")
        );
        assert_eq!(
            probe_url("http://127.0.0.1:8080/p/a/").as_deref(),
            Some("http://127.0.0.1:8080/api/v1/posts/a")
        );
        assert_eq!(probe_url("https://blog.example/2026/10/post"), None);
        assert_eq!(probe_url("https://blog.example/p/"), None);
        assert_eq!(probe_url("https://blog.example/p/a/comments"), None);
        assert_eq!(probe_url("ftp://blog.example/p/a"), None);
    }

    #[test]
    fn parse_post_meta_tolerates_extras_and_rejects_garbage() {
        assert_eq!(
            parse_post_meta(r#"{"id":1,"audience":"only_paid","wordcount":1200,"x":[1]}"#),
            Some(("only_paid".into(), Some(1200)))
        );
        assert_eq!(
            parse_post_meta(r#"{"audience":"everyone"}"#),
            Some(("everyone".into(), None))
        );
        assert_eq!(parse_post_meta("<html>"), None);
        assert_eq!(parse_post_meta(r#"{"wordcount":3}"#), None);
    }
}
