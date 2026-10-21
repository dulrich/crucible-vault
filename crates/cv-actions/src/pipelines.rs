//! Typed `pipelines.toml` and `sources.toml` (plan `v0-automation-pipelines` WP-1, D4/D18).
//!
//! Cadences live in `schedules.toml` (user decision 2026-10-15), not here.
//!
//! Both are strict (`deny_unknown_fields`, like `actions.toml`) and default to "everything off":
//! an absent file (or section) means no pipeline runs and no source is tracked. Loaded by
//! [`crate::ConfigSet`] with the other files: hot-reloaded, and a file that fails to parse or to
//! type-check keeps its last valid version and reports the error.
//!
//! `sources.toml` carries Crucible's registry columns (`https://github.com/dulrich/obsidian-crucible/blob/master/docs/
//! tracked-sources.md`): the Markdown registry tables (`core/Tracked Channels.md`,
//! `core/Tracked Feeds.md`) were the Obsidian-induced storage and are dropped for TOML; row
//! policy (skip/ignore priorities, non-`UC` ids, non-RSS methods) stays with the trackers.

use serde::Deserialize;

/// `pipelines.toml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PipelinesConfig {
    /// `[youtube]`.
    pub youtube: YoutubePipeline,
    /// `[x]`.
    pub x: XPipeline,
    /// `[blogs]`.
    pub blogs: BlogsPipeline,
    /// `[image_describe]`.
    pub image_describe: ImageDescribePipeline,
    /// `[daily_brief]`.
    pub daily_brief: DailyBriefPipeline,
    /// `[scorer]`.
    pub scorer: ScorerConfig,
    /// `[ignore_shorts]`.
    pub ignore_shorts: IgnoreShortsConfig,
    /// `[queue]`.
    pub queue: QueuePipelineConfig,
    /// `[intake]` (plan round-5-reading-pickers-intake WP-11).
    pub intake: IntakeConfig,
    /// `[supersede]` (plan auto-intake-sprint WP-3).
    pub supersede: SupersedeConfig,
    /// `[mail]` (plan auto-intake-sprint WP-4).
    pub mail: MailPipeline,
}

impl PipelinesConfig {
    /// Parses and checks a `pipelines.toml` table.
    pub fn from_table(t: &toml::Table) -> Result<Self, String> {
        let c: Self = toml::Value::Table(t.clone())
            .try_into()
            .map_err(|e: toml::de::Error| e.message().to_owned())?;
        if c.blogs.automatable_window == 0 {
            return Err("blogs.automatable_window must be at least 1".into());
        }
        if c.blogs.complete_percent > 100 {
            return Err("blogs.complete_percent must be at most 100".into());
        }
        if c.supersede.threshold_percent > 100 {
            return Err("supersede.threshold_percent must be at most 100".into());
        }
        let pr = c.mail.processed.trim();
        if pr.is_empty() || pr.contains(['/', '\\']) || matches!(pr, "." | ".." | "new") {
            return Err(
                "mail.processed must be a plain directory name (not new, ., .. or a path)".into(),
            );
        }
        for w in &c.daily_brief.weather {
            if !(-90.0..=90.0).contains(&w.lat) || !(-180.0..=180.0).contains(&w.lon) {
                return Err(format!(
                    "daily_brief.weather {:?}: lat/lon out of range",
                    w.label
                ));
            }
        }
        Ok(c)
    }
}

/// `[youtube]`: metadata notes, channel enrich and the channel tracker.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct YoutubePipeline {
    /// Master switch (default off).
    pub enabled: bool,
    /// Metadata note root (default `_yt_metadata`, Crucible's path).
    pub metadata_root: String,
    /// Write channel `about.md` notes (default off, as in Crucible).
    pub channel_enrich: bool,
}

impl Default for YoutubePipeline {
    fn default() -> Self {
        Self {
            enabled: false,
            metadata_root: "_yt_metadata".into(),
            channel_enrich: false,
        }
    }
}

/// `[x]`: discover / metadata / backfill.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct XPipeline {
    /// Master switch (default off).
    pub enabled: bool,
    /// Metadata note root (default `_x_metadata`).
    pub metadata_root: String,
    /// Clipper inbox folder `x_discover` watches (`None` = unset).
    pub inbox: Option<String>,
}

impl Default for XPipeline {
    fn default() -> Self {
        Self {
            enabled: false,
            metadata_root: "_x_metadata".into(),
            inbox: None,
        }
    }
}

