//! `pipelines.toml` and `sources.toml` schemas.

use super::*;

// ---------------------------------------------------------------------------------------------
// pipelines.toml — `PipelinesConfig`, `crates/cv-actions/src/pipelines.rs` (plan
// v0-automation-pipelines WP-12). Cadences are `schedules.toml` entries, not keys here.
// ---------------------------------------------------------------------------------------------

/// `[youtube]`.
pub static PIPE_YOUTUBE: Record = Record {
    name: "youtube",
    fields: &[
        field("enabled", "Enabled", "Master switch", Node::Bool).dflt(D::Bool(false)),
        field(
            "metadata_root",
            "Metadata root",
            "Metadata note folder",
            Node::Folder,
        )
        .dflt(D::Str("_yt_metadata")),
        field(
            "channel_enrich",
            "Channel enrich",
            "Write channel about.md notes",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "long_kind",
            "Long-video kind",
            "auto_file: kind for a YouTube clip whose transcript has at least 300 words",
            Node::Str,
        )
        .dflt(D::Str("transcript")),
        field(
            "short_kind",
            "Short-video kind",
            "auto_file: kind for a YouTube clip with a shorter transcript (Shorts)",
            Node::Str,
        )
        .dflt(D::Str("post")),
    ],
    rules: &[],
    summary: None,
};

/// `[x]`.
pub static PIPE_X: Record = Record {
    name: "x",
    fields: &[
        field("enabled", "Enabled", "Master switch", Node::Bool).dflt(D::Bool(false)),
        field(
            "metadata_root",
            "Metadata root",
            "Metadata note folder",
            Node::Folder,
        )
        .dflt(D::Str("_x_metadata")),
        field(
            "inbox",
            "Inbox",
            "Clipper inbox folder x_discover watches",
            Node::Folder,
        ),
    ],
    rules: &[],
    summary: None,
};

/// `[blogs]`.
pub static PIPE_BLOGS: Record = Record {
    name: "blogs",
    fields: &[
        field("enabled", "Enabled", "Master switch", Node::Bool).dflt(D::Bool(false)),
        field(
            "automatable_window",
            "Automatable window",
            "body = auto sources whose last N posts had a full body are automatable",
            POS_U32,
        )
        .dflt(D::Int(5)),
        field(
            "complete_percent",
            "Complete percent",
            "A post is complete when its delivered words reach this percent of the full count",
            Node::Int {
                min: Some(0),
                max: Some(100),
            },
        )
        .dflt(D::Int(80)),
    ],
    rules: &[],
    summary: None,
};

/// `[mail]` (plan auto-intake-sprint WP-4).
pub static PIPE_MAIL: Record = Record {
    name: "mail",
    fields: &[
        field("enabled", "Enabled", "Master switch", Node::Bool).dflt(D::Bool(false)),
        field(
            "maildir",
            "Maildir",
            "Maildir root outside the vault (empty = unset: track_mail does nothing)",
            Node::Str,
        ),
        field(
            "processed",
            "Processed",
            "Directory under the Maildir a handled new/ message moves to",
            Node::Str,
        )
        .dflt(D::Str("cur")),
        field(
            "watch",
            "Watch",
            "Enqueue track-mail as soon as a dropped message settles (the schedule stays the backstop)",
            Node::Bool,
        )
        .dflt(D::Bool(true)),
    ],
    rules: &[],
    summary: None,
};

/// `[supersede]` (plan auto-intake-sprint WP-3).
pub static PIPE_SUPERSEDE: Record = Record {
    name: "supersede",
    fields: &[
        field(
            "window_days",
            "Window days",
            "Content candidates are notes dated within this many days of the incoming item",
            U32,
        )
        .dflt(D::Int(21)),
        field(
            "threshold_percent",
            "Threshold percent",
            "Containment percent at or above which one text counts as contained in another",
            Node::Int {
                min: Some(0),
                max: Some(100),
            },
        )
        .dflt(D::Int(80)),
    ],
    rules: &[],
    summary: None,
};

