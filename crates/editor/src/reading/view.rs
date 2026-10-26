//! Drawing the render model with iced widgets (`rich_text`, `image`, `sensor`, containers).

use editor_model::props::{PropValue, Property};
use editor_model::render::{Align, Block, Fold, Inline, ListItem, inline_text};
use iced::font::{Style as FontStyle, Weight};
use iced::widget::{
    Space, button, column, container, image, row, rule, scrollable, sensor, span, text,
};
use iced::{Border, Element, Font, Length, Theme};

use std::cell::Cell;

use editor_model::visible::{Occurrence, VisRun, link_label};

use super::{Doc, ReadingStore};
use crate::find::ReadingPaint;
use crate::message::{Message, PdfTabMsg, ReadingMsg};
pub(crate) use crate::task_status::task_box;
use crate::theme::{Rgb, Tokens};
use crate::widget::selectable::{self, Selection};
#[cfg(test)]
use crate::{
    icons::Icon,
    task_status::{TaskRole, task_mark},
};
use std::borrow::Cow;

/// A clickable target inside rich text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LinkTarget {
    /// A vault target (wiki-link target or relative md-link URL).
    Vault(String),
    /// A URL with a scheme: opened via `open_url` (plan usability-sprint-captures-links WP-3).
    External(String),
}

/// The drawing context; `'c` borrows the per-frame locals (run cursor, find occurrences).
struct Ctx<'a, 'c> {
    store: &'a ReadingStore,
    views: &'a crate::views::ViewStore,
    doc: &'a Doc,
    path: &'a str,
    /// The showing view (tab id) — image pins are per view (plan editor-feedback-round-2 WP-7).
    view: u64,
    t: Tokens,
    /// [`reading_scale`] of `editor.toml` `font_size` (plan editor-ui-round-3 WP-6).
    scale: f32,
    /// Inside a cancelled (`- [-]`) task: text is faint and struck through.
    strike: bool,
    /// The next [`VisRun`] of `doc.runs` to draw — the tree is drawn in the
    /// order [`editor_model::visible::visible_runs`] walked it, so run `i` is `doc.runs[i]`.
    cursor: &'c Cell<usize>,
    /// The find query's occurrences over `doc.runs` (empty without find paint), and the index
    /// of the strong one (the focused pane's bar, at view time).
    occ: &'c [Occurrence],
    active: Option<usize>,
    /// The tab's selection.
    sel: Option<Selection>,
}

impl<'a, 'c> Ctx<'a, 'c> {
    /// Takes the next visible run: its index and table entry.
    fn take_run(&self) -> (usize, Option<&'a VisRun>) {
        let i = self.cursor.get();
        self.cursor.set(i + 1);
        (i, self.doc.runs.get(i))
    }