/// `[mail]`: the Maildir newsletter intake (plan auto-intake-sprint WP-4).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MailPipeline {
    /// Master switch (default off).
    pub enabled: bool,
    /// The Maildir root (outside the vault); empty = unset, so `track_mail` is a no-op.
    pub maildir: String,
    /// The directory (under the Maildir) a processed `new/` message moves to (default `cur`).
    pub processed: String,
    /// Watch `<maildir>` and `<maildir>/new` and enqueue `track-mail` once a dropped message
    /// settles (default on; plan intake-followups-mail-youtube WP-3). The schedule stays the
    /// backstop.
    pub watch: bool,
}

impl Default for MailPipeline {
    fn default() -> Self {
        Self {
            enabled: false,
            maildir: String::new(),
            processed: "cur".into(),
            watch: true,
        }
    }
}

/// `[blogs]`: the blog tracker.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BlogsPipeline {
    /// Master switch (default off).
    pub enabled: bool,
    /// `body = "auto"` sources are automatable when each of their last N posts had a full body
    /// (default 5).
    pub automatable_window: u32,
    /// A post is complete when its delivered words reach this percent of the probed full word
    /// count (default 80; plan auto-intake-sprint WP-2).
    pub complete_percent: u32,
}

impl Default for BlogsPipeline {
    fn default() -> Self {
        Self {
            enabled: false,
            automatable_window: 5,
            complete_percent: 80,
        }
    }
}

/// `[supersede]`: the intake supersede engine (plan auto-intake-sprint WP-3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SupersedeConfig {
    /// Content candidates are notes dated within this many days of the incoming item (default 21).
    pub window_days: u32,
    /// Containment percent at or above which one text counts as contained in another (default 80).
    pub threshold_percent: u32,
}

impl Default for SupersedeConfig {
    fn default() -> Self {
        Self {
            window_days: 21,
            threshold_percent: 80,
        }
    }
}

/// `[image_describe]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ImageDescribePipeline {
    /// Master switch (default off).
    pub enabled: bool,
    /// `providers.toml` provider id.
    pub provider: Option<String>,
    /// Model id.
    pub model: Option<String>,
    /// Response token cap.
    pub max_tokens: Option<u32>,
}

/// `[daily_brief]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DailyBriefPipeline {
    /// Master switch (default off).
    pub enabled: bool,
    /// Daily-note heading the brief replaces (default `# Daily Brief`).
    pub section: String,
    /// Exchange-rate pairs (Frankfurter).
    pub fx: Vec<FxPair>,
    /// Weather places (Open-Meteo).
    pub weather: Vec<WeatherPlace>,
}

impl Default for DailyBriefPipeline {
    fn default() -> Self {
        Self {
            enabled: false,
            section: "# Daily Brief".into(),
            fx: Vec::new(),
            weather: Vec::new(),
        }
    }
}

/// One `[[daily_brief.fx]]` pair.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxPair {
    /// Base currency (ISO 4217).
    pub base: String,
    /// Quote currency.
    pub quote: String,
    /// Display label (default `BASE/QUOTE`).
    #[serde(default)]
    pub label: Option<String>,
}

/// One `[[daily_brief.weather]]` place.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WeatherPlace {
    /// Display label.
    pub label: String,
    /// Latitude.
    pub lat: f64,
    /// Longitude.
    pub lon: f64,
}

/// `[scorer]`: the importance scorer (unset = stub scorer).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ScorerConfig {
    /// SystemOne-shaped scoring endpoint.
    pub url: Option<String>,
    /// SystemOne `model` (`<provider>/<model-id>`); a `url` without it falls back to the stub.
    pub model: Option<String>,
}

/// `[ignore_shorts]`: Ignore Shorts rule.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IgnoreShortsConfig {
    /// Channel ids (or names) whose short videos are ignored.
    pub channels: Vec<String>,
    /// A video at most this long counts as a short (default 120).
    pub max_seconds: u32,
}

/// `[intake]`: where tracker intake items surface.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct IntakeConfig {
    /// `lines` (default: one line per item in the daily note) or `view` (record only; the
    /// `daily-intake` view shows them).
    pub mode: IntakeMode,
}

/// `[intake] mode`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IntakeMode {
    /// Append a daily-note line per item (today's behaviour).
    #[default]
    Lines,
    /// Record the item only; write nothing to the daily note.
    View,
}

impl Default for IgnoreShortsConfig {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            max_seconds: 120,
        }
    }
}

/// `[queue]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct QueuePipelineConfig {
    /// Settled jobs older than this are pruned (default 30).
    pub retention_days: u32,
}

impl Default for QueuePipelineConfig {
    fn default() -> Self {
        Self { retention_days: 30 }
    }
}

/// Source priority (`high`, `normal`, `low`, `skip`, `ignore`; default `normal`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    High,
    #[default]
    Normal,
    Low,
    /// Excluded from runs.
    Skip,
    /// Excluded from runs.
    Ignore,
}

