//! Context menus on tabs, explorer rows and notes.

use super::*;
use crate::context_menu::{Item, MenuMsg, Target};
use crate::message::{ExplorerMsg, SourceMsg, SwitcherMsg};

const AT: iced::Point = iced::Point::new(30.0, 40.0);

fn menu(m: MenuMsg) -> Message {
    Message::Menu(m)
}

fn open(app: &mut App, p: &str) {
    let _ = app.send(Message::Explorer(ExplorerMsg::Open(p.into())));
}

fn tab_target(app: &App, path: &str) -> Target {
    let ps = app.panes.get(app.focus).expect("pane");
    let tab = ps
        .tabs
        .iter()
        .find(|t| t.shows(path, &app.docs))
        .expect("tab");
    Target::Tab {
        pane: app.focus,
        id: tab.id,
        path: path.to_owned(),
    }
}

fn tab_paths(app: &App) -> Vec<String> {
    app.panes
        .get(app.focus)
        .map(|ps| {
            ps.tabs
                .iter()
                .filter_map(|t| t.path(&app.docs).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn row(path: &str, folder: bool) -> Target {
    Target::Row {
        path: path.into(),
        folder,
    }
}

#[test]
fn menu_opens_at_the_cursor_and_closes_on_escape_outside_click_and_choice() {
    let (_tmp, d) = fake_arc();
    d.create_note("m4/a.md", "x").expect("create");
    let mut app = app_over(&d, None);
    // The right-press wrapper's message carries the press point (the anchor).
    let Message::Menu(MenuMsg::Open(t, at)) =
        crate::context_menu::open_msg(row("m4/a.md", false))(AT)
    else {
        panic!("open message");
    };
    assert_eq!((t, at), (row("m4/a.md", false), AT));
    let _ = app.send(menu(MenuMsg::Open(row("m4/a.md", false), AT)));
    let m = app.context_menu.as_ref().expect("open");
    assert_eq!(m.at, iced::Point::new(30.0, 40.0));
    // Escape (the switcher's Close) is the menu's Close.
    let _ = app.send(Message::Switcher(SwitcherMsg::Close));
    assert!(app.context_menu.is_none());
    // Outside click.
    let _ = app.send(menu(MenuMsg::Open(row("m4/a.md", false), AT)));
    let _ = app.send(menu(MenuMsg::Close));
    assert!(app.context_menu.is_none());
    // Choosing an item closes it and runs it.
    let _ = app.send(menu(MenuMsg::Open(row("m4/a.md", false), AT)));
    let _ = app.send(menu(MenuMsg::Choose(Item::Open)));
    assert!(app.context_menu.is_none());
    assert_eq!(
        app.active_tab().and_then(|t| t.path(&app.docs)),
        Some("m4/a.md")
    );
    // Pane input is dropped while it is open.
    let _ = app.send(menu(MenuMsg::Open(row("m4/a.md", false), AT)));
    let _ = app.send(Message::Source(SourceMsg::Action(
        iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Insert('Z')),
    )));
    assert!(!app.active_doc().expect("doc").is_dirty());
    assert!(app.context_menu.is_some());
}

#[test]
fn task_status_menu_sets_the_marker_as_one_undo_step() {
    let (_tmp, d) = fake_arc();
    d.create_note("m4t/t.md", "- [ ] a\n- [x] b\n")
        .expect("create");
    let mut app = app_over(&d, None);
    open(&mut app, "m4t/t.md");
    let _ = app.send(menu(MenuMsg::Open(
        Target::Task {
            path: "m4t/t.md".into(),
            line: 1,
        },
        AT,
    )));
    let _ = app.send(menu(MenuMsg::Choose(Item::TaskStatus('!'))));
    let doc = app.active_doc().expect("doc");
    assert_eq!(doc.text(), "- [ ] a\n- [!] b\n");
    assert!(doc.is_dirty(), "saved by the normal autosave path");
    assert_eq!(
        d.read_file("m4t/t.md").expect("file").0,
        b"- [ ] a\n- [x] b\n"
    );
    let _ = app.send(Message::Source(SourceMsg::Undo));
    assert_eq!(app.active_doc().expect("doc").text(), "- [ ] a\n- [x] b\n");
}

#[test]
fn tab_menu_close_others_copy_path_reveal_and_delete() {
    let (_tmp, d) = fake_arc();
    for p in ["m4tab/a.md", "m4tab/b.md", "m4tab/c.md"] {
        d.create_note(p, "x").expect("create");
    }
    let mut app = app_over(&d, None);
    for p in ["m4tab/a.md", "m4tab/b.md", "m4tab/c.md"] {
        open(&mut app, p);
    }
    assert_eq!(tab_paths(&app).len(), 3);
    let t = tab_target(&app, "m4tab/b.md");
    let _ = app.send(menu(MenuMsg::Open(t.clone(), AT)));
    let _ = app.send(menu(MenuMsg::Choose(Item::CloseOthers)));
    assert_eq!(tab_paths(&app), ["m4tab/b.md"]);

    let _ = app.send(menu(MenuMsg::Open(t.clone(), AT)));
    let _ = app.send(menu(MenuMsg::Choose(Item::CopyPath)));
    assert_eq!(app.copied(), ["m4tab/b.md"]);

    let _ = app.send(Message::Panes(crate::message::PanesMsg::Side(
        crate::ui_state::SideSection::Outline,
    )));
    let _ = app.send(menu(MenuMsg::Open(t.clone(), AT)));
    let _ = app.send(menu(MenuMsg::Choose(Item::Reveal)));
    assert_eq!(app.ui.side.0, Some(crate::ui_state::SideSection::Files));
    assert!(app.ui.expanded.contains("m4tab"));
    assert_eq!(app.explorer.selected.as_deref(), Some("m4tab/b.md"));

    let _ = app.send(menu(MenuMsg::Open(t, AT)));
    let _ = app.send(menu(MenuMsg::Choose(Item::Delete)));
    assert!(app.dialog.is_some(), "Delete routes to the delete dialog");
    assert!(app.context_menu.is_none());
    // The dialog outranks the menu: a right-press is dropped.
    let t = tab_target(&app, "m4tab/b.md");
    let _ = app.send(menu(MenuMsg::Open(t, AT)));
    assert!(app.context_menu.is_none());
}

#[test]
fn explorer_row_menu_open_in_new_pane_rename_and_folder_new_note() {
    let (_tmp, d) = fake_arc();
    for p in ["m4row/a.md", "m4row/b.md"] {
        d.create_note(p, "x").expect("create");
    }
    let mut app = app_over(&d, None);
    open(&mut app, "m4row/a.md");
    let first = app.focus;
    let _ = app.send(menu(MenuMsg::Open(row("m4row/a.md", false), AT)));
    let _ = app.send(menu(MenuMsg::Choose(Item::OpenInNewPane)));
    assert_eq!(app.panes.len(), 2);
    assert_ne!(app.focus, first);
    assert_eq!(
        app.active_tab().and_then(|t| t.path(&app.docs)),
        Some("m4row/a.md")
    );

    let _ = app.send(menu(MenuMsg::Open(row("m4row/b.md", false), AT)));
    let _ = app.send(menu(MenuMsg::Choose(Item::Rename)));
    let e = app.explorer.edit.as_ref().expect("inline edit");
    assert_eq!(e.kind, crate::explorer::EditKind::Rename);
    assert_eq!(e.target, "m4row/b.md");
    assert_eq!(e.input, "b.md");

    let _ = app.send(menu(MenuMsg::Open(row("m4row", true), AT)));
    let _ = app.send(menu(MenuMsg::Choose(Item::NewNote)));
    let e = app.explorer.edit.as_ref().expect("inline edit");
    assert_eq!(e.kind, crate::explorer::EditKind::New);
    assert_eq!(e.target, "m4row");
}
