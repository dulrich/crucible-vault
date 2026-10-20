//! The modal filter, context-menu dispatch and missing-link creation.

use super::*;

impl App {
    /// Plan round-6 WP-4: an unresolved vault link was clicked — `missing_link = "warn"` raises
    /// the notice naming it, `"create"` creates and opens the note.
    pub(super) fn missing_link_click(&mut self, target: &str) -> Task<Message> {
        match self.prefs.missing_link {
            crate::settings::editor_prefs::MissingLink::Warn => {
                self.status
                    .notify(Severity::Warning, format!("unresolved link: {target}"));
                Task::none()
            }
            crate::settings::editor_prefs::MissingLink::Create => self.create_missing(target),
        }
    }

    /// Plan round-6 WP-4: creates the note a missing link names ([`crate::reading::missing_note_path`]
    /// in the explorer's New-note folder) through `CreateNote`; `ExplorerMsg::Done` opens it in
    /// the focused (clicked) pane, a failure is an error notice.
    fn create_missing(&mut self, target: &str) -> Task<Message> {
        let path = crate::reading::missing_note_path(target, &self.explorer.target_folder());
        self.dispatch_op(FileOp::Create { path })
    }

    /// plan editor-data-safety WP-2 (audit E3): while a dialog is open it takes input
    /// priority — user input (keys, chords, tab / editor / find / switcher messages, the
    /// focus-loss flush, a repeated close request) is dropped; async completions (saves,
    /// notifications, loads) still run. Escape (the switcher's Close) is the dialog's Cancel.
    ///
    /// plan editor-ui-round-3 WP-4: an open context menu ranks next (below the dialog, above
    /// pane keys): the same user input is dropped, and Escape is the menu's Close.
    pub(super) fn modal_filter(&self, m: Message) -> Option<Message> {
        use crate::context_menu::MenuMsg;
        if self.dialog.is_some() {
            return match m {
                Message::Switcher(SwitcherMsg::Close | SwitcherMsg::Escape) => {
                    Some(Message::Dialog(crate::dialog::DialogMsg::Cancel))
                }
                // WP-5: Escape is the dialog's (via the switcher listener), never the palette's.
                Message::Palette(PaletteMsg::Escape) => None,
                Message::Menu(MenuMsg::Open(..) | MenuMsg::Choose(_)) => None,
                m if is_modal_blocked(&m) => None,
                m => Some(m),
            };
        }
        if matches!(m, Message::Dialog(_)) {
            return None;
        }
        if self.context_menu.is_some() {
            return match m {
                Message::Switcher(SwitcherMsg::Close | SwitcherMsg::Escape) => {
                    Some(Message::Menu(MenuMsg::Close))
                }
                Message::Palette(PaletteMsg::Escape) => None,
                m if is_modal_blocked(&m) => None,
                m => Some(m),
            };
        }
        Some(m)
    }

    /// WP-4: the right-click menu.
    pub(super) fn update_menu(&mut self, m: crate::context_menu::MenuMsg) -> Task<Message> {
        use crate::context_menu::{ContextMenu, MenuMsg};
        match m {
            MenuMsg::Open(target, at) => {
                self.mode_menu = None;
                self.context_menu = Some(ContextMenu { at, target });
            }
            MenuMsg::Close => self.context_menu = None,
            MenuMsg::Choose(item) => {
                if let Some(menu) = self.context_menu.take() {
                    return self.run_menu_item(menu.target, item);
                }
            }
            MenuMsg::ImageCopied(r) => {
                if let Err(e) = r {
                    self.status.set_error(e);
                }
            }
        }
        Task::none()
    }

