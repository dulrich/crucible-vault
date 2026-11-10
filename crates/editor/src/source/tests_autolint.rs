//! Plan round-9-feedback-todos WP-6 (D26): auto-lint at autosave, against bare buffers.

use std::time::Instant;

use iced::widget::text_editor::{Action, Cursor, Edit, Position};

use super::SourceBuffer;
use super::autolint::{AutoLint, lint_settings, template_paths};
use crate::daemon::ContentHash;

const NOTE: &str = "---\ntitle: T\n---\n\nbody text\nsecond line\n";

fn cfg() -> AutoLint {
    AutoLint {
        enabled: true,
        settings: lint_settings(None).unwrap(),
        templates: Vec::new(),
        today: "2026-10-22".into(),
    }
}

fn buffer(path: &str) -> SourceBuffer {
    SourceBuffer::new(
        crate::daemon::bound_id(path),
        path,
        NOTE.as_bytes(),
        ContentHash::of(NOTE.as_bytes()),
    )
}

fn at(line: usize, column: usize) -> Cursor {
    Cursor {
        position: Position { line, column },
        selection: None,
    }
}

/// A user edit in the body: types `x` at the end of line 4 ("body text").
fn user_edit(b: &mut SourceBuffer) {
    b.move_to(at(4, 9));
    assert!(b.perform(Action::Edit(Edit::Insert('x')), Instant::now()));
}

fn cursor_line_text(b: &SourceBuffer) -> String {
    b.line_text(b.cursor().position.line).unwrap()
}

#[test]
fn lints_the_frontmatter_after_a_user_edit_and_keeps_body_and_cursor() {
    let mut b = buffer("a.md");
    user_edit(&mut b);
    let seq = b.edit_seq();
    assert!(b.auto_lint(&cfg(), Instant::now()));
    assert!(b.edit_seq() > seq && b.is_dirty());
    let text = b.text();
    assert!(text.contains("updated: 2026-10-22"), "{text}");
    assert!(text.contains("word-count:"), "{text}");
    assert!(
        text.ends_with("\n\nbody textx\nsecond line\n"),
        "body untouched: {text:?}"
    );
    // The cursor is still just after the typed `x`, on the same text.
    assert_eq!(cursor_line_text(&b), "body textx");
    assert_eq!(b.cursor().position.column, 10);
    // Idempotent: a second pass has nothing to change.
    assert!(!b.auto_lint(&cfg(), Instant::now()));
}

#[test]
fn works_on_the_plain_representation_too() {
    let mut b = buffer("a.md");
    user_edit(&mut b);
    b.dematerialise();
    assert!(b.auto_lint(&cfg(), Instant::now()));
    assert_eq!(cursor_line_text(&b), "body textx");
    assert!(b.text().contains("updated: 2026-10-22"));
}

#[test]
fn never_lints_on_open_or_reload() {
    let mut b = buffer("a.md");
    assert!(!b.auto_lint(&cfg(), Instant::now()));
    assert_eq!(b.text(), NOTE);
    user_edit(&mut b);
    b.reload(NOTE.as_bytes(), ContentHash::of(NOTE.as_bytes()));
    assert!(!b.auto_lint(&cfg(), Instant::now()));
    assert_eq!(b.text(), NOTE);
}

#[test]
fn skipped_while_the_cursor_is_in_the_frontmatter() {
    let mut b = buffer("a.md");
    user_edit(&mut b);
    b.move_to(at(1, 3));
    assert!(!b.auto_lint(&cfg(), Instant::now()));
    assert!(!b.text().contains("updated"));
}

#[test]
fn skipped_for_custom_and_template_folders() {
    let mut b = buffer("_custom/Settings.md");
    user_edit(&mut b);
    assert!(!b.auto_lint(&cfg(), Instant::now()));
    let mut b = buffer("tpl/daily/x.md");
    user_edit(&mut b);
    let mut c = cfg();
    c.templates = template_paths(Some(
        "[periods.day]\nfolder = \"d\"\ntemplate = \"tpl/daily/day.md\"\n",
    ));
    assert_eq!(c.templates, ["tpl/daily/day.md"]);
    assert!(!b.auto_lint(&c, Instant::now()));
}

