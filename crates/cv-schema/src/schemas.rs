//! Schemas of every vault config file (`_custom/crucible-vault/`) and of all 15 step kinds.
//!
//! Each schema describes its real parser (cited per item); the drift test in
//! `crates/cv/tests/schema_drift.rs` holds them together. Where a parser accepts something the
//! vocabulary cannot model (conditional requirements, placeholder syntax, key aliases), the
//! schema stays looser than the parser rather than inventing constraints — the daemon's reload
//! errors are the second validation layer.

use crate::node::{
    Default as D, Field, INT, Node, POS_INT, POS_U32, Record, Rule, STR_LIST, STR_OR_LIST, U32,
    UINT, Union, Variant, field,
};

// ---------------------------------------------------------------------------------------------
// Shared field helpers
// ---------------------------------------------------------------------------------------------

const PATH_FIELD: Field = field(
    "path",
    "Target note",
    "Vault path (placeholders ok); default: the active/triggering note",
    Node::Str,
);
const KIND_FIELD: Field = field(
    "kind",
    "Kind",
    "A `kinds.toml` kind name, usually \"{param.kind}\"",
    Node::Str,
);

// ---------------------------------------------------------------------------------------------
// Step kinds (`cv_actions::StepKind::ALL`)
// ---------------------------------------------------------------------------------------------

/// `guard` — `crates/cv/src/steps/file.rs:132` (`validate`; keys at `file.rs:110`).
pub static GUARD: Record = Record {
    name: "guard",
    fields: &[
        PATH_FIELD,
        field(
            "tag",
            "Has tag",
            "Every listed tag must be present",
            STR_OR_LIST,
        ),
        field(
            "not_tag",
            "Lacks tag",
            "No listed tag may be present",
            STR_OR_LIST,
        ),
        field(
            "prop",
            "Has property",
            "Every listed property must be set",
            STR_OR_LIST,
        ),
        field(
            "not_prop",
            "Lacks property",
            "No listed property may be set",
            STR_OR_LIST,
        ),
        field(
            "folder",
            "In folder",
            "The note is inside one listed folder",
            STR_OR_LIST,
        ),
        field(
            "not_folder",
            "Not in folder",
            "The note is in no listed folder",
            STR_OR_LIST,
        ),
    ],
    rules: &[],
};

/// `capture` — `crates/cv/src/steps/capture.rs:149` (`validate`; keys at `capture.rs:56`,
/// `[[capture]]` merge at `capture.rs:88`). Not modelled: `target = "path"` needs `path`, and
/// `mode = "section"` needs `section` (conditional requirements).
pub static CAPTURE: Record = Record {
    name: "capture",
    fields: &[
        field(
            "capture",
            "Capture entry",
            "A `[[capture]] name` from captures.toml (its keys are defaults)",
            Node::Str,
        ),
        field(
            "target",
            "Target",
            "Where to write",
            Node::Enum(CAPTURE_TARGETS),
        )
        .dflt(D::Str("active")),
        field(
            "path",
            "Path",
            "Target for target = \"path\" (placeholders ok)",
            Node::Str,
        ),
        field(
            "mode",
            "Mode",
            "section = append into `section`",
            Node::Enum(&["append", "prepend", "replace", "section"]),
        )
        .dflt(D::Str("append")),
        field(
            "section",
            "Section",
            "Heading anchoring the insert, e.g. \"## Log\"",
            Node::Str,
        ),
        field(
            "section_mode",
            "Section mode",
            "source: use the heading above the source line",
            Node::Enum(&["fixed", "source"]),
        )
        .dflt(D::Str("fixed")),
        field(
            "format",
            "Format",
            "{{…}} template rendered around the value",
            Node::Str,
        )
        .dflt(D::Str("{{value}}")),
        field(
            "source",
            "Source",
            "line: the task line at the context line",
            Node::Enum(&["value", "line"]),
        )
        .dflt(D::Str("value")),
        field(
            "value",
            "Value",
            "The captured value (placeholders ok)",
            Node::Str,
        ),
        field("name", "Name", "Descriptive", Node::Str),
        field("label", "Label", "Descriptive", Node::Str),
    ],
    rules: &[],
};

/// Every `target` value `capture.rs` `parse_target` accepts (canonical first, then aliases).
const CAPTURE_TARGETS: &[&str] = &[
    "active",
    "path",
    "daily",
    "daily_next",
    "weekly",
    "monthly",
    "day",
    "week",
    "month",
    "daily-next",
];

