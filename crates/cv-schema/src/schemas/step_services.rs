//! Step-kind schemas, part 2: agent, search, link/metadata, tracker and maintenance steps.

use super::*;

// ---------------------------------------------------------------------------------------------
// Step kinds, continued (`cv_actions::StepKind::ALL`)
// ---------------------------------------------------------------------------------------------

/// `agent` — `cv::steps::agent::parse` (`agent::StepParams`).
pub static AGENT: Record = Record {
    name: "agent",
    fields: &[
        field("agent", "Agent", "An agents.toml id", Node::Str).req(),
        field(
            "model",
            "Model",
            "provider:model or {param.NAME}; default: the agent's",
            Node::Str,
        )
        .hint(RefKind::ModelRef),
        field(
            "output_var",
            "Output var",
            "Var receiving the completion",
            Node::Str,
        )
        .dflt(D::Str("agent_output")),
        field(
            "max_tokens",
            "Max tokens",
            "Override of the agent's cap",
            POS_U32,
        ),
        field(
            "output",
            "Output",
            "replace: replace the note body",
            Node::Enum(&["var", "replace"]),
        )
        .dflt(D::Str("var")),
    ],
    rules: &[],
    summary: None,
};

/// `mark_forwarded` — `cv::steps::file::parse`.
pub static MARK_FORWARDED: Record = Record {
    name: "mark_forwarded",
    fields: &[
        PATH_FIELD,
        field("line", "Line", "0-based; default: the context line", UINT),
    ],
    rules: &[],
    summary: None,
};

/// `run_action` — `cv_actions::model::Step::from_table` and the `RunAction` step module in
/// `cv::steps` (the target must be a known action).
pub static RUN_ACTION: Record = Record {
    name: "run_action",
    fields: &[
        field("action", "Action", "Id of the child action", Node::Str).req(),
        field(
            "params",
            "Params",
            "Params passed to the child run",
            Node::FreeTable,
        ),
    ],
    rules: &[],
    summary: None,
};

const SEARCH_FIELDS: &[Field] = &[
    field(
        "embed",
        "Embedder",
        "provider:model from providers.toml; absent = lexical only",
        Node::Str,
    ),
    PATH_FIELD,
];

/// `search_upsert` — `cv::steps::search::parse` (keys via `cv::steps::params::check_keys`). Every value must be a non-empty string.
pub static SEARCH_UPSERT: Record = Record {
    name: "search_upsert",
    fields: SEARCH_FIELDS,
    rules: &[],
    summary: None,
};

/// `search_delete` — `cv::steps::search::parse`.
pub static SEARCH_DELETE: Record = Record {
    name: "search_delete",
    fields: SEARCH_FIELDS,
    rules: &[],
    summary: None,
};

/// `link_scan` — `crates/cv/src/steps/link_scan.rs` (`parse`; keys at `link_scan.rs` `KEYS`).
pub static LINK_SCAN: Record = Record {
    name: "link_scan",
    fields: &[
        field(
            "scope",
            "Scope",
            "note: the run's active note; vault: every indexed markdown note",
            Node::Enum(&["note", "vault"]),
        )
        .dflt(D::Str("note")),
        field(
            "exclude",
            "Exclude",
            "Folder prefixes never scanned",
            Node::StrList {
                scalar_ok: true,
                choices: &[],
            },
        )
        .dflt(D::StrList(&["_crucible"])),
    ],
    rules: &[],
    summary: None,
};

/// `yt_metadata` — `crates/cv/src/steps/youtube.rs` (`validate`; keys at `META_KEYS`).
pub static YT_METADATA: Record = Record {
    name: "yt_metadata",
    fields: &[field(
        "video_id",
        "Video id",
        "Placeholders ok; default: the note's yt-video-id, else the id in its source URL",
        Node::Str,
    )],
    rules: &[],
    summary: None,
};

/// `yt_channel_enrich` — `crates/cv/src/steps/youtube.rs` (`validate`; keys at `ENRICH_KEYS`).
pub static YT_CHANNEL_ENRICH: Record = Record {
    name: "yt_channel_enrich",
    fields: &[
        field(
            "channel_id",
            "Channel id",
            "Placeholders ok; default: the channel of a just-created metadata note",
            Node::Str,
        )
        .dflt(D::Str("{vars.channel_id}")),
        field(
            "force",
            "Force",
            "Refresh even when the metadata note was not just created / about.md is fresh",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "max_age_days",
            "Max age (days)",
            "Refresh about.md older than this",
            POS_INT,
        )
        .dflt(D::Int(30)),
    ],
    rules: &[],
    summary: None,
};