    /// Selectable text run `i` of `spans` at `size` (plan round-5 WP-7; replaces `rich_text`).
    fn run_el(&self, i: usize, spans: Vec<RSpan<'a>>, size: f32) -> Element<'a, Message> {
        let len = spans.iter().map(|s| s.text.len()).sum();
        let mut c = self.t.accent_ui.to_color();
        c.a = 0.35;
        let range = self.sel.and_then(|s| s.run_range(i, len));
        selectable::run(i, spans, size, range, c).into()
    }

    /// Run `i`'s find occurrences, with their indices over the whole document.
    fn hits(&self, i: usize) -> impl Iterator<Item = (usize, &'c Occurrence)> + use<'c, '_> {
        let from = self.occ.partition_point(|o| o.run < i);
        self.occ[from..]
            .iter()
            .take_while(move |o| o.run == i)
            .enumerate()
            .map(move |(k, o)| (from + k, o))
    }

    /// A design size (px; paragraph = 16) scaled to the current font size.
    fn sz(&self, px: f32) -> f32 {
        scaled(px, self.scale)
    }
}

/// plan editor-ui-round-3 WP-6: the Reading text scale for `editor.toml` `font_size` — a
/// paragraph (design size [`block_size`]`(None)` = 16) renders at exactly `font_size`; headings
/// and every other design size keep their ratio to it. (Was `font_size / 22`, which drew
/// paragraphs at 16/22 of the Source size.)
pub fn reading_scale(font_size: f32) -> f32 {
    font_size / block_size(None)
}

/// `px` (a design size; paragraph = 16) at text scale `scale`.
pub fn scaled(px: f32, scale: f32) -> f32 {
    (px * scale).max(6.0)
}

/// The design size (px; paragraph = 16) of a heading level / paragraph (`None`).
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

/// The reading pane for `path`; `scale` = [`reading_scale`]`(font_size)`; `keys` = the pane
/// takes the selection keys (Shift+Arrows, Esc; plan round-5 WP-7); `find` = the focused pane's
/// find paint when this is its active tab.
#[allow(clippy::too_many_arguments)] // one render site
pub fn reading<'a>(
    store: &'a ReadingStore,
    docs: &'a crate::app::Docs,
    views: &'a crate::views::ViewStore,
    path: &'a str,
    view: u64,
    t: Tokens,
    scale: f32,
    keys: bool,
    find: Option<ReadingPaint<'_>>,
) -> Element<'a, Message> {
    let Some(doc) = store.doc(docs, path) else {
        return text("Rendering…").color(t.text_faint.to_color()).into();
    };
    let cursor = Cell::new(0);
    let occ = find.map_or_else(Vec::new, |f| {
        editor_model::visible::occurrences(&doc.runs, f.query)
    });
    let cx = Ctx {
        store,
        views,
        doc,
        path,
        view,
        t,
        scale,
        strike: false,
        cursor: &cursor,
        occ: &occ,
        active: find.and_then(|f| f.active),
        sel: store.selection(docs, path, view),
    };
    // Fixed slot 0 (properties, `Space` when absent) keeps every body widget at a stable tree
    // position whether or not the note has frontmatter. The filename edit moved to the note
    // control bar's breadcrumb (plan editor-feedback-round-2 WP-6).
    let mut items: Vec<Element<'a, Message>> = vec![properties(&cx, &doc.model.properties)];
    items.extend(doc.model.blocks.iter().enumerate().map(|(i, b)| {
        // plan editor-ui-round-3 WP-6: an id'd container per top-level block, so a widget
        // operation can read its layout bounds (place-keeping across mode switches).
        container(block(&cx, b))
            .id(super::block_id(path, view, i))
            .into()
    }));
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
    let scroll = scrollable(container(column(items).spacing(10)).padding(8))
        .id(crate::find::reading_scroll_id(path, view))
        .on_scroll(move |v| {
            Message::Reading(ReadingMsg::Scrolled {
                path: path.to_owned(),
                view,
                y: v.relative_offset().y,
            })
        })
        .width(Length::Fill)
        .height(Length::Fill);
    let unresolved = &doc.unresolved;
    // Plan round-5 WP-7: the gesture root owns press/drag/keys; the selection is the tab's.
    selectable::root(
        scroll,
        crate::find::reading_scroll_id(path, view),
        cx.sel,
        keys,
        move |sel| {
            Message::Reading(ReadingMsg::Select {
                path: path.to_owned(),
                view,
                sel,
            })
        },
        move || {
            Message::Reading(ReadingMsg::SelectDone {
                path: path.to_owned(),
                view,
            })
        },
        link_msg(path),
    )
    // Plan round-5 WP-8: right press on a link / inside the selection → its context menu.
    .on_right(move |link, inside, at| {
        let target = press_target(path, view, unresolved, link, inside);
        Message::Menu(crate::context_menu::MenuMsg::Open(target, at))
    })
    .into()
}

/// The menu target of a right press in tab `view` of `path`: on a link span (body, table cell or
/// frontmatter value alike — all are `run_el` spans) its link menu, else the selection menu.
pub(crate) fn press_target(
    path: &str,
    view: u64,
    unresolved: &std::collections::HashSet<String>,
    link: Option<LinkTarget>,
    inside: bool,
) -> crate::context_menu::Target {
    match link {
        Some(l) => {
            let (target, external) = match l {
                LinkTarget::Vault(t) => (t, false),
                LinkTarget::External(u) => (u, true),
            };
            let missing = !external && unresolved.contains(&target);
            crate::context_menu::Target::Link {
                src: path.to_owned(),
                target,
                external,
                view,
                selection: inside,
                missing,
            }
        }
        None => crate::context_menu::Target::Selection {
            path: path.to_owned(),
            view,
        },
    }
}

