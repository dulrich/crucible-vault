//! Tracker helpers (plan `v0-automation-pipelines` WP-7): the uploads playlist id, the Ignore
//! Shorts predicate and the intake line title. Pure: callers read files and fetch.

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
