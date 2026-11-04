//! Plan round-22 WP-2: the app side of inline-code copy — a "Copied" status notice.

use super::unsaved_dialog::setup;
use super::*;
use crate::message::ReadingMsg;

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
