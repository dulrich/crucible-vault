//! The palette overlay view.

use cv_protocol::ParamKind;
use iced::widget::{button, column, pick_list, row, text, text_editor, toggler};
use iced::{Element, Length};

use super::prompt::{ParamValue, Prompt};
use super::{INPUT_ID, Palette, PromptMsg, SHOWN};
use crate::message::{Message, PaletteMsg};
use crate::theme::Tokens;

/// The multiline prompt editor's padding (iced's default, made explicit so a middle-click
/// position maps to the content as in the source editor).
pub(super) const MULTILINE_PADDING: f32 = 5.0;

fn prompt_view<'a>(p: &'a Palette, pr: &'a Prompt, t: Tokens) -> Element<'a, Message> {
    let pm = |m| Message::Palette(PaletteMsg::Prompt(m));
    let Some(spec) = pr.current() else {
        return text("").into();
    };
    let header = text(format!(
        "{} — {} ({}/{}){}",
        pr.action.title,
        spec.label,
        pr.step + 1,
        pr.action.params.len(),
        if spec.required { " *" } else { "" }
    ))
    .size(14);
    let input: Element<'a, Message> = match spec.kind {
        ParamKind::Bool => {
            let on = matches!(pr.value(), Some(ParamValue::Bool(true)));
            toggler(on)
                .label(spec.label.as_str())
                .on_toggle(move |b| pm(PromptMsg::Bool(b)))
                .into()
        }
        ParamKind::Enum => pick_list(
            spec.choices.as_slice(),
            Some(pr.text().to_owned()).filter(|s| !s.is_empty()),
            move |c: String| pm(PromptMsg::Choice(c)),
        )
        .into(),
        ParamKind::Multiline => crate::widget::middle_click::middle_click(
            text_editor(&p.multiline)
                .height(120)
                .padding(MULTILINE_PADDING)
                .on_action(move |a| pm(PromptMsg::Editor(a))),
            move |at| pm(PromptMsg::MiddleClick(at)),
        )
        .into(),
        ParamKind::Text | ParamKind::Path | ParamKind::Date => {
            let ph = if spec.kind == ParamKind::Date {
                "YYYY-MM-DD"
            } else {
                ""
            };
            crate::widget::clip_input::clip_input(ph, pr.text())
                .id(INPUT_ID)
                .on_input(move |s| pm(PromptMsg::Text(s)))
                .on_submit(Message::Palette(PaletteMsg::Submit))
                .into()
        }
    };
    let mut col = column![header, input].spacing(4).width(Length::Fill);
    for s in &p.suggestions {
        col = col.push(
            button(text(s.as_str()).size(13))
                .style(button::text)
                .on_press(pm(PromptMsg::Suggest(s.clone()))),
        );
    }
    if let Some(e) = &p.prompt_error {
        col = col.push(text(e.as_str()).size(12).color(t.warn_text.to_color()));
    }
    let hint = if spec.kind == ParamKind::Multiline {
        "Ctrl+Enter: next · Esc: cancel"
    } else {
        "Enter: next · Esc: cancel"
    };
    col.push(
        row![
            button(text("Back").size(13))
                .style(button::text)
                .on_press(pm(PromptMsg::Back)),
            button(text("Next").size(13))
                .style(button::text)
                .on_press(Message::Palette(PaletteMsg::Submit)),
            text(hint).size(12).color(t.text_faint.to_color()),
        ]
        .spacing(8),
    )
    .into()
}

/// The palette overlay (shown above the panes while open).
pub fn view<'a>(p: &'a Palette, t: Tokens) -> Element<'a, Message> {
    if let Some(pr) = &p.prompt {
        return prompt_view(p, pr, t);
    }
    let mut col = column![
        crate::widget::clip_input::clip_input("Run a command…", &p.query)
            .id(INPUT_ID)
            .on_input(|q| Message::Palette(PaletteMsg::Query(q)))
            .on_submit(Message::Palette(PaletteMsg::Submit))
    ]
    .spacing(2)
    .width(Length::Fill);
    if p.loading {
        col = col.push(
            text("loading actions…")
                .size(12)
                .color(t.text_faint.to_color()),
        );
    }
    for (row_i, &i) in p.results.iter().take(SHOWN).enumerate() {
        let Some(e) = p.entries.get(i) else { continue };
        let mut title = text(e.title.as_str()).size(14);
        if row_i == p.selected {
            title = title.color(t.accent_text.to_color());
        }
        let hint = p.hints.get(i).cloned().flatten().unwrap_or_default();
        let hint_color = if !hint.is_empty() && p.query.trim().to_lowercase() == hint {
            t.accent_text
        } else {
            t.text_faint
        };
        col = col.push(
            button(
                row![
                    title.width(Length::Fill),
                    text(hint)
                        .size(12)
                        .font(iced::Font::MONOSPACE)
                        .color(hint_color.to_color()),
                    text(e.group.as_str())
                        .size(12)
                        .color(t.text_faint.to_color()),
                    text(e.keys.join(" ")).size(12).color(t.text_dim.to_color()),
                ]
                .spacing(12),
            )
            .style(button::text)
            .width(Length::Fill)
            .on_press(Message::Palette(PaletteMsg::Pick(i))),
        );
    }
    for n in &p.notice {
        col = col.push(text(n.as_str()).size(12).color(t.warn_text.to_color()));
    }
    col.into()
}
