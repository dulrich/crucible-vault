//! The editor's view of the core daemon (plan `v0-editor.md` WP-1, D1).
//!
//! [`DaemonClient`] covers the `cv-protocol` RPC surface (the daemon is the sole writer) plus the
//! local read API: note bytes straight from disk, a [`LocalIndex`] over `index.db` reopened on
//! `index_generation`, and a read-only [`StateReader`] over `state.db`. Two implementations:
//! [`SocketDaemon`] (the real Unix-socket client) and [`FakeDaemon`] (directory-backed, for
//! building editor features without the daemon's action layer).

mod fake;
mod local;
pub mod socket;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

pub use cv_protocol::{
    ActionInfo, DeleteMode, DeleteNoteResult, DeletePreviewItem, HelloResult, JobStatus,
    LookupChannel, LookupCurrency, LookupGeoResult, LookupModel, LookupModelsResult, Mode,
    Notification, RunActionParams, RunActionResult, StatusResult, Topic, ViewResult,
    ViewsListResult,
};
pub use cv_state::StateReader;
pub use cv_vault::ContentHash;
pub use fake::{ActionHandler, FakeDaemon, FakeLookups};
pub use local::LocalIndex;
pub use socket::SocketDaemon;

/// Every topic the editor listens to.
pub const ALL_TOPICS: [Topic; 8] = [
    Topic::FileChanged,
    Topic::IndexGeneration,
    Topic::JobUpdated,
    Topic::ConfigReloaded,
    Topic::OpenRequested,
    Topic::ShadowWrite,
    Topic::ModeChanged,
    Topic::ViewsInvalidated,
];