/// `materialize` — `crates/cv/src/steps/materialize.rs:437` (`validate`; keys at
/// `materialize.rs:425`, period settings at `materialize.rs:280`). `period` stays a string: it
/// takes placeholders (`"{param.period}"`) and the aliases `daily`/`weekly`/`monthly`.
pub static MATERIALIZE: Record = Record {
    name: "materialize",
    fields: &[
        field(
            "period",
            "Period",
            "day | week | month (or a placeholder)",
            Node::Str,
        ),
        field(
            "date",
            "Date",
            "YYYY-MM-DD inside the period (placeholders ok); default today",
            Node::Str,
        ),
        field(
            "path",
            "Path",
            "Plain note to create when no period (placeholders ok)",
            Node::Str,
        ),
        field(
            "template",
            "Template",
            "Vault path of the template; empty = none",
            Node::Path,
        ),
        field(
            "folder",
            "Folder",
            "Per-step override of the period folder",
            Node::Path,
        ),
        field(
            "format",
            "Format",
            "Per-step override of the period name format (Moment)",
            Node::Str,
        ),
        field(
            "target",
            "Target",
            "active: materialize the triggering note itself",
            Node::Enum(&["active"]),
        ),
        field(
            "only_if_empty",
            "Only if empty",
            "Stop when the note exists and is non-empty",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
    ],
    rules: &[Rule::AtLeastOneOf(&["period", "path", "target"])],
};

/// `move` — `crates/cv/src/steps/file.rs:132`.
pub static MOVE: Record = Record {
    name: "move",
    fields: &[
        PATH_FIELD,
        field("to", "To folder", "Destination folder template", Node::Str),
        field(
            "name",
            "Name",
            "New basename; default: unchanged",
            Node::Str,
        ),
        KIND_FIELD,
    ],
    rules: &[Rule::AtLeastOneOf(&["to", "kind"])],
};

/// `copy` — `crates/cv/src/steps/file.rs:132`.
pub static COPY: Record = Record {
    name: "copy",
    fields: &[
        PATH_FIELD,
        field("to", "To folder", "Destination folder template", Node::Str).req(),
        field(
            "name",
            "Name",
            "Copy basename; default: unchanged",
            Node::Str,
        ),
    ],
    rules: &[],
};

/// `set_props` — `crates/cv/src/steps/file.rs:132`.
pub static SET_PROPS: Record = Record {
    name: "set_props",
    fields: &[
        PATH_FIELD,
        field(
            "props",
            "Properties",
            "key = value; string values expand placeholders",
            Node::FreeTable,
        ),
        field(
            "if_empty",
            "Only if empty",
            "Set a key only when it is empty",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        KIND_FIELD,
    ],
    rules: &[],
};

/// `remove_props` — `crates/cv/src/steps/file.rs:132`.
pub static REMOVE_PROPS: Record = Record {
    name: "remove_props",
    fields: &[
        PATH_FIELD,
        field("props", "Properties", "Keys to remove", STR_OR_LIST),
    ],
    rules: &[],
};

/// `upsert_tags` — `crates/cv/src/steps/file.rs:132`.
pub static UPSERT_TAGS: Record = Record {
    name: "upsert_tags",
    fields: &[
        PATH_FIELD,
        field(
            "tags",
            "Tags",
            "Appended unless present (placeholders ok)",
            STR_OR_LIST,
        ),
        KIND_FIELD,
    ],
    rules: &[Rule::AtLeastOneOf(&["tags", "kind"])],
};

/// `lint` — `crates/cv/src/steps/lint.rs:82` (`validate`; keys at `lint.rs:36`). `steps` keys
/// must be `cv_lint::LINT_STEPS` ids (not modelled).
pub static LINT: Record = Record {
    name: "lint",
    fields: &[
        PATH_FIELD,
        KIND_FIELD,
        field(
            "cleanup_transcript",
            "Clean up transcript",
            "Run the YouTube transcript cleanup first",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "frontmatter_insert",
            "Insert keys",
            "`key: value` lines inserted into frontmatter",
            Node::Str,
        )
        .dflt(D::Str("")),
        field(
            "created_key",
            "Created key",
            "Frontmatter key of the created stamp",
            Node::Str,
        )
        .dflt(D::Str("created")),
        field(
            "modified_key",
            "Modified key",
            "Frontmatter key of the modified stamp",
            Node::Str,
        )
        .dflt(D::Str("updated")),
        field(
            "blank_line_after_yaml",
            "Blank line after YAML",
            "Keep one blank line after frontmatter",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "yaml_key_priority",
            "Key priority",
            "Frontmatter keys sorted first, in order",
            STR_OR_LIST,
        ),
        field(
            "steps",
            "Lint steps",
            "lint step id = enabled",
            Node::Map(&Node::Bool),
        ),
    ],
    rules: &[],
};

/// `localize` — `crates/cv/src/steps/localize.rs:963` (`validate` → `parse_params` at
/// `localize.rs:835`; keys at `localize.rs:812`). Not modelled: templates must not use
/// `{{datetime:…}}`, `name_template` must contain `{{md5}}`, `folder` is required exactly when
/// `scope = "folder"`.
pub static LOCALIZE: Record = Record {
    name: "localize",
    fields: &[
        PATH_FIELD,
        field(
            "mode",
            "Mode",
            "repair: only fix broken attachment refs",
            Node::Enum(&["localize", "repair"]),
        )
        .dflt(D::Str("localize")),
        field(
            "folder_template",
            "Folder template",
            "Attachment folder",
            Node::Str,
        )
        .dflt(D::Str("{{folder}}/_attachments/{{slug}}")),
        field(
            "name_template",
            "Name template",
            "Attachment file name; must contain {{md5}}",
            Node::Str,
        )
        .dflt(D::Str("{{md5}}_MD5.{{ext}}")),
        field("images", "Images", "Localize images", Node::Bool).dflt(D::Bool(true)),
        field("audio", "Audio", "Localize audio", Node::Bool).dflt(D::Bool(false)),
        field("video", "Video", "Localize video", Node::Bool).dflt(D::Bool(false)),
        field("pdf", "PDF", "Localize PDFs", Node::Bool).dflt(D::Bool(false)),
        field(
            "images_whitelist",
            "Image extensions",
            "Default: the native set",
            STR_LIST,
        ),
        field(
            "audio_whitelist",
            "Audio extensions",
            "Default: the native set",
            STR_LIST,
        ),
        field(
            "video_whitelist",
            "Video extensions",
            "Default: the native set",
            STR_LIST,
        ),
        field(
            "pdf_whitelist",
            "PDF extensions",
            "Default: the native set",
            STR_LIST,
        ),
        field(
            "convert_images",
            "Convert images",
            "Re-encode, keep when smaller",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "image_format",
            "Image format",
            "Conversion target",
            Node::Enum(&["webp", "jpeg"]),
        )
        .dflt(D::Str("webp")),
        field(
            "image_quality",
            "Image quality",
            "JPEG quality",
            Node::Float {
                min: None,
                max: None,
            },
        ),
        field("timeout_secs", "Timeout (s)", "Per download", POS_INT).dflt(D::Int(30)),
        field("max_bytes", "Max bytes", "Per download", POS_INT).dflt(D::Int(50 * 1024 * 1024)),
        field(
            "scope",
            "Scope",
            "folder: backfill every note under `folder` with remote images",
            Node::Enum(&["note", "folder"]),
        )
        .dflt(D::Str("note")),
        field(
            "folder",
            "Folder",
            "Backfill folder (scope = folder)",
            Node::Str,
        ),
        field(
            "limit",
            "Limit",
            "Notes attempted per run (scope = folder)",
            POS_INT,
        )
        .dflt(D::Int(50)),
    ],
    rules: &[],
};

/// `agent` — `crates/cv/src/steps/agent.rs:196` (`validate`; `StepParams` at `agent.rs:74`).
pub static AGENT: Record = Record {
    name: "agent",
    fields: &[
        field("agent", "Agent", "An agents.toml id", Node::Str).req(),
        field(
            "model",
            "Model",
            "provider:model or {param.NAME}; default: the agent's",
            Node::Str,
        ),
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
};

/// `mark_forwarded` — `crates/cv/src/steps/file.rs:132`.
pub static MARK_FORWARDED: Record = Record {
    name: "mark_forwarded",
    fields: &[
        PATH_FIELD,
        field("line", "Line", "0-based; default: the context line", UINT),
    ],
    rules: &[],
};

/// `run_action` — `crates/cv-actions/src/model.rs:219` (`Step::from_table`) and
/// `crates/cv/src/steps/mod.rs:230` (the target must be a known action).
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
};

const SEARCH_FIELDS: &[Field] = &[
    field(
        "companion_url",
        "Companion URL",
        "http(s) URL of the search companion",
        Node::Str,
    )
    .dflt(D::Str("http://127.0.0.1:4801")),
    field("vault_id", "Vault id", "Companion vault id", Node::Str).dflt(D::Str("default")),
    field(
        "embed",
        "Embedder",
        "provider:model from providers.toml; absent = lexical only",
        Node::Str,
    ),
    PATH_FIELD,
];

/// `search_upsert` — `crates/cv/src/steps/search.rs:97` (`validate` → `parse`; keys at
/// `search.rs:51`). Every value must be a non-empty string.
pub static SEARCH_UPSERT: Record = Record {
    name: "search_upsert",
    fields: SEARCH_FIELDS,
    rules: &[],
};

/// `search_delete` — `crates/cv/src/steps/search.rs:97`.
pub static SEARCH_DELETE: Record = Record {
    name: "search_delete",
    fields: SEARCH_FIELDS,
    rules: &[],
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
};

/// `track_blogs` — `crates/cv/src/steps/tracker/blogs.rs` (no params; config from `pipelines.toml` `[blogs]` + `sources.toml` `[[blog]]`).
pub static TRACK_BLOGS: Record = Record {
    name: "track_blogs",
    fields: &[],
    rules: &[],
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
};

/// `daily_brief` — `crates/cv/src/steps/daily_brief.rs` (no params: `pipelines.toml`
/// `[daily_brief]` configures it).
pub static DAILY_BRIEF: Record = Record {
    name: "daily_brief",
    fields: &[],
    rules: &[],
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
};

/// The `[[action.steps]]` union, one variant per `StepKind::ALL` member (same order).
pub static STEP: Union = Union {
    name: "step",
    tag: "type",
    variants: &[
        Variant {
            tag: "guard",
            label: "Guard",
            record: &GUARD,
        },
        Variant {
            tag: "capture",
            label: "Capture",
            record: &CAPTURE,
        },
        Variant {
            tag: "materialize",
            label: "Materialize",
            record: &MATERIALIZE,
        },
        Variant {
            tag: "move",
            label: "Move",
            record: &MOVE,
        },
        Variant {
            tag: "copy",
            label: "Copy",
            record: &COPY,
        },
        Variant {
            tag: "set_props",
            label: "Set properties",
            record: &SET_PROPS,
        },
        Variant {
            tag: "remove_props",
            label: "Remove properties",
            record: &REMOVE_PROPS,
        },
        Variant {
            tag: "upsert_tags",
            label: "Upsert tags",
            record: &UPSERT_TAGS,
        },
        Variant {
            tag: "lint",
            label: "Lint",
            record: &LINT,
        },
        Variant {
            tag: "localize",
            label: "Localize attachments",
            record: &LOCALIZE,
        },
        Variant {
            tag: "agent",
            label: "Agent",
            record: &AGENT,
        },
        Variant {
            tag: "mark_forwarded",
            label: "Mark forwarded",
            record: &MARK_FORWARDED,
        },
        Variant {
            tag: "run_action",
            label: "Run action",
            record: &RUN_ACTION,
        },
        Variant {
            tag: "search_upsert",
            label: "Search: upsert",
            record: &SEARCH_UPSERT,
        },
        Variant {
            tag: "search_delete",
            label: "Search: delete",
            record: &SEARCH_DELETE,
        },
        Variant {
            tag: "link_scan",
            label: "Link scan",
            record: &LINK_SCAN,
        },
        Variant {
            tag: "yt_metadata",
            label: "YouTube metadata",
            record: &YT_METADATA,
        },
        Variant {
            tag: "yt_channel_enrich",
            label: "YouTube channel enrich",
            record: &YT_CHANNEL_ENRICH,
        },
        Variant {
            tag: "x_discover",
            label: "X: discover",
            record: &X_DISCOVER,
        },
        Variant {
            tag: "x_metadata",
            label: "X: metadata",
            record: &X_METADATA,
        },
        Variant {
            tag: "x_backfill",
            label: "X: backfill",
            record: &X_BACKFILL,
        },
        Variant {
            tag: "image_describe",
            label: "Describe images",
            record: &IMAGE_DESCRIBE,
        },
        Variant {
            tag: "track_youtube",
            label: "Track YouTube",
            record: &TRACK_YOUTUBE,
        },
        Variant {
            tag: "track_blogs",
            label: "Track blogs",
            record: &TRACK_BLOGS,
        },
        Variant {
            tag: "ignore_video",
            label: "Ignore video",
            record: &IGNORE_VIDEO,
        },
        Variant {
            tag: "daily_brief",
            label: "Daily brief",
            record: &DAILY_BRIEF,
        },
        Variant {
            tag: "capture_repair",
            label: "Capture repair",
            record: &CAPTURE_REPAIR,
        },
    ],
};

// ---------------------------------------------------------------------------------------------
// actions.toml — `crates/cv-actions/src/config.rs:38` (`ActionDef`)
// ---------------------------------------------------------------------------------------------

/// `cv_protocol::ParamSpec` (`crates/cv-protocol/src/lib.rs:220`; not `deny_unknown_fields`).
pub static PARAM: Record = Record {
    name: "param",
    fields: &[
        field("name", "Name", "Param name ({param.NAME})", Node::Str).req(),
        field("label", "Label", "Prompt label", Node::Str).req(),
        field(
            "kind",
            "Kind",
            "Input kind",
            Node::Enum(&["text", "multiline", "path", "enum", "date", "bool"]),
        )
        .req(),
        field(
            "required",
            "Required",
            "The client must supply a value",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "choices",
            "Choices",
            "Enum choices (required for kind = enum)",
            STR_LIST,
        ),
        field("default", "Default", "Default value", Node::Str),
    ],
    rules: &[],
};

/// `PropSelector` (`crates/cv-actions/src/model.rs`).
pub static PROP_SELECTOR: Record = Record {
    name: "prop",
    fields: &[
        field("key", "Key", "Frontmatter property", Node::Str).req(),
        field(
            "value",
            "Value",
            "Required value; absent = key present",
            Node::Str,
        ),
    ],
    rules: &[],
};

/// `Selector` (`crates/cv-actions/src/model.rs`); an all-empty selector disarms (I5).
pub static SELECTOR: Record = Record {
    name: "selector",
    fields: &[
        field(
            "folder",
            "Folder",
            "Folder prefix (no trailing slash)",
            Node::Path,
        ),
        field("tag", "Tag", "Tag without #", Node::Str),
        field(
            "prop",
            "Property",
            "Frontmatter property",
            Node::Record(&PROP_SELECTOR),
        ),
    ],
    rules: &[Rule::AtLeastOneOf(&["folder", "tag", "prop"])],
};

const DAILY: Field = field("daily", "Daily at", "HH:MM local time", Node::Str);
const EVERY: Field = field("every", "Every", "<n>m or <n>h", Node::Str);

/// `StarterDef::Command` (`crates/cv-actions/src/config.rs:68`).
pub static STARTER_COMMAND: Record = Record {
    name: "command",
    fields: &[],
    rules: &[],
};

/// `StarterDef::Event`.
pub static STARTER_EVENT: Record = Record {
    name: "event",
    fields: &[
        field(
            "selector",
            "Selector",
            "Which notes",
            Node::Record(&SELECTOR),
        )
        .req(),
        field(
            "kinds",
            "Events",
            "Change kinds listened to",
            Node::StrList {
                scalar_ok: false,
                choices: &["created", "modified", "deleted", "renamed"],
            },
        )
        .req(),
        field(
            "origin",
            "Origin",
            "Whose edits",
            Node::Enum(&["external", "daemon", "any"]),
        )
        .dflt(D::Str("external")),
    ],
    rules: &[],
};

/// `StarterDef::Schedule`.
pub static STARTER_SCHEDULE: Record = Record {
    name: "schedule",
    fields: &[DAILY, EVERY],
    rules: &[Rule::ExactlyOneOf(&["daily", "every"])],
};

/// `[[action.starters]]` (`StarterDef`, `crates/cv-actions/src/config.rs:68`).
pub static STARTER: Union = Union {
    name: "starter",
    tag: "type",
    variants: &[
        Variant {
            tag: "command",
            label: "Command",
            record: &STARTER_COMMAND,
        },
        Variant {
            tag: "event",
            label: "Event",
            record: &STARTER_EVENT,
        },
        Variant {
            tag: "schedule",
            label: "Schedule",
            record: &STARTER_SCHEDULE,
        },
    ],
};

/// `ResourceSpec` (`crates/cv-actions/src/model.rs`).
pub static RESOURCES: Record = Record {
    name: "resources",
    fields: &[
        field(
            "note",
            "Note",
            "Include the note path as a resource key",
            Node::Bool,
        )
        .dflt(D::Bool(true)),
        field(
            "keys",
            "Extra keys",
            "kind::id templates ({path}, {param.NAME})",
            STR_LIST,
        ),
    ],
    rules: &[],
};

/// One `[[action]]` (`ActionDef`, `crates/cv-actions/src/config.rs:38`).
pub static ACTION: Record = Record {
    name: "action",
    fields: &[
        field("id", "Id", "Stable action id", Node::Str).req(),
        field(
            "label",
            "Label",
            "Palette label; default: the id",
            Node::Str,
        ),
        field("group", "Group", "Palette group", Node::Str),
        field("keys", "Keys", "Key chords", Node::Chords),
        field(
            "params",
            "Params",
            "Prompted parameters",
            Node::List(&Node::Record(&PARAM)),
        ),
        field(
            "needs",
            "Needs",
            "Context the client supplies",
            Node::StrList {
                scalar_ok: false,
                choices: &["active_path", "selection", "value", "line"],
            },
        ),
        field(
            "lane",
            "Lane",
            "Queue lane",
            Node::Enum(&["auto", "user", "background"]),
        )
        .dflt(D::Str("auto")),
        field(
            "durable",
            "Durable",
            "false: the palette awaits completion",
            Node::Bool,
        )
        .dflt(D::Bool(true)),
        field(
            "steps",
            "Steps",
            "Executed in order",
            Node::List(&Node::Union(&STEP)),
        )
        .req(),
        field(
            "starters",
            "Starters",
            "Default: one command starter",
            Node::List(&Node::Union(&STARTER)),
        ),
        field(
            "resources",
            "Resources",
            "Resource-key derivation",
            Node::Record(&RESOURCES),
        ),
    ],
    rules: &[],
};

/// `actions.toml`.
pub static ACTIONS_FILE: Record = Record {
    name: "actions.toml",
    fields: &[field(
        "action",
        "Actions",
        "One [[action]] each",
        Node::List(&Node::Record(&ACTION)),
    )],
    rules: &[],
};

// ---------------------------------------------------------------------------------------------
// schedules.toml — `ScheduleDef`, `crates/cv-actions/src/config.rs:88`
// ---------------------------------------------------------------------------------------------

/// One `[[schedule]]`.
pub static SCHEDULE: Record = Record {
    name: "schedule",
    fields: &[
        field("action", "Action", "Action id to run", Node::Str).req(),
        DAILY,
        EVERY,
    ],
    rules: &[Rule::ExactlyOneOf(&["daily", "every"])],
};

/// `schedules.toml`.
pub static SCHEDULES_FILE: Record = Record {
    name: "schedules.toml",
    fields: &[field(
        "schedule",
        "Schedules",
        "One [[schedule]] each",
        Node::List(&Node::Record(&SCHEDULE)),
    )],
    rules: &[],
};

// ---------------------------------------------------------------------------------------------
// kinds.toml — `crates/cv/src/steps/file.rs:58` (`kind_def`, keys `KIND_KEYS`)
// ---------------------------------------------------------------------------------------------

/// One `[kinds.NAME]`.
pub static KIND: Record = Record {
    name: "kind",
    fields: &[
        field("label", "Label", "Palette text", Node::Str),
        field(
            "tags",
            "Tags",
            "Upserted by upsert_tags { kind }",
            STR_OR_LIST,
        ),
        field(
            "folder",
            "Folder",
            "Destination folder template for move { kind }",
            Node::Str,
        ),
        field(
            "cleanup_transcript",
            "Clean up transcript",
            "Read by lint { kind }",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "props",
            "Properties",
            "Applied by set_props { kind }",
            Node::FreeTable,
        ),
    ],
    rules: &[],
};

/// `kinds.toml`.
pub static KINDS_FILE: Record = Record {
    name: "kinds.toml",
    fields: &[field(
        "kinds",
        "Kinds",
        "Kind name → definition",
        Node::Map(&Node::Record(&KIND)),
    )],
    rules: &[],
};

// ---------------------------------------------------------------------------------------------
// captures.toml — periods `materialize.rs:280`, `[[capture]]` `capture.rs:88`
// ---------------------------------------------------------------------------------------------

/// One `[periods.P]`.
pub static PERIOD: Record = Record {
    name: "period",
    fields: &[
        field("folder", "Folder", "Period notes folder", Node::Path).req(),
        field(
            "format",
            "Format",
            "Moment name format; default per period",
            Node::Str,
        ),
        field(
            "template",
            "Template",
            "Vault path of the template",
            Node::Path,
        ),
        field(
            "asset_folder",
            "Asset folder",
            "Report <folder>/<name> as vars.asset_folder",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
    ],
    rules: &[],
};

/// `[periods]`.
pub static PERIODS: Record = Record {
    name: "periods",
    fields: &[
        field("day", "Day", "Daily notes", Node::Record(&PERIOD)),
        field("week", "Week", "Weekly notes", Node::Record(&PERIOD)),
        field("month", "Month", "Monthly notes", Node::Record(&PERIOD)),
    ],
    rules: &[],
};

/// One `[[capture]]` entry: the capture step's keys minus `capture` (`capture.rs:56`).
pub static CAPTURE_ENTRY: Record = Record {
    name: "capture entry",
    fields: &[
        field(
            "name",
            "Name",
            "Referenced by capture = \"NAME\"",
            Node::Str,
        )
        .req(),
        field("label", "Label", "Descriptive", Node::Str),
        field(
            "target",
            "Target",
            "Where to write",
            Node::Enum(CAPTURE_TARGETS),
        )
        .dflt(D::Str("active")),
        field("path", "Path", "Target for target = \"path\"", Node::Str),
        field(
            "mode",
            "Mode",
            "section = append into `section`",
            Node::Enum(&["append", "prepend", "replace", "section"]),
        )
        .dflt(D::Str("append")),
        field(
            "section",
            "Section",
            "Heading anchoring the insert",
            Node::Str,
        ),
        field(
            "section_mode",
            "Section mode",
            "source: the heading above the source line",
            Node::Enum(&["fixed", "source"]),
        )
        .dflt(D::Str("fixed")),
        field("format", "Format", "{{…}} template", Node::Str).dflt(D::Str("{{value}}")),
        field(
            "source",
            "Source",
            "line: the task line",
            Node::Enum(&["value", "line"]),
        )
        .dflt(D::Str("value")),
        field("value", "Value", "The captured value", Node::Str),
    ],
    rules: &[],
};

/// `captures.toml`.
pub static CAPTURES_FILE: Record = Record {
    name: "captures.toml",
    fields: &[
        field(
            "periods",
            "Period notes",
            "Per-period folder/format/template",
            Node::Record(&PERIODS),
        ),
        field(
            "capture",
            "Captures",
            "Named capture presets",
            Node::List(&Node::Record(&CAPTURE_ENTRY)),
        ),
    ],
    rules: &[],
};

// ---------------------------------------------------------------------------------------------
// agents.toml — `AgentDef`, `crates/cv/src/steps/agent.rs:85` / `agent_def` `agent.rs:134`
// ---------------------------------------------------------------------------------------------

/// One `[ID]` agent.
pub static AGENT_DEF: Record = Record {
    name: "agent",
    fields: &[
        field("name", "Name", "Label (diagnostics only)", Node::Str),
        field("system", "System prompt", "Inline text", Node::Str),
        field(
            "system_file",
            "System prompt file",
            "Vault path",
            Node::Path,
        ),
        field(
            "prompt",
            "Prompt",
            "User prompt template; neither = {{value}}",
            Node::Str,
        ),
        field("prompt_file", "Prompt file", "Vault path", Node::Path),
        field(
            "input",
            "Input",
            "Template rendered into {{value}}/{{input}}",
            Node::Str,
        ),
        field("model", "Model", "Default provider:model", Node::Str),
        field("max_tokens", "Max tokens", "Completion cap (> 0)", POS_U32).req(),
    ],
    rules: &[
        Rule::AtMostOneOf(&["system", "system_file"]),
        Rule::AtMostOneOf(&["prompt", "prompt_file"]),
    ],
};

/// `agents.toml`: agent id → definition at the root.
pub static AGENTS_FILE: Node = Node::Map(&Node::Record(&AGENT_DEF));

// ---------------------------------------------------------------------------------------------
// providers.toml — `crates/cv-providers/src/config.rs:121` (`ProvidersConfig`)
// ---------------------------------------------------------------------------------------------

/// `ModelConfig`.
pub static MODEL: Record = Record {
    name: "model",
    fields: &[
        field("id", "Id", "Model id", Node::Str).req(),
        field(
            "capabilities",
            "Capabilities",
            "Absent = chat only",
            Node::StrList {
                scalar_ok: false,
                choices: &["chat", "embed", "rerank"],
            },
        ),
    ],
    rules: &[],
};

/// `ProviderConfig`. Secrets are referenced by name only.
pub static PROVIDER: Record = Record {
    name: "provider",
    fields: &[
        field("id", "Id", "Non-empty, no ':'", Node::Str).req(),
        field(
            "kind",
            "Kind",
            "Provider kind",
            Node::Enum(&["openai-compatible", "anthropic", "google", "ollama", "cli"]),
        )
        .req(),
        field("name", "Name", "Label", Node::Str),
        field(
            "base_url",
            "Base URL",
            "API base; default per kind",
            Node::Str,
        ),
        field(
            "api_key_env",
            "API key env var",
            "Env var name (never the key)",
            Node::Str,
        ),
        field(
            "api_key_file",
            "API key file",
            "File name under the secrets dir",
            Node::Str,
        ),
        field(
            "max_concurrent_requests",
            "Max concurrent",
            "0/absent = default",
            U32,
        ),
        field(
            "local",
            "Local",
            "Overrides the local-default heuristic",
            Node::Bool,
        ),
        field("max_tokens", "Max tokens", "Provider completion cap", U32),
        field(
            "command",
            "Command",
            "CLI: the executable (required for kind = cli)",
            Node::Str,
        ),
        field("args", "Args", "CLI: prepended args", STR_LIST),
        field("extra_args", "Extra args", "CLI: appended args", STR_LIST),
        field("cwd", "Working dir", "CLI working directory", Node::Str),
        field("timeout_seconds", "Timeout (s)", "CLI timeout", UINT),
        field(
            "models",
            "Models",
            "Served models",
            Node::List(&Node::Record(&MODEL)),
        ),
    ],
    rules: &[],
};

/// `providers.toml`.
pub static PROVIDERS_FILE: Record = Record {
    name: "providers.toml",
    fields: &[field(
        "provider",
        "Providers",
        "One [[provider]] each",
        Node::List(&Node::Record(&PROVIDER)),
    )],
    rules: &[],
};

// ---------------------------------------------------------------------------------------------
// bookmarks.toml — `crates/editor/src/bookmarks.rs` (`Bookmark`, `File`)
// ---------------------------------------------------------------------------------------------

/// One `[[bookmark]]`.
pub static BOOKMARK: Record = Record {
    name: "bookmark",
    fields: &[
        field("name", "Name", "Shown in the panel", Node::Str).req(),
        field("path", "Path", "Vault path", Node::Path).req(),
        // plan editor-ui-round-3 WP-9: chords for "Open bookmark: <name>" (an action's shape).
        field("keys", "Keys", "Key chords", Node::Chords),
    ],
    rules: &[],
};

/// `bookmarks.toml`.
pub static BOOKMARKS_FILE: Record = Record {
    name: "bookmarks.toml",
    fields: &[field(
        "bookmark",
        "Bookmarks",
        "In panel order",
        Node::List(&Node::Record(&BOOKMARK)),
    )],
    rules: &[],
};

// ---------------------------------------------------------------------------------------------
// views/*.toml — `crates/views/src/def.rs` (`BaseDef`, `ViewDef`). The snake_case aliases
// (`group_by`, `column_size`, `display_name`) parse but are not modelled (preserved, unchecked).
// ---------------------------------------------------------------------------------------------

/// `[properties."<id>"]`.
pub static PROPERTY_DEF: Record = Record {
    name: "property",
    fields: &[field(
        "displayName",
        "Display name",
        "Column header",
        Node::Str,
    )],
    rules: &[],
};

const DIRECTION: Node = Node::Enum(&["ASC", "DESC", "asc", "desc"]);

/// `SortSpec`.
pub static SORT: Record = Record {
    name: "sort",
    fields: &[
        field("property", "Property", "Property id", Node::Str).req(),
        field("direction", "Direction", "Sort direction", DIRECTION).dflt(D::Str("ASC")),
    ],
    rules: &[],
};

/// `PivotColumn`.
pub static PIVOT_COLUMN: Record = Record {
    name: "column",
    fields: &[
        field("label", "Label", "Column header", Node::Str).req(),
        field(
            "when",
            "When",
            "Row goes here when truthy; absent = catch-all",
            Node::Expr,
        ),
    ],
    rules: &[],
};

/// One `[[views]]` (`ViewDef`).
pub static VIEW: Record = Record {
    name: "view",
    fields: &[
        field(
            "type",
            "Type",
            "View type",
            Node::Enum(&["table", "pivot", "tasks"]),
        )
        .req(),
        field("name", "Name", "View name", Node::Str).req(),
        field("filters", "Filters", "Per-view filter tree", Node::Filter),
        field("order", "Columns", "Property ids in column order", STR_LIST),
        field(
            "sort",
            "Sort",
            "Sort keys",
            Node::List(&Node::Record(&SORT)),
        ),
        field("groupBy", "Group by", "Grouping key", Node::Record(&SORT)),
        field(
            "summaries",
            "Summaries",
            "Property id → Sum | Average | Count | Min | Max",
            Node::Map(&Node::Str),
        ),
        field("limit", "Limit", "Row cap", UINT),
        field(
            "columnSize",
            "Column sizes",
            "Display hint only",
            Node::Map(&INT),
        ),
        field("rows", "Rows", "Pivot row key expression", Node::Expr),
        field(
            "rows_label",
            "Rows label",
            "Header of the row-key column",
            Node::Str,
        ),
        field("rows_direction", "Rows direction", "Row order", DIRECTION).dflt(D::Str("ASC")),
        field(
            "columns",
            "Pivot columns",
            "First match wins",
            Node::List(&Node::Record(&PIVOT_COLUMN)),
        ),
        field(
            "value",
            "Value",
            "Value expression aggregated per cell",
            Node::Expr,
        ),
        field(
            "aggregate",
            "Aggregate",
            "Cell aggregate",
            Node::Enum(&["sum", "count"]),
        )
        .dflt(D::Str("sum")),
        field("display", "Display", "Cell display expression", Node::Expr),
        field(
            "total_column",
            "Total column",
            "Label; omitted when unset",
            Node::Str,
        ),
        field(
            "total_row",
            "Total row",
            "Label; omitted when unset",
            Node::Str,
        ),
        field(
            "average_row",
            "Average row",
            "Label; omitted when unset",
            Node::Str,
        ),
        field(
            "status",
            "Task status",
            "Allowed status chars; empty = any",
            STR_LIST,
        ),
        field(
            "require_text",
            "Require text",
            "Drop blank tasks",
            Node::Bool,
        )
        .dflt(D::Bool(true)),
        field(
            "task_filter",
            "Task filter",
            "Per-task predicate",
            Node::Expr,
        ),
        field(
            "task_group",
            "Task group",
            "Task grouping",
            Node::Enum(&["heading", "file", "none"]),
        )
        .dflt(D::Str("heading")),
    ],
    rules: &[],
};

/// One `views/*.toml` (`BaseDef`).
pub static VIEW_FILE: Record = Record {
    name: "views/*.toml",
    fields: &[
        // Plan round-5-reading-pickers-intake WP-10: where rows come from (`views::Source`).
        field(
            "source",
            "Source",
            "Row source: index notes or state.db tracker items",
            Node::Enum(&["index", "tracker_items"]),
        )
        .dflt(D::Str("index")),
        field("filters", "Filters", "Base filter tree", Node::Filter),
        field(
            "formulas",
            "Formulas",
            "Formula name → expression",
            Node::Map(&Node::Expr),
        ),
        field(
            "properties",
            "Properties",
            "Property id → display settings",
            Node::Map(&Node::Record(&PROPERTY_DEF)),
        ),
        field(
            "views",
            "Views",
            "One [[views]] each",
            Node::List(&Node::Record(&VIEW)),
        ),
    ],
    rules: &[],
};

// ---------------------------------------------------------------------------------------------
// editor.toml — editor-owned (plan editor-settings-tab WP-2); defaults mirror the current
// consts: `BASE_TEXT_SIZE` (`crates/editor/src/theme.rs:22`), `Surround` default
// (`theme.rs:30`), `SEARCH_LIMIT`/`SEARCH_DEBOUNCE` (`crates/editor/src/panels/mod.rs:32-34`).
// ---------------------------------------------------------------------------------------------

/// `[search]`.
pub static EDITOR_SEARCH: Record = Record {
    name: "search",
    fields: &[
        field(
            "limit",
            "Result limit",
            "Search panel result cap",
            Node::Int {
                min: Some(1),
                max: Some(1000),
            },
        )
        .dflt(D::Int(50)),
        field(
            "debounce_ms",
            "Debounce (ms)",
            "Search input debounce",
            Node::Int {
                min: Some(0),
                max: Some(5000),
            },
        )
        .dflt(D::Int(250)),
    ],
    rules: &[],
};

/// `[palette]`.
pub static EDITOR_PALETTE: Record = Record {
    name: "palette",
    fields: &[field(
        "hidden",
        "Hidden commands",
        "Command/action ids omitted from the palette (chords still work)",
        STR_LIST,
    )],
    rules: &[],
};

/// `editor.toml`.
pub static EDITOR_FILE: Record = Record {
    name: "editor.toml",
    fields: &[
        field(
            "font_size",
            "Font size",
            "Base text size (px)",
            Node::Float {
                min: Some(8.0),
                max: Some(64.0),
            },
        )
        .dflt(D::Float(22.0)),
        field(
            "ui_scale",
            "UI scale",
            "Window scale factor",
            Node::Float {
                min: Some(0.5),
                max: Some(3.0),
            },
        )
        .dflt(D::Float(1.0)),
        field(
            "surround",
            "Surround",
            "Colour surround",
            Node::Enum(&["dark", "med", "light"]),
        )
        .dflt(D::Str("med")),
        field(
            "tab_title",
            "Tab title",
            "Tab header text: frontmatter title, file stem, vault path, or shortest unambiguous path suffix",
            Node::Enum(&["title", "basename", "path", "unambiguous"]),
        )
        .dflt(D::Str("title")),
        field(
            "quick_switch_open",
            "Quick Switch opens",
            "Quick Switch on a note already open: switch to its tab, move the tab into the active pane, or duplicate it there",
            Node::Enum(&["switch", "move", "duplicate"]),
        )
        .dflt(D::Str("move")),
        field(
            "delete_mode",
            "Delete mode",
            "How Delete note removes files: move to .trash/ or delete permanently (the confirm dialog always asks)",
            Node::Enum(&["trash", "permanent"]),
        )
        .dflt(D::Str("trash")),
        field(
            "focus_follows_mouse",
            "Focus follows mouse",
            "The pointer entering a pane focuses it (never while a modal or menu is open, or during a drag)",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "open_url_cmd",
            "Open URL command",
            "Program + args that open an external http/https/mailto link; the URL is appended as the last argument (no shell). Empty = default",
            STR_LIST,
        )
        .dflt(D::StrList(&["xdg-open"])),
        field(
            "search",
            "Search panel",
            "Search panel tuning",
            Node::Record(&EDITOR_SEARCH),
        ),
        field(
            "palette",
            "Palette",
            "Palette options",
            Node::Record(&EDITOR_PALETTE),
        ),
        field(
            "keys",
            "Key overrides",
            "Command id → chords; [] unbinds",
            Node::Map(&Node::Chords),
        ),
    ],
    rules: &[],
};

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
            Node::Path,
        )
        .dflt(D::Str("_yt_metadata")),
        field(
            "channel_enrich",
            "Channel enrich",
            "Write channel about.md notes",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
    ],
    rules: &[],
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
            Node::Path,
        )
        .dflt(D::Str("_x_metadata")),
        field(
            "inbox",
            "Inbox",
            "Clipper inbox folder x_discover watches",
            Node::Path,
        ),
    ],
    rules: &[],
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
    ],
    rules: &[],
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
        ),
        field("model", "Model", "Model id", Node::Str),
        field("max_tokens", "Max tokens", "Response token cap", U32),
    ],
    rules: &[],
};

