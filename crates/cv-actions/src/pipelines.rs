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
    /// The kind `auto_file` files a YouTube clip as when its transcript reaches
    /// `MIN_REFINE_WORDS` (default `transcript`; plan round-19-yt-cleanup-intake WP-2).
    pub long_kind: String,
    /// The kind for a shorter (or Short's) transcript (default `post`).
    pub short_kind: String,
}

impl Default for YoutubePipeline {
    fn default() -> Self {
        Self {
            enabled: false,
            metadata_root: "_yt_metadata".into(),
            channel_enrich: false,
            long_kind: "transcript".into(),
            short_kind: "post".into(),
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
    /// An HTTP scoring endpoint returning `{"score": n}`.
    pub url: Option<String>,
    /// The scoring endpoint's `model` (`<provider>/<model-id>`); a `url` without it falls back
    /// to the stub.
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
    /// `[[serial]]` works (plan round-11-feedback-todos Decision 4; replaces the `core/works`
    /// notes).
    pub serial: Vec<SerialSource>,
}

impl SourcesConfig {
    /// Parses a `sources.toml` table.
    pub fn from_table(t: &toml::Table) -> Result<Self, String> {
        let c: Self = toml::Value::Table(t.clone())
            .try_into()
            .map_err(|e: toml::de::Error| e.message().to_owned())?;
        for (i, s) in c.serial.iter().enumerate() {
            if s.name.trim().is_empty() {
                return Err(format!("serial #{}: needs a non-empty `name`", i + 1));
            }
            if c.serial[..i].iter().any(|o| o.name.trim() == s.name.trim()) {
                return Err(format!("serial {:?}: duplicate name", s.name));
            }
        }
        for n in &c.newsletter {
            if n.from.iter().all(|f| f.trim().is_empty())
                && n.list_id.as_deref().is_none_or(|l| l.trim().is_empty())
                && n.hosts.iter().all(|h| h.trim().is_empty())
            {
                return Err(format!(
                    "newsletter {:?}: needs a `from`, `list_id` or `hosts` matcher",
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
    /// Publication name shown in the Author column (default: `channel`; plan intake-round-2 WP-3).
    #[serde(default)]
    pub short: Option<String>,
    /// Channel id (`UC…`).
    pub id: String,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Priority.
    #[serde(default)]
    pub priority: Priority,
    /// Listed in the Works view (plan round-11-feedback-todos Decision 5).
    #[serde(default)]
    pub works: bool,
}

/// One tracked blog feed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlogSource {
    /// Display name.
    pub name: String,
    /// Publication name shown in the Author column (default: `name`; plan intake-round-2 WP-3).
    #[serde(default)]
    pub short: Option<String>,
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
    /// Listed in the Works view (plan round-11-feedback-todos Decision 5).
    #[serde(default)]
    pub works: bool,
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
    /// Publication name shown in the Author column (default: `name`; plan intake-round-2 WP-3).
    #[serde(default)]
    pub short: Option<String>,
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
    /// Clip hosts (kinds.toml `hosts` forms) whose clips file as this row's `kind` with its
    /// publication (plan round-11-feedback-todos Decision 7); a hosts-only row needs no mail
    /// matcher.
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Listed in the Works view (plan round-11-feedback-todos Decision 5).
    #[serde(default)]
    pub works: bool,
}

fn blog_kind() -> String {
    "blog".into()
}

/// A `[[serial]]` row's status (a view concern; filing ignores it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SerialStatus {
    #[default]
    Active,
    Complete,
    Ignored,
}

/// One `[[serial]]` work (plan round-11-feedback-todos Decision 4): a clip matches it by
/// `prefix`, else by `author` ([`SerialSource::matches`]) and files as its `kind` with its
/// publication.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialSource {
    /// Display name (unique among serial rows).
    pub name: String,
    /// Publication name (default: `name`).
    #[serde(default)]
    pub short: Option<String>,
    /// The bare author name a clip's `author` list must contain.
    #[serde(default)]
    pub author: Option<String>,
    /// Source-URL prefixes; some non-empty one must start the clip's `source`.
    #[serde(default)]
    pub prefix: Vec<String>,
    /// The `kinds.toml` kind its clips file as (no default, plan public-release WP-4 C#30;
    /// absent or blank = never filed: `auto_file` skips the row with a warning; the Works view
    /// still lists it).
    #[serde(default)]
    pub kind: String,
    /// `active`, `complete` or `ignored`.
    #[serde(default)]
    pub status: SerialStatus,
    /// Listed in the Works view (every serial row is; accepted for uniformity, Decision 5).
    #[serde(default)]
    pub works: bool,
}

/// An author for comparison: trimmed, a `[[…]]` wrapper stripped (a link's `|alias` and
/// `#heading` dropped, so the link target names the author), `#` removed, trimmed again.
fn author_key(a: &str) -> String {
    let t = a.trim();
    // Only a wrapped entry is a link; a bare author keeps its `#` handling (`C#` → `C`).
    let inner = if t.starts_with("[[") && t.ends_with("]]") {
        cv_text::wikilink::target(t)
    } else {
        t
    };
    inner.replace('#', "").trim().to_owned()
}

impl SerialSource {
    /// The `publication` value for this work's notes (`short`, else `name`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.name)
    }

    /// Whether a note with these `author` entries and `source` belongs to this work (plan
    /// round-12-feedback-todos Decision 7): a row with any non-blank (trimmed) prefix matches by
    /// prefix alone — some prefix must start the trimmed `source`, the author is ignored; a row
    /// with only an author matches when it equals one of the note's authors (both normalized by
    /// [`author_key`]); a row with neither matches nothing. `status` does not affect matching.
    #[must_use]
    pub fn matches(&self, authors: &[&str], source: &str) -> bool {
        let want = self
            .author
            .as_deref()
            .map(author_key)
            .filter(|a| !a.is_empty());
        let prefixes: Vec<&str> = self
            .prefix
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect();
        if !prefixes.is_empty() {
            let source = source.trim();
            return prefixes.iter().any(|p| source.starts_with(p));
        }
        want.is_some_and(|w| authors.iter().any(|c| author_key(c) == w))
    }
}

/// The `publication` frontmatter value of a source row: the trimmed `short` when non-empty, else
/// `name` (`channel` for YouTube). Plan intake-round-2-clean-dedupe-reading WP-3.
#[must_use]
pub fn publication_of<'a>(short: Option<&'a str>, name: &'a str) -> &'a str {
    match short.map(str::trim) {
        Some(s) if !s.is_empty() => s,
        _ => name,
    }
}

impl YoutubeSource {
    /// The `publication` value for this channel's notes (`short`, else `channel`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.channel)
    }
}

impl BlogSource {
    /// The `publication` value for this feed's notes (`short`, else `name`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.name)
    }
}

impl NewsletterSource {
    /// The `publication` value for this sender's notes (`short`, else `name`).
    #[must_use]
    pub fn publication(&self) -> &str {
        publication_of(self.short.as_deref(), &self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `author_key` is `wikilink::target` plus author-specific `#` removal, and also covers a
    /// bare (unbracketed) author where `target` would cut at the `#`.
    #[test]
    fn author_key_differs_from_wikilink_target_for_bare_authors() {
        assert_eq!(author_key("[[A#b]]"), cv_text::wikilink::target("[[A#b]]"));
        assert_eq!(author_key(" [[A|x]] "), "A");
        assert_eq!(author_key("C#"), "C");
        assert_eq!(cv_text::wikilink::target("C#"), "C");
        assert_eq!(author_key("#tag"), "tag");
        assert_eq!(cv_text::wikilink::target("#tag"), "");
        assert_eq!(author_key("A#b"), "Ab");
        assert_ne!(author_key("A#b"), cv_text::wikilink::target("A#b"));
    }

    /// Plan round-11-feedback-todos WP-4: deploy migration 0007's output (its test fixture's
    /// expected `sources.toml`) parses, with the migrated serial rows intact.
    #[test]
    fn migration_0007_output_parses() {
        let text = r#"[[blog]]
name = "Some Blog"
link = "https://blog.example/feed"

[[newsletter]]
name = "Letter"
from = ["@letter.example"]
works = true

[[youtube]]
channel = "Chan"
id = "UC0"

[[serial]]
name = "Alpha Saga"
short = "Alpha Saga"
author = "Ann Writer"
prefix = ["https://a.example/s/1", "https://a.example/s/1b"]
kind = "fanfic"
status = "active"

[[serial]]
name = "Beta Tale"
short = "Beta Tale"
author = "Bob"
prefix = ["https://b.example/feed/", "https://b2.example/"]
kind = "blog"

[[serial]]
name = "Delta Empty"
short = "Delta Empty"
kind = "fanfic"

[[serial]]
name = "Gamma"
short = "Gamma"
prefix = ["https://g.example/x"]
kind = "fanfic"
status = "complete"

[[serial]]
name = "Theta Shared"
short = "Beta Tale"
author = "Tess"
prefix = ["https://t.example/s/9"]
kind = "fanfic"

[[serial]]
name = "Zeta"
short = "Zeta \"Q\" \\ T"
author = "Zed"
kind = "fanfic"
status = "ignored"
"#;
        let c = SourcesConfig::from_table(&text.parse::<toml::Table>().unwrap()).unwrap();
        let names: Vec<&str> = c.serial.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Alpha Saga",
                "Beta Tale",
                "Delta Empty",
                "Gamma",
                "Theta Shared",
                "Zeta"
            ]
        );
        // `name` is the filename stem; `short` is the title, which two Works may share.
        assert_eq!(c.serial[4].publication(), c.serial[1].publication());
        assert_eq!(c.serial[5].publication(), "Zeta \"Q\" \\ T");
        let a = &c.serial[0];
        assert_eq!(a.author.as_deref(), Some("Ann Writer"));
        assert_eq!(a.prefix.len(), 2);
        assert!(a.matches(&["[[Ann Writer]]"], "https://a.example/s/1b/ch-2"));
        assert_eq!(c.serial[1].kind, "blog");
        assert_eq!(c.serial[3].status, SerialStatus::Complete);
        assert_eq!(c.serial[5].status, SerialStatus::Ignored);
        assert!(!c.serial[2].matches(&["x"], "https://x"));
    }

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
        // A hosts-only row (beehiiv) validates; blank hosts are no matcher.
        assert!(src("hosts = [\"n.example\"]").is_ok());
        assert!(src("hosts = [\" \"]").unwrap_err().contains("matcher"));
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

