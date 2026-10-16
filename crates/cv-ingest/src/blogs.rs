//! Blog tracker policy over parsed feeds (plan `v0-automation-pipelines` WP-8): post-id, body
//! mode, podcast vs article, and the captured blog note an automatable post becomes.
//!
//! Ports (all under `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/`): `blogs.ts:211-344`
//! (`parseRssItems`/`parseAtomEntries`: `url = link || guid`, `postId = guid ||
//! postIdFromUrl(url, canon)`, `(untitled)` title, author dedupe; `buildRemotePost`: the
//! presence-based body modes — `snippet` never has a body, `full` takes `content` else `summary`,
//! `auto` only the dedicated content element; `kind = podcast` when an audio enclosure exists),
//! `blogs.ts:349-361` (`normalizePublishedAt`) and `blogsApi.ts:74` (body → Markdown, here via
//! [`crate::html::html_to_markdown`]). Dropped (Obsidian-induced): `DOMParser`, the
//! `_blog_metadata` note (no longer written, plan Decisions locked) and `requestUrl`.
//!
//! The captured note mirrors a filed blog entry (Web Clipper + `file-as-kind blog` + lint):
//! frontmatter keys `title, source, post-id, author, published, created, description,
//! word-count, tags, read` in that order, `author` a block list of quoted wikilinks, `read: false`.

use cv_text::url_canon::{CanonMethod, PostIdOptions, post_id_from_url};

use crate::feed::FeedItem;
use crate::html::{html_to_markdown, html_to_text, word_count};
use crate::youtube::json_string;

/// Per-source body mode (`sources.toml` `body`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BodyMode {
    /// Only the dedicated content element counts.
    #[default]
    Auto,
    /// Content, else summary.
    Full,
    /// Never a body.
    Snippet,
}

/// Article or podcast episode (audio enclosure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostKind {
    /// A written post.
    Article,
    /// A post with an audio enclosure.
    Podcast,
}

impl PostKind {
    /// Crucible's `BlogPostKind` string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PostKind::Article => "article",
            PostKind::Podcast => "podcast",
        }
    }
}

/// One feed post after policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlogPost {
    /// Stable id: `guid` else the canonicalized URL.
    pub post_id: String,
    /// The canonicalized URL (the id a filed note's lint derives from `source`).
    pub url_id: String,
    /// Title (`(untitled)` when absent).
    pub title: String,
    /// Post URL (`link || guid`).
    pub url: String,
    /// `YYYY-MM-DD` when parseable, else the raw string.
    pub published: String,
    /// Deduplicated authors.
    pub authors: Vec<String>,
    /// Body HTML when the mode yields one.
    pub body_html: Option<String>,
    /// Summary HTML (description source).
    pub summary_html: String,
    /// Article / podcast.
    pub kind: PostKind,
    /// Audio enclosure URL (empty = none).
    pub audio_url: String,
}

impl BlogPost {
    /// Whether the post carries a full body (the automatable evidence).
    #[must_use]
    pub fn has_body(&self) -> bool {
        self.body_html.is_some()
    }
}

fn dedupe(v: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in v {
        let s = s.trim();
        if !s.is_empty() && !out.iter().any(|o| o == s) {
            out.push(s.to_owned());
        }
    }
    out
}