impl Priority {
    /// `skip`/`ignore` rows are excluded (tracked-sources.md).
    #[must_use]
    pub fn is_excluded(self) -> bool {
        matches!(self, Priority::Skip | Priority::Ignore)
    }
}

/// Blog post-id canonicalization (`auto`, `substack`, `strip-params`, `keep-params`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlogCanon {
    #[default]
    Auto,
    Substack,
    StripParams,
    KeepParams,
}

impl BlogCanon {
    /// The `cv_text::url_canon` method.
    #[must_use]
    pub fn method(self) -> cv_text::url_canon::CanonMethod {
        use cv_text::url_canon::CanonMethod as M;
        match self {
            BlogCanon::Auto => M::Auto,
            BlogCanon::Substack => M::Substack,
            BlogCanon::StripParams => M::StripParams,
            BlogCanon::KeepParams => M::KeepParams,
        }
    }
}

/// Blog body mode (`auto`, `full`, `snippet`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyMode {
    #[default]
    Auto,
    Full,
    Snippet,
}

/// `sources.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SourcesConfig {
    /// `[[youtube]]` channels.
    pub youtube: Vec<YoutubeSource>,
    /// `[[blog]]` feeds.
    pub blog: Vec<BlogSource>,
    /// `[[newsletter]]` mail senders (plan auto-intake-sprint WP-4).
    pub newsletter: Vec<NewsletterSource>,
}

impl SourcesConfig {
    /// Parses a `sources.toml` table.
    pub fn from_table(t: &toml::Table) -> Result<Self, String> {
        let c: Self = toml::Value::Table(t.clone())
            .try_into()
            .map_err(|e: toml::de::Error| e.message().to_owned())?;
        for n in &c.newsletter {
            if n.from.iter().all(|f| f.trim().is_empty())
                && n.list_id.as_deref().is_none_or(|l| l.trim().is_empty())
            {
                return Err(format!(
                    "newsletter {:?}: needs a `from` or a `list_id` matcher",
                    n.name
                ));
            }
            if let Some(b) = n.blog.as_deref().filter(|b| !b.trim().is_empty())
                && !c.blog.iter().any(|r| r.name == b)
            {
                return Err(format!(
                    "newsletter {:?}: blog {b:?} names no [[blog]] row",
                    n.name
                ));
            }
        }
        Ok(c)
    }
}

/// One tracked YouTube channel.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct YoutubeSource {
    /// Display name.
    pub channel: String,
    /// Channel id (`UC…`).
    pub id: String,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Priority.
    #[serde(default)]
    pub priority: Priority,
}

/// One tracked blog feed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlogSource {
    /// Display name.
    pub name: String,
    /// RSS/Atom feed URL.
    pub link: String,
    /// Fetch method (`rss`; other values are skipped by the tracker, as in Crucible).
    #[serde(default = "rss")]
    pub method: String,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Priority.
    #[serde(default)]
    pub priority: Priority,
    /// Post-id canonicalization.
    #[serde(default)]
    pub canon: BlogCanon,
    /// Body mode.
    #[serde(default)]
    pub body: BodyMode,
    /// Body phrases (case-insensitive) marking a post incomplete (plan auto-intake-sprint WP-2).
    #[serde(default)]
    pub paywall_markers: Vec<String>,
}

fn rss() -> String {
    "rss".into()
}

/// One `[[newsletter]]` mail sender (plan auto-intake-sprint WP-4): matched by `from` (an exact
/// address, or `@domain` suffix) or `list_id`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewsletterSource {
    /// Display name (also the `tracker_items.source_key` of its mails).
    pub name: String,
    /// Sender matchers, case-insensitive: `a@b.example` or `@b.example`.
    #[serde(default)]
    pub from: Vec<String>,
    /// `List-Id` matcher.
    #[serde(default)]
    pub list_id: Option<String>,
    /// The name of the `[[blog]]` row this newsletter mirrors (access evidence and dedup source).
    #[serde(default)]
    pub blog: Option<String>,
    /// The `kinds.toml` kind its mails are filed as (default `blog`).
    #[serde(default = "blog_kind")]
    pub kind: String,
    /// Extra tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Body phrases (case-insensitive) marking an email incomplete (united with the linked blog row's).
    #[serde(default)]
    pub paywall_markers: Vec<String>,
}

