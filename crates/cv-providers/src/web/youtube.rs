//! YouTube Data API v3 client (plan `v0-automation-pipelines` WP-4): `videos.list`,
//! `channels.list`, `playlistItems.list` over a shared [`WebClient`].
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/youtubeApi.ts` `requestYoutubeApi`
//! (`:120-156`): 429/5xx/network → `Transient`, 404 → `NotFound`, a 403 whose
//! `error.errors[0].reason` (or `error.message`) contains `quota` → [`FetchError::Quota`]
//! ([`classify`]); any other 403 → `Permanent` (bad key / API not enabled). Obsidian's
//! `requestUrl` is the dropped part. Every successful call costs one quota unit; callers record
//! it in `api_usage` under [`SERVICE`]. The key travels in the query string and never reaches
//! errors or logs ([`super::redact_url`]).

use std::time::Duration;

use serde::Deserialize;

use super::{FetchError, FetchRequest, WebClient, decode_json};

/// Breaker / `api_usage` key (Crucible `youtube-api`, `youtubeApi.ts:90`).
pub const SERVICE: &str = "youtube-api";
/// The real API base.
pub const DEFAULT_BASE: &str = "https://www.googleapis.com/youtube/v3";
/// `videos.list` / `channels.list` id batch limit.
pub const MAX_IDS: usize = 50;

/// Counts arrive as decimal strings.
fn count<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let v: Option<serde_json::Value> = Option::deserialize(d)?;
    Ok(match v {
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        _ => None,
    })
}

#[derive(Debug, Deserialize)]
struct List<T> {
    #[serde(default = "Vec::new")]
    items: Vec<T>,
}

/// `videos.list` item (the parts this crate requests).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Video {
    pub id: String,
    pub snippet: VideoSnippet,
    pub content_details: VideoContentDetails,
    pub statistics: VideoStatistics,
}

/// `videos.list` `snippet`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoSnippet {
    pub title: String,
    pub description: String,
    pub channel_id: String,
    pub channel_title: String,
    pub published_at: String,
    pub tags: Vec<String>,
    pub category_id: String,
    pub default_language: Option<String>,
    pub live_broadcast_content: String,
}

/// `videos.list` `contentDetails`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct VideoContentDetails {
    /// ISO-8601 (`PT4M13S`).
    pub duration: String,
}

/// `videos.list` `statistics`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VideoStatistics {
    #[serde(deserialize_with = "count")]
    pub view_count: Option<i64>,
    #[serde(deserialize_with = "count")]
    pub like_count: Option<i64>,
    #[serde(deserialize_with = "count")]
    pub comment_count: Option<i64>,
}

/// `channels.list` item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Channel {
    pub id: String,
    pub snippet: ChannelSnippet,
    pub statistics: ChannelStatistics,
}

/// `channels.list` `snippet`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChannelSnippet {
    pub title: String,
    pub description: String,
    pub custom_url: String,
    pub published_at: String,
    pub country: Option<String>,
    pub thumbnails: Thumbnails,
}

/// Thumbnail set (only the sizes Crucible reads).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Thumbnails {
    pub default: Option<Thumbnail>,
    pub medium: Option<Thumbnail>,
    pub high: Option<Thumbnail>,
}

impl Thumbnails {
    /// Crucible's pick: high, then medium, then default.
    #[must_use]
    pub fn best(&self) -> Option<&str> {
        [&self.high, &self.medium, &self.default]
            .into_iter()
            .flatten()
            .map(|t| t.url.as_str())
            .find(|u| !u.is_empty())
    }
}

impl From<Video> for cv_ingest::youtube::VideoMeta {
    /// The metadata note's fields (`video_id` is the item's `id`).
    fn from(v: Video) -> Self {
        Self {
            video_id: v.id,
            title: v.snippet.title,
            description: v.snippet.description,
            duration: v.content_details.duration,
            channel_id: v.snippet.channel_id,
            channel_title: v.snippet.channel_title,
            published_at: v.snippet.published_at,
            tags: v.snippet.tags,
            category_id: v.snippet.category_id,
            default_language: v.snippet.default_language,
            live_broadcast_content: v.snippet.live_broadcast_content,
            view_count: v.statistics.view_count,
            like_count: v.statistics.like_count,
            comment_count: v.statistics.comment_count,
        }
    }
}

impl From<Channel> for cv_ingest::youtube::ChannelMeta {
    /// The about note's fields (thumbnail = [`Thumbnails::best`]).
    fn from(ch: Channel) -> Self {
        Self {
            channel_id: ch.id,
            thumbnail_url: ch.snippet.thumbnails.best().map(str::to_owned),
            title: ch.snippet.title,
            description: ch.snippet.description,
            custom_url: ch.snippet.custom_url,
            published_at: ch.snippet.published_at,
            country: ch.snippet.country,
            subscriber_count: ch.statistics.subscriber_count,
            video_count: ch.statistics.video_count,
            view_count: ch.statistics.view_count,
        }
    }
}

/// One thumbnail.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Thumbnail {
    pub url: String,
}

/// `channels.list` `statistics`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChannelStatistics {
    #[serde(deserialize_with = "count")]
    pub subscriber_count: Option<i64>,
    #[serde(deserialize_with = "count")]
    pub video_count: Option<i64>,
    #[serde(deserialize_with = "count")]
    pub view_count: Option<i64>,
}

/// `playlistItems.list` item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaylistItem {
    pub snippet: PlaylistSnippet,
    pub content_details: PlaylistContentDetails,
}

/// `playlistItems.list` `snippet`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaylistSnippet {
    pub title: String,
    pub description: String,
    pub channel_id: String,
    pub channel_title: String,
    pub published_at: String,
}

/// `playlistItems.list` `contentDetails`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaylistContentDetails {
    pub video_id: String,
    pub video_published_at: Option<String>,
}

/// The quota-403 [`super::Classifier`] (`requestYoutubeApi :138-142` + `extractApiErrorReason
/// :876-887`): only a 403 naming `quota` overrides the default classification.
#[must_use]
pub fn classify(
    service: &str,
    status: u16,
    _retry_after: Option<Duration>,
    body: &[u8],
) -> Option<FetchError> {
    if status != 403 {
        return None;
    }
    let reason = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let e = v.get("error")?;
            e.pointer("/errors/0/reason")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| e.get("message").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned());
    reason
        .to_ascii_lowercase()
        .contains("quota")
        .then(|| FetchError::Quota {
            service: service.to_owned(),
            retry_after: None,
        })
}

