//! Helpers shared by several test modules, one copy each.

use std::sync::Arc;

use iced::widget::text_editor::{Action, Edit};

use super::fake;
use crate::app::App;
use crate::daemon::{DaemonClient, FakeDaemon};
use crate::message::{ExplorerMsg, Message, SourceMsg, StatusMsg};
use cv_protocol::Notification;

/// Opens `notes` into one pane (rpc inline).
pub(super) fn setup(notes: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let (tmp, d) = fake();
    let d = Arc::new(d);
    for p in notes {
        d.create_note(p, "body").expect("create");
    }
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    for p in notes {
        let _ = app.send(Message::Explorer(ExplorerMsg::Open((*p).to_owned())));
    }
    (tmp, d, app)
}

pub(super) fn type_x(app: &mut App) {
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('x'),
    ))));
}

pub(super) fn feed(app: &mut App, ns: Vec<Notification>) {
    for n in ns {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
}

pub(super) fn queued(app: &mut App) -> Vec<Message> {
    match &mut app.rpc_exec {
        crate::app::RpcExec::Queue(q) => q.drain(..).collect(),
        crate::app::RpcExec::Pool => Vec::new(),
    }
}

pub(super) fn notice(app: &App) -> Option<&str> {
    app.status.notice.as_ref().map(|n| n.text.as_str())
}