fn blog_kind() -> String {
    "blog".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newsletter_rows_and_mail_config_are_validated() {
        let src = |extra: &str| {
            let t: toml::Table = format!(
                "[[blog]]\nname = \"B\"\nlink = \"https://b.example/feed\"\n[[newsletter]]\nname = \"N\"\n{extra}"
            )
            .parse()
            .unwrap();
            SourcesConfig::from_table(&t)
        };
        assert!(src("from = [\"@b.example\"]\nblog = \"B\"").is_ok());
        assert!(src("list_id = \"l.b.example\"").is_ok());
        assert!(src("blog = \"B\"").unwrap_err().contains("matcher"));
        assert!(
            src("from = [\"a@b.example\"]\nblog = \"Nope\"")
                .unwrap_err()
                .contains("names no [[blog]]")
        );
        let mail = |m: &str| PipelinesConfig::from_table(&format!("[mail]\n{m}").parse().unwrap());
        assert_eq!(mail("").unwrap().mail.processed, "cur");
        for bad in ["new", "..", "a/b", ""] {
            assert!(mail(&format!("processed = {bad:?}")).is_err(), "{bad}");
        }
    }

    #[test]
    fn empty_tables_mean_everything_off() {
        let p = PipelinesConfig::from_table(&toml::Table::new()).unwrap();
        assert!(!p.youtube.enabled && !p.x.enabled && !p.blogs.enabled);
        assert!(!p.image_describe.enabled && !p.daily_brief.enabled);
        assert_eq!(p.youtube.metadata_root, "_yt_metadata");
        assert_eq!(p.x.metadata_root, "_x_metadata");
        assert_eq!(p.blogs.automatable_window, 5);
        assert_eq!(p.blogs.complete_percent, 80);
        assert_eq!(p.supersede.window_days, 21);
        assert_eq!(p.supersede.threshold_percent, 80);
        assert_eq!(p.daily_brief.section, "# Daily Brief");
        assert_eq!(p.ignore_shorts.max_seconds, 120);
        assert_eq!(p.queue.retention_days, 30);
        assert_eq!(p.intake.mode, IntakeMode::Lines);
        let t: toml::Table = toml::from_str("[intake]\nmode = \"view\"\n").unwrap();
        assert_eq!(
            PipelinesConfig::from_table(&t).unwrap().intake.mode,
            IntakeMode::View
        );
        let t: toml::Table = toml::from_str("[intake]\nmode = \"side\"\n").unwrap();
        assert!(PipelinesConfig::from_table(&t).is_err());
        assert_eq!(
            SourcesConfig::from_table(&toml::Table::new()).unwrap(),
            SourcesConfig::default()
        );
    }

    #[test]
    fn full_files_parse_and_strictness_holds() {
        let p: toml::Table = r#"
[youtube]
enabled = true
channel_enrich = true
[x]
enabled = true
inbox = "Clippings"
[blogs]
enabled = true
automatable_window = 3
[image_describe]
enabled = true
provider = "local"
model = "vl"
max_tokens = 800
[daily_brief]
enabled = true
fx = [{ base = "EUR", quote = "USD" }]
weather = [{ label = "Home", lat = 52.5, lon = 13.4 }]
[scorer]
url = "http://127.0.0.1:9000"
model = "local/scorer"
[ignore_shorts]
channels = ["UCx"]
max_seconds = 90
[queue]
retention_days = 14
"#
        .parse()
        .unwrap();
        let c = PipelinesConfig::from_table(&p).unwrap();
        assert!(c.youtube.channel_enrich);
        assert_eq!(c.daily_brief.weather[0].lat, 52.5);
        assert_eq!(c.blogs.automatable_window, 3);
        assert_eq!(c.scorer.model.as_deref(), Some("local/scorer"));
        for bad in [
            "[blogs]\nschedule = \"soon\"",
            "[youtube]\nschedule = \"3h\"",
            "[blogs]\nautomatable_window = 0",
        ] {
            let bad: toml::Table = bad.parse().unwrap();
            assert!(PipelinesConfig::from_table(&bad).is_err(), "{bad:?}");
        }
        let bad: toml::Table = "[youtube]\nbogus = 1".parse().unwrap();
        assert!(
            PipelinesConfig::from_table(&bad)
                .unwrap_err()
                .contains("bogus")
        );

        let s: toml::Table = r#"
[[youtube]]
channel = "Chan"
id = "UCabc"
tags = ["ai"]
priority = "high"
[[blog]]
name = "B"
link = "https://b.example/feed"
canon = "strip-params"
body = "full"
"#
        .parse()
        .unwrap();
        let c = SourcesConfig::from_table(&s).unwrap();
        assert_eq!(c.youtube[0].priority, Priority::High);
        assert_eq!(c.blog[0].method, "rss");
        assert_eq!(c.blog[0].priority, Priority::Normal);
        assert_eq!(c.blog[0].canon, BlogCanon::StripParams);
        assert_eq!(c.blog[0].body, BodyMode::Full);
        let bad: toml::Table = "[[blog]]\nname = \"x\"".parse().unwrap();
        assert!(SourcesConfig::from_table(&bad).is_err(), "link required");
    }
}
