//! The Views page (plan `editor-settings-tab`, Views carve-out): one entry per
//! `_custom/crucible-vault/views/*.toml` (from the explorer's path list, no new RPC), each its own
//! [`ConfigFile`] save loop keyed by its vault path, plus "New view file" (an absent file saved
//! through the exact-name `write_config`, never `create_note`).
//!
//! Views keep no last-valid file and their errors never reach `ConfigReloaded`, so diagnostics
//! are local: [`crate::settings::views_check`] runs the `views` crate's own `BaseDef` parse
//! (an error blocks Save) and `Base::compile` (notices shown as warnings).

use iced::widget::{Space, button, column, container, row, text, text_input};
use iced::{Alignment, Element, Length};

use super::PageCx;
use crate::message::Message;
use crate::settings::SettingsMsg;
use crate::theme;

/// The page intro line.
pub const INTRO: &str = "One file per Bases-style view set. A parse error blocks Save; compile notices are warnings. Filters and formulas are edited as expression text.";

/// The page.
pub fn view(cx: PageCx<'_>) -> Element<'_, Message> {
    let t = cx.t;
    let s = cx.s;
    let paths = s.view_paths();
    let mut list = row![].spacing(0);
    for p in &paths {
        let name = p.rsplit('/').next().unwrap_or(p).to_owned();
        let dirty = s.file(p).is_some_and(|f| f.dirty());
        let label = if dirty { format!("{name} •") } else { name };
        let on = s.view_selected.as_deref() == Some(p.as_str());
        list = list.push(
            button(theme::control_label(label, 14.0))
                .height(theme::SEGMENT)
                .padding([0, 12])
                .style(theme::segment_style(t, on))
                .on_press(Message::Settings(SettingsMsg::ViewSelect(p.clone()))),
        );
    }
    let list: Element<'_, Message> = if paths.is_empty() {
        text("no view files yet")
            .size(13)
            .color(t.text_faint.to_color())
            .into()
    } else {
        container(list)
            .padding(1)
            .style(theme::seg_group_style(t))
            .into()
    };
    let new_row = row![
        text_input("new view file name", &s.new_view)
            .size(14)
            .padding([8, 10])
            .width(Length::Fixed(260.0))
            .on_input(|v| Message::Settings(SettingsMsg::NewViewName(v)))
            .on_submit(Message::Settings(SettingsMsg::NewViewCreate)),
        button(theme::control_label("New view file", 14.0))
            .height(theme::CONTROL)
            .padding([0, 14])
            .style(theme::icon_button_style(t, false))
            .on_press_maybe(
                (!s.new_view.trim().is_empty())
                    .then_some(Message::Settings(SettingsMsg::NewViewCreate)),
            ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let body: Element<'_, Message> = match s.view_selected.as_deref().and_then(|p| s.file(p)) {
        Some(f) => super::generic(cx, f, crate::settings::Section::Views.info(), ""),
        None => Space::new().into(),
    };
    column![
        text(INTRO).size(13).color(t.text_faint.to_color()),
        list,
        new_row,
        body,
    ]
    .spacing(10)
    .into()
}
