//! Daemon↔client wire protocol (`plans/v0-core-daemon.md` → Public Interfaces).
//!
//! Transport: newline-delimited JSON-RPC 2.0 over a Unix socket at `$CV_SOCKET`. This crate
//! holds only the serde types; it is sync and runtime-free.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod client;
mod enums;
mod jobs;
pub mod methods;
mod note;
pub use enums::{
    BreakerState, ContextNeed, FailureKind, Lane, TrackerOutcome, TrashLocation, TrashReason,
};
pub use jobs::*;
pub use methods::{METHOD_NAMES, Method};
pub use note::{
    BindNoteParams, BindNoteResult, ChangeKind, FileChange, LocateNoteParams, NoteId, NoteLocation,
    Origin, file_changed,
};

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
    /// The vault refused the operation by policy (e.g. a refused write through a symlinked note).
    pub const REFUSED: i64 = 1005;
    /// The daemon cannot serve the call right now (shutting down, writer gone, runner not up).
    pub const UNAVAILABLE: i64 = 1006;
    /// The note is held by a running daemon job that will replace it wholesale (plan round-18
    /// Decision 2): the request would change it and is refused, nothing mutated. `data` carries
    /// `{ "path": <held path>, "label": <holding action label> }`.
    pub const HELD: i64 = 1007;
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
    /// Per-type wait/run bucket percentiles (plan job-telemetry WP-2).
    JobsStats(JobsStatsParams),
    /// One job's stored telemetry timeline.
    JobTimeline(JobIdParams),
    /// Tracker intake rows.
    IntakeList(IntakeListParams),
    Uri(UriParams),
    Status,
    ViewsList,
    ViewRun(ViewRunParams),
    /// Frankfurter currency list (plan round-5-reading-pickers-intake WP-5) → `[LookupCurrency]`.
    LookupCurrencies,
    /// Open-Meteo place search → `[LookupGeoResult]`.
    LookupGeocode(LookupGeocodeParams),
    /// A provider's model list → [`LookupModelsResult`].
    LookupModels(LookupModelsParams),
    /// YouTube channel titles → `[LookupChannel]`.
    LookupChannels(LookupChannelsParams),
    /// Hybrid search over the daemon's `semsearch` index (plan semsearch-crate WP-4) →
    /// [`SearchResult`].
    Search(SearchParams),
    /// Sets one `tracker_items` row's task status (`None` = open; plan round-6-feedback-todos
    /// WP-6) → `null`; unknown rows are `NOT_FOUND`.
    TrackerItemSetStatus(TrackerItemSetStatusParams),
    /// Sets one boolean frontmatter key of a note (plan round-8-intake-views-anchor WP-3) →
    /// [`NoteSetBoolResult`]; the key / frontmatter block is created if absent. Missing notes
    /// are `NOT_FOUND`.
    NoteSetBool(NoteSetBoolParams),
    /// A client's memory snapshot (plan memory-instrumentation-and-reduction WP-M1) → `null`;
    /// the daemon keeps the latest one per process kind and reports it in `status`. An old
    /// daemon answers `METHOD_NOT_FOUND`, which a client drops silently.
    MemoryReport(MemoryReport),
    /// Runs the `intake-reconcile` action and waits (bounded) for it (plan auto-intake-sprint
    /// WP-6) → [`ReconcileIntakeResult`]. An unknown action is `NOT_FOUND`.
    ReconcileIntake(ReconcileIntakeParams),
    /// The id of the file now at `path` (plan note-identity D2), minted if it has none →
    /// [`BindNoteResult`]. Answered on the daemon's writer thread. A path with no file is
    /// `NOT_FOUND` (never a mint). An old daemon answers `METHOD_NOT_FOUND`.
    BindNote(BindNoteParams),
    /// Where the note `id` is now (plan note-identity D2) → [`NoteLocation`]. Answered on the
    /// daemon's writer thread, so it observes every mutation that returned before it.
    LocateNote(LocateNoteParams),
}

/// `reconcile_intake` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileIntakeParams {
    /// Compute every count but write nothing.
    #[serde(default)]
    pub dry_run: bool,
}

