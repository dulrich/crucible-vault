//! `vault.toml` schema — `VaultConfig`, `crates/cv-actions/src/vault.rs` (plan
//! tidiness-trash-orphans WP-1).

use super::*;

/// `[trash]`.
pub static VAULT_TRASH: Record = Record {
    name: "trash",
    fields: &[
        field(
            "location",
            "Location",
            "Where recoverable deletes go: the vault's own .trash/ folder, or the system (desktop) trash",
            Node::Enum(&["vault", "system"]),
        )
        .dflt(D::Str("vault")),
        field(
            "retention_days",
            "Retention (days)",
            "Trashed files are deleted permanently after this many days; 0 = never",
            UINT,
        )
        .dflt(D::Int(30)),
    ],
    rules: &[],
    summary: None,
};

/// `[remote_media]` — `RemoteMediaConfig`, `crates/cv-actions/src/vault.rs` (plan
/// remote-media-sprint WP-2). Not modelled: templates must not use `{{datetime:…}}`, and
/// `name_template` must contain `{{md5}}`.
pub static VAULT_REMOTE_MEDIA: Record = Record {
    name: "remote_media",
    fields: &[
        field(
            "images",
            "Images",
            "Remote images are downloaded into the vault.",
            Node::Enum(&["localize"]),
        )
        .dflt(D::Str("localize")),
        field(
            "video",
            "Video",
            "Show remote video as a thumbnail with its link, or as a plain link.",
            Node::Enum(&["thumbnail", "link"]),
        )
        .dflt(D::Str("thumbnail")),
        field(
            "audio",
            "Audio",
            "Keep remote audio as a link, or download it into the vault.",
            Node::Enum(&["link", "localize"]),
        )
        .dflt(D::Str("link")),
        field(
            "pdf",
            "PDF",
            "Keep remote PDFs as a link, or download them into the vault.",
            Node::Enum(&["link", "localize"]),
        )
        .dflt(D::Str("link")),
        field(
            "images_whitelist",
            "Image extensions",
            "Image file extensions that are handled; unset means the built-in list.",
            STR_LIST,
        ),
        field(
            "audio_whitelist",
            "Audio extensions",
            "Audio file extensions that are handled; unset means the built-in list.",
            STR_LIST,
        ),
        field(
            "video_whitelist",
            "Video extensions",
            "Video file extensions that are handled; unset means the built-in list.",
            STR_LIST,
        ),
        field(
            "pdf_whitelist",
            "PDF extensions",
            "PDF file extensions that are handled; unset means the built-in list.",
            STR_LIST,
        ),
        field(
            "convert_images",
            "Convert images",
            "Re-encode downloaded images and keep the result when it is smaller.",
            Node::Bool,
        )
        .dflt(D::Bool(false)),
        field(
            "image_format",
            "Image format",
            "The format converted images are saved in.",
            Node::Enum(&["webp", "jpeg"]),
        )
        .dflt(D::Str("webp")),
        field(
            "image_quality",
            "Image quality",
            "JPEG quality, clamped to 30 to 100; unset means 85.",
            Node::Float {
                min: None,
                max: None,
            },
        ),
        field(
            "folder_template",
            "Folder template",
            "The folder downloaded files are saved in.",
            Node::Str,
        )
        .dflt(D::Str("{{folder}}/_attachments/{{slug}}")),
        field(
            "name_template",
            "Name template",
            "The file name of a downloaded file, which must contain {{md5}}.",
            Node::Str,
        )
        .dflt(D::Str("{{md5}}_MD5.{{ext}}")),
        field(
            "timeout_secs",
            "Timeout (seconds)",
            "How long one download may take before it is abandoned.",
            POS_INT,
        )
        .dflt(D::Int(30)),
        field(
            "max_bytes",
            "Max size (bytes)",
            "Downloads larger than this are skipped.",
            POS_INT,
        )
        .dflt(D::Int(50 * 1024 * 1024)),
    ],
    rules: &[],
    summary: None,
};