/// The read-only frontmatter block (an empty `Space` when there is none).
fn properties<'a>(cx: &Ctx<'a, '_>, props: &'a [Property]) -> Element<'a, Message> {
    if props.is_empty() {
        return Space::new().into();
    }
    let t = cx.t;
    let rows = props.iter().map(|p| {
        if let [PropValue::Bool(b)] = p.values.as_slice() {
            let side = cx.sz(14.0);
            let mark = task_box(if *b { 'x' } else { ' ' }, side, t);
            return row![
                container(
                    text(p.key.as_str())
                        .size(cx.sz(14.0))
                        .color(t.text_dim.to_color())
                )
                .width(Length::Fixed(140.0)),
                button(mark)
                    .padding(0)
                    .style(button::text)
                    .on_press(prop_press(cx.path, &p.key)),
            ]
            .spacing(8)
            .into();
        }
        let mut spans = Vec::new();
        for (i, v) in p.values.iter().enumerate() {
            if i > 0 {
                spans.push(span(", ").color(t.text_faint.to_color()));
            }
            spans.push(match v {
                PropValue::Text(s) => span(s.as_str()).color(t.text.to_color()),
                PropValue::Bool(b) => {
                    span(if *b { "true" } else { "false" }).color(t.text.to_color())
                }
                PropValue::Url { url, label } => span(label.as_str())
                    .color(t.accent_ui.to_color())
                    .link(LinkTarget::External(url.clone())),
                PropValue::Link {
                    target,
                    fragment,
                    alias,
                } => span(link_label(target, fragment.as_ref(), alias.as_ref()))
                    .color(link_color(cx, target))
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
            cx.run_el(cx.take_run().0, spans, cx.sz(14.0)),
        ]
        .spacing(8)
        .into()
    });
    boxed(column(rows).spacing(4).into(), t, t.line)
}

/// plan usability-sprint-captures-links WP-4: the click message of a frontmatter boolean.
fn prop_press(path: &str, key: &str) -> Message {
    Message::Reading(ReadingMsg::ToggleProp {
        path: path.to_owned(),
        key: key.to_owned(),
    })
}

/// The `crucible-view` fence hook: the view `<file>#<name>` with `this` = the embedding note.
fn view_fence<'a>(cx: &Ctx<'a, '_>, file: &str, name: &str) -> Element<'a, Message> {
    crate::views::fence(
        cx.views,
        &crate::views::view_id(file, name),
        cx.path,
        cx.t,
        cx.scale,
    )
}

/// Plan round-14 WP-3: the card for an embedded file the editor cannot show — its name and an
/// "open externally" button (both open it).
fn file_card<'a>(cx: &Ctx<'a, '_>, target: &'a str) -> Element<'a, Message> {
    let t = cx.t;
    let name = target.rsplit('/').next().unwrap_or(target);
    let open = Message::Reading(ReadingMsg::OpenLink {
        src: cx.path.to_owned(),
        target: target.to_owned(),
    });
    let card = row![
        text(name).size(cx.sz(14.0)).color(t.text.to_color()),
        Space::new().width(Length::Fill),
        text("Open externally ↗")
            .size(cx.sz(12.0))
            .color(t.accent_ui.to_color()),
    ]
    .spacing(8)
    .align_y(iced::alignment::Vertical::Center);
    let btn = button(card)
        .padding(0)
        .style(button::text)
        .width(Length::Fill)
        .on_press(open);
    boxed(btn.into(), t, t.text_faint)
}