/// `[image_describe]`.
pub static PIPE_IMAGE_DESCRIBE: Record = Record {
    name: "image_describe",
    fields: &[
        field("enabled", "Enabled", "Master switch", Node::Bool).dflt(D::Bool(false)),
        field(
            "provider",
            "Provider",
            "providers.toml provider id",
            Node::Str,
        )
        .hint(RefKind::Provider),
        field("model", "Model", "Model id", Node::Str).hint(RefKind::Model),
        field("max_tokens", "Max tokens", "Response token cap", U32),
    ],
    rules: &[],
    summary: None,
};

/// `[[daily_brief.fx]]`.
pub static PIPE_FX: Record = Record {
    name: "fx",
    fields: &[
        field("base", "Base", "Base currency (ISO 4217 code)", Node::Str)
            .req()
            .hint(RefKind::Currency),
        field(
            "quote",
            "Quote",
            "Quote currency (ISO 4217 code)",
            Node::Str,
        )
        .req()
        .hint(RefKind::Currency),
        field("label", "Label", "Default BASE/QUOTE", Node::Str),
    ],
    rules: &[],
    summary: Some(Summary::Arrow("base", "quote")),
};

/// `[[daily_brief.weather]]`.
pub static PIPE_WEATHER: Record = Record {
    name: "weather",
    fields: &[
        field(
            "label",
            "Label",
            "Place name; a search pick also fills lat/lon",
            Node::Str,
        )
        .req()
        .hint(RefKind::Place),
        field(
            "lat",
            "Latitude",
            "-90..90",
            Node::Float {
                min: Some(-90.0),
                max: Some(90.0),
            },
        )
        .req(),
        field(
            "lon",
            "Longitude",
            "-180..180",
            Node::Float {
                min: Some(-180.0),
                max: Some(180.0),
            },
        )
        .req(),
    ],
    rules: &[],
    summary: None,
};

/// `[daily_brief]`.
pub static PIPE_DAILY_BRIEF: Record = Record {
    name: "daily_brief",
    fields: &[
        field("enabled", "Enabled", "Master switch", Node::Bool).dflt(D::Bool(false)),
        field(
            "section",
            "Section",
            "Daily-note heading the brief fills",
            Node::Str,
        )
        .dflt(D::Str("# Daily Brief")),
        field(
            "fx",
            "Exchange rates",
            "Frankfurter pairs",
            Node::List(&Node::Record(&PIPE_FX)),
        ),
        field(
            "weather",
            "Weather",
            "Open-Meteo places",
            Node::List(&Node::Record(&PIPE_WEATHER)),
        ),
    ],
    rules: &[],
    summary: None,
};

/// `[scorer]`.
pub static PIPE_SCORER: Record = Record {
    name: "scorer",
    fields: &[
        field(
            "url",
            "URL",
            "SystemOne-shaped scoring endpoint (unset = stub)",
            Node::Str,
        ),
        field(
            "model",
            "Model",
            "<provider>/<model-id>; needed with url",
            Node::Str,
        ),
    ],
    rules: &[],
    summary: None,
};

/// `[ignore_shorts]`.
pub static PIPE_IGNORE_SHORTS: Record = Record {
    name: "ignore_shorts",
    fields: &[
        field("channels", "Channels", "Channel ids or names", STR_LIST).hint(RefKind::Channel),
        field(
            "max_seconds",
            "Max seconds",
            "A video this short counts as a Short",
            U32,
        )
        .dflt(D::Int(120)),
    ],
    rules: &[],
    summary: None,
};

/// `[queue]`.
pub static PIPE_QUEUE: Record = Record {
    name: "queue",
    fields: &[field(
        "retention_days",
        "Retention days",
        "Settled jobs older than this are pruned",
        U32,
    )
    .dflt(D::Int(30))],
    rules: &[],
    summary: None,
};