/// Applies Crucible's per-post policy to parsed feed items; items without a URL are dropped.
#[must_use]
pub fn posts_from_feed(items: &[FeedItem], canon: CanonMethod, body: BodyMode) -> Vec<BlogPost> {
    let opts = PostIdOptions {
        method: Some(canon),
        host_rules: Default::default(),
    };
    items
        .iter()
        .filter_map(|it| {
            let url = if it.link.trim().is_empty() {
                it.guid.trim()
            } else {
                it.link.trim()
            };
            if url.is_empty() {
                return None;
            }
            let url_id = post_id_from_url(url, &opts);
            let guid = it.guid.trim();
            let post_id = if guid.is_empty() {
                url_id.clone()
            } else {
                guid.to_owned()
            };
            let content = it.content_html.trim();
            let summary = it.summary_html.trim();
            let body_html = match body {
                BodyMode::Snippet => None,
                BodyMode::Full => [content, summary].into_iter().find(|s| !s.is_empty()),
                BodyMode::Auto => Some(content).filter(|s| !s.is_empty()),
            }
            .map(str::to_owned);
            let title = it.title.trim();
            Some(BlogPost {
                post_id,
                url_id,
                title: if title.is_empty() {
                    "(untitled)".into()
                } else {
                    title.to_owned()
                },
                url: url.to_owned(),
                published: normalize_published(&it.published),
                authors: dedupe(&it.authors),
                body_html,
                summary_html: summary.to_owned(),
                kind: if it.enclosure_audio_url.is_empty() {
                    PostKind::Article
                } else {
                    PostKind::Podcast
                },
                audio_url: it.enclosure_audio_url.clone(),
            })
        })
        .collect()
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Crucible `normalizePublishedAt`: ISO-like input is kept; an RFC 2822 date (`Tue, 14 Oct 2026
/// 10:00:00 +0000`) becomes `YYYY-MM-DD`; anything else is returned trimmed. Deviation: the
/// RFC 2822 day is the stated day, not shifted to UTC (no clock/tz dependency in this crate).
#[must_use]
pub fn normalize_published(raw: &str) -> String {
    let t = raw.trim();
    let b = t.as_bytes();
    if b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
    {
        return t.to_owned();
    }
    let rest = t.split_once(',').map_or(t, |(_, r)| r);
    let mut parts = rest.split_whitespace();
    let (Some(d), Some(m), Some(y)) = (parts.next(), parts.next(), parts.next()) else {
        return t.to_owned();
    };
    let month = MONTHS
        .iter()
        .position(|n| m.len() >= 3 && m[..3].eq_ignore_ascii_case(n));
    match (d.parse::<u32>(), month, y.parse::<u32>()) {
        (Ok(d), Some(m), Ok(y)) if (1..=31).contains(&d) && y >= 1000 => {
            format!("{y:04}-{:02}-{d:02}", m + 1)
        }
        _ => t.to_owned(),
    }
}

/// The note basename for `post`: slugged title, else the URL slug; at most 80 chars.
#[must_use]
pub fn note_slug(post: &BlogPost) -> String {
    let mut s = cv_text::template::slugify(&post.title);
    if s.is_empty() || post.title == "(untitled)" {
        s = cv_text::url_canon::slug_for_canonical(&post.url);
    }
    if s.len() > 80 {
        s.truncate(80);
        s = s.trim_end_matches('-').to_owned();
    }
    if s.is_empty() { "post".into() } else { s }
}

/// The description: the summary as one plain-text line, at most 300 chars.
#[must_use]
pub fn description(post: &BlogPost) -> String {
    let text = html_to_text(&post.summary_html);
    if text.chars().count() <= 300 {
        return text;
    }
    let cut: String = text.chars().take(299).collect();
    format!("{}…", cut.trim_end())
}

/// The captured blog note for `post` (a filed blog entry's shape; lint runs after the write).
/// `tags` already include the kind's tags; `created` is `YYYY-MM-DD`.
#[must_use]
pub fn captured_note(post: &BlogPost, tags: &[String], created: &str) -> String {
    let body = post
        .body_html
        .as_deref()
        .map(html_to_markdown)
        .unwrap_or_default();
    let mut fm = vec![
        "---".to_owned(),
        format!("title: {}", json_string(&post.title)),
        format!("source: {}", post.url),
        format!("post-id: {}", json_string(&post.post_id)),
    ];
    if post.authors.is_empty() {
        fm.push("author: []".into());
    } else {
        fm.push("author:".into());
        for a in &post.authors {
            fm.push(format!("  - {}", json_string(&format!("[[{a}]]"))));
        }
    }
    fm.push(if post.published.is_empty() {
        "published:".into()
    } else {
        format!("published: {}", post.published)
    });
    fm.push(format!("created: {created}"));
    fm.push(format!("description: {}", json_string(&description(post))));
    fm.push(format!(
        "word-count: {}",
        post.body_html.as_deref().map_or(0, word_count)
    ));
    fm.push("tags:".into());
    for t in tags {
        fm.push(format!("  - {t}"));
    }
    fm.push("read: false".into());
    fm.push("---".into());
    let mut out = fm.join("\n");
    out.push('\n');
    if !post.audio_url.is_empty() {
        out.push_str(&format!("\n[Audio]({})\n", post.audio_url));
    }
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::parse_feed;

    const RSS: &str = r#"<rss><channel><title>B</title>
<item><title>Full One</title><link>https://ex.com/p/one?utm_source=x</link><dc:creator>Ann</dc:creator><dc:creator>Ann</dc:creator><pubDate>Tue, 14 Oct 2026 10:00:00 +0000</pubDate><description>Short &amp; sweet</description><content:encoded><![CDATA[<p>Hello <b>world</b></p>]]></content:encoded></item>
<item><title>Snip</title><guid>g-2</guid><link>https://ex.com/two</link><description>Only a summary</description><enclosure url="https://ex.com/a.mp3" type="audio/mpeg"/></item>
</channel></rss>"#;

    const ATOM: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>A</title>
<entry><title>At</title><id>tag:a,1</id><link rel="alternate" href="https://a.org/x/"/><published>2026-10-01T00:00:00Z</published><author><name>Bo</name></author><content type="html">&lt;p&gt;Body&lt;/p&gt;</content></entry>
</feed>"#;

    #[test]
    fn rss_and_atom_posts() {
        let p = posts_from_feed(&parse_feed(RSS).unwrap(), CanonMethod::Auto, BodyMode::Auto);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].post_id, "https://ex.com/p/one");
        assert_eq!(p[0].authors, vec!["Ann".to_owned()]);
        assert_eq!(p[0].published, "2026-10-14");
        assert!(p[0].has_body() && !p[1].has_body());
        assert_eq!(p[1].post_id, "g-2");
        assert_eq!(p[1].kind, PostKind::Podcast);
        assert_eq!(p[0].kind, PostKind::Article);
        let a = posts_from_feed(
            &parse_feed(ATOM).unwrap(),
            CanonMethod::Auto,
            BodyMode::Auto,
        );
        assert_eq!(a[0].post_id, "tag:a,1");
        assert_eq!(a[0].url, "https://a.org/x/");
        assert_eq!(a[0].url_id, "https://a.org/x");
        assert_eq!(a[0].published, "2026-10-01T00:00:00Z");
        assert!(a[0].has_body());
    }

    #[test]
    fn body_modes_are_presence_based() {
        let items = parse_feed(RSS).unwrap();
        let full = posts_from_feed(&items, CanonMethod::Auto, BodyMode::Full);
        assert!(full[0].has_body() && full[1].has_body());
        assert_eq!(full[1].body_html.as_deref(), Some("Only a summary"));
        let snip = posts_from_feed(&items, CanonMethod::Auto, BodyMode::Snippet);
        assert!(snip.iter().all(|p| !p.has_body()));
    }

    #[test]
    fn canon_modes() {
        let item = FeedItem {
            link: "https://ex.com/a/?article_id=7&utm_medium=m#frag".into(),
            ..FeedItem::default()
        };
        let id = |m| {
            posts_from_feed(std::slice::from_ref(&item), m, BodyMode::Auto)[0]
                .post_id
                .clone()
        };
        assert_eq!(id(CanonMethod::Auto), "https://ex.com/a/?article_id=7");
        assert_eq!(id(CanonMethod::StripParams), "https://ex.com/a");
        assert_eq!(id(CanonMethod::Substack), "https://ex.com/a");
        assert_eq!(
            id(CanonMethod::KeepParams),
            "https://ex.com/a/?article_id=7&utm_medium=m"
        );
    }

    #[test]
    fn captured_note_has_filed_blog_shape() {
        let p = &posts_from_feed(&parse_feed(RSS).unwrap(), CanonMethod::Auto, BodyMode::Auto)[0];
        let n = captured_note(p, &["blog".into(), "research".into()], "2026-10-15");
        let keys: Vec<&str> = n
            .lines()
            .skip(1)
            .take_while(|l| *l != "---")
            .filter(|l| !l.starts_with(' '))
            .map(|l| l.split(':').next().unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "title",
                "source",
                "post-id",
                "author",
                "published",
                "created",
                "description",
                "word-count",
                "tags",
                "read"
            ]
        );
        assert!(n.contains("author:\n  - \"[[Ann]]\"\n"));
        assert!(n.contains("description: \"Short & sweet\"\n"));
        assert!(n.contains("tags:\n  - blog\n  - research\nread: false\n---\n"));
        assert!(n.contains("Hello **world**"));
        assert_eq!(note_slug(p), "full-one");
    }
}