/// Plan round-15 WP-4: `![[x.pdf#page=N|W]]` — page `N` rendered inline (the pane width, or `W`
/// clamped to it) through the PDF seam and the shared cache, lazily like an image embed. A click
/// opens the PDF tab at that page. An unresolved file, an out-of-range page, a parse failure or
/// a render failure/refusal shows the round-14 file card instead.
fn pdf_embed<'a>(
    cx: &Ctx<'a, '_>,
    target: &'a str,
    page: u32,
    width: Option<u32>,
) -> Element<'a, Message> {
    use crate::reading::PdfInfo;
    let Some(Some(pdf)) = cx.doc.pdfs.get(target) else {
        return file_card(cx, target);
    };
    let page0 = (page as usize).saturating_sub(1);
    if cx.store.pdf_embed_failed(cx.path, pdf, page0) {
        return file_card(cx, target);
    }
    let info = cx.store.pdf_info.get(pdf);
    let size = match info {
        Some(PdfInfo::Failed) => return file_card(cx, target),
        Some(PdfInfo::Ready { pages, .. }) => match pages.get(page0) {
            Some(&size) => Some(size),
            None => return file_card(cx, target),
        },
        None => None,
    };
    let pinned = cx
        .store
        .pdf_embeds
        .get(&(cx.path.to_owned(), cx.view))
        .and_then(|v| v.pins.get(&(pdf.clone(), page0)))
        .and_then(|k| cx.store.images.get(k));
    let placeholder = match info {
        Some(PdfInfo::Ready { placeholders, .. }) => placeholders.get(page0),
        _ => None,
    };
    let fill = |h: &iced::widget::image::Handle| -> Element<'a, Message> {
        let img = image(h.clone()).width(Length::Fill);
        let mut c = container(img).width(Length::Fill);
        if let Some(w) = width {
            c = c.max_width(w as f32);
        }
        c.into()
    };
    let body: Element<'a, Message> = match pinned.or(placeholder) {
        Some(h) => fill(h),
        None => container(
            text("PDF")
                .size(cx.sz(13.0))
                .color(cx.t.text_faint.to_color()),
        )
        .height(Length::Fixed(120.0))
        .width(Length::Fill)
        .into(),
    };
    let est_w = width.map_or(crate::image_cache::MAX_WIDTH as f32, |w| w as f32);
    let est_h = size.map_or(
        120.0,
        |(pw, ph)| if pw > 0.0 { est_w * ph / pw } else { est_w },
    );
    let (note, view) = (cx.path.to_owned(), cx.view);
    let shown = (note.clone(), pdf.clone());
    let hidden = shown.clone();
    let clicked = Message::PdfTab(PdfTabMsg::EmbedOpen {
        path: pdf.clone(),
        page: page0,
    });
    let card = button(body)
        .padding(0)
        .style(button::text)
        .width(Length::Fill)
        .on_press(clicked);
    sensor(card)
        .key(format!("{note}\u{1}{view}\u{1}{pdf}#page={page}"))
        .anticipate(crate::image_cache::sensor_margin(est_w, est_h))
        .on_show(move |size| {
            Message::PdfTab(PdfTabMsg::EmbedShown {
                note: shown.0.clone(),
                view,
                pdf: shown.1.clone(),
                page: page0,
                width: size.width,
            })
        })
        .on_hide(Message::PdfTab(PdfTabMsg::EmbedHidden {
            note: hidden.0,
            view,
            pdf: hidden.1,
            page: page0,
        }))
        .into()
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