/// `[structure]` — `StructureConfig`, `crates/cv-actions/src/vault.rs` (plan public-release
/// WP-2): the machinery folders the daemon's steps read.
pub static VAULT_STRUCTURE: Record = Record {
    name: "structure",
    fields: &[
        field(
            "app_folder",
            "App folder",
            "The app's own folder (review notes); excluded from scans by default",
            Node::Str,
        )
        .dflt(D::Str("_crucible")),
        field(
            "exclude",
            "Excluded folders",
            "Folders the duplicate scan and the link scan skip (a link-scan step's own exclude wins); unset = .trash, _custom, the app folder and the pipeline metadata roots",
            STR_LIST,
        ),
        field(
            "raw_transcript_folder",
            "Raw transcript folder",
            "Where transcript refining keeps the original note",
            Node::Str,
        )
        .dflt(D::Str("_raw_transcript")),
    ],
    rules: &[],
    summary: None,
};

/// A weak training importance, 0..=5.
const IMPORTANCE: Node = Node::Int {
    min: Some(0),
    max: Some(5),
};

/// `[signals.labels.<tag>]` — `LabelRuleConfig`, `crates/cv-actions/src/vault.rs` (plan
/// public-release WP-4, C#27).
pub static SIGNALS_LABEL: Record = Record {
    name: "label",
    fields: &[
        field(
            "value",
            "Fixed importance",
            "A capture with this tag gets this weak importance (the lowest matching value wins)",
            IMPORTANCE,
        ),
        field(
            "floor",
            "Importance floor",
            "A capture with this tag gets at least this weak importance (applied last)",
            IMPORTANCE,
        ),
        field(
            "ceiling",
            "Importance ceiling",
            "Caps an importance another rule set; never makes a row on its own",
            IMPORTANCE,
        ),
    ],
    rules: &[],
    summary: None,
};

/// `[signals.observations]` — `ObservationsConfig` (C#29). Absent = off.
pub static SIGNALS_OBSERVATIONS: Record = Record {
    name: "observations",
    fields: &[
        field(
            "heading",
            "Section heading",
            "The heading (with its #s) whose bullets cite captures, e.g. # Observations",
            Node::Str,
        )
        .req(),
        field(
            "period",
            "Period",
            "The captures.toml period whose folder is scanned",
            Node::Str,
        )
        .dflt(D::Str("month")),
        field(
            "floor",
            "Importance floor",
            "A cited capture gets at least this weak importance",
            IMPORTANCE,
        )
        .dflt(D::Int(4)),
    ],
    rules: &[],
    summary: None,
};

/// `[signals]` — `SignalsConfig` (plan public-release WP-4): the training-export label
/// vocabulary; absent = no label tags, no Observations signal.
pub static VAULT_SIGNALS: Record = Record {
    name: "signals",
    fields: &[
        field(
            "labels",
            "Label tags",
            "Tags carried into training labels, each with an optional importance rule",
            Node::Map(&Node::Record(&SIGNALS_LABEL)),
        ),
        field(
            "observations",
            "Observations signal",
            "Period-note sections whose links raise a capture's importance; unset = off",
            Node::Record(&SIGNALS_OBSERVATIONS),
        ),
    ],
    rules: &[],
    summary: None,
};

/// `vault.toml`.
pub static VAULT_FILE: Record = Record {
    name: "vault.toml",
    fields: &[
        field(
            "trash",
            "Trash",
            "Trash location and retention",
            Node::Record(&VAULT_TRASH),
        ),
        field(
            "structure",
            "Vault structure",
            "The app folder, the excluded folders and the raw transcript folder",
            Node::Record(&VAULT_STRUCTURE),
        ),
        field(
            "remote_media",
            "Remote content",
            "How remote images, video, audio and PDFs are handled",
            Node::Record(&VAULT_REMOTE_MEDIA),
        ),
        field(
            "signals",
            "Training signals",
            "Label tags and the Observations signal of the training export",
            Node::Record(&VAULT_SIGNALS),
        ),
    ],
    rules: &[],
    summary: None,
};