    fn serial(author: Option<&str>, prefix: &[&str]) -> SerialSource {
        SerialSource {
            name: "W".into(),
            short: None,
            author: author.map(str::to_owned),
            prefix: prefix.iter().map(|p| (*p).to_owned()).collect(),
            kind: "fanfic".into(),
            status: SerialStatus::Active,
            works: false,
        }
    }

    #[test]
    fn serial_matches_carries_the_works_match_cases() {
        let src = "https://ff.example/s/1/2";
        // Neither filter (or blank filters) matches nothing.
        assert!(!serial(None, &[]).matches(&["Ann"], src));
        assert!(!serial(Some("  "), &[" "]).matches(&["Ann"], src));
        // Author only.
        assert!(serial(Some("#Ann"), &[]).matches(&[" Ann "], src));
        assert!(!serial(Some("Ann"), &[]).matches(&["Bob"], src));
        // Prefix only (trimmed both sides).
        assert!(serial(None, &[" https://ff.example/s/1 "]).matches(&[], &format!(" {src}")));
        assert!(!serial(None, &["https://ff.example/s/9"]).matches(&[], src));
        // A prefix decides alone: the author is ignored either way.
        let both = serial(Some("Ann"), &["https://ff.example/s/1"]);
        assert!(both.matches(&["Ann"], src));
        assert!(both.matches(&["Bob"], src));
        assert!(both.matches(&[], src));
        assert!(!both.matches(&["Ann"], "https://other.example/"));
        // Link authors compare by target; a bracketed row author is bare-compared too.
        for a in ["[[Ann]]", "[[Ann|A]]", "[[Ann#Bio]]", " [[ Ann ]] "] {
            assert!(serial(Some("Ann"), &[]).matches(&[a], src), "{a}");
        }
        assert!(serial(Some("[[Ann]]"), &[]).matches(&["Ann"], src));
        assert!(!serial(Some("Ann"), &[]).matches(&["[[Annie]]"], src));
    }

