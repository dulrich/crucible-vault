//! Plan intake-removals-sources-rescan WP-6: the removal review flow — fold controls' clicks,
//! the save before the request, the status texts, "Next note with removals" and "Trim selection
//! as removed" — against the `FakeDaemon` (scripted `resolve_removals` / `next_fold_note`).

use cv_protocol::{
    NextFoldNoteResult, RemovalKind, RemovalOp, RemovalRefusal, RemovalVerdict, RemovalsApplied,
    ResolveRemovalsResult,
};
use iced::widget::text_editor;

use super::commands::{app_with, open, type_char};
use super::*;
use crate::message::{ReadingMsg, RemovalsMsg};
use crate::reading::ViewMode;

const NOTE: &str = "rm/a.md";

/// A note with frontmatter and a two-group fold (written by the pure fold API).
fn folded() -> String {
    let body = "intro\n\nSponsor one\nSponsor two\n\nmiddle\n\nSubscribe now\n\nend\n";
    let body = cv_ingest::enrich::fold_ranges(
        body,
        &[
            (2, 3, cv_ingest::enrich::SpanKind::Sponsor),
            (7, 7, cv_ingest::enrich::SpanKind::Cta),
        ],
    )
    .expect("fold");
    format!("---\ntitle: T\n---\n{body}")
}

fn applied() -> RemovalsApplied {
    RemovalsApplied {
        hash: "h".into(),
        path: NOTE.into(),
        groups: 1,
        lines_labelled: 2,
        restored_at_anchor: 0,
        restored_at_end: 0,
        media_queued: 0,
        cleanup_job: None,
        cleanup_failed: false,
        labels_failed: false,
    }
}

fn notice(app: &App) -> String {
    app.status
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default()
}

fn click(app: &mut App, op: RemovalOp) {
    let _ = app.send(Message::Reading(ReadingMsg::Removals {
        path: NOTE.into(),
        op,
    }));
}

fn first_group(app: &App) -> String {
    app.reading.doc(&app.docs, NOTE).expect("doc").fold_groups[0]
        .id
        .clone()
}

fn fixture(text: &str) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let (tmp, d, mut app) = app_with(&[(NOTE, text)]);
    open(&mut app, NOTE);
    d.set_removals_reply(Some(ResolveRemovalsResult::Applied(applied())))
        .expect("script");
    crate::reading::tests::reading(&mut app, NOTE, ViewMode::Reading);
    (tmp, d, app)
}

/// Requirement: groups come from `fold_groups` with their kinds; an old-layout fold has none;
/// a note without a fold has no groups (so its layout is unchanged).
#[test]
fn doc_carries_the_fold_groups() {
    let (_t, _d, app) = fixture(&folded());
    let g = &app.reading.doc(&app.docs, NOTE).expect("doc").fold_groups;
    let kinds: Vec<_> = g
        .iter()
        .map(|g| g.kind.as_ref().map(|k| k.as_str().to_owned()))
        .collect();
    assert_eq!(kinds, [Some("sponsor".to_owned()), Some("cta".to_owned())]);

    let old = "x\n\n%% cv-enrich %%\n> [!removed]-\n> old line\n\n%% /cv-enrich %%\n";
    let (_t, _d, app) = fixture(old);
    let g = &app.reading.doc(&app.docs, NOTE).expect("doc").fold_groups;
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].kind, None, "old layout has no kind");

    let (_t, _d, app) = fixture("---\ntitle: T\n---\nplain\n");
    assert!(
        app.reading
            .doc(&app.docs, NOTE)
            .expect("doc")
            .fold_groups
            .is_empty()
    );
}