/// `[intake]` (plan round-5-reading-pickers-intake WP-11).
pub static PIPE_INTAKE: Record = Record {
    name: "intake",
    fields: &[field(
        "mode",
        "Mode",
        "lines: one daily-note line per tracked item; view: record only, shown by the daily-intake view",
        Node::Enum(&["lines", "view"]),
    )
    .dflt(D::Str("lines"))],
    rules: &[],
    summary: None,
};

/// `pipelines.toml`.
pub static PIPELINES_FILE: Record = Record {
    name: "pipelines.toml",
    fields: &[
        field(
            "youtube",
            "YouTube",
            "Metadata, enrich, tracker",
            Node::Record(&PIPE_YOUTUBE),
        ),
        field(
            "x",
            "X",
            "Discover, metadata, backfill",
            Node::Record(&PIPE_X),
        ),
        field("blogs", "Blogs", "Blog tracker", Node::Record(&PIPE_BLOGS)),
        field(
            "supersede",
            "Supersede",
            "Intake dedup and supersede",
            Node::Record(&PIPE_SUPERSEDE),
        ),
        field(
            "mail",
            "Mail",
            "Maildir newsletter intake",
            Node::Record(&PIPE_MAIL),
        ),
        field(
            "image_describe",
            "Image describe",
            "Clip image descriptions",
            Node::Record(&PIPE_IMAGE_DESCRIBE),
        ),
        field(
            "daily_brief",
            "Daily Brief",
            "FX + weather section",
            Node::Record(&PIPE_DAILY_BRIEF),
        ),
        field(
            "scorer",
            "Scorer",
            "Importance scorer",
            Node::Record(&PIPE_SCORER),
        ),
        field(
            "ignore_shorts",
            "Ignore Shorts",
            "Short-video rule",
            Node::Record(&PIPE_IGNORE_SHORTS),
        ),
        field("queue", "Queue", "Job retention", Node::Record(&PIPE_QUEUE)),
        field(
            "intake",
            "Intake",
            "Where tracked items surface",
            Node::Record(&PIPE_INTAKE),
        ),
    ],
    rules: &[],
    summary: None,
};

// ---------------------------------------------------------------------------------------------
// sources.toml — `SourcesConfig`, `crates/cv-actions/src/pipelines.rs`
// ---------------------------------------------------------------------------------------------

const PRIORITY: Node = Node::Enum(&["high", "normal", "low", "skip", "ignore"]);

/// One `[[youtube]]` channel.
pub static SOURCE_YOUTUBE: Record = Record {
    name: "youtube",
    fields: &[
        field("channel", "Channel", "Display name", Node::Str).req(),
        field(
            "short",
            "Short",
            "Publication name shown in the Author column; default = the row's name/channel",
            Node::Str,
        ),
        field("id", "Id", "Channel id (UC…)", Node::Str).req(),
        field("tags", "Tags", "Tags", STR_LIST),
        field(
            "priority",
            "Priority",
            "skip/ignore exclude the row",
            PRIORITY,
        )
        .dflt(D::Str("normal")),
        WORKS_FIELD,
    ],
    rules: &[],
    summary: None,
};

/// One `[[blog]]` feed.
pub static SOURCE_BLOG: Record = Record {
    name: "blog",
    fields: &[
        field("name", "Name", "Display name", Node::Str).req(),
        field(
            "short",
            "Short",
            "Publication name shown in the Author column; default = the row's name/channel",
            Node::Str,
        ),
        field("link", "Feed", "RSS/Atom feed URL", Node::Str).req(),
        field(
            "method",
            "Method",
            "Fetch method; non-rss rows are skipped",
            Node::Str,
        )
        .dflt(D::Str("rss")),
        field("tags", "Tags", "Tags", STR_LIST),
        field(
            "priority",
            "Priority",
            "skip/ignore exclude the row",
            PRIORITY,
        )
        .dflt(D::Str("normal")),
        field(
            "canon",
            "Canon",
            "Post-id canonicalization",
            Node::Enum(&["auto", "substack", "strip-params", "keep-params"]),
        )
        .dflt(D::Str("auto")),
        field(
            "body",
            "Body",
            "auto / full (always capture) / snippet (never)",
            Node::Enum(&["auto", "full", "snippet"]),
        )
        .dflt(D::Str("auto")),
        field(
            "paywall_markers",
            "Paywall markers",
            "Body phrases (case-insensitive) that mark a post incomplete",
            STR_LIST,
        ),
        WORKS_FIELD,
    ],
    rules: &[],
    summary: None,
};

