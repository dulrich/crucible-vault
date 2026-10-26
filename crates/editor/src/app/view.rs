//! Rendering: the tab body, tab bar, note bar, panes, side column and the root `view`.

use super::tab_drag::{Cell, DropTarget};
use super::*;
use iced::widget::column;

impl App {
    /// The single tab-body render site (WP-3 swaps this expression for the mode switch).
    pub(super) fn tab_body<'a>(&'a self, tab: &'a Tab) -> Element<'a, Message> {
        let path = match &tab.kind {
            TabKind::Settings => {
                return crate::settings::view(&self.settings, &self.explorer.paths, self.tokens);
            }
            TabKind::Log => return crate::log::view(&self.status.log, self.tokens),
            TabKind::Image(p) => return self.image_body(p, tab.id),
            TabKind::Pdf(p) => return self.pdf_body(p, tab.id),
            TabKind::Note(p) => p,
        };
        let Some(doc) = buf(&self.docs, path) else {
            return iced::widget::Space::new().into();
        };
        crate::reading::tab_body(
            &self.reading,
            &self.docs,
            &self.views,
            doc,
            tab.id,
            self.tokens,
            self.prefs.font_size,
            &self.palette.keymap,
            self.active_tab().is_some_and(|t| t.id == tab.id),
            self.find_bar_for(tab.id),
            self.link_ta_overlay(path, tab.id),
        )
    }

    /// The find bar whose paint tab `tab` shows — the focused pane's, when `tab` is
    /// its active tab (a second pane on the same note paints nothing).
    pub(crate) fn find_bar_for(&self, tab: u64) -> Option<&crate::find::FindBar> {
        let ps = self.panes.get(self.focus)?;
        (ps.active_tab()?.id == tab).then_some(())?;
        ps.find.as_ref()
    }

    /// The iced theme for the current surround.
    pub fn theme(&self) -> Theme {
        let name = match self.prefs.surround {
            Surround::Dark => "N1 Dark",
            Surround::Med => "N1 Med",
            Surround::Light => "N1 Light",
        };
        self.tokens.iced_theme(name)
    }

    /// The backdrop-click / close message for the open overlay (`None` = no overlay open).
    pub fn overlay_close(&self) -> Option<Message> {
        if self.dialog.is_some() {
            Some(Message::Dialog(crate::dialog::DialogMsg::Cancel))
        } else if self.switcher.open {
            Some(Message::Switcher(SwitcherMsg::Close))
        } else if self.palette.open {
            Some(Message::Palette(PaletteMsg::Close))
        } else if self.search_modal {
            Some(Message::Panels(crate::message::PanelsMsg::CloseSearchModal))
        } else {
            None
        }
    }

    /// The header text of `ps`'s tab `tab` under the `tab_title` pref.
    pub fn tab_header_title(&self, ps: &PaneState, tab: &Tab) -> String {
        match &tab.kind {
            TabKind::Settings => "Settings".to_owned(),
            TabKind::Log => "Log".to_owned(),
            TabKind::Image(p) | TabKind::Pdf(p) => p.rsplit('/').next().unwrap_or(p).to_owned(),
            TabKind::Note(p) => {
                let siblings: Vec<&str> = ps.tabs.iter().filter_map(Tab::path).collect();
                let fm = match self.prefs.tab_title {
                    crate::header::TabTitle::Title => {
                        buf(&self.docs, p).and_then(crate::source::SourceBuffer::frontmatter_title)
                    }
                    _ => None,
                };
                crate::header::resolve(self.prefs.tab_title, p, &siblings, fm.as_deref())
            }
        }
    }