/// `reconcile_intake` result: the job's terminal (or, after the bounded wait, last) state and
/// its aggregate outcome counts (never note text or paths).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileIntakeResult {
    pub job_id: String,
    pub status: JobState,
    /// The failure text of a failed job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The step's outcome vars (`null_paths`, `linked`, …); empty unless the job completed.
    #[serde(default)]
    pub counts: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    pub protocol_version: u32,
    pub client: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol_version: u32,
    pub index_schema: u32,
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
    /// Plan note-identity D2: the note to write. When present the daemon writes at the id's
    /// current path (ignoring a stale `path`), or revives a retired id at `path` when
    /// `base_hash` is empty (D3); an unresolvable id fails, never falling back to `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NoteId>,
}

/// `write_note` result: the new hash, or a CAS conflict. `path` (by-id writes only) is the path
/// the daemon acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteNoteResult {
    Written {
        hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Conflict {
        current_hash: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
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
    /// Plan note-identity D2: the note to rename. When present the daemon renames the id's
    /// current path (ignoring a stale `from`); an unresolvable id fails.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NoteId>,
}

/// `delete_preview` params (by path, or by id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathParams {
    pub path: String,
    /// By-id addressing (plan tn-review-2026-11-10-remediation Decision 2): the note acted on is
    /// the id's live path (a stale `path` is ignored); an unresolvable id fails, never falling
    /// back to `path`. Absent = by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NoteId>,
}

/// How a delete removes a file: `trash` moves it to the trash `vault.toml` `[trash] location`
/// names (default `<vault>/.trash/<same path>`, ` (n)` suffix on collision; or the system trash),
/// `permanent` removes it outright.
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
    /// By-id addressing (plan tn-review-2026-11-10-remediation Decision 2): the note acted on is
    /// the id's live path (a stale `path` is ignored); an unresolvable id fails, never falling
    /// back to `path`. Absent = by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NoteId>,
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
    /// By-id requests only: the path acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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
    /// By-id addressing (plan tn-review-2026-11-10-remediation Decision 2): the note acted on is
    /// the id's live path (a stale `path` is ignored); an unresolvable id fails, never falling
    /// back to `path`. Absent = by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NoteId>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteNoteResult {
    /// Paths removed (the note first).
    pub removed: Vec<String>,
    /// Attachments left in place (referenced elsewhere at delete time, or already gone).
    pub skipped: Vec<String>,
    /// By-id requests only: the path acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
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
    pub needs: Vec<ContextNeed>,
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
    /// The cursor's line in `active_path`, **0-based** (the first line is 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_hash: Option<String>,
    /// Plan note-identity D5: the id of the note at `active_path` (the runner binds it at enqueue
    /// when absent; a job without one runs unbound).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_id: Option<NoteId>,
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
    pub outcome: TrackerOutcome,
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
    pub state: BreakerState,
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
    /// Memory snapshot (additive, plan memory-instrumentation-and-reduction WP-M1); boxed so
    /// the status result stays small inside client message enums (serde-transparent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Box<MemoryStatus>>,
}

/// One process's memory snapshot (plan memory-instrumentation-and-reduction WP-M1). The wire
/// mirror of the vendored `memstat::Snapshot` (which is Serialize-only); all sizes in bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReport {
    #[serde(default)]
    pub uptime_secs: u64,
    /// `VmRSS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rss: Option<u64>,
    /// `Pss_Anon` (smaps_rollup).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anon: Option<u64>,
    /// `VmSwap`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap: Option<u64>,
    /// `VmHWM` (peak RSS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hwm: Option<u64>,
    /// glibc `mallinfo2` live bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub malloc_in_use: Option<u64>,
    /// glibc `mallinfo2` bytes held from the OS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub malloc_system: Option<u64>,
    #[serde(default)]
    pub gauges: Vec<GaugeValue>,
}

/// One named gauge reading: a value or the error that replaced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaugeValue {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `status.memory`: the daemon's own snapshot plus the editor's latest report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryStatus {
    pub daemon: MemoryReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<ReportedMemory>,
}

