//! Drawing the render model with iced widgets (`rich_text`, `image`, `sensor`, containers).

use editor_model::render::{
    Align, Block, Fold, Inline, ListItem, PropValue, Property, inline_text,
};
use iced::font::{Style as FontStyle, Weight};
use iced::widget::{
    Space, button, column, container, image, rich_text, row, rule, scrollable, sensor, span, text,
    text_input,
};
use iced::{Border, Element, Font, Length, Theme};

use super::{Doc, ReadingStore};
use crate::message::{Message, ReadingMsg};
use crate::theme::{Rgb, Tokens};

/// A clickable target inside rich text.
#[derive(Debug, Clone)]
enum LinkTarget {
    /// A vault target (wiki-link target or relative md-link URL).
    Vault(String),
    /// A URL with a scheme: shown, never followed (no browser hand-off in v0).
    External,
}

struct Ctx<'a> {
    store: &'a ReadingStore,
    views: &'a crate::views::ViewStore,
    doc: &'a Doc,
    path: &'a str,
    t: Tokens,
    /// `editor.toml` `font_size / 22` (plan editor-settings-tab WP-2, audit M1).
    scale: f32,
}

impl Ctx<'_> {
    /// A design size (px at the 22px base) scaled to the current font size.
    fn sz(&self, px: f32) -> f32 {
        scaled(px, self.scale)
    }
}

/// `px` (a size at the 22px base font) at text scale `scale`.
pub fn scaled(px: f32, scale: f32) -> f32 {
    (px * scale).max(6.0)
}

/// The design size (px at the 22px base) of a heading level / paragraph (`None`).
pub fn block_size(heading: Option<u8>) -> f32 {
    match heading {
        Some(1) => 28.0,
        Some(2) => 24.0,
        Some(3) => 20.0,
        Some(4) => 18.0,
        Some(_) => 16.0,
        None => 16.0,
    }
}

/// The reading pane for `path`; `scale` = `font_size / 22`.
pub fn reading<'a>(
    store: &'a ReadingStore,
    views: &'a crate::views::ViewStore,
    path: &'a str,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let Some(doc) = store.doc(path) else {
        return text("Rendering…").color(t.text_faint.to_color()).into();
    };
    let cx = Ctx {
        store,
        views,
        doc,
        path,
        t,
        scale,
    };
    // Fixed slots 0 (filename) and 1 (properties, `Space` when absent) keep every body widget
    // at a stable tree position whether or not the note has frontmatter.
    let mut items: Vec<Element<'a, Message>> = vec![
        title_input(store, path, t, scale),
        properties(&cx, &doc.model.properties),
    ];
    items.extend(doc.model.blocks.iter().map(|b| block(&cx, b)));
    if !doc.model.footnotes.is_empty() {
        items.push(rule::horizontal(1).into());
        for (i, f) in doc.model.footnotes.iter().enumerate() {
            items.push(
                row![
                    text(format!("[{}]", i + 1))
                        .size(cx.sz(14.0))
                        .color(t.text_dim.to_color()),
                    column(f.blocks.iter().map(|b| block(&cx, b))).spacing(4),
                ]
                .spacing(6)
                .into(),
            );
        }
    }
    scrollable(container(column(items).spacing(10)).padding(8))
        .id(crate::find::reading_scroll_id(path))
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

/// The editable filename (the note's stem); Enter / blur renames, Escape reverts.
fn title_input<'a>(
    store: &'a ReadingStore,
    path: &'a str,
    t: Tokens,
    scale: f32,
) -> Element<'a, Message> {
    let p_in = path.to_owned();
    text_input("note name", store.title(path))
        .id(super::title_id(path))
        .size(scaled(22.0, scale))
        .on_input(move |value| {
            Message::Reading(ReadingMsg::TitleInput {
                path: p_in.clone(),
                value,
            })
        })
        .on_submit(Message::Reading(ReadingMsg::TitleSubmit {
            path: path.to_owned(),
        }))
        .style(move |theme: &Theme, status| text_input::Style {
            border: Border {
                width: 0.0,
                ..text_input::default(theme, status).border
            },
            background: iced::Background::Color(iced::Color::TRANSPARENT),
            value: t.heading.to_color(),
            ..text_input::default(theme, status)
        })
        .into()
}

