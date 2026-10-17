//! The real client: newline-delimited JSON-RPC 2.0 over the daemon's Unix socket. Shaped after
//! `crates/cv/src/client.rs` (handshake, `index_schema` check), but the editor never links the
//! `cv` crate. A dedicated reader thread owns all socket reads and demultiplexes responses (by
//! id) from notifications; callers block on a per-call channel, never on the socket itself.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cv_protocol::{
    CreateNoteParams, CreateNoteResult, DeleteMode, DeleteNoteParams, DeleteNoteResult,
    DeleteParams, DeletePreviewResult, HelloParams, HelloResult, JobIdParams, PROTOCOL_VERSION,
    PathParams, RenameParams, Request, RpcError, RpcId, RpcNotification, RpcRequest,
    SubscribeParams, ViewRunParams, WriteAttachmentParams, WriteAttachmentResult, WriteNoteParams,
    WriteNoteResult,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::{
    ActionInfo, ContentHash, DaemonClient, DaemonError, DeletePreviewItem, JobStatus, LocalIndex,
    Notification, Result, RunActionParams, RunActionResult, StatusResult, Topic, ViewResult,
    ViewsListResult,
};

const CALL_TIMEOUT: Duration = Duration::from_secs(30);

type Reply = std::result::Result<Value, RpcError>;

#[derive(Default)]
struct Shared {
    pending: Mutex<HashMap<u64, Sender<Reply>>>,
    subscriber: Mutex<Option<Sender<Notification>>>,
}

/// A connected, greeted daemon client.
pub struct SocketDaemon {
    writer: Mutex<UnixStream>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    hello: HelloResult,
    vault_root: PathBuf,
    state_db: PathBuf,
    index: LocalIndex,
}

impl std::fmt::Debug for SocketDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketDaemon")
            .field("hello", &self.hello)
            .finish_non_exhaustive()
    }
}

/// The default socket: `$CV_SOCKET`, else `$XDG_RUNTIME_DIR/crucible-vault/daemon.sock`.
pub fn default_socket() -> Option<PathBuf> {
    if let Some(s) = std::env::var_os("CV_SOCKET") {
        return Some(PathBuf::from(s));
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join("crucible-vault").join("daemon.sock"))
}

impl SocketDaemon {
    /// Connects, greets, and refuses a daemon whose `index_schema` differs from
    /// `cv_index::SCHEMA_VERSION`.
    pub fn connect(socket: &Path) -> Result<Self> {
        Self::connect_expecting(socket, cv_index::SCHEMA_VERSION)
    }

    /// [`Self::connect`] with an explicit expected schema (tests).
    pub fn connect_expecting(socket: &Path, expected_schema: u32) -> Result<Self> {
        let stream = UnixStream::connect(socket)?;
        let reader = BufReader::new(stream.try_clone()?);
        let shared = Arc::new(Shared::default());
        let rshared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("cv-daemon-reader".into())
            .spawn(move || read_loop(reader, &rshared))?;
        let mut me = Self {
            writer: Mutex::new(stream),
            shared,
            next_id: AtomicU64::new(1),
            hello: HelloResult {
                protocol_version: 0,
                index_schema: 0,
                mode: cv_protocol::Mode::Shadow,
                vault_root: String::new(),
                index_db: String::new(),
                state_db: String::new(),
                generation: 0,
            },
            vault_root: PathBuf::new(),
            state_db: PathBuf::new(),
            index: LocalIndex::new(Path::new("")),
        };
        let hello: HelloResult = me.call_typed(Request::Hello(HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client: "cvault-editor".into(),
        }))?;
        if hello.index_schema != expected_schema {
            return Err(DaemonError::SchemaMismatch {
                expected: expected_schema,
                found: hello.index_schema,
            });
        }
        me.vault_root = PathBuf::from(&hello.vault_root);
        me.state_db = PathBuf::from(&hello.state_db);
        me.index = LocalIndex::new(Path::new(&hello.index_db));
        me.hello = hello;
        Ok(me)
    }

    fn call(&self, req: Request) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.shared
            .pending
            .lock()
            .map_err(|_| DaemonError::Disconnected)?
            .insert(id, tx);
        let mut line = serde_json::to_string(&RpcRequest::new(RpcId::Num(id), req))
            .map_err(|e| DaemonError::Protocol(e.to_string()))?;
        line.push('\n');
        {
            let mut w = self.writer.lock().map_err(|_| DaemonError::Disconnected)?;
            w.write_all(line.as_bytes())?;
        }
        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(DaemonError::Rpc(e)),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(mut p) = self.shared.pending.lock() {
                    p.remove(&id);
                }
                Err(DaemonError::Protocol(format!("call {id} timed out")))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(DaemonError::Disconnected),
        }
    }

    fn call_typed<T: DeserializeOwned>(&self, req: Request) -> Result<T> {
        let v = self.call(req)?;
        serde_json::from_value(v).map_err(|e| DaemonError::Protocol(e.to_string()))
    }
}

fn read_loop(mut reader: BufReader<UnixStream>, shared: &Shared) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            tracing::warn!("daemon sent a malformed line");
            continue;
        };
        match v.get("id").and_then(Value::as_u64) {
            Some(id) => {
                let reply = match v.get("error") {
                    Some(e) => Err(serde_json::from_value(e.clone()).unwrap_or(RpcError {
                        code: cv_protocol::error_codes::INTERNAL_ERROR,
                        message: "unparseable error".into(),
                        data: None,
                    })),
                    None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                };
                let tx = shared.pending.lock().ok().and_then(|mut p| p.remove(&id));
                if let Some(tx) = tx {
                    let _ = tx.send(reply);
                }
            }
            None => match serde_json::from_value::<RpcNotification>(v) {
                Ok(n) => {
                    if let Ok(mut s) = shared.subscriber.lock()
                        && let Some(tx) = s.as_ref()
                        && tx.send(n.event).is_err()
                    {
                        *s = None;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "unknown notification"),
            },
        }
    }
    // Connection gone: fail every waiting call and close the notification channel.
    if let Ok(mut p) = shared.pending.lock() {
        p.clear();
    }
    if let Ok(mut s) = shared.subscriber.lock() {
        *s = None;
    }
}