fn block<'a>(cx: &Ctx<'a, '_>, b: &'a Block) -> Element<'a, Message> {
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
            let mut col: Vec<Element<'a, Message>> = vec![cx.run_el(
                cx.take_run().0,
                vec![span(head).color(edge.to_color())],
                cx.sz(16.0),
            )];
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
            // plan editor-round-4 WP-3: code-block text is visible text (find-painted).
            let (i, _) = cx.take_run();
            let mut spans: Vec<RSpan<'a>> = Vec::new();
            painted(cx, i, 0, &mut spans, Cow::Borrowed(code.as_str()), |p| {
                span(p).font(Font::MONOSPACE).color(t.text.to_color())
            });
            col.push(cx.run_el(i, spans, cx.sz(14.0)));
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
            link,
        } => {
            // Plan round-5 WP-7: an image holds a run slot (its alt text is copy text).
            cx.take_run();
            let img = image_block(cx, src, alt, *width, *remote, link.as_deref());
            match link {
                Some(href) => linked_image(cx, img, href, src, *remote),
                None => img,
            }
        }
        Block::Transclusion {
            target,
            fragment,
            content,
        } => {
            let label = match fragment {
                Some(f) => format!("↳ {target}#{f}"),
                None => format!("↳ {target}"),
            };
            let head: Element<'a, Message> = cx.run_el(
                cx.take_run().0,
                vec![
                    span(label)
                        .color(t.accent_ui.to_color())
                        .link(LinkTarget::Vault(target.clone())),
                ],
                cx.sz(13.0),
            );
            let body: Element<'a, Message> = match content {
                Some(blocks) => column(blocks.iter().map(|c| block(cx, c)))
                    .spacing(6)
                    .into(),
                None => text("(not found)").color(t.text_faint.to_color()).into(),
            };
            boxed(column![head, body].spacing(6).into(), t, t.accent_ui)
        }
        Block::ViewFence { file, name } => view_fence(cx, file, name),
        Block::FileEmbed { target } => {
            // Plan round-14 WP-3 (audit M2): a file card; a click resolves the target like a
            // link, and `open_into` hands the external file to the system opener.
            cx.take_run();
            file_card(cx, target)
        }
        Block::PdfEmbed {
            target,
            page,
            width,
        } => {
            cx.take_run();
            pdf_embed(cx, target, *page, *width)
        }
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

/// Whether a task's item text is struck through (Crucible theme.css ~616-621: `[-]` only).
fn task_struck(c: char) -> bool {
    c == '-'
}

/// The click message of a task marker: host-note tasks toggle; transcluded ones (no line)
/// stay display-only (audit M1).
fn task_press(path: &str, task: Option<char>, line: Option<usize>) -> Option<Message> {
    match (task, line) {
        (Some(_), Some(line)) => Some(Message::Reading(ReadingMsg::ToggleTask {
            path: path.to_owned(),
            line,
        })),
        _ => None,
    }
}

fn list_item<'a>(
    cx: &Ctx<'a, '_>,
    ordered: bool,
    n: usize,
    it: &'a ListItem,
) -> Element<'a, Message> {
    // Paragraph text is `block_size(None)` at iced's default 1.3 line height: the marker sits in
    // a box one first-line tall so it centres on the first line, not on the whole item.
    let line_h = cx.sz(block_size(None)) * 1.3;
    let marker: Element<'a, Message> = match (it.task, ordered) {
        (Some(c), _) => task_box(c, cx.sz(block_size(None)) * 0.95, cx.t),
        (None, true) => text(format!("{n}."))
            .font(Font::MONOSPACE)
            .color(cx.t.text_dim.to_color())
            .into(),
        (None, false) => text("•")
            .font(Font::MONOSPACE)
            .color(cx.t.text_dim.to_color())
            .into(),
    };
    let marker = container(marker).height(line_h).center_y(line_h);
    let marker: Element<'a, Message> = match task_press(cx.path, it.task, it.line) {
        Some(msg) => {
            let b = button(marker).padding(0).style(button::text).on_press(msg);
            // plan editor-ui-round-3 WP-4: right-click opens the status menu.
            match it.line {
                Some(line) => crate::context_menu::on_right(
                    b,
                    crate::context_menu::Target::Task {
                        path: cx.path.to_owned(),
                        line,
                    },
                ),
                None => b.into(),
            }
        }
        None => marker.into(),
    };
    let child_cx = Ctx {
        strike: cx.strike || it.task.is_some_and(task_struck),
        ..*cx
    };
    row![
        marker,
        column(it.children.iter().map(|c| block(&child_cx, c))).spacing(4),
    ]
    .spacing(6)
    .into()
}