/// The read-only frontmatter block (an empty `Space` when there is none).
fn properties<'a>(cx: &Ctx<'a>, props: &'a [Property]) -> Element<'a, Message> {
    if props.is_empty() {
        return Space::new().into();
    }
    let t = cx.t;
    let rows = props.iter().map(|p| {
        let mut spans = Vec::new();
        for (i, v) in p.values.iter().enumerate() {
            if i > 0 {
                spans.push(span(", ").color(t.text_faint.to_color()));
            }
            spans.push(match v {
                PropValue::Text(s) => span(s.as_str()).color(t.text.to_color()),
                PropValue::Link {
                    target,
                    fragment,
                    alias,
                } => span(link_label(target, fragment.as_ref(), alias.as_ref()))
                    .color(t.accent_ui.to_color())
                    .link(LinkTarget::Vault(target.clone())),
            });
        }
        row![
            container(
                text(p.key.as_str())
                    .size(cx.sz(14.0))
                    .color(t.text_dim.to_color())
            )
            .width(Length::Fixed(140.0)),
            rich_text(spans)
                .size(cx.sz(14.0))
                .on_link_click(link_msg(cx.path)),
        ]
        .spacing(8)
        .into()
    });
    boxed(column(rows).spacing(4).into(), t, t.line)
}

fn link_label(target: &str, fragment: Option<&String>, alias: Option<&String>) -> String {
    match (alias, fragment) {
        (Some(a), _) => a.clone(),
        (None, Some(f)) if target.is_empty() => format!("#{f}"),
        (None, Some(f)) => format!("{target}#{f}"),
        (None, None) => target.to_owned(),
    }
}

/// The `crucible-view` fence hook: the view `<file>#<name>` with `this` = the embedding note.
fn view_fence<'a>(cx: &Ctx<'a>, file: &str, name: &str) -> Element<'a, Message> {
    crate::views::fence(cx.views, &crate::views::view_id(file, name), cx.path, cx.t)
}