/// `x_discover` — `crates/cv/src/steps/x.rs` (`validate`; keys at `BATCH_KEYS`).
pub static X_DISCOVER: Record = Record {
    name: "x_discover",
    fields: &[field(
        "limit",
        "Limit",
        "Statuses ensured per run; the rest wait for the next run",
        POS_INT,
    )
    .dflt(D::Int(50))],
    rules: &[],
    summary: None,
};

/// `x_metadata` — `crates/cv/src/steps/x.rs` (`validate`; keys at `META_KEYS`).
pub static X_METADATA: Record = Record {
    name: "x_metadata",
    fields: &[field(
        "status_id",
        "Status id",
        "Placeholders ok; default: the note's x-status-id, else the status in its source URL",
        Node::Str,
    )],
    rules: &[],
    summary: None,
};

/// `x_backfill` — `crates/cv/src/steps/x.rs` (`validate`; keys at `BATCH_KEYS`).
pub static X_BACKFILL: Record = Record {
    name: "x_backfill",
    fields: &[field(
        "limit",
        "Limit",
        "Statuses ensured per run; the rest wait for the next run",
        POS_INT,
    )
    .dflt(D::Int(50))],
    rules: &[],
    summary: None,
};

/// `image_describe` — `crates/cv/src/steps/image_describe.rs` (`parse`; keys at
/// `image_describe.rs` `KEYS`).
pub static IMAGE_DESCRIBE: Record = Record {
    name: "image_describe",
    fields: &[
        field(
            "scope",
            "Scope",
            "note: the run's active note's images; vault: every indexed _MD5 image",
            Node::Enum(&["note", "vault"]),
        )
        .dflt(D::Str("note")),
        field(
            "limit",
            "Limit",
            "Images described per run; the rest wait for the next run",
            POS_INT,
        )
        .dflt(D::Int(100)),
        field(
            "prune_transient",
            "Prune transient",
            "First drop transient failed and degenerate vision records (backfill)",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
    ],
    rules: &[],
    summary: None,
};

/// `track_youtube` — `crates/cv/src/steps/tracker/youtube.rs` (`limit`, `KEYS`).
pub static TRACK_YOUTUBE: Record = Record {
    name: "track_youtube",
    fields: &[field(
        "limit_per_source",
        "Limit per source",
        "Uploads fetched per channel each run (1-50, the feed window)",
        Node::Int {
            min: Some(1),
            max: Some(50),
        },
    )
    .dflt(D::Int(50))],
    rules: &[],
    summary: None,
};

/// `track_blogs` — `crates/cv/src/steps/tracker/blogs.rs` (no params; config from `pipelines.toml` `[blogs]` + `sources.toml` `[[blog]]`).
pub static TRACK_BLOGS: Record = Record {
    name: "track_blogs",
    fields: &[],
    rules: &[],
    summary: None,
};

/// `track_mail` — `crates/cv/src/steps/mail.rs` (no params; config from `pipelines.toml` `[mail]` + `sources.toml` `[[newsletter]]`).
pub static TRACK_MAIL: Record = Record {
    name: "track_mail",
    fields: &[],
    rules: &[],
    summary: None,
};

/// `auto_file` — `crates/cv/src/steps/auto_file.rs` (no params; dedups the active clip through the supersede engine, then files it by kind).
pub static AUTO_FILE: Record = Record {
    name: "auto_file",
    fields: &[],
    rules: &[],
    summary: None,
};

/// `ignore_video` — `crates/cv/src/steps/ignore.rs` (`mode`, `KEYS`).
pub static IGNORE_VIDEO: Record = Record {
    name: "ignore_video",
    fields: &[field(
        "mode",
        "Mode",
        "auto: ignore the active metadata note's video when it is a short on an [ignore_shorts] channel; manual: always ignore it",
        Node::Enum(&["auto", "manual"]),
    )
    .dflt(D::Str("auto"))],
    rules: &[],
    summary: None,
};

/// `daily_brief` — `crates/cv/src/steps/daily_brief.rs` (no params: `pipelines.toml`
/// `[daily_brief]` configures it).
pub static DAILY_BRIEF: Record = Record {
    name: "daily_brief",
    fields: &[],
    rules: &[],
    summary: None,
};

/// `capture_repair` — `crates/cv/src/steps/capture_repair.rs` (`parse_params`). Not modelled:
/// `folder` is required.
pub static CAPTURE_REPAIR: Record = Record {
    name: "capture_repair",
    fields: &[
        field(
            "folder",
            "Folder",
            "Captured blog notes to rename + retag",
            Node::Str,
        ),
        field(
            "limit",
            "Limit",
            "Notes changed per run (default: all)",
            POS_INT,
        ),
    ],
    rules: &[],
    summary: None,
};
