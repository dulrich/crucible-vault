//! Daemon↔client wire protocol (`plans/v0-core-daemon.md` → Public Interfaces).
//!
//! Transport: newline-delimited JSON-RPC 2.0 over a Unix socket at `$CV_SOCKET`. This crate
//! holds only the serde types; it is sync and runtime-free.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Protocol version exchanged in `hello`; a mismatch is refused, never degraded.
pub const PROTOCOL_VERSION: u32 = 1;
/// JSON-RPC version string.
pub const JSONRPC_VERSION: &str = "2.0";

// ---------------------------------------------------------------------------------------------
// JSON-RPC envelope
// ---------------------------------------------------------------------------------------------

/// Request or response id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Num(u64),
    Str(String),
}

/// A client→daemon request: `{"jsonrpc":"2.0","id":..,"method":..,"params":..}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: RpcId,
    #[serde(flatten)]
    pub call: Request,
}

impl RpcRequest {
    pub fn new(id: RpcId, call: Request) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            call,
        }
    }
}

/// A daemon→client response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: RpcId,
    #[serde(flatten)]
    pub outcome: RpcOutcome,
}

/// Either `result` or `error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcOutcome {
    Result(Value),
    Error(RpcError),
}

/// JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Error codes used by the daemon (JSON-RPC reserves -32768..-32000 for its own).
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// `hello` protocol version or index schema mismatch.
    pub const VERSION_MISMATCH: i64 = 1001;
    /// A call before a successful `hello`.
    pub const HANDSHAKE_REQUIRED: i64 = 1002;
    /// Content-hash CAS failure; `data` carries `{current_hash}`.
    pub const CONFLICT: i64 = 1003;
    pub const NOT_FOUND: i64 = 1004;
}

/// A daemon→client notification (no id), sent after `subscribe`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcNotification {
    pub jsonrpc: String,
    #[serde(flatten)]
    pub event: Notification,
}