fn boxed(content: Element<'_, Message>, t: Tokens, edge: Rgb) -> Element<'_, Message> {
    container(content)
        .padding(8)
        .width(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: Some(t.inset.to_color().into()),
            border: Border {
                color: edge.to_color(),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

fn block<'a>(cx: &Ctx<'a>, b: &'a Block) -> Element<'a, Message> {
    let t = cx.t;
    match b {
        Block::Heading { level, inlines } => {
            let size = block_size(Some(*level));
            inlines_view(cx, inlines, size, Some(t.heading), true)
        }
        Block::Paragraph(inlines) => inlines_view(cx, inlines, block_size(None), None, false),
        Block::List {
            ordered,
            start,
            items,
        } => column(
            items
                .iter()
                .enumerate()
                .map(|(i, it)| list_item(cx, *ordered, start + i, it)),
        )
        .spacing(4)
        .into(),
        Block::Table {
            header,
            rows,
            alignments,
        } => {
            let cell = |c: &'a Vec<Inline>, i: usize, bold: bool| -> Element<'a, Message> {
                let align = match alignments.get(i) {
                    Some(Align::Center) => iced::alignment::Horizontal::Center,
                    Some(Align::Right) => iced::alignment::Horizontal::Right,
                    _ => iced::alignment::Horizontal::Left,
                };
                container(inlines_view(cx, c, 15.0, None, bold))
                    .padding(4)
                    .width(Length::FillPortion(1))
                    .align_x(align)
                    .into()
            };
            let mut lines: Vec<Element<'a, Message>> = vec![
                row(header.iter().enumerate().map(|(i, c)| cell(c, i, true))).into(),
                rule::horizontal(1).into(),
            ];
            for r in rows {
                lines.push(row(r.iter().enumerate().map(|(i, c)| cell(c, i, false))).into());
            }
            boxed(column(lines).into(), t, t.line)
        }
        Block::Callout {
            kind,
            title,
            fold,
            children,
        } => {
            let marker = match fold {
                Some(Fold::Folded) => "▸ ",
                Some(Fold::Open) => "▾ ",
                None => "",
            };
            let head = format!(
                "{marker}{}",
                title.clone().unwrap_or_else(|| capitalize(kind))
            );
            let edge = callout_color(kind, t);
            let mut col: Vec<Element<'a, Message>> =
                vec![text(head).size(cx.sz(16.0)).color(edge.to_color()).into()];
            // Folding is shown by the marker; the body stays readable (no fold state in v0).
            col.extend(children.iter().map(|c| block(cx, c)));
            boxed(column(col).spacing(6).into(), t, edge)
        }
        Block::CodeBlock { lang, text: code } => {
            let mut col: Vec<Element<'a, Message>> = Vec::new();
            if !lang.is_empty() {
                col.push(
                    text(lang.as_str())
                        .size(cx.sz(12.0))
                        .color(t.text_faint.to_color())
                        .into(),
                );
            }
            col.push(
                text(code.as_str())
                    .font(Font::MONOSPACE)
                    .size(cx.sz(14.0))
                    .color(t.text.to_color())
                    .into(),
            );
            boxed(column(col).spacing(4).into(), t, t.line)
        }
        Block::BlockQuote(children) => {
            container(column(children.iter().map(|c| block(cx, c))).spacing(6))
                .padding(iced::Padding {
                    left: 12.0,
                    ..iced::Padding::ZERO
                })
                .style(move |_: &Theme| container::Style {
                    text_color: Some(t.text_dim.to_color()),
                    border: Border {
                        color: t.line.to_color(),
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    ..container::Style::default()
                })
                .into()
        }
        Block::ThematicBreak => rule::horizontal(1).into(),
        Block::Image {
            src,
            alt,
            width,
            remote,
        } => image_block(cx, src, alt, *width, *remote),
        Block::Transclusion {
            target,
            fragment,
            content,
        } => {
            let label = match fragment {
                Some(f) => format!("↳ {target}#{f}"),
                None => format!("↳ {target}"),
            };
            let head: Element<'a, Message> = rich_text([span(label)
                .color(t.accent_ui.to_color())
                .link(LinkTarget::Vault(target.clone()))])
            .size(cx.sz(13.0))
            .on_link_click(link_msg(cx.path))
            .into();
            let body: Element<'a, Message> = match content {
                Some(blocks) => column(blocks.iter().map(|c| block(cx, c)))
                    .spacing(6)
                    .into(),
                None => text("(not found)").color(t.text_faint.to_color()).into(),
            };
            boxed(column![head, body].spacing(6).into(), t, t.accent_ui)
        }
        Block::ViewFence { file, name } => view_fence(cx, file, name),
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

fn callout_color(kind: &str, t: Tokens) -> Rgb {
    match kind {
        "warning" | "caution" | "attention" => t.orange,
        "danger" | "error" | "bug" | "failure" | "fail" | "missing" => t.red,
        "success" | "check" | "done" => t.green,
        "question" | "help" | "faq" => t.yellow,
        "tip" | "hint" | "important" => t.cyan,
        "quote" | "cite" => t.muted,
        _ => t.blue,
    }
}

fn list_item<'a>(cx: &Ctx<'a>, ordered: bool, n: usize, it: &'a ListItem) -> Element<'a, Message> {
    let marker = match (it.task, ordered) {
        (Some(c), _) => format!("[{c}]"),
        (None, true) => format!("{n}."),
        (None, false) => "•".to_owned(),
    };
    let done = matches!(it.task, Some('x' | 'X' | '-'));
    let color = if done { cx.t.text_faint } else { cx.t.text_dim };
    let marker = text(marker).font(Font::MONOSPACE).color(color.to_color());
    // Host-note tasks are clickable; transcluded ones (no line) stay display-only (audit M1).
    let marker: Element<'a, Message> = match it.line {
        Some(line) if it.task.is_some() => button(marker)
            .padding(0)
            .style(button::text)
            .on_press(Message::Reading(ReadingMsg::ToggleTask {
                path: cx.path.to_owned(),
                line,
            }))
            .into(),
        _ => marker.into(),
    };
    row![
        marker,
        column(it.children.iter().map(|c| block(cx, c))).spacing(4),
    ]
    .spacing(6)
    .into()
}

fn image_block<'a>(
    cx: &Ctx<'a>,
    src: &'a str,
    alt: &'a str,
    width: Option<u32>,
    remote: bool,
) -> Element<'a, Message> {
    let t = cx.t;
    let placeholder = |label: String| -> Element<'a, Message> {
        boxed(
            text(label)
                .size(cx.sz(13.0))
                .color(t.text_faint.to_color())
                .into(),
            t,
            t.line,
        )
    };
    if remote {
        // Never fetched: localization is the daemon's job (inventory #7).
        return placeholder(format!("[remote image: {alt}]"));
    }
    let Some(Some(key)) = cx.doc.images.get(src) else {
        return placeholder(format!("[missing image: {alt}]"));
    };
    let content: Element<'a, Message> = match cx.store.images.get(key) {
        Some(h) => {
            let img = image(h.clone());
            match width {
                Some(w) => img.width(Length::Fixed(w as f32)).into(),
                None => img.into(),
            }
        }
        None => container(text(alt).size(cx.sz(13.0)).color(t.text_faint.to_color()))
            .height(Length::Fixed(120.0))
            .width(Length::Fill)
            .into(),
    };
    let tab = cx.path.to_owned();
    let shown_key = key.clone();
    let shown_tab = tab.clone();
    sensor(content)
        .key_ref(key.as_str())
        .on_show(move |size| {
            Message::Reading(ReadingMsg::ImageShown {
                tab: shown_tab.clone(),
                key: shown_key.clone(),
                width: width.map_or(size.width, |w| w as f32),
            })
        })
        .on_hide(Message::Reading(ReadingMsg::ImageHidden {
            tab,
            key: key.clone(),
        }))
        .into()
}