/// Requirement: a Delete click sends the note id, the saved hash and the group id.
#[test]
fn delete_click_sends_id_hash_and_group() {
    let (_t, d, mut app) = fixture(&folded());
    let g = first_group(&app);
    let op = RemovalOp::Group {
        group: g,
        verdict: RemovalVerdict::Delete,
    };
    click(&mut app, op.clone());
    let calls = d.removals_calls().expect("calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(Some(calls[0].id), app.note_id(NOTE));
    assert_eq!(
        calls[0].base_hash,
        d.read_file(NOTE).expect("file").1.as_str()
    );
    assert_eq!(calls[0].op, op);
    assert!(notice(&app).starts_with("Removals applied: 1 groups resolved, 2 lines labelled"));
}

/// Requirement: a dirty buffer is saved first; the request carries the post-save hash.
#[test]
fn dirty_buffer_saves_before_the_request() {
    let (_t, d, mut app) = fixture(&folded());
    let before = d.read_file(NOTE).expect("file").1;
    type_char(&mut app, 'x');
    click(&mut app, crate::reading::fold_controls::delete_all());
    let calls = d.removals_calls().expect("calls");
    assert_eq!(calls.len(), 1, "{:?}", notice(&app));
    let (_, after) = d.read_file(NOTE).expect("file");
    assert_ne!(after, before, "the edit was saved");
    assert_eq!(calls[0].base_hash, after.as_str(), "the post-save hash");
}

/// Requirement: a save conflict aborts with the save error and sends nothing.
#[test]
fn save_conflict_sends_nothing() {
    let (tmp, d, mut app) = fixture(&folded());
    type_char(&mut app, 'x');
    std::fs::write(tmp.path().join("vault").join(NOTE), "changed elsewhere\n").expect("write");
    click(&mut app, crate::reading::fold_controls::delete_all());
    assert!(d.removals_calls().expect("calls").is_empty());
    let e = app.status.error_text().expect("error").to_owned();
    assert!(e.contains("save conflict"), "{e}");
}

/// Requirement: applied counts (media worded as queued, never trashed), failures reported.
#[test]
fn applied_status_is_counts_only() {
    use crate::app::removals::applied_text;
    let a = RemovalsApplied {
        groups: 2,
        lines_labelled: 5,
        restored_at_anchor: 1,
        restored_at_end: 2,
        media_queued: 3,
        ..applied()
    };
    let t = applied_text(&a);
    assert_eq!(
        t,
        "Removals applied: 2 groups resolved, 5 lines labelled, 3 lines restored, 3 media queued for cleanup"
    );
    assert!(!t.contains("trash"));
    let f = applied_text(&RemovalsApplied {
        labels_failed: true,
        cleanup_failed: true,
        ..applied()
    });
    assert!(
        f.contains("labels could not be recorded") && f.contains("cleanup could not be queued")
    );
}

/// Requirement: each refusal has its own short text, shown as an error.
#[test]
fn each_refusal_has_its_own_text() {
    let (_t, d, mut app) = fixture(&folded());
    let mut seen = Vec::new();
    for r in [
        RemovalRefusal::StaleHash,
        RemovalRefusal::UnknownGroup,
        RemovalRefusal::BadRange,
        RemovalRefusal::Generated,
        RemovalRefusal::UnknownNote,
    ] {
        d.set_removals_reply(Some(ResolveRemovalsResult::Refused {
            reason: r,
            current_hash: None,
        }))
        .expect("script");
        click(&mut app, crate::reading::fold_controls::delete_all());
        seen.push(app.status.error_text().expect("error").to_owned());
    }
    assert_eq!(seen[0], "Removals: the note changed; try again");
    let mut uniq = seen.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 5, "{seen:?}");
}

/// Requirement: an older daemon reports "restart the daemon", for both requests.
#[test]
fn old_daemon_says_restart() {
    let (_t, d, mut app) = fixture(&folded());
    d.set_removals_reply(None).expect("script");
    click(&mut app, crate::reading::fold_controls::delete_all());
    let e = app.status.error_text().expect("error").to_owned();
    assert!(e.contains("restart it after installing"), "{e}");
    let _ = app.send(Message::Removals(RemovalsMsg::Next));
    let e = app.status.error_text().expect("error").to_owned();
    assert!(
        e.starts_with("Next note with removals:") && e.contains("restart"),
        "{e}"
    );
}

