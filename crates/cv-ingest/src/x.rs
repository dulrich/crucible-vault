//! `_x_metadata` note builders (plan `v0-automation-pipelines` WP-5).
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/xApi.ts`: `buildXMetadataNoteBody
//! :203-221`, `buildXTombstoneNoteBody :224-234`, `resolveFolderSlug :190-197`,
//! `authorHandleFromUrl :178-186`, `stripScriptTags :170-172`. Key order and quoting match
//! (`yamlString` = `JSON.stringify`, [`json_string`]); the body uses [`html_to_markdown`] in place
//! of Obsidian's `htmlToMarkdown` (an Obsidian-provided API, not a behaviour constraint).

use cv_text::url_canon::extract_x_status_from_url;

use crate::fm::json_string;
use crate::html::html_to_markdown;
use cv_text::slugify;

/// Crucible `source_command` stamped on every X metadata note.
pub const SOURCE_COMMAND: &str = "x-fetch-post-metadata";

/// The oEmbed fields a live note is built from (typed: no JSON here).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XPost {
    pub author_name: String,
    pub author_url: String,
    pub html: String,
    /// oEmbed `type`.
    pub kind: String,
    pub version: String,
}

/// `authorHandleFromUrl`: the profile URL's path with slashes trimmed (`''` when absent).
#[must_use]
pub fn author_handle(author_url: &str) -> String {
    let Some((_, rest)) = author_url.split_once("://") else {
        return String::new();
    };
    let path = rest.find('/').map_or("", |i| &rest[i..]);
    let path = path.split(['?', '#']).next().unwrap_or("");
    path.trim_matches('/').to_owned()
}

/// `resolveFolderSlug`: slugified author handle, else the canonical URL's handle, else
/// `unknown-author` — never empty.
#[must_use]
pub fn folder_slug(author_url: &str, canonical_url: &str) -> String {
    let from_author = slugify(&author_handle(author_url));
    if !from_author.is_empty() {
        return from_author;
    }
    let from_canon = extract_x_status_from_url(canonical_url)
        .and_then(|r| r.handle)
        .map(|h| slugify(&h))
        .unwrap_or_default();
    if from_canon.is_empty() {
        "unknown-author".to_owned()
    } else {
        from_canon
    }
}

/// `stripScriptTags`: drops every `<script…>…</script>` block (case-insensitive).
#[must_use]
pub fn strip_script_tags(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(off) = lower[i..].find("<script") {
        let start = i + off;
        let Some(close) = lower[start..].find("</script>") else {
            break;
        };
        out.push_str(&html[i..start]);
        i = start + close + "</script>".len();
    }
    out.push_str(&html[i..]);
    out
}

/// The post text as Markdown (the oEmbed blockquote, scripts stripped).
#[must_use]
pub fn post_markdown(html: &str) -> String {
    html_to_markdown(&strip_script_tags(html)).trim().to_owned()
}

/// `buildXMetadataNoteBody`: a live post note. `published` is never emitted (oEmbed has none).
#[must_use]
pub fn metadata_note(status_id: &str, canonical_url: &str, p: &XPost, fetched_at: &str) -> String {
    let handle = author_handle(&p.author_url);
    let mut fm = vec![
        "---".to_owned(),
        format!("status-id: {}", json_string(status_id)),
        format!("url: {canonical_url}"),
    ];
    if !p.author_name.is_empty() {
        fm.push(format!("author: {}", json_string(&p.author_name)));
    }
    if !handle.is_empty() {
        fm.push(format!("author-handle: {}", json_string(&handle)));
    }
    if !p.author_url.is_empty() {
        fm.push(format!("author-url: {}", p.author_url));
    }
    if !p.kind.is_empty() {
        fm.push(format!("oembed-type: {}", json_string(&p.kind)));
    }
    if !p.version.is_empty() {
        fm.push(format!("oembed-version: {}", json_string(&p.version)));
    }
    fm.push("state: ok".to_owned());
    fm.push(format!("fetched_at: {fetched_at}"));
    fm.push(format!("source_command: {SOURCE_COMMAND}"));
    fm.push("---".to_owned());
    fm.push(String::new());
    let label = [p.author_name.as_str(), handle.as_str(), status_id]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or(status_id);
    format!(
        "{}# Post by {label}\n\n{}\n",
        fm.join("\n"),
        post_markdown(&p.html)
    )
}

/// `buildXTombstoneNoteBody` (reason `deleted-or-private`): frontmatter only.
#[must_use]
pub fn tombstone_note(status_id: &str, canonical_url: &str, fetched_at: &str) -> String {
    format!(
        "---\nstatus-id: {}\nurl: {canonical_url}\nstate: unavailable\nunavailable-reason: \"deleted-or-private\"\nfetched_at: {fetched_at}\nsource_command: {SOURCE_COMMAND}\n---\n",
        json_string(status_id)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "2026-10-15T00:00:00.000Z";

    #[test]
    fn metadata_note_matches_crucible_key_order() {
        let p = XPost {
            author_name: "Synth \"Q\"".into(),
            author_url: "https://twitter.com/SynthUser".into(),
            html: "<blockquote class=\"twitter-tweet\"><p>Hello <a href=\"https://t.co/x\">link</a></p>&mdash; Synth</blockquote><script async src=\"w.js\"></script>".into(),
            kind: "rich".into(),
            version: "1.0".into(),
        };
        let n = metadata_note("123", "https://x.com/SynthUser/status/123", &p, T);
        let want = "---\nstatus-id: \"123\"\nurl: https://x.com/SynthUser/status/123\nauthor: \"Synth \\\"Q\\\"\"\nauthor-handle: \"SynthUser\"\nauthor-url: https://twitter.com/SynthUser\noembed-type: \"rich\"\noembed-version: \"1.0\"\nstate: ok\nfetched_at: 2026-10-15T00:00:00.000Z\nsource_command: x-fetch-post-metadata\n---\n# Post by Synth \"Q\"\n\n";
        assert!(n.starts_with(want), "{n}");
        assert!(n.contains("Hello [link](https://t.co/x)"), "{n}");
        assert!(!n.contains("script"));
        let bare = metadata_note("9", "https://x.com/i/web/status/9", &XPost::default(), T);
        assert!(bare.contains("url: https://x.com/i/web/status/9\nstate: ok\n"));
        assert!(bare.contains("# Post by 9\n"));
    }

    #[test]
    fn tombstone_and_slug() {
        assert_eq!(
            tombstone_note("5", "https://x.com/i/web/status/5", T),
            "---\nstatus-id: \"5\"\nurl: https://x.com/i/web/status/5\nstate: unavailable\nunavailable-reason: \"deleted-or-private\"\nfetched_at: 2026-10-15T00:00:00.000Z\nsource_command: x-fetch-post-metadata\n---\n"
        );
        assert_eq!(folder_slug("https://x.com/Some_User/", ""), "some-user");
        assert_eq!(folder_slug("", "https://x.com/Other/status/1"), "other");
        assert_eq!(
            folder_slug("", "https://x.com/i/web/status/1"),
            "unknown-author"
        );
    }
}
