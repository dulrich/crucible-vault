//! `kinds.toml`, `captures.toml` and `agents.toml` schemas.

use super::*;

// ---------------------------------------------------------------------------------------------
// kinds.toml — `cv::steps::file::kind_def` (keys `file::KIND_KEYS`)
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
            Node::Folder,
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
        field(
            "hosts",
            "Hosts",
            "Clip hosts auto_file files under this kind (www. ignored)",
            STR_LIST,
        ),
        field(
            "works",
            "In Works",
            "Publications of this kind are listed in the Works view unless the publication sets its own `works`",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
    ],
    rules: &[],
    summary: None,
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
    summary: None,
};

// ---------------------------------------------------------------------------------------------
// captures.toml — periods `cv::steps::materialize::period`, `[[capture]]`
// `cv::steps::capture::parse_merged`
// ---------------------------------------------------------------------------------------------

/// One `[periods.P]`.
pub static PERIOD: Record = Record {
    name: "period",
    fields: &[
        field("folder", "Folder", "Period notes folder", Node::Folder).req(),
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
    summary: None,
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
    summary: None,
};

/// One `[[capture]]` entry: the capture step's keys minus `capture` (`cv::steps::capture`).
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
            Node::Enum(super::step_notes::CAPTURE_TARGETS),
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
    summary: None,
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
    summary: None,
};

// ---------------------------------------------------------------------------------------------
// agents.toml — `cv::steps::agent::AgentDef` / `agent::agent_def`
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
        field(
            "model",
            "Model",
            "Default provider:model (live list per provider)",
            Node::Str,
        )
        .hint(RefKind::ModelRef),
        field("max_tokens", "Max tokens", "Completion cap (> 0)", POS_U32).req(),
    ],
    rules: &[
        Rule::AtMostOneOf(&["system", "system_file"]),
        Rule::AtMostOneOf(&["prompt", "prompt_file"]),
    ],
    summary: None,
};

/// `agents.toml`: agent id → definition at the root.
pub static AGENTS_FILE: Node = Node::Map(&Node::Record(&AGENT_DEF));
