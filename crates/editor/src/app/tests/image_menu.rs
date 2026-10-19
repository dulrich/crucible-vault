//! Image context menu, explorer row Move…, hover-to-focus.

use std::sync::Mutex;

use super::*;
use crate::clipboard_image::{ClipboardWriter, copy_image};
use crate::context_menu::{Item, MenuMsg, Target, items};
use crate::message::{PaletteMsg, PanesMsg, SourceMsg};
use iced::widget::pane_grid::Pane;

const AT: iced::Point = iced::Point::new(10.0, 10.0);

#[derive(Default)]
struct FakeWriter(Mutex<Vec<(String, Vec<u8>)>>);
impl ClipboardWriter for FakeWriter {
    fn write(&self, mime: &str, bytes: &[u8]) -> std::result::Result<(), String> {
        self.0
            .lock()
            .expect("lock")
            .push((mime.to_owned(), bytes.to_vec()));
        Ok(())
    }
}
struct FailWriter;
impl ClipboardWriter for FailWriter {
    fn write(&self, _: &str, _: &[u8]) -> std::result::Result<(), String> {
        Err("xclip unavailable: not found".into())
    }
}

fn img(src: &str, remote: bool) -> Target {
    Target::Image {
        src: src.into(),
        remote,
    }
}

fn labels(t: &Target) -> Vec<Item> {
    items(t).into_iter().map(|(i, _)| i).collect()
}

#[test]
fn image_menu_items_vault_vs_remote() {
    assert_eq!(
        labels(&img("a/p.png", false)),
        [Item::CopyImage, Item::CopyPath]
    );
    assert_eq!(labels(&img("https://x/p.png", true)), [Item::CopyUrl]);
    for t in [img("a/p.png", false), img("https://x/p.png", true)] {
        assert!(!labels(&t).contains(&Item::OpenInNewPane), "audit E1");
    }
}

#[test]
fn copy_image_writes_bytes_with_mime() {
    let (_tmp, d) = fake_arc();
    let p = d.vault_root().join("att");
    std::fs::create_dir_all(&p).expect("mkdir");
    std::fs::write(p.join("p.jpg"), b"\xff\xd8JPEG").expect("write");
    let w = FakeWriter::default();
    let r = copy_image(d.as_ref(), &w, "att/p.jpg");
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(
        *w.0.lock().expect("lock"),
        [("image/jpeg".to_owned(), b"\xff\xd8JPEG".to_vec())]
    );
    // A failing writer / missing file is an Err (status notice), never a panic.
    assert!(copy_image(d.as_ref(), &FailWriter, "att/p.jpg").is_err());
    assert!(copy_image(d.as_ref(), &w, "att/missing.png").is_err());
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Menu(MenuMsg::ImageCopied(Err("boom".into()))));
    assert_eq!(app.status.error_text(), Some("boom"));
}

#[test]
fn image_menu_copy_actions_route() {
    let (_tmp, d) = fake_arc();
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Menu(MenuMsg::Open(img("att/p.png", false), AT)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CopyImage)));
    assert_eq!(app.copied_images(), ["att/p.png"]);
    let _ = app.send(Message::Menu(MenuMsg::Open(img("att/p.png", false), AT)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CopyPath)));
    let _ = app.send(Message::Menu(MenuMsg::Open(
        img("https://x/r.png", true),
        AT,
    )));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CopyUrl)));
    assert_eq!(app.copied(), ["att/p.png", "https://x/r.png"]);
    assert!(app.context_menu.is_none());
}