fn image_block<'a>(
    cx: &Ctx<'a, '_>,
    src: &'a str,
    alt: &'a str,
    width: Option<u32>,
    remote: bool,
    link: Option<&str>,
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
        // WP-3: the placeholder opens the image URL in the browser.
        // plan editor-round-4 WP-4: right-click → Copy URL.
        let target = crate::context_menu::Target::Image {
            src: src.to_owned(),
            remote: true,
        };
        let label = placeholder(format!("[remote image: {alt}]"));
        // Plan round-6 WP-3: a linked remote image clicks through the outer href instead
        // (the caller wraps it); the right-click target is the caller's linked-image menu.
        if link.is_some() {
            return label;
        }
        return crate::context_menu::on_right(
            button(label)
                .padding(0)
                .style(button::text)
                .on_press(Message::OpenUrl(src.to_owned())),
            target,
        );
    }
    let Some(Some(key)) = cx.doc.images.get(src) else {
        return placeholder(format!("[missing image: {alt}]"));
    };
    // Plan round-5 WP-2: the header-read size sizes the placeholder exactly as the decoded image
    // will be (a transparent same-aspect handle, `expand` + Contain), so nothing below jumps.
    let dims = cx.store.images.dims(key);
    let natural = |d: &crate::image_cache::Dims| width.unwrap_or(d.width) as f32;
    let alt_text = || text(alt).size(cx.sz(13.0)).color(t.text_faint.to_color());
    let content: Element<'a, Message> = match (cx.store.images.get(key), dims) {
        // Plan round-6 WP-2: the placeholder stays the base layer once decoded, so the laid-out
        // size never changes on decode (a `|width` beyond the natural width upscales).
        (h, Some(d)) => {
            let boxed = sized_image(d, width, h.cloned());
            if h.is_some() {
                boxed.into()
            } else {
                boxed.push(alt_text()).into()
            }
        }
        (Some(h), None) => {
            let img = image(h.clone());
            match width {
                Some(w) => img.width(Length::Fixed(w as f32)).into(),
                None => img.into(),
            }
        }
        (None, None) => container(alt_text())
            .height(Length::Fixed(120.0))
            .width(Length::Fill)
            .into(),
    };
    // Plan round-5 WP-2: anticipate by the element's own extent so an image taller than the
    // viewport stays pinned (shown) while any part of it overlaps the viewport.
    let margin = match dims {
        Some(d) => {
            let w = natural(d).min(crate::image_cache::MAX_WIDTH as f32);
            crate::image_cache::sensor_margin(w, d.scaled_height(w))
        }
        None => crate::image_cache::sensor_margin(crate::image_cache::MAX_WIDTH as f32, 120.0),
    };
    let tab = cx.path.to_owned();
    let shown_key = key.clone();
    let shown_tab = tab.clone();
    let view = cx.view;
    let menu_target = crate::context_menu::Target::Image {
        src: key.clone(),
        remote: false,
    };
    let sensed = sensor(content)
        .key_ref(key.as_str())
        .anticipate(margin)
        .on_show(move |size| {
            Message::Reading(ReadingMsg::ImageShown {
                tab: shown_tab.clone(),
                view,
                key: shown_key.clone(),
                width: width.map_or(size.width, |w| w as f32),
            })
        })
        .on_hide(Message::Reading(ReadingMsg::ImageHidden {
            tab,
            view,
            key: key.clone(),
        }));
    // plan editor-round-4 WP-4: right-click → Copy image / Copy path.
    // Plan round-6 WP-3: a linked image's menu is attached by [`linked_image`] instead.
    if link.is_some() {
        return sensed.into();
    }
    crate::context_menu::on_right(sensed, menu_target)
}

/// Plan round-6 WP-3: the open message for a Reading href in note `path` (external → URL
/// opener, vault → OpenLink), as a [`LinkTarget`].
fn href_target(href: &str) -> LinkTarget {
    if editor_model::render::is_remote(href) {
        LinkTarget::External(href.to_owned())
    } else {
        LinkTarget::Vault(href.to_owned())
    }
}

/// Plan round-6 WP-4: a link's color — the accent, dimmed (half alpha) when the vault target is
/// in the Doc's `unresolved` set.
fn link_color(cx: &Ctx<'_, '_>, target: &str) -> iced::Color {
    let c = cx.t.accent_ui.to_color();
    if cx.doc.unresolved.contains(target) {
        iced::Color { a: c.a * 0.5, ..c }
    } else {
        c
    }
}

/// Plan round-6 WP-3: the left-click message of a linked image with `href` in note `path`.
fn linked_image_open(path: &str, href: &str) -> Message {
    link_msg(path)(href_target(href))
}