/// The Data API endpoint set. Tests point `base` at a local server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YoutubeApi {
    base: String,
}

impl Default for YoutubeApi {
    fn default() -> Self {
        Self::new(DEFAULT_BASE)
    }
}

impl YoutubeApi {
    /// An API rooted at `base` (no trailing slash needed).
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
        }
    }

    async fn list<T: serde::de::DeserializeOwned>(
        &self,
        client: &WebClient,
        endpoint: &str,
        params: &[(&str, &str)],
        key: &str,
    ) -> Result<Vec<T>, FetchError> {
        let mut all: Vec<(&str, &str)> = params.to_vec();
        all.push(("key", key));
        let url = reqwest::Url::parse_with_params(&format!("{}/{endpoint}", self.base), &all)
            .map_err(|e| FetchError::Malformed {
                service: SERVICE.to_owned(),
                detail: format!("invalid url: {e}"),
            })?;
        let url = url.to_string();
        let mut req = FetchRequest::get(SERVICE, &url);
        req.classify = Some(classify);
        let body = client.fetch(req).await?;
        Ok(decode_json::<List<T>>(SERVICE, &body)?.items)
    }

    /// `videos.list` (`part=snippet,contentDetails,statistics`) for ≤ [`MAX_IDS`] ids: one unit.
    /// A missing/private video is simply absent from the result (the live not-found shape).
    pub async fn videos(
        &self,
        client: &WebClient,
        key: &str,
        ids: &[&str],
    ) -> Result<Vec<Video>, FetchError> {
        let ids = ids[..ids.len().min(MAX_IDS)].join(",");
        let p = [("part", "snippet,contentDetails,statistics"), ("id", &ids)];
        self.list(client, "videos", &p, key).await
    }

    /// `channels.list` (`part=snippet,statistics`) for ≤ [`MAX_IDS`] ids: one unit.
    pub async fn channels(
        &self,
        client: &WebClient,
        key: &str,
        ids: &[&str],
    ) -> Result<Vec<Channel>, FetchError> {
        let ids = ids[..ids.len().min(MAX_IDS)].join(",");
        let p = [("part", "snippet,statistics"), ("id", &ids)];
        self.list(client, "channels", &p, key).await
    }

    /// `playlistItems.list` (`part=snippet,contentDetails`), first page only (≤ 50): one unit.
    pub async fn playlist_items(
        &self,
        client: &WebClient,
        key: &str,
        playlist_id: &str,
        max_results: u32,
    ) -> Result<Vec<PlaylistItem>, FetchError> {
        let max = max_results.clamp(1, 50).to_string();
        let p = [
            ("part", "snippet,contentDetails"),
            ("playlistId", playlist_id),
            ("maxResults", &max),
        ];
        self.list(client, "playlistItems", &p, key).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::WebConfig;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serves `(status, body)` for the first path segment after the base, echoing nothing.
    async fn serve(routes: Vec<(&'static str, u16, &'static str)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let routes = routes.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = sock.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let head = String::from_utf8_lossy(&buf).into_owned();
                    let path = head.split(' ').nth(1).unwrap_or("/");
                    let (status, body) = routes
                        .iter()
                        .find(|(p, _, _)| path.starts_with(p))
                        .map_or((500, ""), |(_, s, b)| (*s, *b));
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn client() -> WebClient {
        WebClient::new(WebConfig::default()).unwrap()
    }

    const VIDEOS: &str = r#"{"items":[{"id":"v1","snippet":{"title":"T","channelId":"UC1","channelTitle":"C","tags":["a"],"categoryId":"10","liveBroadcastContent":"none"},"contentDetails":{"duration":"PT1M"},"statistics":{"viewCount":"123","likeCount":"4"}}]}"#;
    const CHANNELS: &str = r#"{"items":[{"id":"UC1","snippet":{"title":"C","customUrl":"@c","thumbnails":{"default":{"url":"d"},"high":{"url":"h"}}},"statistics":{"subscriberCount":"9","videoCount":"2"}}]}"#;
    const PLAYLIST: &str = r#"{"items":[{"snippet":{"title":"P","channelId":"UC1","publishedAt":"2026-01-01T00:00:00Z"},"contentDetails":{"videoId":"v9","videoPublishedAt":"2026-01-01T00:00:00Z"}}]}"#;

    #[tokio::test]
    async fn decodes_videos_channels_and_playlist_items() {
        let base = serve(vec![
            ("/videos", 200, VIDEOS),
            ("/channels", 200, CHANNELS),
            ("/playlistItems", 200, PLAYLIST),
        ])
        .await;
        let (api, c) = (YoutubeApi::new(base), client());
        let v = api.videos(&c, "K", &["v1"]).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].statistics.view_count, Some(123));
        assert_eq!(v[0].statistics.comment_count, None);
        assert_eq!(v[0].content_details.duration, "PT1M");
        assert_eq!(v[0].snippet.default_language, None);
        let ch = api.channels(&c, "K", &["UC1"]).await.unwrap();
        assert_eq!(ch[0].snippet.thumbnails.best(), Some("h"));
        assert_eq!(ch[0].statistics.subscriber_count, Some(9));
        let p = api.playlist_items(&c, "K", "UU1", 50).await.unwrap();
        assert_eq!(p[0].content_details.video_id, "v9");
    }

    #[tokio::test]
    async fn quota_403_is_quota_other_403_is_permanent_and_key_is_redacted() {
        let base = serve(vec![
            (
                "/videos",
                403,
                r#"{"error":{"errors":[{"reason":"quotaExceeded"}],"message":"x"}}"#,
            ),
            (
                "/channels",
                403,
                r#"{"error":{"errors":[{"reason":"forbidden"}]}}"#,
            ),
            ("/playlistItems", 404, ""),
        ])
        .await;
        let (api, c) = (YoutubeApi::new(base), client());
        let e = api.videos(&c, "SECRETKEY", &["v1"]).await.unwrap_err();
        assert!(matches!(e, FetchError::Quota { .. }), "{e:?}");
        let e = api.channels(&c, "SECRETKEY", &["UC1"]).await.unwrap_err();
        assert!(
            matches!(e, FetchError::Permanent { status: 403, .. }),
            "{e:?}"
        );
        assert!(!format!("{e:?}{e}").contains("SECRETKEY"));
        let e = api.playlist_items(&c, "K", "UU1", 5).await.unwrap_err();
        assert!(matches!(e, FetchError::NotFound { .. }), "{e:?}");
        assert!(
            classify(
                SERVICE,
                403,
                None,
                br#"{"error":{"message":"Daily Quota hit"}}"#
            )
            .is_some()
        );
    }
}