/// `[[daily_brief.fx]]`.
pub static PIPE_FX: Record = Record {
    name: "fx",
    fields: &[
        field("base", "Base", "Base currency (ISO 4217)", Node::Str).req(),
        field("quote", "Quote", "Quote currency", Node::Str).req(),
        field("label", "Label", "Default BASE/QUOTE", Node::Str),
    ],
    rules: &[],
};

/// `[[daily_brief.weather]]`.
pub static PIPE_WEATHER: Record = Record {
    name: "weather",
    fields: &[
        field("label", "Label", "Place name", Node::Str).req(),
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
};

/// `[ignore_shorts]`.
pub static PIPE_IGNORE_SHORTS: Record = Record {
    name: "ignore_shorts",
    fields: &[
        field("channels", "Channels", "Channel ids or names", STR_LIST),
        field(
            "max_seconds",
            "Max seconds",
            "A video this short counts as a Short",
            U32,
        )
        .dflt(D::Int(120)),
    ],
    rules: &[],
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
    ],
    rules: &[],
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
        field("id", "Id", "Channel id (UC…)", Node::Str).req(),
        field("tags", "Tags", "Tags", STR_LIST),
        field(
            "priority",
            "Priority",
            "skip/ignore exclude the row",
            PRIORITY,
        )
        .dflt(D::Str("normal")),
    ],
    rules: &[],
};