    #[test]
    fn serial_rows_parse_validate_and_reject_bad_status() {
        let parse = |s: &str| SourcesConfig::from_table(&s.parse::<toml::Table>().unwrap());
        let c = parse("[[serial]]\nname = \"W\"\nauthor = \"Ann\"\n[[serial]]\nname = \"X\"\nshort = \"XX\"\nkind = \"book\"\nstatus = \"complete\"\nworks = true\nprefix = [\"https://x.example/\"]\n").unwrap();
        assert_eq!(
            c.serial[0].kind, "",
            "no default kind (plan public-release WP-4)"
        );
        assert_eq!(c.serial[0].status, SerialStatus::Active);
        assert_eq!(c.serial[0].publication(), "W");
        assert_eq!(c.serial[1].publication(), "XX");
        assert_eq!(c.serial[1].status, SerialStatus::Complete);
        assert!(c.serial[1].works);
        assert!(parse("[[serial]]\nname = \"W\"\nstatus = \"done\"\n").is_err());
        assert!(parse("[[serial]]\nname = \" \"\n").is_err());
        assert!(
            parse("[[serial]]\nname = \"W\"\n[[serial]]\nname = \"W\"\n")
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn short_parses_defaults_to_name_and_unknown_keys_still_fail() {
        let parse = |s: &str| SourcesConfig::from_table(&s.parse::<toml::Table>().unwrap());
        let c = parse(
            "[[blog]]\nname = \"B\"\nlink = \"https://b.example/feed\"\nshort = \" BB \"\n[[blog]]\nname = \"C\"\nlink = \"https://c.example/feed\"\nshort = \"  \"\n[[newsletter]]\nname = \"N\"\nfrom = [\"@n.example\"]\n[[youtube]]\nchannel = \"Y\"\nid = \"UCy\"\nshort = \"YY\"\n",
        )
        .unwrap();
        assert_eq!(c.blog[0].publication(), "BB");
        assert_eq!(c.blog[1].publication(), "C");
        assert_eq!(c.newsletter[0].publication(), "N");
        assert_eq!(c.youtube[0].publication(), "YY");
        assert!(
            parse("[[blog]]\nname = \"B\"\nlink = \"https://b.example/feed\"\nshrot = \"x\"\n")
                .is_err()
        );
        assert!(parse("[[youtube]]\nchannel = \"Y\"\nid = \"UCy\"\nname = \"x\"\n").is_err());
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
