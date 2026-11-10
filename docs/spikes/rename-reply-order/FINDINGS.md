# Spike: rename reply vs. notification ordering in the editor (2026-11-09)

Question: after note-path-continuity (`0d10649`..`6880411`), why does a renamed note's tab stay open but refuse to save until reopened?

## Method

Three throwaway tests (appendix below). Each run appended them to `crates/editor/src/app/tests/title_sync.rs`, ran `cargo test -p editor spike_ -- --nocapture`, and restored the file with `git checkout` (nothing left in the tree).

Each test delivers the daemon's `Renamed { from }` + `Deleted` pair (`FakeDaemon::daemon_move`, the bridge's real shape) **before** the reply the editor is waiting on.

## Results

| Test | Sequence | Result |
|---|---|---|
| `spike_title_rename_notification_before_reply` | title edit saved → title-sync rename RPC in flight → pair → `TitleRenamed` | **FAIL**: tab follows to the new path, `is_renaming()` stays `true`, `begin_save()` returns `None` for every later edit |
| `spike_manual_rename_notification_before_reply` | clean note, explorer/filename-input rename → pair → `ExplorerMsg::Done` | pass: tab follows, a later edit saves at the new path |
| `spike_save_in_flight_across_a_followed_move` | save in flight → pair → `SourceMsg::Saved` addressed to the old path | **FAIL**: `is_saving()` stays `true`, a conflict bar is raised, and neither a later edit nor Keep mine can start a save |

## Mechanism

- Per-document async state (`SaveState::renaming`, `SaveState::in_flight`) lives on the document, and documents are keyed by path (`App::docs`).
- Completions are addressed by the path captured at dispatch: `Message::TitleRenamed { from }` (`app/files.rs` `title_renamed`: `self.docs.get_mut(from)`), `SourceMsg::Saved { path }` (`app/saves.rs` `on_save_landed`: `buf_mut(&mut self.docs, path)`).
- A followed move (`follow_move_event` → `retarget` / `move_path_state`) re-keys the document between dispatch and completion. The completion then finds no document at the old path, so the flag it was meant to clear is never cleared. `begin_save` (`source/save.rs:233`) queues `save_again` forever.
- Before note-path-continuity WP-1 the daemon's own `Renamed` reached the editor only after the watcher's 150 ms debounce, so the RPC reply always won. D1 made daemon events immediate ("before the call returns"), so the notification now races the reply and can win.

## Not measured

- The live wire order (how often the notification beats the reply against the real daemon). It is a race between the bridge's broadcast and the request handler's reply; the tests force the losing order rather than sampling it.
- Which gesture the user's report used. Only the title-edit rename reproduces a stuck tab; the explorer / filename-input rename of a clean note passed in this ordering.

## Appendix: the repro tests

Appended to `crates/editor/src/app/tests/title_sync.rs` for each run, then removed.

```rust
// ---- SPIKE (docs/spikes/rename-reply-order): notification-before-reply ordering ----
fn spike_feed(app: &mut App, ns: Vec<Notification>) {
    for n in ns {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
}

/// Title-sync rename; the daemon's `Renamed{from}` + `Deleted` pair is consumed BEFORE the
/// rename RPC's reply (`TitleRenamed`).
#[test]
fn spike_title_rename_notification_before_reply() {
    let (_tmp, d) = fake_arc();
    let mut app = open_and_retitle(&d, "sp/Old.md", "New");
    app.rpc_exec = crate::app::RpcExec::Pool; // hold the rename RPC
    save(&mut app, &d, "sp/Old.md");
    let doc = app.doc("sp/Old.md").expect("still at from");
    assert!(doc.is_renaming());
    let seq = doc.edit_seq();
    let pair = d.daemon_move("sp/Old.md", "sp/New.md").expect("rename on disk");
    spike_feed(&mut app, pair);
    let _ = app.send(Message::TitleRenamed {
        from: "sp/Old.md".into(),
        to: "sp/New.md".into(),
        seq,
        result: Ok("sp/New.md".into()),
    });
    assert_eq!(app.open_paths(), ["sp/New.md"]);
    let doc = app.doc_mut("sp/New.md").expect("followed");
    eprintln!("SPIKE title: renaming after reply = {}", doc.is_renaming());
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("typed".into(), Instant::now()));
    assert!(doc.begin_save().is_some(), "SPIKE title: a save can start at the new path");
}

/// Explorer / filename-input rename of a clean note; pair consumed BEFORE `ExplorerMsg::Done`.
#[test]
fn spike_manual_rename_notification_before_reply() {
    use crate::explorer::{FileOp, FileOpDone};
    let (_tmp, d) = fake_arc();
    d.create_note("sm/Old.md", OLD).expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("sm/Old.md".into())));
    let pair = d.daemon_move("sm/Old.md", "sm/New.md").expect("rename on disk");
    spike_feed(&mut app, pair);
    let _ = app.send(Message::Explorer(ExplorerMsg::Done(FileOpDone {
        op: FileOp::Rename { from: "sm/Old.md".into(), to: "sm/New.md".into() },
        result: Ok("sm/New.md".into()),
    })));
    assert_eq!(app.open_paths(), ["sm/New.md"]);
    eprintln!("SPIKE manual: status = {:?}", app.status.error_text());
    let doc = app.doc_mut("sm/New.md").expect("followed");
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("typed".into(), Instant::now()));
    save(&mut app, &d, "sm/New.md");
    assert_eq!(d.read_file("sm/New.md").expect("to").0, b"---\ntitle: Old\n---\ntyped\n");
    assert!(app.doc("sm/New.md").is_some_and(|t| !t.is_dirty()));
}

/// A plain save is in flight when a move of the note is followed; its reply is addressed to `from`.
#[test]
fn spike_save_in_flight_across_a_followed_move() {
    let (_tmp, d) = fake_arc();
    d.create_note("sf/a.md", OLD).expect("create");
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Explorer(ExplorerMsg::Open("sf/a.md".into())));
    let doc = app.doc_mut("sf/a.md").expect("open");
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("one".into(), Instant::now()));
    let job = app.doc_mut("sf/a.md").and_then(|t| t.begin_save()).expect("save starts");
    let outcome = job.run(d.as_ref()); // the write lands at `from` on the daemon
    let pair = d.daemon_move("sf/a.md", "sf/b.md").expect("move");
    for n in pair {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
    let _ = app.send(job.done(outcome)); // reply arrives after the follow
    eprintln!("SPIKE inflight: open={:?} status={:?}", app.open_paths(), app.status.error_text());
    let doc = app.doc_mut("sf/b.md").expect("followed");
    eprintln!("SPIKE inflight: saving={} dirty={} conflict={:?}", doc.is_saving(), doc.is_dirty(), doc.conflict());
    doc.select_range((3, 0), (3, 1));
    assert!(doc.paste("two".into(), Instant::now()));
    assert!(doc.begin_save().is_some(), "SPIKE inflight: a later save can start");
}
```