/// One `[[blog]]` feed.
pub static SOURCE_BLOG: Record = Record {
    name: "blog",
    fields: &[
        field("name", "Name", "Display name", Node::Str).req(),
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
    ],
    rules: &[],
};

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
    ],
    rules: &[],
};

// ---------------------------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------------------------

static ACTIONS_NODE: Node = Node::Record(&ACTIONS_FILE);
static SCHEDULES_NODE: Node = Node::Record(&SCHEDULES_FILE);
static KINDS_NODE: Node = Node::Record(&KINDS_FILE);
static CAPTURES_NODE: Node = Node::Record(&CAPTURES_FILE);
static PROVIDERS_NODE: Node = Node::Record(&PROVIDERS_FILE);
static BOOKMARKS_NODE: Node = Node::Record(&BOOKMARKS_FILE);
static EDITOR_NODE: Node = Node::Record(&EDITOR_FILE);
static VIEW_NODE: Node = Node::Record(&VIEW_FILE);
static PIPELINES_NODE: Node = Node::Record(&PIPELINES_FILE);
static SOURCES_NODE: Node = Node::Record(&SOURCES_FILE);

/// Every config file name with a schema (the daemon's `CONFIG_FILES`, then `bookmarks.toml`,
/// `editor.toml`). Views are per-file: see [`view_schema`].
pub const FILES: [&str; 10] = [
    "actions.toml",
    "schedules.toml",
    "kinds.toml",
    "captures.toml",
    "agents.toml",
    "providers.toml",
    "pipelines.toml",
    "sources.toml",
    "bookmarks.toml",
    "editor.toml",
];