/// Requirement: next-with-removals opens the returned note; `None` says so; the active note's id
/// is sent (none active: neither id nor path).
#[test]
fn next_note_opens_the_returned_path_or_reports_none() {
    let (_t, d, mut app) = app_with(&[(NOTE, "a\n"), ("rm/b.md", "b\n")]);
    d.set_next_fold_reply(Some(NextFoldNoteResult { path: None }))
        .expect("script");
    let _ = app.send(Message::Removals(RemovalsMsg::Next));
    let calls = d.next_fold_calls().expect("calls");
    assert_eq!(
        (calls[0].id, calls[0].path.clone()),
        (None, None),
        "none active"
    );
    open(&mut app, NOTE);
    d.set_next_fold_reply(Some(NextFoldNoteResult {
        path: Some("rm/b.md".into()),
    }))
    .expect("script");
    let _ = app.send(Message::Removals(RemovalsMsg::Next));
    let calls = d.next_fold_calls().expect("calls");
    assert_eq!(calls[1].id, app.note_id(NOTE));
    assert_eq!(
        app.active_tab().and_then(|t| t.path(&app.docs)),
        Some("rm/b.md")
    );
    d.set_next_fold_reply(Some(NextFoldNoteResult { path: None }))
        .expect("script");
    let _ = app.send(Message::Removals(RemovalsMsg::Next));
    assert_eq!(notice(&app), "no other note has removals");
}

const FM_NOTE: &str = "---\ntitle: T\n---\nintro\n\nfirst line\nsecond line\n\nend\n";

fn trim(kind: RemovalKind) -> Message {
    Message::Removals(RemovalsMsg::Trim(kind))
}

/// Requirement: Trim in Source maps the selected whole lines to 0-based inclusive body lines
/// (frontmatter excluded) and carries the chosen kind.
#[test]
fn trim_in_source_sends_the_body_relative_range() {
    let (_t, d, mut app) = fixture(FM_NOTE);
    crate::reading::tests::reading(&mut app, NOTE, ViewMode::Source);
    // Full-note lines 5..=6 ("first line", "second line"), selected from mid-line to mid-line.
    app.docs
        .get_mut(NOTE)
        .expect("doc")
        .buffer
        .move_to(text_editor::Cursor {
            position: text_editor::Position { line: 6, column: 4 },
            selection: Some(text_editor::Position { line: 5, column: 2 }),
        });
    let _ = app.send(trim(RemovalKind::Cta));
    let calls = d.removals_calls().expect("calls");
    assert_eq!(calls.len(), 1, "{:?}", app.status.error_text());
    assert_eq!(
        calls[0].op,
        RemovalOp::Trim {
            first: 2,
            last: 3,
            kind: Some(RemovalKind::Cta)
        }
    );
}

/// Requirement: Trim in Reading maps the drawn selection to the same body-relative lines.
#[test]
fn trim_in_reading_sends_the_body_relative_range() {
    let (_t, d, mut app) = fixture(FM_NOTE);
    let view = app.view_of(NOTE);
    let run = {
        let doc = app.reading.doc(&app.docs, NOTE).expect("doc");
        doc.runs
            .iter()
            .position(|r| r.text == "end")
            .expect("end run")
    };
    let pos = |byte| editor_model::reading::SelPos { run, byte };
    let _ = app.send(Message::Reading(ReadingMsg::Select {
        path: NOTE.into(),
        view,
        sel: Some(editor_model::reading::Selection::new(pos(0), pos(3))),
    }));
    let _ = app.send(trim(RemovalKind::Footer));
    let calls = d.removals_calls().expect("calls");
    assert_eq!(calls.len(), 1, "{:?}", app.status.error_text());
    // Full-note line 8 ("end") is body line 5.
    assert_eq!(
        calls[0].op,
        RemovalOp::Trim {
            first: 5,
            last: 5,
            kind: Some(RemovalKind::Footer)
        }
    );
}

/// Requirement: no selection sends nothing and says so.
#[test]
fn trim_without_a_selection_sends_nothing() {
    let (_t, d, mut app) = fixture(FM_NOTE);
    let _ = app.send(trim(RemovalKind::Sponsor));
    assert!(d.removals_calls().expect("calls").is_empty());
    let e = app.status.error_text().expect("error").to_owned();
    assert!(e.contains("select the lines"), "{e}");
}
