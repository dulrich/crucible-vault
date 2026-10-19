//! YouTube metadata note builders (plan `v0-automation-pipelines` WP-4): pure, byte-compatible
//! ports of Crucible's `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/youtubeApi.ts`
//! (`buildMetadataNoteBody :408-435`, `buildYoutubeTombstoneNoteBody :454-468`,
//! `buildChannelAboutNoteBody :780-794`, `parseIso8601Duration :852-861`); Crucible `slugify`
//! is the one `cv_text::template::slugify` (WP-R10). The caller supplies `fetched_at` (Crucible's
//! `new Date().toISOString()`), so output is deterministic. Nothing Obsidian-induced to drop.

/// One video's metadata (Crucible `YoutubeVideoMetadata`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoMeta {
    pub video_id: String,
    pub title: String,
    pub description: String,
    /// ISO-8601 duration as returned (`PT4M13S`).
    pub duration: String,
    pub channel_id: String,
    pub channel_title: String,
    pub published_at: String,
    pub tags: Vec<String>,
    pub category_id: String,
    pub default_language: Option<String>,
    pub live_broadcast_content: String,
    pub view_count: Option<i64>,
    pub like_count: Option<i64>,
    pub comment_count: Option<i64>,
}

/// One channel's metadata (Crucible `YoutubeChannelMetadata`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelMeta {
    pub channel_id: String,
    pub title: String,
    pub description: String,
    pub custom_url: String,
    pub published_at: String,
    pub country: Option<String>,
    pub thumbnail_url: Option<String>,
    pub subscriber_count: Option<i64>,
    pub video_count: Option<i64>,
    pub view_count: Option<i64>,
}

/// The canonical watch URL (`youtubeWatchUrl`).
#[must_use]
pub fn watch_url(video_id: &str) -> String {
    format!("https://www.youtube.com/watch?v={video_id}")
}

/// The channel URL Crucible writes.
#[must_use]
pub fn channel_url(channel_id: &str) -> String {
    format!("https://www.youtube.com/channel/{channel_id}")
}

/// Crucible's `yamlString` = `JSON.stringify` of a string.
#[must_use]
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn num(n: Option<i64>) -> String {
    n.map_or_else(|| "null".to_owned(), |n| n.to_string())
}

/// `parseIso8601Duration`: `PT#H#M#S` → seconds; `None` for anything else (and for `PT`).
#[must_use]
pub fn parse_iso8601_duration(value: &str) -> Option<i64> {
    let mut rest = value.strip_prefix("PT")?;
    let mut total = 0i64;
    for (unit, mul) in [('H', 3600i64), ('M', 60), ('S', 1)] {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 && rest[digits..].starts_with(unit) {
            let n: i64 = rest[..digits].parse().ok()?;
            total = total.checked_add(n.checked_mul(mul)?)?;
            rest = &rest[digits + 1..];
        }
    }
    if !rest.is_empty() {
        return None;
    }
    // Crucible: `total > 0 || /PT0?S/.test(value)`.
    (total > 0 || value.contains("PT0S") || value.contains("PTS")).then_some(total)
}

fn body(fm: &[String], title: &str, description: &str) -> String {
    format!(
        "---\n{}\n---\n# {title}\n\n## Description\n\n{description}\n",
        fm.join("\n")
    )
}

fn opt_json(v: Option<&str>) -> String {
    v.map_or_else(|| "null".to_owned(), json_string)
}

/// `buildMetadataNoteBody`.
#[must_use]
pub fn metadata_note(m: &VideoMeta, fetched_at: &str) -> String {
    let live = if m.live_broadcast_content.is_empty() {
        "none"
    } else {
        &m.live_broadcast_content
    };
    let mut fm = vec![
        format!("videoId: {}", m.video_id),
        format!("title: {}", json_string(&m.title)),
        format!("url: {}", watch_url(&m.video_id)),
        format!("channelId: {}", m.channel_id),
        format!("channelTitle: {}", json_string(&m.channel_title)),
        format!("publishedAt: {}", m.published_at),
        format!("duration: {}", m.duration),
        format!(
            "duration_seconds: {}",
            num(parse_iso8601_duration(&m.duration))
        ),
        format!("categoryId: {}", json_string(&m.category_id)),
        format!(
            "defaultLanguage: {}",
            opt_json(m.default_language.as_deref())
        ),
        format!("liveBroadcastContent: {live}"),
    ];
    if !m.tags.is_empty() {
        fm.push("tags:".into());
        fm.extend(m.tags.iter().map(|t| format!("  - {}", json_string(t))));
    }
    fm.push(format!("viewCount: {}", num(m.view_count)));
    fm.push(format!("likeCount: {}", num(m.like_count)));
    fm.push(format!("commentCount: {}", num(m.comment_count)));
    fm.push(format!("fetched_at: {fetched_at}"));
    fm.push("source_command: youtube-fetch-video-metadata".into());
    let title = if m.title.is_empty() {
        &m.video_id
    } else {
        &m.title
    };
    body(&fm, title, &m.description)
}

/// `buildYoutubeTombstoneNoteBody` (reason `deleted-or-private`): frontmatter only.
#[must_use]
pub fn tombstone_note(video_id: &str, fetched_at: &str) -> String {
    format!(
        "---\nyt-video-id: {}\nurl: {}\nstate: unavailable\nunavailable-reason: \"deleted-or-private\"\nfetched_at: {fetched_at}\nsource_command: youtube-fetch-video-metadata\n---\n",
        json_string(video_id),
        watch_url(video_id)
    )
}