/// The root schema of a config file by name (`"actions.toml"`, …; `"views/NAME.toml"` too).
#[must_use]
pub fn schema_for(file: &str) -> Option<&'static Node> {
    Some(match file {
        "actions.toml" => &ACTIONS_NODE,
        "schedules.toml" => &SCHEDULES_NODE,
        "kinds.toml" => &KINDS_NODE,
        "captures.toml" => &CAPTURES_NODE,
        "agents.toml" => &AGENTS_FILE,
        "providers.toml" => &PROVIDERS_NODE,
        "pipelines.toml" => &PIPELINES_NODE,
        "sources.toml" => &SOURCES_NODE,
        "bookmarks.toml" => &BOOKMARKS_NODE,
        "editor.toml" => &EDITOR_NODE,
        f if f.starts_with("views/") && f.ends_with(".toml") => &VIEW_NODE,
        _ => return None,
    })
}

/// The schema of one `views/*.toml` file.
#[must_use]
pub fn view_schema() -> &'static Node {
    &VIEW_NODE
}

/// The schema of `editor.toml`.
#[must_use]
pub fn editor_schema() -> &'static Node {
    &EDITOR_NODE
}

/// The params record of a step kind by its TOML `type` name.
#[must_use]
pub fn step_schema(kind: &str) -> Option<&'static Record> {
    STEP.variant(kind).map(|v| v.record)
}
