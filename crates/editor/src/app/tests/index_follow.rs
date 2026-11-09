//! Explorer/switcher follow index membership (audit B1) and Reading
//! re-lowers on a daemon-origin rewrite (the lint path).

use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::explorer::{FileOp, FileOpDone};
use crate::message::{ExplorerMsg, ReadingMsg, SourceMsg, SwitcherMsg, TabsMsg};
use crate::reading::ViewMode;

fn generation(app: &mut App, d: &FakeDaemon) {
    let n = d.generation().expect("gen");
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::IndexGeneration { n },
    )));
}

fn candidates(app: &App) -> Vec<String> {
    app.switcher.results.clone()
}

fn file_changed(app: &mut App, path: &str, kind: ChangeKind, origin: Origin) {
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: path.to_owned(),
            kind,
            hash: None,
            origin,
            from: None,
            replaced_by: None,
            seq: None,
        },
    )));
}

#[test]
fn ignore_rebuild_removes_and_restores_in_explorer_and_switcher() {
    let (_tmp, d) = fake_arc();
    let vault = d.vault_root().to_path_buf();
    d.create_note("r3ign/x.md", "x").expect("create");
    let mut app = app_over(&d, None);
    generation(&mut app, &d);
    let _ = app.send(Message::Switcher(SwitcherMsg::Open));
    assert!(app.explorer.contains("r3ign/x.md"));
    assert!(candidates(&app).contains(&"r3ign/x.md".to_owned()));

    std::fs::write(vault.join(".gitignore"), "r3ign/\n").expect("ignore");
    d.create_note("r3other/a.md", "a").expect("rebuild");
    generation(&mut app, &d);
    assert!(!app.explorer.contains("r3ign/x.md"), "ignored: dropped");
    assert!(!candidates(&app).contains(&"r3ign/x.md".to_owned()));

    std::fs::remove_file(vault.join(".gitignore")).expect("unignore");
    d.create_note("r3other/b.md", "b").expect("rebuild");
    generation(&mut app, &d);
    assert!(app.explorer.contains("r3ign/x.md"), "restored");
    assert!(candidates(&app).contains(&"r3ign/x.md".to_owned()));
}

#[test]
fn op_or_delta_insert_of_non_indexed_path_is_dropped() {
    let (_tmp, d) = fake_arc();
    let vault = d.vault_root().to_path_buf();
    std::fs::write(vault.join(".gitignore"), "r3ign/\n").expect("ignore");
    d.create_note("r3ok/a.md", "a").expect("rebuild");
    let mut app = app_over(&d, None);
    std::fs::create_dir_all(vault.join("r3ign")).expect("mkdir");
    std::fs::write(vault.join("r3ign/new.md"), "n").expect("write");
    let _ = app.send(Message::Explorer(ExplorerMsg::Done(FileOpDone {
        op: FileOp::Create {
            path: "r3ign/new.md".into(),
        },
        result: Ok("r3ign/new.md".into()),
    })));
    assert!(!app.explorer.contains("r3ign/new.md"), "op insert dropped");
    file_changed(
        &mut app,
        "r3ign/new.md",
        ChangeKind::Created,
        Origin::External,
    );
    assert!(
        !app.explorer.contains("r3ign/new.md"),
        "delta insert dropped"
    );
    // An indexed path still inserts.
    d.create_note("r3ok/b.md", "b").expect("create");
    let _ = app.send(Message::Explorer(ExplorerMsg::Done(FileOpDone {
        op: FileOp::Create {
            path: "r3ok/b.md".into(),
        },
        result: Ok("r3ok/b.md".into()),
    })));
    assert!(app.explorer.contains("r3ok/b.md"));
}

/// The Lint repro: a daemon-origin rewrite of an open note's frontmatter reaches the editor
/// as `file_changed` without a new index generation (incremental update). Clean buffer: the
/// reload must re-lower Reading. Dirty buffer: conflict bar, nothing overwritten.
#[test]
fn reading_relowers_after_daemon_rewrite_clean_and_dirty() {
    let (_tmp, d) = fake_arc();
    let vault = d.vault_root().to_path_buf();
    for p in ["r3lint/a.md", "r3lint/b.md"] {
        d.create_note(p, "---\nstatus: old\n---\nbody\n")
            .expect("create");
    }
    let mut app = app_over(&d, None);
    generation(&mut app, &d);
    for p in ["r3lint/a.md", "r3lint/b.md"] {
        let _ = app.send(Message::Tabs(TabsMsg::Open(p.to_owned())));
        let _ = app.send(Message::Reading(ReadingMsg::SetMode {
            path: p.to_owned(),
            mode: ViewMode::Reading,
        }));
    }
    let shown =
        |app: &App, p: &str| format!("{:?}", app.reading.doc(&app.docs, p).expect("doc").model);
    assert!(shown(&app, "r3lint/a.md").contains("old"));

    // b is dirty (an in-memory edit; b is the active tab).
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('Z'),
    ))));
    assert!(app.doc("r3lint/b.md").expect("b").is_dirty());
    let gen_before = d.generation().expect("gen");

    // The lint write: daemon-origin, incremental (no generation notification).
    for p in ["r3lint/a.md", "r3lint/b.md"] {
        std::fs::write(vault.join(p), "---\nstatus: linted\n---\nbody\n").expect("write");
        file_changed(&mut app, p, ChangeKind::Modified, Origin::Daemon);
    }
    assert_eq!(d.generation().expect("gen"), gen_before);
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
    assert!(
        shown(&app, "r3lint/a.md").contains("linted"),
        "clean buffer: Reading shows the new frontmatter"
    );
    let b = app.doc("r3lint/b.md").expect("b");
    assert!(
        b.is_dirty() && b.conflict().is_some(),
        "dirty: conflict bar"
    );
    assert!(
        !b.text().contains("linted"),
        "dirty buffer never overwritten"
    );
}