/// `buildChannelAboutNoteBody`.
#[must_use]
pub fn channel_about_note(m: &ChannelMeta, fetched_at: &str) -> String {
    let mut fm = vec![
        format!("channelId: {}", m.channel_id),
        format!("title: {}", json_string(&m.title)),
        format!("url: {}", channel_url(&m.channel_id)),
    ];
    if !m.custom_url.is_empty() {
        fm.push(format!("customUrl: {}", json_string(&m.custom_url)));
    }
    fm.push(format!("publishedAt: {}", m.published_at));
    fm.push(format!("country: {}", opt_json(m.country.as_deref())));
    if let Some(t) = m.thumbnail_url.as_deref().filter(|t| !t.is_empty()) {
        fm.push(format!("thumbnail: {t}"));
    }
    fm.push(format!("subscriberCount: {}", num(m.subscriber_count)));
    fm.push(format!("videoCount: {}", num(m.video_count)));
    fm.push(format!("viewCount: {}", num(m.view_count)));
    fm.push(format!("fetched_at: {fetched_at}"));
    fm.push("source_command: youtube-fetch-channel-metadata".into());
    let title = if m.title.is_empty() {
        &m.channel_id
    } else {
        &m.title
    };
    body(&fm, title, &m.description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_note_matches_crucible_format() {
        let m = VideoMeta {
            video_id: "dQw4w9WgXcQ".into(),
            title: "A \"quoted\" title".into(),
            description: "Line 1\nLine 2".into(),
            duration: "PT1H2M3S".into(),
            channel_id: "UCabc".into(),
            channel_title: "Chan".into(),
            published_at: "2020-01-02T03:04:05Z".into(),
            tags: vec!["t1".into(), "t\\2".into()],
            category_id: "10".into(),
            default_language: None,
            live_broadcast_content: String::new(),
            view_count: Some(12),
            like_count: None,
            comment_count: Some(0),
        };
        let want = "---\nvideoId: dQw4w9WgXcQ\ntitle: \"A \\\"quoted\\\" title\"\nurl: https://www.youtube.com/watch?v=dQw4w9WgXcQ\nchannelId: UCabc\nchannelTitle: \"Chan\"\npublishedAt: 2020-01-02T03:04:05Z\nduration: PT1H2M3S\nduration_seconds: 3723\ncategoryId: \"10\"\ndefaultLanguage: null\nliveBroadcastContent: none\ntags:\n  - \"t1\"\n  - \"t\\\\2\"\nviewCount: 12\nlikeCount: null\ncommentCount: 0\nfetched_at: 2026-10-15T00:00:00.000Z\nsource_command: youtube-fetch-video-metadata\n---\n# A \"quoted\" title\n\n## Description\n\nLine 1\nLine 2\n";
        assert_eq!(metadata_note(&m, "2026-10-15T00:00:00.000Z"), want);
        let bare = VideoMeta {
            video_id: "x".into(),
            ..VideoMeta::default()
        };
        let t = metadata_note(&bare, "F");
        assert!(!t.contains("tags:") && t.contains("---\n# x\n"), "{t}");
    }

    #[test]
    fn tombstone_and_about_match_crucible() {
        assert_eq!(
            tombstone_note("abc", "F"),
            "---\nyt-video-id: \"abc\"\nurl: https://www.youtube.com/watch?v=abc\nstate: unavailable\nunavailable-reason: \"deleted-or-private\"\nfetched_at: F\nsource_command: youtube-fetch-video-metadata\n---\n"
        );
        let c = ChannelMeta {
            channel_id: "UC1".into(),
            title: "T".into(),
            description: "D".into(),
            custom_url: "@t".into(),
            published_at: "P".into(),
            country: None,
            thumbnail_url: Some("https://i/x.jpg".into()),
            subscriber_count: Some(5),
            video_count: None,
            view_count: Some(7),
        };
        assert_eq!(
            channel_about_note(&c, "F"),
            "---\nchannelId: UC1\ntitle: \"T\"\nurl: https://www.youtube.com/channel/UC1\ncustomUrl: \"@t\"\npublishedAt: P\ncountry: null\nthumbnail: https://i/x.jpg\nsubscriberCount: 5\nvideoCount: null\nviewCount: 7\nfetched_at: F\nsource_command: youtube-fetch-channel-metadata\n---\n# T\n\n## Description\n\nD\n"
        );
    }

    #[test]
    fn duration_and_slugify() {
        assert_eq!(parse_iso8601_duration("PT4M13S"), Some(253));
        assert_eq!(parse_iso8601_duration("PT2H"), Some(7200));
        assert_eq!(parse_iso8601_duration("PT0S"), Some(0));
        assert_eq!(parse_iso8601_duration("PT"), None);
        assert_eq!(parse_iso8601_duration("P1DT2H"), None);
        assert_eq!(parse_iso8601_duration(""), None);
        // WP-R10: channel slugs come from the one cv-text `slugify` (measured: no existing
        // channel folder or `sources.toml` channel changes slug).
        assert_eq!(
            cv_text::slugify("  Café Société -- Live! "),
            "cafe-societe-live"
        );
        assert_eq!(cv_text::slugify("Plain Channel 42"), "plain-channel-42");
        assert_eq!(cv_text::slugify("***"), "");
        assert_eq!(json_string("a\u{1}b"), "\"a\\u0001b\"");
    }
}
