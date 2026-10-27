//! Page-relative image refs (plan `round-15-relative-images-pdf-viewer` WP-1): a web clip's
//! `![](/images/a.png)` / `![](a.png)` resolves against the page the note was clipped from, its
//! frontmatter `source` URL, not against the vault.

use cv_note::FrontmatterDoc;
use cv_text::attachments::{AttachmentMatch, Syntax, is_md5_name, md_ref_raw_destination};
use cv_text::vpath::basename;
use url::Url;

/// The note's frontmatter `source` when it is an `http(s)` URL.
pub(super) fn source_url(content: &str) -> Option<Url> {
    let raw = FrontmatterDoc::new(content).get_str_trimmed("source")?;
    let u = Url::parse(&raw).ok()?;
    matches!(u.scheme(), "http" | "https").then_some(u)
}

/// Joins the original, still percent-encoded destination `raw_dest` to `source` with standard
/// URL-reference resolution; `None` unless the result is an `http(s)` URL. The fragment is
/// dropped (it is never sent). The destination must be the encoded text, never the decoded
/// link: `x%23y.png` is the file `x#y.png`, not a fragment (audit E2).
pub(super) fn join_source(source: &Url, raw_dest: &str) -> Option<Url> {
    let mut u = source.join(raw_dest.trim()).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    u.set_fragment(None);
    Some(u)
}

/// Whether `m` is a markdown image with a non-remote, non-managed target: the only ref shape
/// the source-relative rule considers (vault resolution is checked by the caller).
pub(super) fn is_relative_md_image(m: &AttachmentMatch) -> bool {
    m.is_embed
        && m.syntax == Syntax::Md
        && !m.is_remote
        && !is_md5_name(basename(m.link.split('#').next().unwrap_or("")))
}

/// The remote form of `m` joined to `source`: `m` with its `link` the absolute URL and
/// `is_remote` set (so the remote download path handles it); `original`/`syntax`/display text
/// stay, so the rewritten ref replaces the original text.
pub(super) fn as_remote(m: &AttachmentMatch, source: &Url) -> Option<AttachmentMatch> {
    if !is_relative_md_image(m) {
        return None;
    }
    let raw = md_ref_raw_destination(&m.original)?;
    let url = join_source(source, &raw)?;
    Some(AttachmentMatch {
        link: url.into(),
        is_remote: true,
        ..m.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(source: &str, dest: &str) -> Option<String> {
        join_source(&Url::parse(source).unwrap(), dest).map(String::from)
    }

    #[test]
    fn joins_root_relative_bare_and_parent_refs() {
        let s = "https://h.example/blog/2026/post";
        assert_eq!(j(s, "/img/a.png").unwrap(), "https://h.example/img/a.png");
        assert_eq!(j(s, "a.png").unwrap(), "https://h.example/blog/2026/a.png");
        assert_eq!(j(s, "../x.png").unwrap(), "https://h.example/blog/x.png");
        assert_eq!(
            j(s, "./y/z.png").unwrap(),
            "https://h.example/blog/2026/y/z.png"
        );
    }

    #[test]
    fn query_and_fragment_of_the_source_are_ignored() {
        let s = "https://h.example/blog/post?id=7#section";
        assert_eq!(j(s, "a.png").unwrap(), "https://h.example/blog/a.png");
        assert_eq!(j(s, "/a.png").unwrap(), "https://h.example/a.png");
        assert_eq!(j(s, "a.png#frag").unwrap(), "https://h.example/blog/a.png");
    }

    #[test]
    fn encoded_reserved_characters_stay_in_the_path() {
        let s = "https://h.example/p/post";
        // `x%23y.png` is the file `x#y.png`: never a `#y.png` fragment.
        assert_eq!(j(s, "x%23y.png").unwrap(), "https://h.example/p/x%23y.png");
        assert_eq!(j(s, "a%3Fb.png").unwrap(), "https://h.example/p/a%3Fb.png");
    }

    #[test]
    fn non_http_results_are_refused() {
        let s = "https://h.example/p/post";
        assert_eq!(j(s, "mailto:a@b.c"), None);
        assert_eq!(j(s, "file:///etc/x.png"), None);
    }

    #[test]
    fn source_must_be_an_http_url() {
        assert!(source_url("---\nsource: https://h.example/p\n---\nx").is_some());
        assert!(source_url("---\nsource: \"ftp://h.example/p\"\n---\nx").is_none());
        assert!(source_url("---\nsource: not a url\n---\nx").is_none());
        assert!(source_url("no frontmatter").is_none());
    }

    #[test]
    fn as_remote_joins_the_encoded_destination() {
        let note =
            "---\nsource: https://h.example/p/post\n---\n![alt](x%23y.png) ![](/i/a.png \"t\")\n";
        let src = source_url(note).unwrap();
        let ms = cv_text::attachments::parse_attachment_refs(note);
        let links: Vec<String> = ms
            .iter()
            .filter_map(|m| as_remote(m, &src))
            .map(|m| m.link)
            .collect();
        assert_eq!(
            links,
            ["https://h.example/p/x%23y.png", "https://h.example/i/a.png"]
        );
    }
}
