//! The real client: the shared [`cv_protocol::client::RpcClient`] (one framing, handshake and
//! error-decode implementation with `cv`; std-only, a reader thread demultiplexes responses from
//! notifications) behind the editor's [`DaemonClient`] seam. The editor never links `cv`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use cv_protocol::client::{ClientError, RpcClient};
use cv_protocol::{
    CreateNoteParams, DeleteMode, DeleteNoteParams, DeleteNoteResult, DeleteParams, HelloResult,
    PathParams, RenameParams, SubscribeParams, ViewRunParams, WriteAttachmentParams,
    WriteNoteParams, WriteNoteResult, methods as m,
};

use super::{
    ActionInfo, ContentHash, DaemonClient, DaemonError, DeletePreviewItem, LocalIndex,
    Notification, Result, RunActionParams, RunActionResult, StatusResult, Topic, ViewResult,
    ViewsListResult,
};

impl From<ClientError> for DaemonError {
    /// The new `REFUSED`/`UNAVAILABLE` codes stay in [`DaemonError::Rpc`] (the code is carried).
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Io(e) => Self::Io(e),
            ClientError::Rpc(e) => Self::Rpc(e),
            ClientError::SchemaMismatch { expected, found } => {
                Self::SchemaMismatch { expected, found }
            }
            ClientError::Protocol(m) => Self::Protocol(m),
            ClientError::Disconnected => Self::Disconnected,
        }
    }
}

/// A connected, greeted daemon client.
pub struct SocketDaemon {
    rpc: RpcClient,
    vault_root: PathBuf,
    state_db: PathBuf,
    index: LocalIndex,
}

impl std::fmt::Debug for SocketDaemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketDaemon")
            .field("hello", &self.rpc.hello)
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
        let rpc = RpcClient::connect(socket, "cvault-editor", expected_schema)?;
        Ok(Self {
            vault_root: PathBuf::from(&rpc.hello.vault_root),
            state_db: PathBuf::from(&rpc.hello.state_db),
            index: LocalIndex::new(Path::new(&rpc.hello.index_db)),
            rpc,
        })
    }

    fn call<M: cv_protocol::Method>(&self, params: M::Params) -> Result<M::Result> {
        Ok(self.rpc.call::<M>(params)?)
    }
}

impl DaemonClient for SocketDaemon {
    fn hello(&self) -> &HelloResult {
        &self.rpc.hello
    }

    fn write_note(
        &self,
        path: &str,
        content: &str,
        base_hash: &ContentHash,
    ) -> Result<ContentHash> {
        let r = self.call::<m::WriteNote>(WriteNoteParams {
            path: path.to_owned(),
            content: content.to_owned(),
            base_hash: base_hash.as_str().to_owned(),
        })?;
        match r {
            WriteNoteResult::Written { hash } => Ok(ContentHash::from_hex(hash)),
            WriteNoteResult::Conflict { current_hash } => Err(DaemonError::Conflict {
                current_hash: ContentHash::from_hex(current_hash),
            }),
        }
    }

    fn create_note(&self, path: &str, content: &str) -> Result<String> {
        let (path, content) = (path.to_owned(), content.to_owned());
        Ok(self
            .call::<m::CreateNote>(CreateNoteParams { path, content })?
            .path)
    }

    fn write_attachment(&self, note_path: &str, ext: &str, bytes: &[u8]) -> Result<String> {
        use base64::Engine as _;
        let p = WriteAttachmentParams {
            note_path: note_path.to_owned(),
            ext: ext.to_owned(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        };
        Ok(self.call::<m::WriteAttachment>(p)?.path)
    }

    fn rename(&self, from: &str, to: &str) -> Result<()> {
        let (from, to) = (from.to_owned(), to.to_owned());
        self.call::<m::Rename>(RenameParams { from, to }).map(drop)
    }

    fn delete(&self, path: &str) -> Result<()> {
        let (path, mode) = (path.to_owned(), DeleteMode::Trash);
        self.call::<m::Delete>(DeleteParams { path, mode })
            .map(drop)
    }

    fn delete_preview(&self, path: &str) -> Result<Vec<DeletePreviewItem>> {
        let path = path.to_owned();
        Ok(self.call::<m::DeletePreview>(PathParams { path })?.items)
    }

    fn delete_note(
        &self,
        path: &str,
        attachments: &[String],
        mode: DeleteMode,
    ) -> Result<DeleteNoteResult> {
        let (path, attachments) = (path.to_owned(), attachments.to_vec());
        self.call::<m::DeleteNote>(DeleteNoteParams {
            path,
            attachments,
            mode,
        })
    }

    fn actions_list(&self) -> Result<Vec<ActionInfo>> {
        self.call::<m::ActionsList>(())
    }

    fn run_action(&self, params: RunActionParams) -> Result<RunActionResult> {
        self.call::<m::RunAction>(params)
    }

    fn status(&self) -> Result<StatusResult> {
        self.call::<m::Status>(())
    }

    fn views_list(&self) -> Result<ViewsListResult> {
        self.call::<m::ViewsList>(())
    }

    fn view_run(&self, view_id: &str, this_path: Option<&str>) -> Result<ViewResult> {
        let (view_id, this_path) = (view_id.to_owned(), this_path.map(str::to_owned));
        self.call::<m::ViewRun>(ViewRunParams { view_id, this_path })
    }

    fn lookup_currencies(&self) -> Result<Vec<cv_protocol::LookupCurrency>> {
        self.call::<m::LookupCurrencies>(())
    }

    fn lookup_geocode(&self, query: &str) -> Result<Vec<cv_protocol::LookupGeoResult>> {
        let query = query.to_owned();
        self.call::<m::LookupGeocode>(cv_protocol::LookupGeocodeParams { query })
    }

    fn lookup_models(&self, provider: &str) -> Result<cv_protocol::LookupModelsResult> {
        let provider = provider.to_owned();
        self.call::<m::LookupModels>(cv_protocol::LookupModelsParams { provider })
    }

    fn search(&self, query: &str, limit: u32) -> Result<cv_protocol::SearchResult> {
        let query = query.to_owned();
        self.call::<m::Search>(cv_protocol::SearchParams { query, limit })
    }

    fn lookup_channels(&self, ids: &[String]) -> Result<Vec<cv_protocol::LookupChannel>> {
        let ids = ids.to_vec();
        self.call::<m::LookupChannels>(cv_protocol::LookupChannelsParams { ids })
    }

    fn tracker_item_set_status(&self, p: &cv_protocol::TrackerItemSetStatusParams) -> Result<()> {
        self.call::<m::TrackerItemSetStatus>(p.clone())
    }

    fn note_set_bool(&self, p: &cv_protocol::NoteSetBoolParams) -> Result<()> {
        self.call::<m::NoteSetBool>(p.clone()).map(drop)
    }

    fn memory_report(&self, report: &cv_protocol::MemoryReport) -> Result<()> {
        self.call::<m::MemoryReport>(report.clone()).map(drop)
    }

    fn subscribe(&self, topics: &[Topic]) -> Result<Receiver<Notification>> {
        let rx = self.rpc.notifications()?;
        let topics = topics.to_vec();
        self.call::<m::Subscribe>(SubscribeParams { topics })?;
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
