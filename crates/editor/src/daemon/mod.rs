//! The editor's view of the core daemon (plan `v0-editor.md` WP-1, D1).
//!
//! [`DaemonClient`] covers the `cv-protocol` RPC surface (the daemon is the sole writer) plus the
//! local read API: note bytes straight from disk, a [`LocalIndex`] over `index.db` reopened on
//! `index_generation`, and the `state.db` path. Two implementations: [`SocketDaemon`] (the real
//! Unix-socket client) and, behind the `fake` cargo feature (and in tests), `FakeDaemon`
//! (directory-backed, for building editor features without the daemon's action layer).

#[cfg(any(test, feature = "fake"))]
mod fake;
mod local;
pub mod socket;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

pub use cv_protocol::{
    ActionInfo, DeleteMode, DeleteNoteResult, DeletePreviewItem, HelloResult, JobStatus,
    LookupChannel, LookupCurrency, LookupGeoResult, LookupModel, LookupModelsResult, Notification,
    PathResolution, ResolvePathResult, RunActionParams, RunActionResult, StatusResult, Topic,
    ViewResult, ViewsListResult,
};
pub use cv_vault::ContentHash;
#[cfg(any(test, feature = "fake"))]
pub use fake::{ActionHandler, FakeDaemon, FakeLookups};
pub use local::LocalIndex;
pub use socket::SocketDaemon;

/// The CAS "no base / create-only" sentinel: the empty hash. `write_note` with it creates a
/// file that must not exist yet; a `Conflict` carrying it means the file is absent.
pub fn no_base() -> ContentHash {
    ContentHash::from_hex(String::new())
}

/// Whether `hash` is the [`no_base`] sentinel.
pub fn is_no_base(hash: &ContentHash) -> bool {
    hash.as_str().is_empty()
}

