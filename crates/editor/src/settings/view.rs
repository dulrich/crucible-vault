//! The Settings tab's view: the section rail, the page body and the shared file header / save
//! status line.

use iced::widget::{Space, button, column, container, row, text};
use iced::{Element, Length, Theme};

use super::{ConfigFile, LoadState, Section, Settings, SettingsMsg, body_scroll_id, pages, status};
use crate::icons::Icon;
use crate::message::Message;
use crate::theme::Tokens;

fn msg(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

/// The rail + page.
pub fn view<'a>(s: &'a Settings, paths: &'a [String], t: Tokens) -> Element<'a, Message> {
    let mut rail = column![].spacing(2).width(Length::Fixed(220.0));
    for sec in Section::ALL {
        if let Some(g) = sec.group() {
            rail = rail
                .push(container(text(g).size(12).color(t.text_faint.to_color())).padding([6, 10]));
        }
        let on = s.section == sec;
        let dirty = sec
            .file()
            .and_then(|p| s.file(p))
            .is_some_and(ConfigFile::dirty);
        let label = if dirty {
            format!("{} •", sec.label())
        } else {
            sec.label().to_owned()
        };
        rail = rail.push(
            button(
                row![
                    text(sec.icon().glyph().to_string())
                        .font(crate::icons::FONT)
                        .size(crate::theme::ICON_SIZE)
                        .line_height(1.0)
                        .align_y(iced::alignment::Vertical::Center)
                        .height(Length::Fill),
                    crate::theme::control_label(label, 14.0),
                ]
                .height(Length::Fill)
                .spacing(10)
                .align_y(iced::Alignment::Center),
            )
            .height(crate::theme::SEGMENT)
            .width(Length::Fill)
            .padding([0, 10])
            .style(crate::theme::segment_style(t, on))
            .on_press(msg(SettingsMsg::Section(sec))),
        );
    }
    let rail = container(rail)
        .padding(1)
        .style(crate::theme::seg_group_style(t));
    let body = crate::theme::gutter_scroll(container(page(s, paths, t)).padding([0, 16]))
        .id(body_scroll_id(s.section))
        .on_scroll({
            let sec = s.section;
            move |v| msg(SettingsMsg::BodyScrolled(sec, v.absolute_offset().y))
        })
        .width(Length::Fill)
        .height(Length::Fill);
    let body: Element<'a, Message> = if s.section == Section::Hotkeys {
        column![pages::hotkeys::notice_bar(&s.hotkeys, t), body]
            .height(Length::Fill)
            .into()
    } else {
        body.into()
    };
    row![rail, body].spacing(12).height(Length::Fill).into()
}

/// One section's page (view build timed like `sync_buffers`: `CV_EDITOR_TRACE` debug line
/// when it takes ≥1 ms).
pub fn page<'a>(s: &'a Settings, paths: &'a [String], t: Tokens) -> Element<'a, Message> {
    let t0 = std::time::Instant::now();
    let el = page_inner(s, paths, t);
    let ms = t0.elapsed().as_secs_f64() * 1e3;
    if ms >= 1.0 {
        tracing::debug!(
            elapsed_ms = ms,
            section = s.section.label(),
            "settings::page"
        );
    }
    el
}

fn page_inner<'a>(s: &'a Settings, paths: &'a [String], t: Tokens) -> Element<'a, Message> {
    let cx = pages::PageCx { s, paths, t };
    let heading = text(s.section.label()).size(20).color(t.heading.to_color());
    let body: Element<'a, Message> = match s.section {
        Section::Status => status::view(&s.status, t),
        Section::Hotkeys => pages::hotkeys::view(cx),
        Section::Views => pages::views::view(cx),
        sec => match sec.file().and_then(|p| s.file(p)) {
            // The Editor page's intro is its migration / malformed note, when there is one.
            Some(f) => pages::generic(
                cx,
                f,
                sec.info(),
                match sec {
                    Section::Editor => s.editor_note.as_deref().unwrap_or(sec.info().intro),
                    _ => sec.info().intro,
                },
            ),
            None => Space::new().into(),
        },
    };
    column![heading, body].spacing(12).into()
}

/// The file path, load state, form errors, reload errors and notice of a file page.
pub fn file_header(f: &ConfigFile, t: Tokens) -> Element<'_, Message> {
    let state = match f.state() {
        LoadState::NotLoaded => "not loaded".to_owned(),
        LoadState::Absent => "absent (the first valid edit creates it)".to_owned(),
        LoadState::Loaded => "loaded".to_owned(),
        LoadState::Malformed(e) => format!("does not parse: {e}"),
        LoadState::Error(e) => format!("read failed: {e}"),
    };
    let bad = matches!(f.state(), LoadState::Malformed(_) | LoadState::Error(_));
    let mut col = column![
        text(f.path()).size(13).color(t.text_dim.to_color()),
        text(state).size(13).color(if bad {
            t.error_text.to_color()
        } else {
            t.text_faint.to_color()
        }),
    ]
    .spacing(4);
    if let Some(n) = f.notice() {
        col = col.push(text(n).size(13).color(t.warn_text.to_color()));
    }
    for e in f.errors() {
        col = col.push(text(e.to_string()).size(13).color(t.error_text.to_color()));
    }
    for w in f.warnings() {
        col = col.push(text(w.as_str()).size(13).color(t.warn_text.to_color()));
    }
    for e in f.reload_errors() {
        col = col.push(
            row![
                text(Icon::TriangleAlert.glyph().to_string())
                    .font(crate::icons::FONT)
                    .size(crate::theme::ICON_SIZE)
                    .color(t.warn_text.to_color()),
                text(format!("daemon: {e}"))
                    .size(13)
                    .color(t.warn_text.to_color()),
            ]
            .spacing(6),
        );
    }
    col.into()
}

/// The save status line (plan `editor-feedback-round-2` WP-2: replaces the Save / Revert bar):
/// "saving…" / "saved" / "not saved: N errors" and "Start from defaults" for a malformed
/// file. Fixed height.
pub fn save_bar(f: &ConfigFile, t: Tokens) -> Element<'_, Message> {
    let mut bar = row![].spacing(8).align_y(iced::Alignment::Center);
    if matches!(f.state(), LoadState::Malformed(_)) && f.doc().is_none() {
        bar = bar.push(
            button(crate::theme::control_label("Start from defaults", 14.0))
                .height(crate::theme::CONTROL)
                .padding([0, 14])
                .style(crate::theme::icon_button_style(t, false))
                .on_press(msg(SettingsMsg::StartOver(f.path().to_owned()))),
        );
    }
    if let Some((label, warn)) = f.save_status() {
        let c = if warn { t.warn_text } else { t.text_faint };
        bar = bar.push(text(label).size(13).color(c.to_color()));
    }
    container(bar)
        .height(crate::theme::CONTROL)
        .align_y(iced::Alignment::Center)
        .style(|_: &Theme| container::Style::default())
        .into()
}
