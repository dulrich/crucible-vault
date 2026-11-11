//! The settings the embedder pushes to [`crate::ViewHost`] (review S6, plan
//! tn-review-2026-11-10-remediation Decision 7): the vault-structure folders view expressions read
//! through `folder("<key>")` and the Trash view's retention, typed in one struct the daemon fills
//! from its typed config.

/// A `folder("<key>")` key. [`Self::key`] is the only place the key strings are spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FolderKey {
    /// `captures.toml` `[periods.day].folder`.
    Day,
    /// `pipelines.toml` `[x] inbox` (the Web Clipper's folder by default).
    XInbox,
    /// `pipelines.toml` `[x] metadata_root`.
    XMetadata,
    /// `pipelines.toml` `[youtube] metadata_root`.
    YoutubeMetadata,
    /// `vault.toml` `[structure] app_folder`.
    App,
    /// `vault.toml` `[structure] raw_transcript_folder`.
    RawTranscript,
}

impl FolderKey {
    /// Every key, in declaration order.
    pub const ALL: [Self; 6] = [
        Self::Day,
        Self::XInbox,
        Self::XMetadata,
        Self::YoutubeMetadata,
        Self::App,
        Self::RawTranscript,
    ];

    /// The key as view expressions spell it.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::XInbox => "x_inbox",
            Self::XMetadata => "x_metadata",
            Self::YoutubeMetadata => "youtube_metadata",
            Self::App => "app",
            Self::RawTranscript => "raw_transcript",
        }
    }

    /// The key `s` names, if any.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == s)
    }
}

/// The vault-structure folders, one field per [`FolderKey`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folders {
    pub day: String,
    pub x_inbox: String,
    pub x_metadata: String,
    pub youtube_metadata: String,
    pub app: String,
    pub raw_transcript: String,
}

impl Folders {
    /// The folder `key` names.
    #[must_use]
    pub fn get(&self, key: FolderKey) -> &str {
        match key {
            FolderKey::Day => &self.day,
            FolderKey::XInbox => &self.x_inbox,
            FolderKey::XMetadata => &self.x_metadata,
            FolderKey::YoutubeMetadata => &self.youtube_metadata,
            FolderKey::App => &self.app,
            FolderKey::RawTranscript => &self.raw_transcript,
        }
    }
}

/// What the embedder pushes to the host in one [`crate::ViewHost::set_settings`] call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostSettings {
    /// `folder("<key>")` values.
    pub folders: Folders,
    /// `vault.toml` `[trash] retention_days` (the daemon pushes `ConfigSet`'s typed value, so tidy
    /// and the Trash view share one value and default). `0` = no expiry.
    pub trash_retention_days: u32,
}