/// Every topic the editor listens to.
pub const ALL_TOPICS: [Topic; 6] = [
    Topic::FileChanged,
    Topic::IndexGeneration,
    Topic::JobUpdated,
    Topic::ConfigReloaded,
    Topic::OpenRequested,
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

/// Tags an error string as a daemon `HELD` refusal (see [`rpc_err`]).
const HELD_TAG: &str = "\u{1f}held\u{1f}";

/// Plan round-18 WP-2: the error string a stringifying RPC site carries for `e`. A daemon
/// `HELD` refusal keeps its code as a tagged string (label from `data.label`, else the
/// message), so [`held_label`] recovers it where the result is reported; any other error is
/// its plain `Display`.
pub fn rpc_err(e: &DaemonError) -> String {
    if let DaemonError::Rpc(r) = e
        && r.code == cv_protocol::error_codes::HELD
    {
        let label = r
            .data
            .as_ref()
            .and_then(|d| d.get("label"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&r.message);
        return format!("{HELD_TAG}{label}");
    }
    e.to_string()
}

/// The holder label when `e` came from [`rpc_err`] on a `HELD` refusal.
pub fn held_label(e: &str) -> Option<&str> {
    e.strip_prefix(HELD_TAG)
}

/// Result alias.
pub type Result<T> = std::result::Result<T, DaemonError>;

/// The editor↔daemon seam. Methods block: callers in the iced app run them off the UI thread
/// (`tokio::task::spawn_blocking` inside `Task::perform`). Implementations are shared across threads (`Arc<dyn DaemonClient>`).
pub trait DaemonClient: Send + Sync {
    /// The `hello` answer captured at connect time (generation as of the handshake).
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
    /// Daemon status (generation, queue).
    fn status(&self) -> Result<StatusResult>;
    /// View ids.
    fn views_list(&self) -> Result<ViewsListResult>;
    /// Evaluates one view. `select` = the host's in-place selection (plan round-13 WP-4).
    fn view_run(
        &self,
        view_id: &str,
        this_path: Option<&str>,
        sort: Option<&cv_protocol::ViewSort>,
        select: Option<&cv_protocol::ViewSelect>,
    ) -> Result<ViewResult>;
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
    /// Sets one `tracker_items` row's task status (`None` = open; plan round-6-feedback-todos
    /// WP-6); the refresh arrives as `views_invalidated`.
    fn tracker_item_set_status(&self, p: &cv_protocol::TrackerItemSetStatusParams) -> Result<()>;
    /// Sets one boolean frontmatter key of a note the editor has not open (plan
    /// round-8-intake-views-anchor WP-3); the refresh arrives as a file change.
    fn note_set_bool(&self, p: &cv_protocol::NoteSetBoolParams) -> Result<()>;
    /// Reports this process's memory snapshot to the daemon (plan memory-instrumentation-and-
    /// reduction WP-M2); an old daemon answers method-not-found, which callers drop.
    fn memory_report(&self, report: &cv_protocol::MemoryReport) -> Result<()>;
    /// Subscribes to `topics`; notifications arrive on the returned channel, which closes when
    /// the connection drops. A later call replaces the earlier receiver.
    fn subscribe(&self, topics: &[Topic]) -> Result<Receiver<Notification>>;
    /// The vault root on disk (from `hello`).
    fn vault_root(&self) -> &Path;
    /// The local read-only index handle.
    fn index(&self) -> &LocalIndex;
    /// The `state.db` path (from `hello`).
    fn state_db(&self) -> &Path;

    /// Where the note bound at `path` as of lineage cursor `since` lives now (plan
    /// note-path-continuity D2; `None` = no cursor, report the path now plus the head). An old
    /// daemon answers method-not-found (see [`bind_read`]).
    fn resolve_path(&self, path: &str, since: Option<u64>) -> Result<ResolvePathResult>;

    /// [`Self::read_file`] bound to a lineage cursor describing the same incarnation as the
    /// bytes (see [`bind_read`]).
    fn read_bound(&self, path: &str) -> Result<BoundRead> {
        bind_read(
            path,
            |since| self.resolve_path(path, since),
            || self.read_file(path),
        )
    }

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
}

/// A local note read bound to its lineage cursor (plan note-path-continuity D2, cycle-2 audit E1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundRead {
    pub bytes: Vec<u8>,
    pub hash: ContentHash,
    /// The lineage cursor the bytes' incarnation is valid at (`None` = an old daemon without
    /// lineage: behave as before).
    pub cursor: Option<u64>,
}

/// How often [`bind_read`] retries a read that raced an identity transition on its path.
const BIND_ATTEMPTS: usize = 4;

/// Binds a local read of `path` to a cursor of the same incarnation: take the head `h1` (no
/// cursor), read the bytes locally, then confirm with `resolve(Some(h1))` that no identity
/// transition left the path between `h1` and that answer's head `h2`; the bytes then belong to
/// the incarnation alive over `[h1, h2]`, and `h2` is the cursor. A raced read (moved, deleted,
/// replaced, reused) retries. An old daemon (method not found) yields the plain read, unbound.
/// A missing file is the read's `NotFound` (the caller resolves it).
pub fn bind_read(
    path: &str,
    mut resolve: impl FnMut(Option<u64>) -> Result<ResolvePathResult>,
    mut read: impl FnMut() -> Result<(Vec<u8>, ContentHash)>,
) -> Result<BoundRead> {
    for _ in 0..BIND_ATTEMPTS {
        let h1 = match resolve(None) {
            Ok(r) => r.head,
            Err(DaemonError::Rpc(e)) if e.code == cv_protocol::error_codes::METHOD_NOT_FOUND => {
                let (bytes, hash) = read()?;
                return Ok(BoundRead {
                    bytes,
                    hash,
                    cursor: None,
                });
            }
            Err(e) => return Err(e),
        };
        let got = read();
        let after = resolve(Some(h1))?;
        let live = matches!(&after.resolution, PathResolution::Live { path: p } if p == path);
        match got {
            Ok((bytes, hash)) if live => {
                return Ok(BoundRead {
                    bytes,
                    hash,
                    cursor: Some(after.head),
                });
            }
            Ok(_) => {} // raced a transition: the bytes may be another incarnation
            Err(DaemonError::NotFound(p)) if !live => return Err(DaemonError::NotFound(p)),
            Err(DaemonError::NotFound(_)) => {} // appeared after the read: read again
            Err(e) => return Err(e),
        }
    }
    Err(DaemonError::Protocol(format!(
        "{path}: identity kept changing during the read"
    )))
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

#[cfg(test)]
mod bind_tests {
    use super::*;
    use std::cell::RefCell;

    /// A scripted daemon: a lineage log over one path plus its current bytes.
    #[derive(Default)]
    struct World {
        head: u64,
        /// `(seq, leaving edge)` on `a.md`: `Some(to)` = moved, `None` = deleted.
        edges: Vec<(u64, Option<String>)>,
        bytes: Option<Vec<u8>>,
    }

    impl World {
        fn resolve(&self, since: Option<u64>) -> ResolvePathResult {
            let resolution = match since.and_then(|c| self.edges.iter().find(|(s, _)| *s > c)) {
                Some((_, Some(to))) => PathResolution::Moved { to: to.clone() },
                Some((_, None)) => PathResolution::Deleted,
                None if self.bytes.is_some() => PathResolution::Live {
                    path: "a.md".into(),
                },
                None => PathResolution::Unknown,
            };
            ResolvePathResult {
                resolution,
                head: self.head,
            }
        }
    }

    /// Cycle-2 audit E1: X is moved a.md→b.md and Y created at a.md between the cursor and the
    /// local read. The read returns Y's bytes; the bound result must carry a cursor of Y's
    /// incarnation (never the pre-move cursor that would later "follow" X's move with Y's bytes).
    #[test]
    fn bound_read_bytes_and_cursor_are_the_same_incarnation() {
        let w = RefCell::new(World {
            head: 5,
            bytes: Some(b"X".to_vec()),
            ..World::default()
        });
        let reads = RefCell::new(0);
        let got = bind_read(
            "a.md",
            |since| Ok(w.borrow().resolve(since)),
            || {
                let mut n = reads.borrow_mut();
                *n += 1;
                if *n == 1 {
                    // The race, after the first cursor was taken.
                    let mut w = w.borrow_mut();
                    w.edges.push((6, Some("b.md".into())));
                    w.head = 7; // seq 7: Y created at a.md
                    w.bytes = Some(b"Y".to_vec());
                }
                let b = w.borrow().bytes.clone().unwrap();
                let h = ContentHash::of(&b);
                Ok((b, h))
            },
        )
        .unwrap();
        assert_eq!(*reads.borrow(), 2, "the raced read was retried");
        assert_eq!(got.bytes, b"Y");
        assert_eq!(got.cursor, Some(7));
        // That cursor resolves to Y's own incarnation, still live at a.md.
        assert_eq!(
            w.borrow().resolve(got.cursor).resolution,
            PathResolution::Live {
                path: "a.md".into()
            }
        );
        // No race: bound at once.
        let quiet = bind_read(
            "a.md",
            |since| Ok(w.borrow().resolve(since)),
            || Ok((b"Y".to_vec(), ContentHash::of(b"Y"))),
        )
        .unwrap();
        assert_eq!(quiet.cursor, Some(7));
        // An old daemon: the plain read, unbound.
        let old = bind_read(
            "a.md",
            |_| {
                Err(DaemonError::Rpc(cv_protocol::RpcError {
                    code: cv_protocol::error_codes::METHOD_NOT_FOUND,
                    message: "nope".into(),
                    data: None,
                }))
            },
            || Ok((b"Z".to_vec(), ContentHash::of(b"Z"))),
        )
        .unwrap();
        assert_eq!((old.bytes, old.cursor), (b"Z".to_vec(), None));
        // A deleted note is the read's NotFound.
        w.borrow_mut().edges.push((8, None));
        w.borrow_mut().bytes = None;
        let gone = bind_read(
            "a.md",
            |since| Ok(w.borrow().resolve(since)),
            || Err(DaemonError::NotFound("a.md".into())),
        );
        assert!(matches!(gone, Err(DaemonError::NotFound(_))));
    }
}
