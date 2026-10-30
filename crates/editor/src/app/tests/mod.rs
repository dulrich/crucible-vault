//! Window-less `App` tests over `FakeDaemon` (synthetic vaults only, I1): the shared fixture
//! (`fake`, `first_note`, `app_over`, `fake_arc`, `WAIT`) and one module per behaviour.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use cv_protocol::{
    ActionContext, ActionInfo, ChangeKind, HelloResult, JobState, Notification, Origin, Request,
    RpcNotification, RpcRequest, RunActionParams,
};
use cv_testkit::{SyntheticVault, VaultSpec};
use serde_json::{Value, json};

use crate::app::App;
use crate::daemon::{ALL_TOPICS, ContentHash, DaemonClient, DaemonError, FakeDaemon, SocketDaemon};
use crate::message::{Message, StatusMsg, TabsMsg};

const WAIT: Duration = Duration::from_secs(5);

fn fake() -> (tempfile::TempDir, FakeDaemon) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    (tmp, d)
}

fn first_note(d: &FakeDaemon) -> String {
    d.index()
        .with_query(|q| q.folder_files("notes"))
        .expect("query")
        .into_iter()
        .find(|f| f.is_md)
        .expect("a note")
        .path
}

pub(super) fn app_over(d: &Arc<FakeDaemon>, state: Option<std::path::PathBuf>) -> App {
    let dc: Arc<dyn DaemonClient> = Arc::clone(d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), state);
    app.queue_rpcs();
    app
}

pub(super) fn fake_arc() -> (tempfile::TempDir, Arc<FakeDaemon>) {
    let (tmp, d) = fake();
    (tmp, Arc::new(d))
}

mod autosave;
mod chrome;
mod close_quit;
mod commands;
mod context_menu;
mod daemon_client;
mod daemon_move;
mod doc_state;
mod file_kinds;
mod find_keys;
mod hold_gaps;
mod holds;
mod image_menu;
mod index_follow;
mod link_typeahead;
mod reconnect;
mod replaced;
mod save_continue;
mod scroll_matrix;
mod shared_docs;
mod tab_drag;
mod title_sync;
mod unsaved_dialog;
mod view_scale;
mod workspace;
