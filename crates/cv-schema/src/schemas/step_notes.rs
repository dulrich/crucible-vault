//! Step-kind schemas, part 1: the note-editing steps (`guard` … `localize`).

use super::*;

// ---------------------------------------------------------------------------------------------
// Step kinds (`cv_actions::StepKind::ALL`)
// ---------------------------------------------------------------------------------------------

/// `guard` — `cv::steps::file` (`StepModule::parse` → `file::parse`; keys via
/// `cv::steps::params::check_keys`).
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
    summary: None,
};

/// `capture` — `cv::steps::capture` (`StepModule::parse`; keys via
/// `cv::steps::params::check_keys`, `[[capture]]` merge in `capture::parse_merged`). Not modelled: `target = "path"` needs `path`, and
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
    summary: None,
};

/// Every `target` value `capture.rs` `parse_target` accepts (canonical first, then aliases).
pub(super) const CAPTURE_TARGETS: &[&str] = &[
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

/// `materialize` — `cv::steps::materialize` (`StepModule::parse`; keys via
/// `cv::steps::params::check_keys`, period settings in `materialize::period`). `period` stays a string: it
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
            Node::Folder,
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
    summary: None,
};

/// `move` — `cv::steps::file::parse`.
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
    summary: None,
};

/// `copy` — `cv::steps::file::parse`.
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
    summary: None,
};

/// `set_props` — `cv::steps::file::parse`.
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
    summary: None,
};

/// `remove_props` — `cv::steps::file::parse`.
pub static REMOVE_PROPS: Record = Record {
    name: "remove_props",
    fields: &[
        PATH_FIELD,
        field("props", "Properties", "Keys to remove", STR_OR_LIST),
    ],
    rules: &[],
    summary: None,
};

/// `upsert_tags` — `cv::steps::file::parse`.
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
    summary: None,
};

/// `lint` — `cv::steps::lint` (`StepModule::parse`; keys via `cv::steps::params::check_keys`). `steps` keys
/// must be `cv_note::LINT_STEPS` ids (not modelled).
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
    summary: None,
};

/// `localize` — `cv::steps::localize::params::parse_params` (keys via
/// `cv::steps::params::check_keys`). Not modelled: templates must not use
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
        // Mode overrides of vault.toml `[remote_media]` (plan remote-media-sprint WP-2); the
        // parser also accepts the legacy bools, which this vocabulary cannot express.
        field(
            "images",
            "Images",
            "Override of Remote content images; unset uses vault.toml",
            Node::Enum(&["localize"]),
        ),
        field(
            "audio",
            "Audio",
            "Override of Remote content audio; unset uses vault.toml",
            Node::Enum(&["link", "localize"]),
        ),
        field(
            "video",
            "Video",
            "Override of Remote content video; unset uses vault.toml",
            Node::Enum(&["thumbnail", "link"]),
        ),
        field(
            "pdf",
            "PDF",
            "Override of Remote content PDFs; unset uses vault.toml",
            Node::Enum(&["link", "localize"]),
        ),
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
    summary: None,
};