#[test]
fn explorer_row_menu_has_move_and_view_has_no_button_row() {
    let (_tmp, d) = fake_arc();
    d.create_note("r4mv/a.md", "x").expect("create");
    let row = Target::Row {
        path: "r4mv/a.md".into(),
        folder: false,
    };
    assert!(labels(&row).contains(&Item::Move));
    let mut app = app_over(&d, None);
    let _ = app.send(Message::Menu(MenuMsg::Open(row, AT)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::Move)));
    let e = app.explorer.edit.as_ref().expect("inline edit");
    assert_eq!(e.kind, crate::explorer::EditKind::Move);
    assert_eq!(e.target, "r4mv/a.md");
    // The button row is gone: its buttons no longer appear in the explorer view source.
    let src = include_str!("../../explorer.rs");
    let view = &src[src.find("pub fn view<'a>").expect("view")..];
    let view = &view[..view.find("\n}\n").expect("end")];
    for label in ["\"New note\"", "\"Rename\"", "\"Move\"", "\"Trash\""] {
        assert!(!view.contains(label), "{label}");
    }
}

#[test]
fn focus_follows_mouse_pref_round_trips() {
    use crate::settings::editor_prefs::parse;
    use crate::settings::{DocEdit, ScalarValue};
    let med = crate::theme::Surround::Med;
    assert!(!parse("", med).expect("p").focus_follows_mouse);
    let mut doc = cv_schema::ConfigDoc::parse("").expect("doc");
    DocEdit::Set(
        cv_schema::Path::root().key("focus_follows_mouse"),
        ScalarValue::Bool(true),
    )
    .apply(&mut doc)
    .expect("apply");
    let text = doc.to_string();
    assert!(text.contains("focus_follows_mouse = true"), "{text}");
    assert!(parse(&text, med).expect("p").focus_follows_mouse);
}

/// Two panes: `a` in the first, `b` in the second (focused); hover focus on.
fn two_panes(app: &mut App) -> (Pane, Pane) {
    let _ = app.send(Message::Tabs(TabsMsg::Open("r4h/a.md".into())));
    let first = app.focus;
    let _ = app.send(Message::Panes(PanesMsg::SplitRight));
    let second = app.focus;
    let _ = app.send(Message::Tabs(TabsMsg::Open("r4h/b.md".into())));
    app.prefs.focus_follows_mouse = true;
    (first, second)
}

#[test]
fn hover_focus_ignored_when_off_with_modal_or_drag() {
    let (_tmp, d) = fake_arc();
    d.create_note("r4h/a.md", "aaa\n").expect("create");
    d.create_note("r4h/b.md", "bbb\n").expect("create");
    let mut app = app_over(&d, None);
    let (first, second) = two_panes(&mut app);
    app.prefs.focus_follows_mouse = false;
    let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
    assert_eq!(app.focus, second, "pref off");
    app.prefs.focus_follows_mouse = true;
    let _ = app.send(Message::Palette(PaletteMsg::Open));
    assert!(app.palette.open);
    let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
    assert_eq!(app.focus, second, "palette open");
    let _ = app.send(Message::Palette(PaletteMsg::Close));
    let _ = app.send(Message::Menu(MenuMsg::Open(img("x.png", false), AT)));
    let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
    assert_eq!(app.focus, second, "context menu open");
    let _ = app.send(Message::Menu(MenuMsg::Close));
    let _ = app.send(Message::Panes(PanesMsg::MouseButton(true)));
    let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
    assert_eq!(app.focus, second, "drag in progress");
    let _ = app.send(Message::Panes(PanesMsg::MouseButton(false)));
    let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
    assert_eq!(app.focus, first);
}

#[test]
fn hover_focus_routes_source_actions_to_the_entered_pane() {
    use iced::widget::text_editor::{Action, Edit};
    let (_tmp, d) = fake_arc();
    d.create_note("r4h/a.md", "aaa\n").expect("create");
    d.create_note("r4h/b.md", "bbb\n").expect("create");
    let mut app = app_over(&d, None);
    let (first, _second) = two_panes(&mut app);
    let _ = app.send(Message::Panes(PanesMsg::Hover(first)));
    assert_eq!(app.focus, first);
    assert_eq!(app.active_note().map(|t| t.path), Some("r4h/a.md"));
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('Z'),
    ))));
    assert!(app.doc("r4h/a.md").expect("a").text().contains('Z'));
    assert!(!app.doc("r4h/b.md").expect("b").text().contains('Z'));
}