/// Plan round-6 WP-3: wraps an image of `[![alt](src)](href)` so a left click opens `href`
/// (no padding: layout unchanged) and a right click offers the link then the image items.
fn linked_image<'a>(
    cx: &Ctx<'a, '_>,
    img: Element<'a, Message>,
    href: &str,
    src: &str,
    remote: bool,
) -> Element<'a, Message> {
    let open = linked_image_open(cx.path, href);
    let (target, external) = match href_target(href) {
        LinkTarget::Vault(t) => (t, false),
        LinkTarget::External(u) => (u, true),
    };
    let missing = !external && cx.doc.unresolved.contains(&target);
    let link = crate::context_menu::Target::Link {
        src: cx.path.to_owned(),
        target,
        external,
        view: cx.view,
        selection: false,
        missing,
    };
    crate::context_menu::on_right(
        button(img).padding(0).style(button::text).on_press(open),
        linked_image_target(cx, link, src, remote),
    )
}

/// Plan round-6 WP-3: the linked-image menu target — the link half plus the image half (the
/// resolved vault key, or the URL when remote); a missing local image offers the link only.
fn linked_image_target(
    cx: &Ctx<'_, '_>,
    link: crate::context_menu::Target,
    src: &str,
    remote: bool,
) -> crate::context_menu::Target {
    let image = if remote {
        Some(src.to_owned())
    } else {
        cx.doc.images.get(src).cloned().flatten()
    };
    match image {
        Some(src) => crate::context_menu::Target::LinkedImage {
            link: Box::new(link),
            image: Box::new(crate::context_menu::Target::Image { src, remote }),
        },
        None => link,
    }
}

/// Plan round-6 WP-2: an image of header size `d` shown `width` (or natural) wide: the
/// same-aspect placeholder is the base layer and fixes the laid-out size; the decoded handle, if
/// any, fills it. Placeholder and decoded layouts are therefore the same height by construction.
pub(super) fn sized_image<'a, M: 'a, R>(
    d: &crate::image_cache::Dims,
    width: Option<u32>,
    decoded: Option<image::Handle>,
) -> iced::widget::Stack<'a, M, Theme, R>
where
    R: iced::advanced::image::Renderer<Handle = image::Handle> + 'a,
{
    let base = image(d.placeholder.clone())
        .width(Length::Fixed(width.unwrap_or(d.width) as f32))
        .expand(true);
    let s = iced::widget::Stack::new().push(base);
    match decoded {
        Some(h) => s.push(
            image(h)
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Fill),
        ),
        None => s,
    }
}

fn link_msg(path: &str) -> impl Fn(LinkTarget) -> Message + 'static {
    let src = path.to_owned();
    move |l| match l {
        LinkTarget::Vault(target) => Message::Reading(ReadingMsg::OpenLink {
            src: src.clone(),
            target,
        }),
        // WP-3: handed to the opener (which refuses non-allowlisted schemes).
        LinkTarget::External(url) => Message::OpenUrl(url),
    }
}

/// A rich-text span of the reading view.
type RSpan<'a> = iced::widget::text::Span<'a, LinkTarget>;

/// `text[r]`, borrowed when `text` is.
fn sub<'a>(text: &Cow<'a, str>, r: std::ops::Range<usize>) -> Cow<'a, str> {
    match text {
        Cow::Borrowed(s) => Cow::Borrowed(&s[r]),
        Cow::Owned(s) => Cow::Owned(s[r].to_owned()),
    }
}