fn link_msg(path: &str) -> impl Fn(LinkTarget) -> Message + 'static {
    let src = path.to_owned();
    move |l| match l {
        LinkTarget::Vault(target) => Message::Reading(ReadingMsg::OpenLink {
            src: src.clone(),
            target,
        }),
        // External URLs are not followed; a no-op tick keeps the click inert.
        LinkTarget::External => Message::Reading(ReadingMsg::Tick),
    }
}

fn inlines_view<'a>(
    cx: &Ctx<'a>,
    inlines: &'a [Inline],
    size: f32,
    color: Option<Rgb>,
    bold: bool,
) -> Element<'a, Message> {
    let size = cx.sz(size);
    let t = cx.t;
    let base = color.unwrap_or(t.text);
    let mut spans = Vec::with_capacity(inlines.len());
    for i in inlines {
        let s = match i {
            Inline::Text { text, style } => {
                let mut font = Font::DEFAULT;
                if style.strong || bold {
                    font.weight = Weight::Bold;
                }
                if style.em {
                    font.style = FontStyle::Italic;
                }
                let mut s = span(text.as_str())
                    .font(font)
                    .color(base.to_color())
                    .strikethrough(style.strike);
                if style.highlight {
                    s = s.background(t.sel_tint.to_color()).color(t.text.to_color());
                }
                s
            }
            Inline::Code(c) => span(c.as_str())
                .font(Font::MONOSPACE)
                .background(t.inset.to_color())
                .color(t.text.to_color()),
            Inline::WikiLink {
                target,
                fragment,
                alias,
            } => {
                // No permanent underline: rich_text underlines the hovered link itself.
                span(link_label(target, fragment.as_ref(), alias.as_ref()))
                    .color(t.accent_ui.to_color())
                    .link(LinkTarget::Vault(target.clone()))
            }
            Inline::Link { url, label } => {
                let l = if editor_model::render::is_remote(url) {
                    LinkTarget::External
                } else {
                    LinkTarget::Vault(url.clone())
                };
                span(label.as_str()).color(t.accent_ui.to_color()).link(l)
            }
            Inline::Tag(tag) => span(format!("#{tag}")).color(t.accent.to_color()),
            Inline::FootnoteRef { ix, .. } => span(format!("[{ix}]"))
                .size((size - 4.0).max(10.0))
                .color(t.accent_ui.to_color()),
            Inline::LineBreak => span("\n"),
        };
        spans.push(s);
    }
    if spans.is_empty() {
        spans.push(span(inline_text(inlines)));
    }
    rich_text(spans)
        .size(size)
        .on_link_click(link_msg(cx.path))
        .into()
}
