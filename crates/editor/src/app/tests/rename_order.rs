//! Plan note-identity D7 / WP-3: the rename-vs-reply ordering regressions
//! (`docs/spikes/rename-reply-order`). The daemon's `Renamed { from }` + `Deleted` pair and the
//! RPC reply an editor operation waits on race on the wire; each scenario is written once and
//! run in every [`Order`] through [`deliver`].
//!
//! Orders that fail only because documents are still keyed by path are `#[ignore]`d with a
//! `note-identity WP-4` marker; WP-4 (documents keyed by id) un-marks them.

use std::time::Instant;

use super::*;
use crate::explorer::{FileOp, FileOpDone};
use crate::message::ExplorerMsg;

const OLD: &str = "---\ntitle: Old\n---\nx\n";

/// Where an operation's RPC reply lands relative to the daemon's notifications for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Order {
    /// The reply, then every notification.
    ReplyFirst,
    /// Every notification, then the reply.
    NotificationFirst,
    /// The first notification, the reply, then the rest (a rename pair split by the reply).
    ReplyBetween,
}

/// Feeds `app` the daemon `notifications` and the operation's `reply` in `order`. The reply is a
/// closure so a caller builds it from state it owns (a held save job, a `TitleRenamed`).
pub(super) fn deliver(
    app: &mut App,
    order: Order,
    notifications: Vec<Notification>,
    reply: impl FnOnce(&mut App),
) {
    let feed = |app: &mut App, ns: &[Notification]| {
        for n in ns {
            let _ = app.send(Message::Status(StatusMsg::Notification(n.clone())));
        }
    };
    match order {
        Order::ReplyFirst => {
            reply(app);
            feed(app, &notifications);
        }
        Order::NotificationFirst => {
            feed(app, &notifications);
            reply(app);
        }
        Order::ReplyBetween => {
            let split = 1.min(notifications.len());
            feed(app, &notifications[..split]);
            reply(app);
            feed(app, &notifications[split..]);
        }
    }
}

/// Defines one `#[test]` per order for `$scenario(Order)`; an order may carry an `ignore`
/// reason.
macro_rules! in_orders {
    ($scenario:ident: $($name:ident => $order:expr $(, ignore = $why:literal)?;)+) => {
        $(
            $(#[ignore = $why])?
            #[test]
            fn $name() {
                $scenario($order);
            }
        )+
    };
}

/// Title-sync rename in flight; the pair and the `TitleRenamed` reply arrive in `order`.
fn title_rename_scenario(order: Order) {
    let (_tmp, d) = fake_arc();
    d.create_note("sp/Old.md", OLD).expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("sp/Old.md".into())));
    let doc = app.doc_mut("sp/Old.md").expect("open");
    doc.select_range((1, 7), (1, 10));
    assert!(doc.paste("New".into(), Instant::now()));
    app.rpc_exec = crate::app::RpcExec::Pool; // hold the rename RPC
    let job = app
        .doc_mut("sp/Old.md")
        .and_then(|t| t.begin_save())
        .expect("save starts");
    let outcome = job.run(d.as_ref());
    let _ = app.send(job.done(outcome));
    let doc = app.doc("sp/Old.md").expect("still at from");
    assert!(doc.is_renaming());
    let seq = doc.edit_seq();
    let pair = d
        .daemon_move("sp/Old.md", "sp/New.md")
        .expect("rename on disk");
    deliver(&mut app, order, pair, |app| {
        let _ = app.send(Message::TitleRenamed {
            from: "sp/Old.md".into(),
            to: "sp/New.md".into(),
            seq,
            result: Ok("sp/New.md".into()),
        });
    });
    assert_eq!(app.open_paths(), ["sp/New.md"]);
    let doc = app.doc_mut("sp/New.md").expect("followed");
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("typed".into(), Instant::now()));
    assert!(
        doc.begin_save().is_some(),
        "a save can start at the new path ({order:?})"
    );
}

/// Explorer / filename-input rename of a clean note; the pair and `ExplorerMsg::Done` in `order`.
fn manual_rename_scenario(order: Order) {
    let (_tmp, d) = fake_arc();
    d.create_note("sm/Old.md", OLD).expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("sm/Old.md".into())));
    let pair = d
        .daemon_move("sm/Old.md", "sm/New.md")
        .expect("rename on disk");
    deliver(&mut app, order, pair, |app| {
        let _ = app.send(Message::Explorer(ExplorerMsg::Done(FileOpDone {
            op: FileOp::Rename {
                from: "sm/Old.md".into(),
                to: "sm/New.md".into(),
            },
            result: Ok("sm/New.md".into()),
        })));
    });
    assert_eq!(app.open_paths(), ["sm/New.md"]);
    let doc = app.doc_mut("sm/New.md").expect("followed");
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("typed".into(), Instant::now()));
    super::unsaved_dialog::save(&mut app, &d, "sm/New.md");
    assert_eq!(
        d.read_file("sm/New.md").expect("to").0,
        b"---\ntitle: Old\n---\ntyped\n"
    );
    assert!(app.doc("sm/New.md").is_some_and(|t| !t.is_dirty()));
}

/// A plain save is in flight when a move is followed; its reply is addressed to `from`.
fn save_in_flight_scenario(order: Order) {
    let (_tmp, d) = fake_arc();
    d.create_note("sf/a.md", OLD).expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("sf/a.md".into())));
    let doc = app.doc_mut("sf/a.md").expect("open");
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("one".into(), Instant::now()));
    let job = app
        .doc_mut("sf/a.md")
        .and_then(|t| t.begin_save())
        .expect("save starts");
    let outcome = job.run(d.as_ref()); // the write lands at `from` on the daemon
    let pair = d.daemon_move("sf/a.md", "sf/b.md").expect("move");
    deliver(&mut app, order, pair, |app| {
        let _ = app.send(job.done(outcome));
    });
    let doc = app.doc_mut("sf/b.md").expect("followed");
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("two".into(), Instant::now()));
    assert!(
        doc.begin_save().is_some(),
        "a later save can start ({order:?})"
    );
}

in_orders! { title_rename_scenario:
    title_rename_reply_first => Order::ReplyFirst;
    title_rename_notification_first => Order::NotificationFirst, ignore = "note-identity WP-4: the rename flag stays set because TitleRenamed addresses the path the followed move re-keyed";
    title_rename_reply_between_the_pair => Order::ReplyBetween, ignore = "note-identity WP-4: the rename flag stays set because TitleRenamed addresses the path the followed move re-keyed";
}

in_orders! { manual_rename_scenario:
    manual_rename_reply_first => Order::ReplyFirst;
    manual_rename_notification_first => Order::NotificationFirst;
    manual_rename_reply_between_the_pair => Order::ReplyBetween;
}

in_orders! { save_in_flight_scenario:
    save_in_flight_reply_first => Order::ReplyFirst;
    save_in_flight_notification_first => Order::NotificationFirst, ignore = "note-identity WP-4: the save flag stays set because SourceMsg::Saved addresses the path the followed move re-keyed";
    save_in_flight_reply_between_the_pair => Order::ReplyBetween, ignore = "note-identity WP-4: the save flag stays set because SourceMsg::Saved addresses the path the followed move re-keyed";
}