/// Pushes visible `text` — the piece of run `run` starting at byte `base` of its text — as
/// spans built by `make`, split on the find occurrences inside it: every occurrence light, the
/// active one strong (plan editor-ui-round-3 WP-5; every visible segment, plan editor-round-4
/// WP-3). The occurrences come from the run table (the one walk), never re-counted here.
fn painted<'a>(
    cx: &Ctx<'a, '_>,
    run: usize,
    base: usize,
    spans: &mut Vec<RSpan<'a>>,
    text: Cow<'a, str>,
    make: impl Fn(Cow<'a, str>) -> RSpan<'a>,
) {
    let t = cx.t;
    let end = base + text.len();
    let mut at = 0;
    for (k, o) in cx.hits(run) {
        if o.range.start < base || o.range.end > end {
            continue;
        }
        let m = o.range.start - base..o.range.end - base;
        if m.start > at {
            spans.push(make(sub(&text, at..m.start)));
        }
        let (bg, fg) = if cx.active == Some(k) {
            (t.find_strong(), t.on_accent)
        } else {
            (t.find_light(), t.bg)
        };
        spans.push(
            make(sub(&text, m.clone()))
                .background(bg.to_color())
                .color(fg.to_color()),
        );
        at = m.end;
    }
    if at < text.len() || at == 0 {
        let len = text.len();
        spans.push(make(sub(&text, at..len)));
    }
}

fn inlines_view<'a>(
    cx: &Ctx<'a, '_>,
    inlines: &'a [Inline],
    size: f32,
    color: Option<Rgb>,
    bold: bool,
) -> Element<'a, Message> {
    let size = cx.sz(size);
    let t = cx.t;
    let base = if cx.strike {
        t.text_faint
    } else {
        color.unwrap_or(t.text)
    };
    let (run, _) = cx.take_run();
    let mut spans: Vec<RSpan<'a>> = Vec::with_capacity(inlines.len());
    for i in inlines {
        // The piece's byte offset in the run text (= `VisRun::text`).
        let at: usize = spans.iter().map(|s| s.text.len()).sum();
        match i {
            Inline::Text { text, style } => {
                let mut font = Font::DEFAULT;
                if style.strong || bold {
                    font.weight = Weight::Bold;
                }
                if style.em {
                    font.style = FontStyle::Italic;
                }
                let style_span = |piece: Cow<'a, str>| {
                    let mut s = span(piece)
                        .font(font)
                        .color(base.to_color())
                        .strikethrough(style.strike || cx.strike);
                    if style.highlight {
                        s = s.background(t.sel_tint.to_color()).color(t.text.to_color());
                    }
                    s
                };
                painted(
                    cx,
                    run,
                    at,
                    &mut spans,
                    Cow::Borrowed(text.as_str()),
                    style_span,
                );
            }
            // plan editor-round-4 WP-3: code spans, link labels and tags are visible text too,
            // painted like `Text`; split link pieces keep their click target.
            Inline::Code(c) => painted(cx, run, at, &mut spans, Cow::Borrowed(c.as_str()), |p| {
                span(p)
                    .font(Font::MONOSPACE)
                    .background(t.inset.to_color())
                    .color(t.text.to_color())
            }),
            Inline::WikiLink { target, .. } => {
                // No permanent underline: the run widget underlines the hovered link itself.
                let label = editor_model::visible::inline_visible(i)
                    .unwrap_or_default()
                    .into_owned();
                let color = link_color(cx, target);
                painted(cx, run, at, &mut spans, Cow::Owned(label), |p| {
                    span(p).color(color).link(LinkTarget::Vault(target.clone()))
                });
            }
            Inline::Link { url, label } => {
                let l = if editor_model::render::is_remote(url) {
                    LinkTarget::External(url.clone())
                } else {
                    LinkTarget::Vault(url.clone())
                };
                let color = link_color(cx, url);
                painted(
                    cx,
                    run,
                    at,
                    &mut spans,
                    Cow::Borrowed(label.as_str()),
                    |p| span(p).color(color).link(l.clone()),
                );
            }
            Inline::Tag(tag) => painted(
                cx,
                run,
                at,
                &mut spans,
                Cow::Owned(format!("#{tag}")),
                |p| span(p).color(t.accent.to_color()),
            ),
            Inline::FootnoteRef { ix, .. } => spans.push(
                span(format!("[{ix}]"))
                    .size((size - 4.0).max(10.0))
                    .color(t.accent_ui.to_color()),
            ),
            Inline::LineBreak => spans.push(span("\n")),
        }
    }
    if spans.is_empty() {
        spans.push(span(inline_text(inlines)));
    }
    cx.run_el(run, spans, size)
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod task_tests;