#[test]
fn skipped_when_disabled_or_without_frontmatter() {
    let mut b = buffer("a.md");
    user_edit(&mut b);
    let mut c = cfg();
    c.enabled = false;
    assert!(!b.auto_lint(&c, Instant::now()));

    let bare = "just a body\n";
    let mut b = SourceBuffer::new(
        crate::daemon::bound_id("b.md"),
        "b.md",
        bare.as_bytes(),
        ContentHash::of(bare.as_bytes()),
    );
    b.move_to(at(0, 4));
    assert!(b.perform(Action::Edit(Edit::Insert('x')), Instant::now()));
    assert!(!b.auto_lint(&cfg(), Instant::now()));
}

#[test]
fn one_undo_reverts_the_user_edit_and_the_lint_together() {
    let mut b = buffer("a.md");
    user_edit(&mut b);
    assert!(b.auto_lint(&cfg(), Instant::now()));
    let linted = b.text();
    assert_eq!((b.history().undo_len(), b.history().redo_len()), (1, 0));
    assert!(b.undo());
    assert_eq!(b.text(), NOTE);
    assert_eq!(b.cursor().position, Position { line: 4, column: 9 });
    assert_eq!((b.history().undo_len(), b.history().redo_len()), (0, 1));
    // After an undo auto-lint holds off (nothing to join into, redo pending).
    assert!(!b.auto_lint(&cfg(), Instant::now()));
    assert!(b.redo());
    assert_eq!(b.text(), linted);
    assert_eq!(cursor_line_text(&b), "body textx");
}

/// The App wiring: an `AutosaveDue` tick lints first, then saves the linted text; `auto_lint =
/// false` in `editor.toml`'s prefs disables it.
#[test]
fn autosave_tick_lints_then_saves_through_the_app() {
    use std::sync::Arc;

    use cv_testkit::{SyntheticVault, VaultSpec};

    use crate::app::App;
    use crate::daemon::{DaemonClient, FakeDaemon};
    use crate::message::{Message, SourceMsg, TabsMsg};

    for enabled in [true, false] {
        let tmp = tempfile::tempdir().expect("tmp");
        let vault = tmp.path().join("vault");
        SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
        let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
        let p = "wp6/note.md";
        d.create_note(p, NOTE).expect("create");
        let dc: Arc<dyn DaemonClient> = d.clone();
        let mut app = App::new(Some(dc), None);
        app.queue_rpcs(); // WP-R4: a note open completes through its queued RPC
        app.prefs.auto_lint = enabled;
        let _ = app.send(Message::Tabs(TabsMsg::Open(p.to_owned())));
        let b = app.active_doc_mut().expect("doc");
        user_edit(b);
        let seq = b.edit_seq();
        let id = app.note_id(p).expect("open");
        let _ = app.send(Message::Source(SourceMsg::AutosaveDue { id, seq }));
        let b = app.active_doc_mut().expect("doc");
        assert_eq!(b.text().contains("updated: "), enabled);
        assert!(b.is_saving(), "the (linted) text went out as the save");
        assert_eq!(cursor_line_text(b), "body textx");
    }
}

#[test]
fn malformed_lint_params_are_an_error_not_defaults() {
    assert!(
        lint_settings(Some(
            "[[action]]\nid = \"lint-note\"\n[[action.steps]]\ntype = \"lint\"\ncreated_key = 3\n"
        ))
        .is_err()
    );
    let s = lint_settings(Some(
        "[[action]]\nid = \"lint-note\"\n[[action.steps]]\ntype = \"lint\"\nmodified_key = \"\"\n",
    ))
    .unwrap();
    assert_eq!(s.modified_key, "");
}
