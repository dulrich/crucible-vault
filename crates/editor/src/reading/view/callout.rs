//! Reading callout rendering (split out of `reading/view.rs`): the callout box, its removed-content
//! fold controls, and the kind label and edge colour.

use super::*;

/// A top-level block: the first removed callout of a note with fold groups carries the Delete /
/// Keep controls (plan intake-removals-sources-rescan WP-6; every other block, and a callout
/// inside a transclusion or list, is drawn by [`block`] unchanged).
pub(super) fn top_block<'a>(cx: &Ctx<'a, '_>, i: usize, b: &'a Block) -> Element<'a, Message> {
    match (b, &cx.doc.fold_controls) {
        (
            Block::Callout {
                kind,
                title,
                fold,
                children,
            },
            Some(c),
        ) if c.block == i => callout(cx, kind, title.as_deref(), *fold, children, Some(&c.rows)),
        _ => block(cx, b),
    }
}

/// A small text button of the fold controls.
fn fold_button<'a>(cx: &Ctx<'a, '_>, label: &'static str, msg: Message) -> Element<'a, Message> {
    let t = cx.t;
    button(text(label).size(cx.sz(13.0)))
        .padding([1, 8])
        .style(move |_: &Theme, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                text_color: if hot { t.accent_ui } else { t.text_dim }.to_color(),
                border: Border {
                    color: t.line.to_color(),
                    width: 1.0,
                    radius: 3.0.into(),
                },
                ..button::Style::default()
            }
        })
        .on_press(msg)
        .into()
}

/// A callout; `fold_rows` (the removed-content fold of a note with groups) adds "Delete all" on
/// the title line and one row per group below it (kind label, preview, Delete, Keep); the fold's
/// own lines are drawn as usual.
pub(super) fn callout<'a>(
    cx: &Ctx<'a, '_>,
    kind: &'a str,
    title: Option<&'a str>,
    fold: Option<Fold>,
    children: &'a [Block],
    fold_rows: Option<&[crate::reading::fold_controls::FoldRow]>,
) -> Element<'a, Message> {
    let t = cx.t;
    let marker = match fold {
        Some(Fold::Folded) => "▸ ",
        Some(Fold::Open) => "▾ ",
        None => "",
    };
    let head = format!(
        "{marker}{}",
        title.map_or_else(|| capitalize(kind), str::to_owned)
    );
    let edge = callout_color(kind, t);
    let head_run = cx.run_el(
        cx.take_run().0,
        vec![span(head).color(edge.to_color())],
        cx.sz(16.0),
    );
    let mut col: Vec<Element<'a, Message>> = Vec::new();
    if let Some(rows) = fold_rows {
        let msg = |op| Message::Reading(ReadingMsg::Removals { note: cx.note, op });
        col.push(
            row![
                container(head_run).width(Length::Fill),
                fold_button(
                    cx,
                    "Delete all",
                    msg(crate::reading::fold_controls::delete_all())
                ),
            ]
            .spacing(8)
            .align_y(iced::alignment::Vertical::Center)
            .into(),
        );
        for r in rows {
            let label = r.kind.clone().unwrap_or_default();
            let info = if r.preview.is_empty() {
                format!("{} lines", r.lines)
            } else {
                format!("{} lines: {}", r.lines, r.preview)
            };
            col.push(
                row![
                    container(text(label).size(cx.sz(12.0)).color(edge.to_color()))
                        .width(Length::Fixed(cx.sz(64.0))),
                    container(text(info).size(cx.sz(12.0)).color(t.text_faint.to_color()))
                        .width(Length::Fill),
                    fold_button(cx, "Delete", msg(r.delete.clone())),
                    fold_button(cx, "Keep", msg(r.keep.clone())),
                ]
                .spacing(6)
                .align_y(iced::alignment::Vertical::Center)
                .into(),
            );
        }
    } else {
        col.push(head_run);
    }
    // Folding is shown by the marker; the body stays readable (no fold state in v0).
    col.extend(children.iter().map(|c| block(cx, c)));
    boxed(column(col).spacing(6).into(), t, edge)
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