/// Client-side failures.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// Socket or file I/O.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The daemon answered with a JSON-RPC error.
    #[error("daemon error {}: {}", .0.code, .0.message)]
    Rpc(cv_protocol::RpcError),
    /// The daemon's `index_schema` is not the one this client reads.
    #[error("index_schema mismatch: editor reads {expected}, daemon has {found}")]
    SchemaMismatch { expected: u32, found: u32 },
    /// Malformed traffic.
    #[error("protocol: {0}")]
    Protocol(String),
    /// `write_note` CAS refusal: the file on disk no longer has the base hash.
    #[error("conflict: file changed on disk")]
    Conflict { current_hash: ContentHash },
    /// The connection is gone.
    #[error("daemon disconnected")]
    Disconnected,
    /// No such file, job or action.
    #[error("not found: {0}")]
    NotFound(String),
    /// A vault path that is absolute or escapes the vault.
    #[error("invalid vault path")]
    InvalidPath,
    /// Local index read.
    #[error("index: {0}")]
    Index(#[from] cv_index::Error),
    /// Local state read.
    #[error("state: {0}")]
    State(#[from] cv_state::StateError),
}

/// Result alias.
pub type Result<T> = std::result::Result<T, DaemonError>;

/// The editor↔daemon seam. Methods block: callers in the iced app run them off the UI thread
/// (`app::blocking`). Implementations are shared across threads (`Arc<dyn DaemonClient>`).
pub trait DaemonClient: Send + Sync {
    /// The `hello` answer captured at connect time (mode/generation as of the handshake).
    fn hello(&self) -> &HelloResult;
    /// CAS write: `Ok(new hash)` or [`DaemonError::Conflict`] with the on-disk hash.
    fn write_note(&self, path: &str, content: &str, base_hash: &ContentHash)
    -> Result<ContentHash>;
    /// Creates a note; the daemon may suffix the name, so the final path is returned.
    fn create_note(&self, path: &str, content: &str) -> Result<String>;
    /// Stores attachment `bytes` (extension `ext`) for `note_path` in the vault's attachment
    /// folder; returns the vault path (content-addressed, so identical bytes reuse one file).
    fn write_attachment(&self, note_path: &str, ext: &str, bytes: &[u8]) -> Result<String>;
    /// Renames (the daemon rewrites links).
    fn rename(&self, from: &str, to: &str) -> Result<()>;
    /// Deletes.
    fn delete(&self, path: &str) -> Result<()>;
    /// The note's embeds / attachments with whether something else references them
    /// (plan editor-ui-round-3 WP-3).
    fn delete_preview(&self, path: &str) -> Result<Vec<DeletePreviewItem>>;
    /// Deletes `path` and those of `attachments` nothing else references at delete time.
    fn delete_note(
        &self,
        path: &str,
        attachments: &[String],
        mode: DeleteMode,
    ) -> Result<DeleteNoteResult>;
    /// The action registry.
    fn actions_list(&self) -> Result<Vec<ActionInfo>>;
    /// Runs an action.
    fn run_action(&self, params: RunActionParams) -> Result<RunActionResult>;
    /// One job's status.
    fn job(&self, job_id: &str) -> Result<JobStatus>;
    /// Cancels a job.
    fn cancel(&self, job_id: &str) -> Result<JobStatus>;
    /// Daemon status (mode, generation, queue).
    fn status(&self) -> Result<StatusResult>;
    /// View ids.
    fn views_list(&self) -> Result<ViewsListResult>;
    /// Evaluates one view.
    fn view_run(&self, view_id: &str, this_path: Option<&str>) -> Result<ViewResult>;
    /// Supported currencies, sorted by code (plan round-5-reading-pickers-intake WP-5).
    fn lookup_currencies(&self) -> Result<Vec<LookupCurrency>>;
    /// Place search (`City, CC` labels).
    fn lookup_geocode(&self, query: &str) -> Result<Vec<LookupGeoResult>>;
    /// `provider`'s model list; `live: false` = declared models only (provider unreachable).
    fn lookup_models(&self, provider: &str) -> Result<LookupModelsResult>;
    /// YouTube channel titles for `ids` (unknown ids absent).
    fn lookup_channels(&self, ids: &[String]) -> Result<Vec<LookupChannel>>;
    /// Hybrid search through the daemon's `semsearch` index (plan semsearch-crate WP-4); the
    /// editor never opens the index itself.
    fn search(&self, query: &str, limit: u32) -> Result<cv_protocol::SearchResult>;
    /// Marks one `tracker_items` row done / not done (plan round-5-reading-pickers-intake
    /// WP-11); the refresh arrives as `views_invalidated`.
    fn tracker_item_set_done(&self, p: &cv_protocol::TrackerItemSetDoneParams) -> Result<()>;
    /// Sets one `tracker_items` row's task status (`None` = open; plan round-6-feedback-todos
    /// WP-6); the refresh arrives as `views_invalidated`.
    fn tracker_item_set_status(&self, p: &cv_protocol::TrackerItemSetStatusParams) -> Result<()>;
    /// Subscribes to `topics`; notifications arrive on the returned channel, which closes when
    /// the connection drops. A later call replaces the earlier receiver.
    fn subscribe(&self, topics: &[Topic]) -> Result<Receiver<Notification>>;
    /// The vault root on disk (from `hello`).
    fn vault_root(&self) -> &Path;
    /// The local read-only index handle.
    fn index(&self) -> &LocalIndex;
    /// The `state.db` path (from `hello`).
    fn state_db(&self) -> &Path;

    /// Reads a note from disk with the same content hash the daemon uses for CAS.
    fn read_file(&self, path: &str) -> Result<(Vec<u8>, ContentHash)> {
        let abs = vault_abs(self.vault_root(), path)?;
        let bytes = std::fs::read(abs).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => DaemonError::NotFound(path.to_owned()),
            _ => DaemonError::Io(e),
        })?;
        let hash = ContentHash::of(&bytes);
        Ok((bytes, hash))
    }

    /// Opens `state.db` read-only.
    fn state_reader(&self) -> Result<StateReader> {
        Ok(StateReader::open(self.state_db())?)
    }
}

/// Joins a vault-relative path onto `root`, refusing absolute paths and `..`.
pub fn vault_abs(root: &Path, rel: &str) -> Result<PathBuf> {
    cv_vault::validate_rel_path(rel).map_err(|_| DaemonError::InvalidPath)?;
    Ok(root.join(rel))
}

/// Opens a fresh daemon client (plan editor-data-safety WP-1). Production:
/// `SocketDaemon::connect(default_socket())`; tests: a `FakeDaemon` factory. Blocking.
pub type Connector = Arc<dyn Fn() -> Result<Arc<dyn DaemonClient>> + Send + Sync>;

/// A fresh connection: the client and its notification receiver (subscribed to
/// [`ALL_TOPICS`]), carried from the reconnect task to the UI thread.
#[derive(Clone)]
pub struct Connection {
    pub client: Arc<dyn DaemonClient>,
    pub rx: Arc<Mutex<Option<Receiver<Notification>>>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

/// Runs `connector` and subscribes the new client; any failure is a failed attempt.
pub fn connect(connector: &Connector) -> Result<Connection> {
    let client = connector()?;
    let rx = client.subscribe(&ALL_TOPICS)?;
    Ok(Connection {
        client,
        rx: Arc::new(Mutex::new(Some(rx))),
    })
}