impl DaemonClient for SocketDaemon {
    fn hello(&self) -> &HelloResult {
        &self.hello
    }

    fn write_note(
        &self,
        path: &str,
        content: &str,
        base_hash: &ContentHash,
    ) -> Result<ContentHash> {
        let r: WriteNoteResult = self.call_typed(Request::WriteNote(WriteNoteParams {
            path: path.to_owned(),
            content: content.to_owned(),
            base_hash: base_hash.as_str().to_owned(),
        }))?;
        match r {
            WriteNoteResult::Written { hash } => Ok(ContentHash::from_hex(hash)),
            WriteNoteResult::Conflict { current_hash } => Err(DaemonError::Conflict {
                current_hash: ContentHash::from_hex(current_hash),
            }),
        }
    }

    fn create_note(&self, path: &str, content: &str) -> Result<String> {
        let r: CreateNoteResult = self.call_typed(Request::CreateNote(CreateNoteParams {
            path: path.to_owned(),
            content: content.to_owned(),
        }))?;
        Ok(r.path)
    }

    fn write_attachment(&self, note_path: &str, ext: &str, bytes: &[u8]) -> Result<String> {
        use base64::Engine as _;
        let r: WriteAttachmentResult =
            self.call_typed(Request::WriteAttachment(WriteAttachmentParams {
                note_path: note_path.to_owned(),
                ext: ext.to_owned(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            }))?;
        Ok(r.path)
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.call(Request::Rename(RenameParams {
            from: from.to_owned(),
            to: to.to_owned(),
        }))
        .map(drop)
    }

    fn delete(&self, path: &str) -> Result<()> {
        self.call(Request::Delete(DeleteParams {
            path: path.to_owned(),
            mode: DeleteMode::Trash,
        }))
        .map(drop)
    }

    fn delete_preview(&self, path: &str) -> Result<Vec<DeletePreviewItem>> {
        let r: DeletePreviewResult = self.call_typed(Request::DeletePreview(PathParams {
            path: path.to_owned(),
        }))?;
        Ok(r.items)
    }

    fn delete_note(
        &self,
        path: &str,
        attachments: &[String],
        mode: DeleteMode,
    ) -> Result<DeleteNoteResult> {
        self.call_typed(Request::DeleteNote(DeleteNoteParams {
            path: path.to_owned(),
            attachments: attachments.to_vec(),
            mode,
        }))
    }

    fn actions_list(&self) -> Result<Vec<ActionInfo>> {
        self.call_typed(Request::ActionsList)
    }

    fn run_action(&self, params: RunActionParams) -> Result<RunActionResult> {
        self.call_typed(Request::RunAction(params))
    }

    fn job(&self, job_id: &str) -> Result<JobStatus> {
        self.call_typed(Request::Job(JobIdParams {
            job_id: job_id.to_owned(),
        }))
    }

    fn cancel(&self, job_id: &str) -> Result<JobStatus> {
        self.call_typed(Request::Cancel(JobIdParams {
            job_id: job_id.to_owned(),
        }))
    }

    fn status(&self) -> Result<StatusResult> {
        self.call_typed(Request::Status)
    }

    fn views_list(&self) -> Result<ViewsListResult> {
        self.call_typed(Request::ViewsList)
    }

    fn view_run(&self, view_id: &str, this_path: Option<&str>) -> Result<ViewResult> {
        self.call_typed(Request::ViewRun(ViewRunParams {
            view_id: view_id.to_owned(),
            this_path: this_path.map(str::to_owned),
        }))
    }

    fn lookup_currencies(&self) -> Result<Vec<cv_protocol::LookupCurrency>> {
        self.call_typed(Request::LookupCurrencies)
    }

    fn lookup_geocode(&self, query: &str) -> Result<Vec<cv_protocol::LookupGeoResult>> {
        self.call_typed(Request::LookupGeocode(cv_protocol::LookupGeocodeParams {
            query: query.to_owned(),
        }))
    }

    fn lookup_models(&self, provider: &str) -> Result<cv_protocol::LookupModelsResult> {
        self.call_typed(Request::LookupModels(cv_protocol::LookupModelsParams {
            provider: provider.to_owned(),
        }))
    }

    fn lookup_channels(&self, ids: &[String]) -> Result<Vec<cv_protocol::LookupChannel>> {
        self.call_typed(Request::LookupChannels(cv_protocol::LookupChannelsParams {
            ids: ids.to_vec(),
        }))
    }

    fn subscribe(&self, topics: &[Topic]) -> Result<Receiver<Notification>> {
        let (tx, rx) = mpsc::channel();
        *self
            .shared
            .subscriber
            .lock()
            .map_err(|_| DaemonError::Disconnected)? = Some(tx);
        self.call(Request::Subscribe(SubscribeParams {
            topics: topics.to_vec(),
        }))?;
        Ok(rx)
    }

    fn vault_root(&self) -> &Path {
        &self.vault_root
    }

    fn index(&self) -> &LocalIndex {
        &self.index
    }

    fn state_db(&self) -> &Path {
        &self.state_db
    }
}