    /// Plan round-10 WP-8: `content` as a tab-drop target. The enter/exit handlers exist only
    /// while a drag is live (stable widget tree, no per-move message churn otherwise).
    fn drop_area<'a>(
        &self,
        content: impl Into<Element<'a, Message>>,
        target: DropTarget,
    ) -> Element<'a, Message> {
        let mut area = iced::widget::mouse_area(content);
        if self.tab_drag.active() {
            area = area
                .on_enter(Message::Tabs(TabsMsg::DragEnter(target)))
                .on_exit(Message::Tabs(TabsMsg::DragLeave(target)));
        }
        area.into()
    }

    /// The tab bar: the pane's tab headers (fixed height, clipped), then the close-pane button.
    fn tab_bar<'a>(&'a self, pane: Pane, ps: &'a PaneState) -> Element<'a, Message> {
        use crate::header::{BAR_HEIGHT, MARKER_WIDTH, TITLE_MAX_CHARS, ellipsize, marker};
        use crate::icons::Icon;
        use crate::theme::{FONT_FAMILY, control_label, icon_button};
        let t = self.tokens;
        let focused = pane == self.focus;
        // Plan round-11 WP-6: mid-drag the strip is drawn from the previewed layout. `i` is the
        // slot (the drop index); `(tp, ti)` the tab's real home, so presses/closes address it.
        let cells = self.tab_drag.layout(pane, ps.tabs.len());
        let items = cells.iter().enumerate().filter_map(|(slot_i, c)| match *c {
            Cell::Tab(ti) => ps
                .tabs
                .get(ti)
                .map(|tab| (slot_i, pane, ti, ps, tab, false)),
            Cell::Dragged => {
                let (fp, fi) = self.tab_drag.dragged()?;
                let fps = self.panes.get(fp)?;
                fps.tabs.get(fi).map(|tab| (slot_i, fp, fi, fps, tab, true))
            }
        });
        let headers = row(items.map(|(i, tp, ti, ps, tab, dragged)| {
            let active = if dragged { true } else { ps.active == Some(ti) };
            // Focus cue (borders are debug-only): the focused pane's active tab is an
            // `accent_text` label on a `panel_2` block; an unfocused pane's active tab keeps the
            // block with a `text` label; inactive tabs are `text_dim`.
            let colour = match (active, focused) {
                (true, true) => t.accent_text,
                (true, false) => t.text,
                _ => t.text_dim,
            };
            let (dirty, saving) = match &tab.kind {
                TabKind::Settings => (self.settings.any_dirty(), false),
                TabKind::Log | TabKind::Image(_) | TabKind::Pdf(_) => (false, false),
                TabKind::Note(p) => {
                    buf(&self.docs, p).map_or((false, false), |d| (d.is_dirty(), d.is_saving()))
                }
            };
            let title = ellipsize(&self.tab_header_title(ps, tab), TITLE_MAX_CHARS);
            let lbl = control_label(title, 14.0)
                .font(iced::Font::with_name(FONT_FAMILY))
                .wrapping(iced::widget::text::Wrapping::None)
                .color(colour.to_color());
            // The marker owns a fixed-width slot, blank when clean: the header never resizes.
            let mark =
                container(control_label(marker(dirty, saving), 14.0).color(colour.to_color()))
                    .width(MARKER_WIDTH)
                    .height(BAR_HEIGHT)
                    .center_x(MARKER_WIDTH);
            // Plan round-10 WP-8: a press (not a click) activates the tab and arms a drag, so the
            // header is a `mouse_area`, not a `button` (a button fires on release and captures).
            let select = iced::widget::mouse_area(
                container(row![lbl, mark].spacing(2).height(BAR_HEIGHT))
                    .height(BAR_HEIGHT)
                    .padding([0, 6]),
            )
            .interaction(iced::mouse::Interaction::Pointer)
            .on_press(Message::Tabs(TabsMsg::DragPress(tp, ti)))
            .on_enter(Message::Tabs(TabsMsg::HeaderEnter(tab.id)))
            .on_exit(Message::Tabs(TabsMsg::HeaderLeave(tab.id)));
            // The rollover fill a `button` header used to give (lost when headers became
            // `mouse_area`s for drag); the active block keeps `panel_2`.
            let hovered = !active && self.tab_drag.header_hover() == Some(tab.id);
            let hover = match tab.path() {
                Some(p) => p.to_owned(),
                None => self.tab_header_title(ps, tab),
            };
            let select = iced::widget::tooltip(
                select,
                container(text(hover).size(13))
                    .padding(6)
                    .style(crate::theme::seg_group_style(t)),
                iced::widget::tooltip::Position::Bottom,
            );
            let chip = row![
                select,
                button(control_label("×", 14.0))
                    .height(BAR_HEIGHT)
                    .padding([0, 4])
                    .style(button::text)
                    .on_press(Message::Tabs(TabsMsg::Close(tp, ti))),
            ]
            .spacing(0)
            .height(BAR_HEIGHT)
            .align_y(iced::Alignment::Center);
            // WP-4: right-click a note tab for its context menu.
            let chip: Element<'a, Message> = match tab.path() {
                None => chip.into(),
                Some(p) => crate::context_menu::on_right(
                    chip,
                    crate::context_menu::Target::Tab {
                        pane: tp,
                        id: tab.id,
                        path: p.to_owned(),
                    },
                ),
            };
            // The hovered drop slot shows an accent edge (only while a drag is live).
            let slot = DropTarget::Slot(pane, i);
            let drop_here = dragged;
            let chip = container(chip)
                .height(BAR_HEIGHT)
                .clip(true)
                .style(move |_: &Theme| container::Style {
                    background: if active {
                        Some(t.panel_2.to_color().into())
                    } else if hovered {
                        Some(t.panel_2.to_color().scale_alpha(0.5).into())
                    } else {
                        None
                    },
                    border: iced::Border {
                        color: if drop_here {
                            t.accent_ui.to_color()
                        } else {
                            iced::Color::TRANSPARENT
                        },
                        width: if drop_here { 1.0 } else { 0.0 },
                        radius: crate::theme::RADIUS.into(),
                    },
                    ..container::Style::default()
                });
            self.drop_area(chip, slot)
        }))
        .spacing(4)
        .height(BAR_HEIGHT)
        .align_y(iced::Alignment::Center);
        let headers = container(headers)
            .width(Length::Fill)
            .height(BAR_HEIGHT)
            .clip(true);
        let headers = self.drop_area(headers, DropTarget::End(pane));
        // The close-pane button is pane-level, so it lives at the tab bar's right end.
        let close: Element<'a, Message> = if self.panes.len() > 1 {
            icon_button(Icon::X, Some(Message::Panes(PanesMsg::Close(pane))), t).into()
        } else {
            Space::new().into()
        };
        row![headers, close]
            .spacing(4)
            .height(BAR_HEIGHT)
            .align_y(iced::Alignment::Center)
            .into()
    }

    /// The note control bar: back/forward seg-group, centred breadcrumb (folders + editable
    /// filename), the view-mode control. Only for note tabs.
    fn note_bar<'a>(
        &'a self,
        pane: Pane,
        ps: &'a PaneState,
        tab: NoteRef<'a>,
        mode_btn: Element<'a, Message>,
    ) -> Element<'a, Message> {
        use crate::header::{BAR_HEIGHT, folders};
        use crate::icons::Icon;
        use crate::theme::{FONT_FAMILY, SEGMENT, control_label, icon_text};
        let t = self.tokens;
        let seg = |icon: Icon, msg: Option<Message>| {
            let enabled = msg.is_some();
            button(icon_text(icon))
                .width(SEGMENT)
                .height(SEGMENT)
                .padding(0)
                .style(move |th: &Theme, st| {
                    let mut s = crate::theme::segment_style(t, false)(th, st);
                    if !enabled {
                        s.text_color = t.text_faint.to_color();
                    }
                    s
                })
                .on_press_maybe(msg)
        };
        let history = container(
            row![
                seg(
                    Icon::ChevronLeft,
                    ps.history
                        .can_back()
                        .then_some(Message::Panes(PanesMsg::Back(pane))),
                ),
                seg(
                    Icon::ChevronRight,
                    ps.history
                        .can_forward()
                        .then_some(Message::Panes(PanesMsg::Forward(pane))),
                ),
            ]
            .spacing(0),
        )
        .padding(1)
        .style(crate::theme::seg_group_style(t));
        let path = tab.path;
        let mut crumbs: Vec<Element<'a, Message>> = Vec::new();
        for f in folders(path) {
            crumbs.push(
                control_label(f, 14.0)
                    .font(iced::Font::with_name(FONT_FAMILY))
                    .wrapping(iced::widget::text::Wrapping::None)
                    .color(t.text_dim.to_color())
                    .into(),
            );
            crumbs.push(
                control_label("/", 14.0)
                    .color(t.text_faint.to_color())
                    .into(),
            );
        }
        let name = self.reading.title(&self.docs, path);
        // Sized to its text (approximate advance) so the breadcrumb stays centred.
        #[allow(clippy::cast_precision_loss)]
        let name_w = (name.chars().count().max(4) as f32)
            .mul_add(8.5, 20.0)
            .min(360.0);
        let p_in = path.to_owned();
        crumbs.push(
            iced::widget::text_input("note name", name)
                .id(crate::reading::title_id(path))
                .size(14)
                .width(name_w)
                .on_input(move |value| {
                    Message::Reading(crate::message::ReadingMsg::TitleInput {
                        path: p_in.clone(),
                        value,
                    })
                })
                .on_submit(Message::Reading(crate::message::ReadingMsg::TitleSubmit {
                    path: path.to_owned(),
                }))
                .style(
                    move |theme: &Theme, status| iced::widget::text_input::Style {
                        border: iced::Border {
                            width: 0.0,
                            ..iced::widget::text_input::default(theme, status).border
                        },
                        background: iced::Background::Color(iced::Color::TRANSPARENT),
                        value: t.text.to_color(),
                        ..iced::widget::text_input::default(theme, status)
                    },
                )
                .into(),
        );
        let breadcrumb = container(
            row(crumbs)
                .spacing(4)
                .height(BAR_HEIGHT)
                .align_y(iced::Alignment::Center),
        )
        .center_x(Length::Fill)
        .height(BAR_HEIGHT)
        .clip(true);
        row![history, breadcrumb, mode_btn]
            .spacing(4)
            .height(BAR_HEIGHT)
            .align_y(iced::Alignment::Center)
            .into()
    }

    fn pane_view<'a>(&'a self, pane: Pane, ps: &'a PaneState) -> pane_grid::Content<'a, Message> {
        use crate::icons::Icon;
        use crate::theme::{icon_button_style, icon_text};
        let t = self.tokens;
        let focused = pane == self.focus;
        let mode = ps
            .active_note()
            .map(|tab| self.reading.mode(&self.docs, tab.path));
        let mode_icon = match mode {
            Some(crate::reading::ViewMode::Source) => Icon::Code,
            Some(crate::reading::ViewMode::Split) => Icon::Columns2,
            Some(crate::reading::ViewMode::Reading) | None => Icon::BookOpen,
        };
        let menu_open = self.mode_menu == Some(pane) && mode.is_some();
        let mode_btn = button(icon_text(mode_icon))
            .width(crate::theme::CONTROL)
            .height(crate::theme::CONTROL)
            .padding(0)
            .style(icon_button_style(t, menu_open))
            .on_press_maybe(
                mode.map(|_| Message::Panes(PanesMsg::ModeMenu((!menu_open).then_some(pane)))),
            );
        // Stable slots: tab bar, note control bar (`Space` for Settings / no tab), find bar,
        // period nav bar (`Space` when the note gets none), body — so the body's tree position
        // (and the editor's widget state) never depends on which bars are showing.
        let note_bar: Element<'a, Message> = match ps.active_note() {
            Some(tab) => self.note_bar(pane, ps, tab, mode_btn.into()),
            None => Space::new().into(),
        };
        let nav_bar: Element<'a, Message> = ps
            .active_note()
            .and_then(|tab| {
                nav::bar(
                    pane,
                    tab.path,
                    self.today(),
                    &self.period_folders,
                    &self.prefs.nav_links,
                    t,
                )
            })
            .map_or_else(
                || Space::new().into(),
                |bar| {
                    container(bar)
                        .height(crate::header::BAR_HEIGHT)
                        .width(Length::Fill)
                        .clip(true)
                        .into()
                },
            );
        let body: Element<'a, Message> = match ps.active_tab() {
            Some(tab) => self.tab_body(tab),
            None => text("No file open").color(t.text_faint.to_color()).into(),
        };
        let col = column![
            self.tab_bar(pane, ps),
            note_bar,
            // Stable slot: an empty `Space` while the find bar is closed.
            crate::find::view(pane, ps.find.as_ref(), t),
            nav_bar,
            body,
        ]
        .spacing(4);
        // The dropdown sits in a stack layer above the pane body (layer 0 never moves).
        let menu: Element<'a, Message> = match (menu_open, ps.active_note(), mode) {
            (true, Some(tab), Some(current)) => {
                use crate::reading::ViewMode;
                let item = |label: &'static str, icon: Icon, m: ViewMode| {
                    let on = m == current;
                    button(
                        row![
                            text(icon.glyph().to_string())
                                .font(crate::icons::FONT)
                                .size(crate::theme::ICON_SIZE),
                            text(label).size(14),
                        ]
                        .spacing(8)
                        .align_y(iced::Alignment::Center),
                    )
                    .height(crate::theme::SEGMENT)
                    .width(Length::Fill)
                    .padding([0, 10])
                    .style(crate::theme::segment_style(t, on))
                    .on_press(Message::Reading(
                        crate::message::ReadingMsg::SetMode {
                            path: tab.path.to_owned(),
                            mode: m,
                        },
                    ))
                };
                let list = container(
                    column![
                        item("Source", Icon::Code, ViewMode::Source),
                        item("Split", Icon::Columns2, ViewMode::Split),
                        item("Reading", Icon::BookOpen, ViewMode::Reading),
                    ]
                    .spacing(0),
                )
                .padding(1)
                .width(160)
                .style(crate::theme::seg_group_style(t));
                iced::widget::mouse_area(
                    container(iced::widget::opaque(list))
                        .padding(iced::Padding {
                            // Below the tab bar + note control bar (`BAR_HEIGHT` each, 4 apart).
                            top: 2.0f32.mul_add(crate::header::BAR_HEIGHT, 8.0),
                            ..iced::Padding::ZERO
                        })
                        .align_x(iced::alignment::Horizontal::Right)
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .on_press(Message::Panes(PanesMsg::ModeMenu(None)))
                .into()
            }
            _ => Space::new().into(),
        };
        let body = iced::widget::stack![col, menu];
        let border = if !self.ui.pane_borders {
            None
        } else if focused {
            Some(t.accent_ui)
        } else {
            Some(t.line)
        };
        // plan editor-round-4 WP-4: the pointer entering a pane may focus it. The wrapper is
        // always present (stable widget tree); only its `on_enter` follows the pref.
        let mut hover = iced::widget::mouse_area(container(body).padding(8).height(Length::Fill));
        if self.tab_drag.active() {
            hover = hover
                .on_enter(Message::Tabs(TabsMsg::DragEnter(DropTarget::Body(pane))))
                .on_exit(Message::Tabs(TabsMsg::DragLeave(DropTarget::Body(pane))));
        } else if self.prefs.focus_follows_mouse && !focused {
            hover = hover.on_enter(Message::Panes(PanesMsg::Hover(pane)));
        }
        pane_grid::Content::new(hover).style(move |_: &Theme| container::Style {
            border: iced::Border {
                color: border.map_or(iced::Color::TRANSPARENT, crate::theme::Rgb::to_color),
                width: if border.is_some() { 1.0 } else { 0.0 },
                radius: 0.0.into(),
            },
            ..container::Style::default()
        })
    }

    fn side_view(&self) -> Element<'_, Message> {
        use crate::icons::Icon;
        use crate::theme::icon_button;
        let t = self.tokens;
        let collapsed = self.ui.side_collapsed;
        // Collapsed: a 46px icon strip; any icon expands. The full column stays in the tree (a
        // zero-width clipped container) so the explorer's scroll offset and focus survive.
        let strip: Element<'_, Message> = if collapsed {
            let expand = || Some(Message::Panes(PanesMsg::ToggleSide));
            column![
                icon_button(Icon::PanelLeftOpen, expand(), t),
                icon_button(Icon::FolderTree, expand(), t),
                icon_button(Icon::Bookmark, expand(), t),
            ]
            .spacing(6)
            .padding(6)
            .into()
        } else {
            Space::new().into()
        };
        let header = row![
            Space::new().width(Length::Fill),
            icon_button(
                Icon::PanelLeftClose,
                Some(Message::Panes(PanesMsg::ToggleSide)),
                t,
            ),
        ];
        let full = column![
            header,
            container(explorer::view(&self.explorer, t)).height(Length::Fill),
            bookmarks::view(&self.bookmarks, t),
        ]
        .spacing(8)
        .padding(4);
        let full = container(full)
            .width(if collapsed {
                Length::Fixed(0.0)
            } else {
                Length::Fill
            })
            .height(Length::Fill)
            .clip(true);
        row![strip, full].height(Length::Fill).into()
    }

    /// The view.
    pub fn view(&self) -> Element<'_, Message> {
        let t = self.tokens;
        let panel = move |_: &Theme| container::Style {
            background: Some(t.panel.to_color().into()),
            text_color: Some(t.text.to_color()),
            ..container::Style::default()
        };
        let inset = move |_: &Theme| container::Style {
            background: Some(t.inset.to_color().into()),
            text_color: Some(t.text_dim.to_color()),
            ..container::Style::default()
        };

        let top = row![
            button(text("Quick switch (Ctrl+P)").size(14))
                .style(button::text)
                .on_press(Message::Switcher(SwitcherMsg::Open)),
            button(text("Command palette (Ctrl+Shift+P)").size(14))
                .style(button::text)
                .on_press(Message::Palette(PaletteMsg::Open)),
            button(text("Panels").size(14))
                .style(button::text)
                .on_press(Message::Panels(PanelsMsg::Toggle)),
            button(text("Split right").size(14))
                .style(button::text)
                .on_press(Message::Panes(PanesMsg::SplitRight)),
            button(text("Split down").size(14))
                .style(button::text)
                .on_press(Message::Panes(PanesMsg::SplitDown)),
            button(text("Settings (Ctrl+,)").size(14))
                .style(button::text)
                .on_press(Message::Settings(crate::settings::SettingsMsg::Open)),
        ]
        .spacing(8);

        let grid = PaneGrid::new(&self.panes, |pane, ps, _| self.pane_view(pane, ps))
            .spacing(4)
            .on_click(|p| Message::Panes(PanesMsg::Focus(p)))
            .on_resize(8, |e| Message::Panes(PanesMsg::Resized(e)));

        let toast = self
            .palette
            .jobs
            .toast
            .as_ref()
            .map(|t| format!("▸ {}", t.line()))
            .unwrap_or_default();
        // plan round-5 WP-3: the notice — one truncated line, full text on hover. Plan
        // round-7 WP-3: the only place warnings/errors show, with a severity glyph + colour;
        // a click opens the Log.
        let notice: Element<'_, Message> = match &self.status.notice {
            Some(n) => {
                let color = crate::log::severity_color(n.severity, t).to_color();
                iced::widget::tooltip(
                    button(
                        row![
                            text(crate::log::severity_glyph(n.severity))
                                .size(16)
                                .color(color),
                            text(n.inline()).size(16).color(color),
                        ]
                        .spacing(6),
                    )
                    .padding(0)
                    .style(button::text)
                    .on_press(Message::Log(crate::log::LogMsg::Open)),
                    container(text(n.text.clone()).size(13))
                        .padding(6)
                        .max_width(640)
                        .style(crate::theme::seg_group_style(t)),
                    iced::widget::tooltip::Position::Top,
                )
                .into()
            }
            None => Space::new().into(),
        };
        // Plan round-7 WP-3: a dedicated Log icon-button carrying the Warning+ unread count.
        let unread = self.status.log.unread;
        let log_button = button(
            text(if unread > 0 {
                format!("☰ Log {unread}")
            } else {
                "☰ Log".to_owned()
            })
            .size(16)
            .color(if unread > 0 {
                t.warn_text.to_color()
            } else {
                t.text_dim.to_color()
            }),
        )
        .padding(0)
        .style(button::text)
        .on_press(Message::Log(crate::log::LogMsg::Open));
        let status = row![
            log_button,
            text(self.status.line()).size(16),
            text(toast).size(16).color(t.accent_text.to_color()),
            notice,
            button(text(format!("Surround: {}", self.prefs.surround.label())).size(16))
                .style(button::text)
                .on_press(Message::Theme(ThemeMsg::Cycle)),
        ]
        .spacing(16);

        // plan round-10 WP-7: the whole header row is omitted while the menu bar is hidden.
        let mut main = column![];
        if self.menu_bar_shown {
            main = main.push(container(top).padding(4).width(Length::Fill).style(panel));
        }
        let side_width = if self.ui.side_collapsed {
            Length::Fixed(46.0)
        } else {
            Length::FillPortion(1)
        };
        let mut body = row![
            container(self.side_view())
                .style(inset)
                .width(side_width)
                .height(Length::Fill),
            container(grid)
                .padding(4)
                .width(Length::FillPortion(4))
                .height(Length::Fill),
        ];
        if self.panels.visible {
            body = body.push(
                container(crate::panels::view(&self.panels, t))
                    .style(inset)
                    .width(Length::FillPortion(1))
                    .height(Length::Fill),
            );
        }
        let base = main.push(body.height(Length::Fill)).push(
            container(status)
                .padding(4)
                .width(Length::Fill)
                .style(panel),
        );

        // Widget state is matched by tree position: the base layer never changes shape when the
        // switcher / palette opens; they render as a centred card in layer 1 (`Space` when
        // closed). The backdrop closes on click; the card itself swallows clicks (`opaque`).
        let overlay: Element<'_, Message> = match self.overlay_close() {
            Some(close) => {
                let mut card = column![].spacing(6).width(Length::Fill);
                if let Some(d) = &self.dialog {
                    card = card.push(crate::dialog::view(d, t));
                } else if self.switcher.open {
                    card = card.push(switcher::view(&self.switcher, t));
                } else if self.palette.open {
                    card = card.push(crate::palette::view(&self.palette, t));
                } else if self.search_modal {
                    card = card.push(crate::panels::search_view(
                        &self.panels,
                        t,
                        crate::panels::SEARCH_MODAL_ID,
                    ));
                }
                let card = container(card)
                    .padding(10)
                    .width(640)
                    .style(move |_: &Theme| container::Style {
                        background: Some(t.panel.to_color().into()),
                        text_color: Some(t.text.to_color()),
                        border: iced::Border {
                            color: t.line.to_color(),
                            width: 1.0,
                            radius: crate::theme::RADIUS.into(),
                        },
                        ..container::Style::default()
                    });
                iced::widget::mouse_area(
                    container(iced::widget::opaque(card))
                        .padding(iced::Padding {
                            top: 80.0,
                            ..iced::Padding::ZERO
                        })
                        .align_x(iced::alignment::Horizontal::Center)
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .on_press(close)
                .into()
            }
            None => Space::new().into(),
        };
        // WP-4: the context menu is layer 2 (`Space` when closed; never beneath a dialog).
        let menu: Element<'_, Message> = match (&self.context_menu, &self.dialog) {
            (Some(m), None) => crate::context_menu::view(m, t),
            _ => Space::new().into(),
        };
        iced::widget::stack![base, overlay, menu].into()
    }
}
