//! The note-gone pill (split out of `app/view.rs`): the error pill drawn above the body of a note
//! whose file is gone from disk.

use super::*;

/// Plan note-gone-restore WP-1: the Reading body's horizontal inset — the column padding
/// (`reading/view.rs`, `padding(8)`) on both sides plus, on the right, the scroll gutter and the
/// embedded scrollbar ([`crate::theme::gutter_scroll`]: iced's default 10 px bar).
const READING_INSET: iced::Padding = iced::Padding {
    top: 0.0,
    bottom: 0.0,
    left: 8.0,
    right: 8.0 + crate::theme::SCROLL_GUTTER + 10.0,
};

/// Plan note-gone-banner WP-1: the error pill (full width in Source/Split, inset to the body
/// column in Reading) for a note whose file is gone from disk
/// (clean or dirty): Keep recreates the file from the buffer, Close discards it and closes the
/// note's tabs (the missing-file `TakeTheirs` path).
pub(super) fn gone_pill<'a>(id: NoteId, reading: bool, t: Tokens) -> Element<'a, Message> {
    use crate::icons::Icon;
    use iced::font::Weight;
    use iced::widget::{rich_text, span};
    let err = t.error_text.to_color();
    let mut bold = iced::Font::with_name(crate::theme::FONT_FAMILY);
    bold.weight = Weight::Bold;
    let plain = |s: &'static str| -> iced::widget::text::Span<'a, ()> { span(s) };
    let strong = |s: &'static str| -> iced::widget::text::Span<'a, ()> { span(s).font(bold) };
    let msg = rich_text(vec![
        plain("The file is gone from disk (deleted or moved out of the vault); choose "),
        strong("Keep"),
        plain(" to recreate it or "),
        strong("Close"),
        plain(" if this is intentional."),
    ])
    .size(13)
    .color(err)
    .width(Length::Fill);
    let icon = text(Icon::CircleAlert.glyph().to_string())
        .font(crate::icons::FONT)
        .size(crate::theme::ICON_SIZE)
        .color(err);
    let bar = row![
        icon,
        msg,
        button(text("Keep").size(13)).on_press(Message::Source(SourceMsg::KeepMine(id))),
        button(text("Close").size(13)).on_press(Message::Source(SourceMsg::TakeTheirs(id))),
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    let pill = container(bar)
        .padding(6)
        .width(Length::Fill)
        .style(move |_| container::Style {
            background: Some(t.panel_2.to_color().into()),
            border: iced::Border {
                color: err,
                width: 1.0,
                radius: 6.0.into(),
            },
            ..container::Style::default()
        });
    let inset = if reading {
        READING_INSET
    } else {
        iced::Padding::ZERO
    };
    container(pill).padding(inset).width(Length::Fill).into()
}
