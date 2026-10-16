//! Tracker helpers (plan `v0-automation-pipelines` WP-7): seen-set id extraction, the uploads
//! playlist id and the Ignore Shorts predicate. Pure: callers read files and fetch.

use std::collections::BTreeSet;

use cv_text::url_canon::extract_video_id_from_url;

/// Whether `s` is a bare 11-char YouTube video id.
fn is_video_id(s: &str) -> bool {
    s.len() == 11
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_sep(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '[' | ']' | '"' | '\'')
}

/// Every YouTube video id in `text`: ids of YouTube URLs anywhere (Crucible intake notes under
/// `_crucible/orchestration/youtube/new-videos`, `utils/feedSources.ts:101`) plus bare ids listed
/// as `- <id>` bullets under a `## Videos` heading (Crucible `_crucible/orchestration/ignored.md`,
/// `utils/ignoredIds.ts:12`).
#[must_use]
pub fn youtube_ids_in_text(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut in_videos = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            in_videos = t == "## Videos";
            continue;
        }
        if in_videos
            && let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))
            && is_video_id(rest.trim())
        {
            out.insert(rest.trim().to_owned());
        }
        for tok in t.split(is_sep) {
            if tok.contains("youtu")
                && let Some(id) = extract_video_id_from_url(tok)
            {
                out.insert(id);
            }
        }
    }
    out
}

/// An item title made safe for a `[title](url)` intake line: one line, brackets escaped.
#[must_use]
pub fn line_title(title: &str) -> String {
    let one = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = one.replace('[', "\\[").replace(']', "\\]");
    if t.is_empty() { "(untitled)".into() } else { t }
}

/// The uploads playlist of a channel (`UC…` → `UU…`, `youtubeApi.ts:340`); `None` for a
/// non-`UC` id.
#[must_use]
pub fn uploads_playlist_id(channel_id: &str) -> Option<String> {
    let rest = channel_id.strip_prefix("UC")?;
    (!rest.is_empty()).then(|| format!("UU{rest}"))
}

/// The Ignore Shorts predicate (WP-7 tracker, WP-9 `ignore_video`): the video's channel is in
/// `channels` (an entry equal to `channel_id`, or equal case-insensitively to any of
/// `channel_names`) and its duration is known and strictly below `max_seconds`.
#[must_use]
pub fn is_ignored_short(
    channel_id: &str,
    channel_names: &[&str],
    duration_seconds: Option<i64>,
    channels: &[String],
    max_seconds: u32,
) -> bool {
    let Some(d) = duration_seconds else {
        return false;
    };
    if d >= i64::from(max_seconds) {
        return false;
    }
    channels.iter().map(|c| c.trim()).any(|c| {
        !c.is_empty()
            && (c == channel_id
                || channel_names
                    .iter()
                    .any(|n| n.trim().eq_ignore_ascii_case(c)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_from_urls_and_ignored_list() {
        let t = "# New videos\n- [ ] [A](https://www.youtube.com/watch?v=aaaaaaaaaaa) x\nhttps://youtu.be/bbbbbbbbbbb\n## Videos\n- ccccccccccc\n- not-an-id\n## Blogs\n- ddddddddddd\n";
        let ids: Vec<_> = youtube_ids_in_text(t).into_iter().collect();
        assert_eq!(ids, ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"]);
    }

    #[test]
    fn uploads_and_shorts() {
        assert_eq!(uploads_playlist_id("UCabc").as_deref(), Some("UUabc"));
        assert_eq!(uploads_playlist_id("HCabc"), None);
        let ch = vec!["UCa".to_owned(), "Some Name".to_owned()];
        assert!(is_ignored_short("UCa", &[], Some(30), &ch, 120));
        assert!(!is_ignored_short("UCa", &[], Some(120), &ch, 120));
        assert!(!is_ignored_short("UCa", &[], None, &ch, 120));
        assert!(is_ignored_short("UCz", &["some name"], Some(5), &ch, 120));
        assert!(!is_ignored_short("UCz", &["other"], Some(5), &ch, 120));
    }
}