/// A client-reported snapshot and its age at the time of the `status` call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportedMemory {
    pub report: MemoryReport,
    pub age_secs: u64,
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
    /// The editor's column-header sort (plan round-11 WP-5): the primary key of a table view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<ViewSort>,
    /// Plan round-13 WP-4: the host's in-place selection (`property == value`), ANDed with the
    /// view's filters before grouping, sorting, paging and summaries. Absent = unfiltered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub select: Option<ViewSelect>,
}

/// A UI selection filter (plan round-13 WP-4): rows whose `property` equals `value`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewSelect {
    pub property: String,
    pub value: String,
}

/// A UI sort over one table column: `property` is the column key (its `order` id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewSort {
    pub property: String,
    pub direction: ViewSortDir,
}

/// Direction of a [`ViewSort`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewSortDir {
    Asc,
    Desc,
}

/// Reserved: the `view_run` result. Plan B (`v0-editor`, Public Interfaces) defines the
/// concrete shape; A1 carries it opaquely so the daemon seam never re-encodes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ViewResult(pub Value);

/// One `lookup_currencies` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupCurrency {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupGeocodeParams {
    pub query: String,
}

/// One `lookup_geocode` hit: `label` is `City, CC`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LookupGeoResult {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin1: Option<String>,
    pub country_code: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupModelsParams {
    /// A `providers.toml` provider id.
    pub provider: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// `lookup_models` result: `live = false` means the provider was unreachable (or has no listing
/// endpoint) and `models` are the ones declared in `providers.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupModelsResult {
    pub models: Vec<LookupModel>,
    pub live: bool,
}

/// `search` params (plan semsearch-crate WP-4); the daemon clamps `limit` to `1..=1000`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchParams {
    pub query: String,
    pub limit: u32,
}

/// One `search` hit; `path` is the vault-relative note path (the index source).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub title: String,
    pub heading: String,
    pub snippet: String,
    pub score: f64,
}

/// `search` result. `semantic` is true only when the vector leg ran (the query was embedded in
/// the index's recorded space); false means keyword-only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    pub semantic: bool,
}

/// `tracker_item_set_status` params: the row key plus the new status (`None` = open).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackerItemSetStatusParams {
    pub source_kind: String,
    pub source_key: String,
    pub item_id: String,
    pub status: Option<char>,
}

/// `note_set_bool` params: vault-relative note path, frontmatter key, new value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSetBoolParams {
    pub path: String,
    pub key: String,
    pub value: bool,
    /// By-id addressing (plan tn-review-2026-11-10-remediation Decision 2): the note acted on is
    /// the id's live path (a stale `path` is ignored); an unresolvable id fails, never falling
    /// back to `path`. Absent = by path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NoteId>,
}

/// `note_set_bool` result: the note's hash after the call (unchanged when already set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteSetBoolResult {
    pub hash: String,
    /// By-id requests only: the path acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `subscribe` result: every topic the connection now receives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeResult {
    pub topics: Vec<Topic>,
}

/// `rename` result: notes whose links were rewritten, and those skipped on a CAS conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameResult {
    pub rewritten: Vec<String>,
    pub skipped_conflicts: Vec<String>,
    /// By-id renames only: the source path the daemon renamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// `delete` result (an empty object for a by-path request).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteResult {
    /// By-id requests only: the path acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupChannelsParams {
    pub ids: Vec<String>,
}

/// One `lookup_channels` entry (ids YouTube does not know are absent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LookupChannel {
    pub id: String,
    pub title: String,
}

// ---------------------------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------------------------

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
        /// Round 9 WP-3: on the `Renamed` half of a rename pair, the old path.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        /// Round 11 WP-1 (Decision 1): on a `Deleted`, the note that absorbed this one (an
        /// auto-file replacement or containment trash); a client re-points the deleted path's
        /// views at it instead of reporting a deletion. Old peers ignore / omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaced_by: Option<String>,
        /// Plan note-identity D2: the id of the note this change concerns (both halves of a
        /// rename carry it; a delete carries the retired id). Old peers ignore / omit it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<NoteId>,
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
    /// Reserved for plan B's view evaluator (hosted on the daemon's view thread).
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
            Self::ViewsInvalidated { .. } => Topic::ViewsInvalidated,
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
