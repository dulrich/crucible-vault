//! The source pane: the text editor with its floating conflict bar.

use iced::widget::text_editor;
use iced::widget::{Space, button, column, container, row, text};
use iced::{Element, Length};

use super::buffer::Repr;
use super::keys::key_binding;
use super::{HighlightSettings, KindColors, MarkdownHighlighter, SourceBuffer, highlighter};
use crate::message::{Message, SourceMsg};
use crate::theme::Tokens;

/// The text editor's padding (middle-click points are mapped through it like iced's own clicks).
pub(crate) const EDITOR_PADDING: f32 = 5.0;

/// The source stack's slots, in order: the editor is always layer 0, whatever the dirty /
/// conflict state; the conflict bar floats above it (a fixed slot).
pub const SLOTS: [&str; 2] = ["editor", "conflict"];

/// The source editor's widget id for `path` (find bar Escape refocuses it).
pub fn editor_id(path: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("source-editor:{path}"))
}

/// The text editor's style: iced's default, except that under the transparent surround the
/// background is transparent (the default repaints the palette background, which would stack
/// its alpha over the translucent window).
pub(crate) fn editor_style(
    theme: &iced::Theme,
    status: text_editor::Status,
    transparent: bool,
) -> text_editor::Style {
    let mut style = text_editor::default(theme, status);
    if transparent {
        style.background = iced::Background::Color(iced::Color::TRANSPARENT);
    }
    style
}

/// Renders a document's source buffer with its conflict bar; `font_size` is the
/// `editor.toml` base text size (live, plan editor-settings-tab WP-2); `keymap` is the effective
/// keymap its key bindings route by (WP-4). `link` is the open `[[` note list overlay (plan
/// round-5 WP-9), already positioned; while it is `Some` its keys are intercepted. `find` is the
/// focused pane's find bar when this is its active tab.
pub fn view<'a>(
    doc: &'a SourceBuffer,
    tokens: Tokens,
    transparent: bool,
    font_size: f32,
    keymap: &'a crate::palette::Keymap,
    find: Option<&crate::find::FindBar>,
    link: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let link_open = link.is_some();
    // The find paint comes from the focused pane's bar, at view time.
    let (find, find_all) = find.map_or_else(
        || (None, std::sync::Arc::from([])),
        |b| b.source_paint(doc.edit_seq),
    );
    let editor: Element<'a, Message> = match &doc.content {
        Repr::Shaped(content) => crate::widget::middle_click::middle_click(
            text_editor::TextEditor::new(content)
                .id(editor_id(&doc.path))
                .size(font_size)
                .height(Length::Fill)
                .padding(EDITOR_PADDING)
                .style(move |theme, status| editor_style(theme, status, transparent))
                .on_action(|a| Message::Source(SourceMsg::Action(a)))
                .key_binding(move |kp| key_binding(kp, keymap, link_open))
                .highlight_with::<MarkdownHighlighter>(
                    HighlightSettings {
                        colors: KindColors::from_tokens(&tokens),
                        epoch: doc.epoch,
                        find,
                        find_all,
                    },
                    highlighter::to_format,
                ),
            move |at| {
                Message::Source(SourceMsg::MiddleClick {
                    path: doc.path.clone(),
                    at,
                })
            },
        )
        .on_command_left(move |at| {
            Message::Source(SourceMsg::CommandClick {
                path: doc.path.clone(),
                at,
            })
        })
        .into(),
        // Never visible in practice (the app materialises visible tabs before drawing).
        Repr::Plain { text: t, .. } => text(t.as_str()).size(font_size).into(),
    };
    // The editor's widget state (focus, cursor scroll) is matched by tree position, so it is
    // always layer 0 of a stack; the conflict slot floats in layer 1 (an empty `Space` when
    // unused). Plan editor-feedback-round-2 WP-6: it is out of the editor's vertical flow, so a
    // conflict bar never pushes the editor down (dirty / saving live on the tab header, see
    // `crate::header`).
    let conflict: Element<'a, Message> = if doc.conflict().is_some() {
        let msg = if doc.is_missing() {
            "The file is gone from disk (deleted or moved away). Keep mine recreates it; Take theirs closes it."
        } else {
            "File changed on disk while you were editing."
        };
        let bar = row![
            text(msg).color(tokens.warn_text.to_color()),
            button(text("Keep mine"))
                .on_press(Message::Source(SourceMsg::KeepMine(doc.path.clone()))),
            button(text("Take theirs"))
                .on_press(Message::Source(SourceMsg::TakeTheirs(doc.path.clone()))),
        ]
        .spacing(8);
        container(bar)
            .padding(6)
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(tokens.panel_2.to_color().into()),
                ..container::Style::default()
            })
            .into()
    } else {
        Space::new().into()
    };
    let overlay = column![conflict].spacing(4).padding(4).width(Length::Fill);
    // Plan round-5 WP-1: the stack is always wrapped (stable tree shape); the sensor reports
    // the laid-out height so `top_line` tracking uses the real viewport line count.
    let path = doc.path.as_str();
    let report = move |s: iced::Size| {
        Message::Source(SourceMsg::Viewport {
            path: path.to_owned(),
            lines: super::viewport_lines(s.height, font_size),
        })
    };
    let link: Element<'a, Message> = link.unwrap_or_else(|| Space::new().into());
    iced::widget::sensor(iced::widget::stack![editor, overlay, link])
        .on_show(report)
        .on_resize(report)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::{Background, Color, Theme};

    #[test]
    fn editor_background_transparent_only_under_transparent_surround() {
        let theme = Theme::Dark;
        for status in [text_editor::Status::Active, text_editor::Status::Hovered] {
            let opaque = editor_style(&theme, status, false);
            assert_eq!(opaque, text_editor::default(&theme, status));
            let clear = editor_style(&theme, status, true);
            assert_eq!(clear.background, Background::Color(Color::TRANSPARENT));
            assert_eq!(clear.selection, opaque.selection);
            assert_eq!(clear.value, opaque.value);
            assert_eq!(clear.placeholder, opaque.placeholder);
        }
    }
}
