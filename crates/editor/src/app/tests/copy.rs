//! Copy feedback tests: plan round-21 WP-2 (the Reading code-block copy button, with its
//! confirmation and find-paint interplay) and plan round-22 WP-2 (the app side of inline-code
//! copy — a "Copied" status notice).

use super::unsaved_dialog::setup;
use super::*;
use crate::message::ReadingMsg;
use crate::reading::ViewMode;
use crate::reading::tests::{bar, find_on, reading, strong_hit};
use crate::reading::view;
use editor_model::render::{self, Block};

#[test]
fn inline_code_copy_shows_a_copied_notice() {
    let (_tmp, _d, mut app) = setup(&["c/a.md"]);
    assert!(app.status.notice.is_none());
    let task = app.send(Message::Reading(ReadingMsg::Copy {
        text: "certutil -hashfile x".into(),
        feedback: crate::message::CopyFeedback::Notice,
    }));
    assert!(task.units() > 0, "the value went to the clipboard");
    assert_eq!(
        app.status.notice.as_ref().map(|n| n.text.as_str()),
        Some("Copied")
    );
}

fn code_texts(src: &str) -> Vec<String> {
    render::lower(src)
        .blocks
        .into_iter()
        .filter_map(|b| match b {
            Block::CodeBlock { text, .. } => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn copy_message_carries_exactly_the_block_text() {
    let texts = code_texts("```rust\nfn a() {}\nlet x = 1;\n```\n\n```\nplain\n```\n");
    assert_eq!(texts.len(), 2);
    for (i, code) in texts.iter().enumerate() {
        match view::copy_message("n.md", 3, i, code) {
            Message::Reading(ReadingMsg::Copy {
                text,
                feedback: crate::message::CopyFeedback::Block { path, view, run },
            }) => {
                assert_eq!((path.as_str(), view, run), ("n.md", 3, i));
                assert_eq!(&text, code);
                assert!(!text.contains("```") && !text.contains("rust"), "{text:?}");
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn copy_click_writes_the_clipboard_and_confirms_then_clears() {
    let p = "r21/code.md";
    let (_t, mut app) = find_on(p, b"```rust\nfn a() {}\n```\n", ViewMode::Reading, "fn");
    let view = app.active_tab().map(|t| t.id).expect("tab");
    let copied = |app: &App| app.reading.code_copied(&app.docs, p, view);
    assert_eq!(copied(&app), None);
    drop(app.view());
    let t = app.send(view::copy_message(p, view, 0, "fn a() {}\n"));
    assert!(t.units() > 0, "the code went to the clipboard");
    assert_eq!(copied(&app), Some(0));
    assert!(
        app.status.notice.is_none(),
        "a block confirms in its button"
    );
    drop(app.view());
    let _ = app.send(Message::Reading(ReadingMsg::CodeCopiedDone {
        path: p.into(),
        view,
        run: 0,
    }));
    assert_eq!(copied(&app), None);
}

/// WP-R7 (review E1): the copied state is per block, find paint is unchanged, and Source draws
/// no Reading code block (so no copy button).
#[test]
fn copied_state_is_per_block_and_find_paint_is_unchanged() {
    let p = "r21/two.md";
    let body = b"```\nfoo one\n```\n\n```\nfoo two\n```\n";
    let (_t, mut app) = find_on(p, body, ViewMode::Reading, "foo");
    assert_eq!(bar(&app).count(), "1/2");
    let view = app.active_tab().map(|t| t.id).expect("tab");
    let _ = app.send(view::copy_message(p, view, 1, "foo two\n"));
    // A timer for another block leaves this block's state alone.
    let _ = app.send(Message::Reading(ReadingMsg::CodeCopiedDone {
        path: p.into(),
        view,
        run: 0,
    }));
    assert_eq!(app.reading.code_copied(&app.docs, p, view), Some(1));
    drop(app.view());
    assert_eq!(bar(&app).count(), "1/2");
    assert_eq!(strong_hit(&app, p), Some((0, 0)));
    reading(&mut app, p, ViewMode::Source);
    assert!(
        app.reading.doc(&app.docs, p).is_none(),
        "Source renders no Reading code block, so no copy button"
    );
    drop(app.view());
}