    /// Runs a chosen menu item on its target.
    fn run_menu_item(
        &mut self,
        target: crate::context_menu::Target,
        item: crate::context_menu::Item,
    ) -> Task<Message> {
        use crate::context_menu::{Item, Target};
        match (target, item) {
            // Plan round-6 WP-3: a linked image routes image items to its image half (the
            // distinct Copy image path → the image's Copy path / URL) and the rest to its link.
            (Target::LinkedImage { link, image }, item) => {
                if item.is_image_item() {
                    let item = match (&*image, item) {
                        (Target::Image { remote: true, .. }, Item::CopyImagePath) => Item::CopyUrl,
                        (_, Item::CopyImagePath) => Item::CopyPath,
                        (_, i) => i,
                    };
                    self.run_menu_item(*image, item)
                } else {
                    self.run_menu_item(*link, item)
                }
            }
            (Target::Task { path, line }, Item::TaskStatus(c)) => {
                self.set_task_status(&path, line, c)
            }
            // Plan round-6-feedback-todos WP-6: the intake status menu (`' '` = open).
            (Target::TrackerStatus { key }, Item::TaskStatus(c)) => {
                self.update_inner(Message::Views(crate::message::ViewsMsg::SetStatus {
                    key,
                    status: (c != ' ').then_some(c),
                }))
            }
            (Target::Tab { id, .. }, Item::Close) => self.close_tab(id),
            (Target::Tab { pane, id, .. }, Item::CloseOthers) => {
                let others: Vec<u64> = self
                    .panes
                    .get(pane)
                    .map(|ps| ps.tabs.iter().map(|t| t.id).filter(|t| *t != id).collect())
                    .unwrap_or_default();
                Task::batch(others.into_iter().map(|o| self.close_tab(o)))
            }
            (Target::Tab { pane, id, path }, Item::Rename) => {
                let i = self
                    .panes
                    .get(pane)
                    .and_then(|ps| ps.tabs.iter().position(|t| t.id == id));
                if let Some(i) = i {
                    self.focus_tab(pane, i, &path);
                }
                iced::widget::operation::focus(crate::reading::title_id(&path))
            }
            (Target::Tab { path, .. } | Target::Row { path, .. }, Item::Reveal) => {
                self.reveal(&path);
                Task::none()
            }
            (Target::Image { src, remote: false }, Item::CopyImage) => {
                self.sink.copied_image(&src);
                let Some(d) = self.daemon.clone() else {
                    self.status
                        .set_error("copy image: no daemon connection".into());
                    return Task::none();
                };
                let writer = self.clip_writer.clone();
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            crate::clipboard_image::copy_image(d.as_ref(), writer.as_ref(), &src)
                        })
                        .await
                        .unwrap_or_else(|_| Err("copy image: worker failed".into()))
                    },
                    |r| Message::Menu(crate::context_menu::MenuMsg::ImageCopied(r)),
                )
            }
            // Plan round-6 WP-4: "Create note" on an unresolved vault link (any `missing_link`).
            (Target::Link { target, .. }, Item::CreateNote) => self.create_missing(&target),
            // Plan round-5 WP-8: the Reading link / selection menu.
            (
                Target::Link {
                    src,
                    target,
                    external: false,
                    ..
                },
                Item::Open,
            ) => self.update_inner(Message::Reading(crate::message::ReadingMsg::OpenLink {
                src,
                target,
            })),
            (
                Target::Link {
                    target,
                    external: true,
                    ..
                },
                Item::Open,
            ) => self.update_inner(Message::OpenUrl(target)),
            (
                Target::Link {
                    src,
                    target,
                    external: false,
                    ..
                },
                item @ (Item::OpenInNewPane | Item::CopyPath),
            ) => {
                let Some(path) = self
                    .reading
                    .resolve_link(&src, &target, self.daemon.as_ref())
                else {
                    self.status
                        .notify(Severity::Warning, format!("unresolved link: {target}"));
                    return Task::none();
                };
                if item == Item::CopyPath {
                    self.sink.copied(&path);
                    return iced::clipboard::write(path);
                }
                self.open_in_new_pane(&path);
                Task::none()
            }
            (
                Target::Link {
                    src: path, view, ..
                }
                | Target::Selection { path, view },
                Item::Copy,
            ) => match self.reading.selection_text(&self.docs, &path, view) {
                Some(text) => {
                    self.sink.copied(&text);
                    iced::clipboard::write(text)
                }
                None => Task::none(),
            },
            (
                Target::Link {
                    target: src,
                    external: true,
                    ..
                },
                Item::CopyUrl,
            )
            | (Target::Image { src, remote: true }, Item::CopyUrl)
            | (Target::Image { src, remote: false }, Item::CopyPath)
            | (Target::Tab { path: src, .. } | Target::Row { path: src, .. }, Item::CopyPath) => {
                self.sink.copied(&src);
                iced::clipboard::write(src)
            }
            (Target::Row { path, .. }, Item::Move) => {
                self.explorer.selected = Some(path);
                self.update_explorer(ExplorerMsg::BeginMove)
            }
            (Target::Tab { path, .. } | Target::Row { path, .. }, Item::Delete) => {
                self.begin_delete(path)
            }
            (Target::Row { path, .. }, Item::Open) => self.update_explorer(ExplorerMsg::Open(path)),
            (Target::Row { path, .. }, Item::OpenInNewPane) => {
                self.explorer.selected = Some(path.clone());
                self.open_in_new_pane(&path);
                Task::none()
            }
            (Target::Row { path, .. }, Item::Rename) => {
                self.explorer.selected = Some(path);
                self.update_explorer(ExplorerMsg::BeginRename)
            }
            // Plan round-6 WP-5: an Outline heading jumps in the active note, or opens the note
            // in a new pane at that heading.
            (Target::Anchor { line, .. }, Item::Open) => {
                self.update_inner(Message::Panels(PanelsMsg::OutlineJump(line)))
            }
            (Target::Anchor { path, line }, Item::OpenInNewPane) => {
                self.open_in_new_pane(&path);
                self.update_inner(Message::Panels(PanelsMsg::OutlineJump(line)))
            }
            (Target::Row { path, .. }, Item::NewNote) => {
                self.explorer.selected = Some(path);
                self.update_explorer(ExplorerMsg::BeginNew)
            }
            _ => Task::none(),
        }
    }

    /// Reveal in explorer: open the sidebar, expand the note's ancestor folders, select it.
    /// (The virtual list has no scroll-to operation, so the row is not scrolled into view.)
    fn reveal(&mut self, path: &str) {
        self.ui.side_collapsed = false;
        let mut p = path;
        while let Some((parent, _)) = p.rsplit_once('/') {
            self.ui.expanded.insert(parent.to_owned());
            p = parent;
        }
        self.explorer.selected = Some(path.to_owned());
        self.explorer.relayout(&self.ui.expanded);
    }
}

/// User input a modal layer (dialog, then context menu) drops: every [`MsgKind::Input`] of the
/// tab / editor / find / switcher sub-enums, plus palette chords. Completions (saves, ticks,
/// `Created`, pastes) and modifier state always run.
///
/// [`MsgKind::Input`]: crate::message::MsgKind::Input
fn is_modal_blocked(m: &Message) -> bool {
    use crate::message::MsgKind;
    let kind = match m {
        Message::Switcher(m) => m.kind(),
        Message::Find(m) => m.kind(),
        Message::Tabs(m) => m.kind(),
        Message::Source(m) => m.kind(),
        Message::Palette(PaletteMsg::Key(_)) => MsgKind::Input,
        _ => MsgKind::Completion,
    };
    kind == MsgKind::Input
}
