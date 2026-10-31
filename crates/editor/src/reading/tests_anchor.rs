//! Plan round-19-yt-cleanup-intake WP-1: `[[Note#Heading]]` / `[[#Heading]]` navigation, the
//! mode-aware Outline jump and the tab-strip scroll fraction.

use super::*;

/// Source lines: `# Top` 0, `## Target` at line 8, with blocks between.
const NOTE: &[u8] = b"# Top\n\none\n\ntwo\n\nthree\n\n## Target\n\nbody\n\n## Target\n\nlater\n";

fn open_link(src: &str, target: &str, fragment: Option<&str>) -> Message {
    Message::Reading(ReadingMsg::OpenLink {
        src: src.into(),
        target: target.into(),
        fragment: fragment.map(str::to_owned),
    })
}

fn tab_count(app: &App) -> usize {
    app.panes.iter().map(|(_, ps)| ps.tabs.len()).sum()
}

fn top(app: &App, p: &str) -> Option<usize> {
    app.reading.top_block(&app.docs, p, app.view_of(p))
}

#[test]
fn same_note_reading_click_scrolls_to_the_heading_block_without_a_tab() {
    let p = "a.md";
    let (_t, d) = setup(&[(p, NOTE)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Reading);
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
    let want = app
        .reading
        .doc(&app.docs, p)
        .expect("doc")
        .model
        .block_for_line(8)
        .expect("heading block");
    assert!(want > 0);
    assert_eq!(top(&app, p), None);
    // Ctrl held: a same-note anchor still opens no tab.
    app.modifiers = iced::keyboard::Modifiers::CTRL;
    let n = tab_count(&app);
    let task = app.send(open_link(p, "", Some("Target")));
    assert!(task.units() > 0, "a scroll task");
    assert_eq!(
        top(&app, p),
        Some(want),
        "the stored top is the heading's block"
    );
    assert_eq!(tab_count(&app), n);
}

#[test]
fn same_note_source_jump_moves_the_cursor_to_the_heading() {
    let p = "a.md";
    let (_t, d) = setup(&[(p, NOTE)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Source);
    let _ = app.send(open_link(p, "", Some(" Target ")));
    assert_eq!(app.active_doc().expect("doc").cursor().position.line, 8);
}

#[test]
fn cross_note_open_then_jump_and_unknown_fragment_stays_at_top() {
    let (_t, d) = setup(&[("a.md", b"[[B#Target]]\n"), ("B.md", NOTE)]);
    let mut app = app_on(&d, "a.md");
    let _ = app.send(open_link("a.md", "B", Some("Target")));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("B.md"));
    let want = app
        .reading
        .doc(&app.docs, "B.md")
        .expect("doc")
        .model
        .block_for_line(8);
    assert!(want.is_some());
    assert_eq!(top(&app, "B.md"), want);
    // No match / block ref: opens, no stored place, no error.
    let (_t, d) = setup(&[("a.md", b"x\n"), ("B.md", NOTE)]);
    let mut app = app_on(&d, "a.md");
    let _ = app.send(open_link("a.md", "B", Some("Nope")));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("B.md"));
    let _ = app.send(open_link("B.md", "", Some("^abc")));
    assert_eq!(top(&app, "B.md"), None);
    assert!(app.status.notice.is_none());
}

#[test]
fn outline_jump_scrolls_reading() {
    let p = "a.md";
    let (_t, d) = setup(&[(p, NOTE)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Reading);
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
    let task = app.send(Message::Panels(crate::message::PanelsMsg::OutlineJump(12)));
    assert!(task.units() > 0);
    let want = app
        .reading
        .doc(&app.docs, p)
        .and_then(|d| d.model.block_for_line(12));
    assert!(want.is_some());
    assert_eq!(top(&app, p), want);
}

#[test]
fn heading_match_and_fragment_split_rules() {
    use crate::app::scroll::heading_line;
    assert_eq!(
        heading_line(std::str::from_utf8(NOTE).unwrap(), "Target"),
        Some(8)
    );
    assert_eq!(heading_line("# A\n", " A "), Some(0));
    assert_eq!(heading_line("# A\n", "a"), None);
    assert_eq!(heading_line("# A\n", "^blk"), None);
    assert_eq!(heading_line("# A\n", ""), None);
    assert_eq!(links::split_fragment("N#H"), ("N".into(), Some("H".into())));
    assert_eq!(
        links::split_fragment("#H"),
        (String::new(), Some("H".into()))
    );
    assert_eq!(links::split_fragment("N#"), ("N".into(), None));
    assert_eq!(links::split_fragment("N"), ("N".into(), None));
}

#[test]
fn tab_strip_fraction_and_activation_scroll() {
    use crate::app::scroll::strip_fraction;
    assert_eq!(strip_fraction(0, 1), 0.0);
    assert_eq!(strip_fraction(0, 5), 0.0);
    assert_eq!(strip_fraction(4, 5), 1.0);
    assert!((strip_fraction(2, 5) - 0.5).abs() < f32::EPSILON);
    let files: Vec<(String, Vec<u8>)> = (0..30)
        .map(|i| (format!("n{i}.md"), b"x\n".to_vec()))
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let (_t, d) = setup(&refs);
    let mut app = app_on(&d, "n0.md");
    for (p, _) in &files[1..] {
        let _ = app.send(Message::Tabs(TabsMsg::Open(p.clone())));
    }
    assert_eq!(tab_count(&app), 30);
    // Opening a tab snaps the strip; the many-tab bar still builds.
    let task = app.send(Message::Tabs(TabsMsg::Open("n3.md".into())));
    assert!(task.units() > 0);
    drop(app.view());
    let _ = app.send(Message::Tabs(TabsMsg::DragPress(app.focus, 5)));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("n5.md"));
}