/// One `[[newsletter]]` mail sender (plan auto-intake-sprint WP-4).
pub static SOURCE_NEWSLETTER: Record = Record {
    name: "newsletter",
    fields: &[
        field(
            "name",
            "Name",
            "Display name (the Intake source)",
            Node::Str,
        )
        .req(),
        field(
            "short",
            "Short",
            "Publication name shown in the Author column; default = the row's name/channel",
            Node::Str,
        ),
        field(
            "from",
            "From",
            "Sender matchers, case-insensitive: a@b.example or @b.example",
            STR_LIST,
        ),
        field("list_id", "List-Id", "List-Id header matcher", Node::Str),
        field(
            "blog",
            "Blog",
            "Name of the [[blog]] row this newsletter mirrors",
            Node::Str,
        ),
        field(
            "kind",
            "Kind",
            "kinds.toml kind its mails are filed as",
            Node::Str,
        )
        .dflt(D::Str("blog")),
        field("tags", "Tags", "Extra tags", STR_LIST),
        field(
            "paywall_markers",
            "Paywall markers",
            "Body phrases (case-insensitive) that mark an email incomplete",
            STR_LIST,
        ),
        field(
            "hosts",
            "Clip hosts",
            "Web-clip hosts (www. ignored) filed as this row's kind with its publication; a hosts-only row needs no mail matcher",
            STR_LIST,
        ),
        WORKS_FIELD,
    ],
    rules: &[],
    summary: None,
};

/// One `[[serial]]` work (plan round-11-feedback-todos Decision 4).
pub static SOURCE_SERIAL: Record = Record {
    name: "serial",
    fields: &[
        field("name", "Name", "Display name (unique)", Node::Str).req(),
        field(
            "short",
            "Short",
            "Publication name shown in the Author column; default = the row's name/channel",
            Node::Str,
        ),
        field(
            "author",
            "Author",
            "Display author; matches by author only when no prefix is set (a clip's author list must contain it; [[ ]] and # ignored)",
            Node::Str,
        ),
        field(
            "prefix",
            "Source prefixes",
            "Source-URL prefixes; when any is set the row matches by prefix alone (author ignored); neither prefix nor author matches nothing",
            STR_LIST,
        ),
        field(
            "kind",
            "Kind",
            "kinds.toml kind its clips are filed as",
            Node::Str,
        )
        .dflt(D::Str("fanfic")),
        field(
            "status",
            "Status",
            "Works-view status; filing ignores it",
            Node::Enum(&["active", "complete", "ignored"]),
        )
        .dflt(D::Str("active")),
        WORKS_FIELD,
    ],
    rules: &[],
    summary: None,
};

/// The `works` flag every sources row accepts (Decision 5).
const WORKS_FIELD: Field = field(
    "works",
    "In Works",
    "List this row in the Works view",
    Node::Bool,
)
.dflt(D::Bool(false));

/// `sources.toml`.
pub static SOURCES_FILE: Record = Record {
    name: "sources.toml",
    fields: &[
        field(
            "youtube",
            "YouTube channels",
            "One [[youtube]] each",
            Node::List(&Node::Record(&SOURCE_YOUTUBE)),
        ),
        field(
            "blog",
            "Blogs",
            "One [[blog]] each",
            Node::List(&Node::Record(&SOURCE_BLOG)),
        ),
        field(
            "newsletter",
            "Newsletters",
            "One [[newsletter]] each",
            Node::List(&Node::Record(&SOURCE_NEWSLETTER)),
        ),
        field(
            "serial",
            "Serials",
            "One [[serial]] each",
            Node::List(&Node::Record(&SOURCE_SERIAL)),
        ),
    ],
    rules: &[],
    summary: None,
};