impl RpcNotification {
    pub fn new(event: Notification) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            event,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------------------------

/// Every RPC method with its params (`method` + `params`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Hello(HelloParams),
    Subscribe(SubscribeParams),
    WriteNote(WriteNoteParams),
    CreateNote(CreateNoteParams),
    WriteAttachment(WriteAttachmentParams),
    Rename(RenameParams),
    Delete(DeleteParams),
    DeletePreview(PathParams),
    DeleteNote(DeleteNoteParams),
    ActionsList,
    RunAction(RunActionParams),
    Job(JobIdParams),
    Cancel(JobIdParams),
    /// Queue listing (plan v0-automation-pipelines WP-11).
    JobsList(JobsListParams),
    /// Re-queue one failed/cancelled job.
    JobRetry(JobIdParams),
    /// Tracker intake rows.
    IntakeList(IntakeListParams),
    Uri(UriParams),
    Status,
    ViewsList,
    ViewRun(ViewRunParams),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    pub protocol_version: u32,
    pub client: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Shadow,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol_version: u32,
    pub index_schema: u32,
    pub mode: Mode,
    pub vault_root: String,
    pub index_db: String,
    pub state_db: String,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    FileChanged,
    IndexGeneration,
    JobUpdated,
    ConfigReloaded,
    OpenRequested,
    ShadowWrite,
    ModeChanged,
    ViewsInvalidated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeParams {
    pub topics: Vec<Topic>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteNoteParams {
    pub path: String,
    pub content: String,
    pub base_hash: String,
}

/// `write_note` result: the new hash, or a CAS conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteNoteResult {
    Written { hash: String },
    Conflict { current_hash: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateNoteParams {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateNoteResult {
    /// Final, collision-suffixed path.
    pub path: String,
}

/// `write_attachment`: binary attachment bytes for `note_path`, stored under the `localize`
/// step's default attachment templates (content-MD5 name, idempotent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteAttachmentParams {
    /// The note the attachment belongs to (drives `{{folder}}`/`{{slug}}`).
    pub note_path: String,
    /// Lowercase file extension without the dot (`[a-z0-9]{1,8}`).
    pub ext: String,
    /// Standard base64 of the bytes.
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteAttachmentResult {
    /// Vault path of the (new or reused) attachment.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameParams {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathParams {
    pub path: String,
}

/// How a delete removes a file: `trash` moves it to `<vault>/.trash/<same path>` (` (n)` suffix
/// on collision; the default and the pre-WP-3 behaviour), `permanent` removes it outright.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    #[default]
    Trash,
    Permanent,
}

/// `delete` params; `mode` is optional on the wire (old clients omit it = `trash`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteParams {
    pub path: String,
    #[serde(default)]
    pub mode: DeleteMode,
}

/// One `delete_preview` item: an embed / non-note link target of the note that exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePreviewItem {
    pub path: String,
    /// Some indexed file other than the note links to it.
    pub referenced_elsewhere: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePreviewResult {
    pub items: Vec<DeletePreviewItem>,
}

/// `delete_note`: delete `path` (CAS delete) and then each listed attachment that nothing
/// other than the note references at that moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteNoteParams {
    pub path: String,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default)]
    pub mode: DeleteMode,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteNoteResult {
    /// Paths removed (the note first).
    pub removed: Vec<String>,
    /// Attachments left in place (referenced elsewhere at delete time, or already gone).
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamKind {
    Text,
    Multiline,
    Path,
    Enum,
    Date,
    Bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamSpec {
    pub name: String,
    pub label: String,
    pub kind: ParamKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionInfo {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
    #[serde(default)]
    pub params: Vec<ParamSpec>,
    /// Context the action needs from the client (e.g. `active_path`, `selection`).
    #[serde(default)]
    pub needs: Vec<String>,
    pub durable: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionContext {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunActionParams {
    pub action_id: String,
    #[serde(default)]
    pub params: serde_json::Map<String, Value>,
    #[serde(default)]
    pub context: ActionContext,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunActionResult {
    pub job_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobIdParams {
    pub job_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Pending,
    Running,
    Done,
    Failed,
    Deferred,
    Cancelled,
}

/// Result of `job` / `cancel`, and payload of `job_updated`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    pub job_id: String,
    pub status: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub output_paths: Vec<String>,
    /// Terminal statuses only: the note the run asks the client to open (e.g. a `move`
    /// destination). Additive (absent from older daemons; omitted when `None`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_path: Option<String>,
}

/// `jobs_list` params. `status`: `queued|running|done|failed|cancelled|deferred`, where
/// `deferred` is queued with a future `defer_until_ms` and `queued` excludes those rows.
/// `limit` defaults to 50 (max 500); `recent_first` defaults to `true`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_first: Option<bool>,
}

/// One queue row in `jobs_list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSummary {
    pub job_id: String,
    pub job_type: String,
    /// `user` | `background` (the store lane name).
    pub lane: String,
    /// `queued|running|done|failed|cancelled|deferred`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub attempts: i64,
    pub created_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_until_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsListResult {
    pub jobs: Vec<JobSummary>,
}

/// `intake_list` params; `day` is `YYYY-MM-DD`, `outcome` `line|note|ignored|skipped`.
/// `limit` defaults to 50 (max 500).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntakeListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// One `tracker_items` row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntakeItem {
    pub source_kind: String,
    pub source_key: String,
    pub item_id: String,
    pub title: String,
    pub url: String,
    pub first_seen_ms: i64,
    pub day: String,
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_body: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntakeListResult {
    pub items: Vec<IntakeItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UriParams {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UriResult {
    Created {
        path: String,
    },
    OpenRequested {
        path: String,
    },
    /// Daemon unreachable; the CLI spooled the URL for the daemon to drain on start.
    Spooled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceHealth {
    pub service: String,
    /// `closed` | `open` | `half_open`.
    pub state: String,
    /// Breaker detail (additive, WP-11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_score: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_at_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusResult {
    pub mode: Mode,
    pub generation: u64,
    #[serde(default)]
    pub queue: std::collections::BTreeMap<String, u64>,
    #[serde(default)]
    pub config_errors: Vec<String>,
    #[serde(default)]
    pub services: Vec<ServiceHealth>,
    /// `tracker_items` row count (additive, WP-11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker_items: Option<u64>,
    /// `link_records` row count (additive, WP-11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_records: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewsListResult {
    pub view_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewRunParams {
    pub view_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub this_path: Option<String>,
}

/// Reserved: the `view_run` result. Plan B (`v0-editor`, Public Interfaces) defines the
/// concrete shape; A1 carries it opaquely so the daemon seam never re-encodes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewResult(pub Value);

// ---------------------------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Daemon,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOp {
    Write,
    Create,
    Rename,
    Delete,
}

/// Every notification with its params (`method` + `params`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Notification {
    FileChanged {
        path: String,
        kind: ChangeKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hash: Option<String>,
        origin: Origin,
    },
    IndexGeneration {
        n: u64,
    },
    JobUpdated(JobStatus),
    ConfigReloaded {
        errors: Vec<String>,
    },
    OpenRequested {
        path: String,
    },
    ShadowWrite {
        op: WriteOp,
        path: String,
    },
    ModeChanged {
        mode: Mode,
    },
    /// Reserved for plan B's view evaluator (hosted via the `ViewService` seam).
    ViewsInvalidated {
        view_ids: Vec<String>,
    },
}

impl Notification {
    pub fn topic(&self) -> Topic {
        match self {
            Self::FileChanged { .. } => Topic::FileChanged,
            Self::IndexGeneration { .. } => Topic::IndexGeneration,
            Self::JobUpdated(_) => Topic::JobUpdated,
            Self::ConfigReloaded { .. } => Topic::ConfigReloaded,
            Self::OpenRequested { .. } => Topic::OpenRequested,
            Self::ShadowWrite { .. } => Topic::ShadowWrite,
            Self::ModeChanged { .. } => Topic::ModeChanged,
            Self::ViewsInvalidated { .. } => Topic::ViewsInvalidated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde_json::json;

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(v: &T) -> Value {
        let s = serde_json::to_string(v).expect("serialize");
        let back: T = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(&back, v);
        serde_json::from_str(&s).expect("value")
    }

    #[test]
    fn hello_request_wire_shape() {
        let req = RpcRequest::new(
            RpcId::Num(1),
            Request::Hello(HelloParams {
                protocol_version: PROTOCOL_VERSION,
                client: "cv".into(),
            }),
        );
        let v = round_trip(&req);
        assert_eq!(
            v,
            json!({"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol_version":1,"client":"cv"}})
        );
    }

    #[test]
    fn unit_requests_round_trip() {
        for call in [Request::ActionsList, Request::Status, Request::ViewsList] {
            round_trip(&RpcRequest::new(RpcId::Str("a".into()), call));
        }
    }

    #[test]
    fn all_requests_round_trip() {
        let calls = vec![
            Request::Subscribe(SubscribeParams {
                topics: vec![Topic::FileChanged, Topic::ViewsInvalidated],
            }),
            Request::WriteNote(WriteNoteParams {
                path: "a.md".into(),
                content: "x".into(),
                base_hash: "h".into(),
            }),
            Request::CreateNote(CreateNoteParams {
                path: "b.md".into(),
                content: String::new(),
            }),
            Request::WriteAttachment(WriteAttachmentParams {
                note_path: "b.md".into(),
                ext: "png".into(),
                data_base64: "AA==".into(),
            }),
            Request::Rename(RenameParams {
                from: "a.md".into(),
                to: "c.md".into(),
            }),
            Request::Delete(DeleteParams {
                path: "c.md".into(),
                mode: DeleteMode::Permanent,
            }),
            Request::DeletePreview(PathParams {
                path: "c.md".into(),
            }),
            Request::DeleteNote(DeleteNoteParams {
                path: "c.md".into(),
                attachments: vec!["img.png".into()],
                mode: DeleteMode::Trash,
            }),
            Request::RunAction(RunActionParams {
                action_id: "capture.daily".into(),
                params: serde_json::Map::from_iter([("k".to_owned(), json!("v"))]),
                context: ActionContext {
                    active_path: Some("a.md".into()),
                    line: Some(3),
                    ..ActionContext::default()
                },
            }),
            Request::Job(JobIdParams {
                job_id: "j1".into(),
            }),
            Request::Cancel(JobIdParams {
                job_id: "j1".into(),
            }),
            Request::JobsList(JobsListParams {
                status: Some("deferred".into()),
                job_type: Some("t".into()),
                limit: Some(5),
                offset: Some(1),
                recent_first: Some(false),
            }),
            Request::JobsList(JobsListParams::default()),
            Request::JobRetry(JobIdParams {
                job_id: "j1".into(),
            }),
            Request::IntakeList(IntakeListParams {
                source_kind: Some("youtube".into()),
                source_key: Some("ch".into()),
                day: Some("2026-10-15".into()),
                outcome: Some("line".into()),
                limit: Some(2),
                offset: Some(0),
            }),
            Request::Uri(UriParams {
                url: "obsidian://new?file=x".into(),
            }),
            Request::ViewRun(ViewRunParams {
                view_id: "v".into(),
                this_path: None,
            }),
        ];
        for (i, call) in calls.into_iter().enumerate() {
            round_trip(&RpcRequest::new(RpcId::Num(i as u64), call));
        }
    }

    #[test]
    fn delete_without_mode_is_trash() {
        let v: RpcRequest = serde_json::from_value(
            json!({"jsonrpc":"2.0","id":1,"method":"delete","params":{"path":"a.md"}}),
        )
        .expect("parse");
        assert_eq!(
            v.call,
            Request::Delete(DeleteParams {
                path: "a.md".into(),
                mode: DeleteMode::Trash,
            })
        );
        let v: RpcRequest = serde_json::from_value(
            json!({"jsonrpc":"2.0","id":2,"method":"delete_note","params":{"path":"a.md"}}),
        )
        .expect("parse");
        let Request::DeleteNote(p) = v.call else {
            panic!("wrong variant")
        };
        assert_eq!(p.mode, DeleteMode::Trash);
        assert!(p.attachments.is_empty());
    }

    #[test]
    fn run_action_defaults_context_and_params() {
        let v: RpcRequest = serde_json::from_value(
            json!({"jsonrpc":"2.0","id":2,"method":"run_action","params":{"action_id":"x"}}),
        )
        .expect("parse");
        let Request::RunAction(p) = v.call else {
            panic!("wrong variant")
        };
        assert!(p.params.is_empty());
        assert_eq!(p.context, ActionContext::default());
    }

    #[test]
    fn responses_round_trip() {
        let ok = RpcResponse {
            jsonrpc: JSONRPC_VERSION.into(),
            id: RpcId::Num(1),
            outcome: RpcOutcome::Result(json!({"hash":"abc"})),
        };
        assert_eq!(
            round_trip(&ok),
            json!({"jsonrpc":"2.0","id":1,"result":{"hash":"abc"}})
        );
        let err = RpcResponse {
            jsonrpc: JSONRPC_VERSION.into(),
            id: RpcId::Num(2),
            outcome: RpcOutcome::Error(RpcError {
                code: error_codes::VERSION_MISMATCH,
                message: "index_schema mismatch".into(),
                data: None,
            }),
        };
        round_trip(&err);
    }

    #[test]
    fn results_round_trip() {
        round_trip(&HelloResult {
            protocol_version: 1,
            index_schema: 1,
            mode: Mode::Shadow,
            vault_root: "/v".into(),
            index_db: "/i.db".into(),
            state_db: "/s.db".into(),
            generation: 7,
        });
        assert_eq!(
            round_trip(&WriteNoteResult::Conflict {
                current_hash: "h2".into()
            }),
            json!({"conflict":{"current_hash":"h2"}})
        );
        round_trip(&WriteNoteResult::Written { hash: "h".into() });
        round_trip(&CreateNoteResult {
            path: "x 1.md".into(),
        });
        round_trip(&WriteAttachmentResult {
            path: "_attachments/x/a_MD5.png".into(),
        });
        round_trip(&vec![ActionInfo {
            id: "a".into(),
            title: "A".into(),
            group: None,
            keys: vec![],
            params: vec![ParamSpec {
                name: "p".into(),
                label: "P".into(),
                kind: ParamKind::Enum,
                required: true,
                choices: vec!["x".into()],
                default: None,
            }],
            needs: vec!["active_path".into()],
            durable: true,
        }]);
        round_trip(&RunActionResult {
            job_id: "j".into(),
            open_path: Some("d.md".into()),
        });
        round_trip(&UriResult::Spooled);
        round_trip(&UriResult::Created {
            path: "n.md".into(),
        });
        round_trip(&StatusResult {
            mode: Mode::Live,
            generation: 1,
            queue: [("pending".to_owned(), 2)].into_iter().collect(),
            config_errors: vec![],
            services: vec![ServiceHealth {
                service: "search".into(),
                state: "open".into(),
                failure_score: Some(3.0),
                retry_at_ms: Some(42),
                last_reason: Some("transient Timeout".into()),
            }],
            tracker_items: Some(4),
            link_records: Some(9),
        });
        // An older daemon's status (no WP-11 fields) still parses.
        let old: StatusResult = serde_json::from_value(json!({
            "mode": "live", "generation": 1, "services": [{"service": "s", "state": "closed"}]
        }))
        .expect("old status");
        assert_eq!(old.tracker_items, None);
        assert_eq!(old.services[0].failure_score, None);
        round_trip(&JobsListResult {
            jobs: vec![JobSummary {
                job_id: "j".into(),
                job_type: "t".into(),
                lane: "background".into(),
                status: "deferred".into(),
                service: Some("x".into()),
                attempts: 2,
                created_ms: 1,
                settled_at_ms: None,
                defer_until_ms: Some(5),
                error: Some("e".into()),
                failure_kind: None,
            }],
        });
        round_trip(&IntakeListResult {
            items: vec![IntakeItem {
                source_kind: "blog".into(),
                source_key: "f".into(),
                item_id: "i".into(),
                title: "t".into(),
                url: "u".into(),
                first_seen_ms: 1,
                day: "2026-10-15".into(),
                outcome: "line".into(),
                score: Some(0.5),
                full_body: Some(true),
            }],
        });
        round_trip(&ViewsListResult { view_ids: vec![] });
        let vr = ViewResult(json!({"rows":[]}));
        assert_eq!(round_trip(&vr), json!({"rows":[]}));
    }

    #[test]
    fn job_status_open_path_is_additive() {
        // Old daemon payload (no open_path) still parses.
        let old: JobStatus =
            serde_json::from_value(json!({"job_id":"j","status":"done","output_paths":[]}))
                .expect("old");
        assert_eq!(old.open_path, None);
        // None is omitted, so old clients see the exact old shape.
        let v = serde_json::to_value(&old).expect("ser");
        assert!(v.get("open_path").is_none());
        // Some round-trips; an old client ignoring unknown fields still parses it.
        let new = JobStatus {
            open_path: Some("k/a.md".into()),
            ..old
        };
        assert_eq!(round_trip(&new)["open_path"], "k/a.md");
    }

    #[test]
    fn notifications_round_trip_and_topic() {
        let all = vec![
            Notification::FileChanged {
                path: "a.md".into(),
                kind: ChangeKind::Modified,
                hash: Some("h".into()),
                origin: Origin::External,
            },
            Notification::IndexGeneration { n: 3 },
            Notification::JobUpdated(JobStatus {
                job_id: "j".into(),
                status: JobState::Deferred,
                message: None,
                output_paths: vec!["o.md".into()],
                open_path: None,
            }),
            Notification::ConfigReloaded { errors: vec![] },
            Notification::OpenRequested {
                path: "a.md".into(),
            },
            Notification::ShadowWrite {
                op: WriteOp::Rename,
                path: "a.md".into(),
            },
            Notification::ModeChanged { mode: Mode::Shadow },
            Notification::ViewsInvalidated {
                view_ids: vec!["v".into()],
            },
        ];
        for n in all {
            let topic = n.topic();
            let v = round_trip(&RpcNotification::new(n));
            assert_eq!(v["method"], serde_json::to_value(topic).expect("topic"));
            assert!(v.get("id").is_none());
        }
    }
}
