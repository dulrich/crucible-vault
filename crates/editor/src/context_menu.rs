//! plan editor-ui-round-3 WP-4: the one reusable right-click menu.
//!
//! An overlay anchored at the pointer, drawn above the panes (below the modal dialog, which
//! keeps top input priority). It closes on Escape (routed through `App::modal_filter`), a
//! click outside the card, or choosing an item. One menu is open at a time: opening another
//! replaces it. The anchor is read at press time by [`crate::widget::right_click`] (no
//! per-move subscription).

use iced::widget::pane_grid::Pane;
use iced::widget::{Space, button, column, container, row, text};
use iced::{Element, Length, Point, Theme};

use crate::message::Message;
use crate::theme::Tokens;

/// What the menu was opened on.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// A host-note task box in Reading (source `line` of `path`).
    Task { path: String, line: usize },
    /// A tab header: the tab with [`crate::app::Tab::id`] `id` in `pane`.
    Tab { pane: Pane, id: u64, path: String },
    /// An explorer row (`folder` = a folder row).
    Row { path: String, folder: bool },
    /// plan editor-round-4 WP-4: a Reading image — `src` is the vault path (resolved key) or,
    /// when `remote`, the URL.
    Image { src: String, remote: bool },
    /// Plan round-5 WP-8: a Reading link in note `src` — `target` is the unresolved wiki target
    /// / relative md URL, or (when `external`) the URL. `selection` = the press was inside the
    /// tab's Reading selection (tab id `view`), which adds Copy.
    Link {
        src: String,
        target: String,
        external: bool,
        view: u64,
        selection: bool,
        /// Plan round-6 WP-4: an unresolved vault link (the Doc's `unresolved`) — adds
        /// "Create note".
        missing: bool,
    },
    /// Plan round-6 WP-5: an Outline panel heading — an in-note anchor at source `line` of
    /// `path` (Open / Open in new pane only).
    Anchor { path: String, line: usize },
    /// Plan round-5 WP-8: a right press inside tab `view`'s Reading selection of `path`.
    Selection { path: String, view: u64 },
    /// Plan round-6 WP-3: a linked image `[![alt](src)](href)` — the link half (a
    /// [`Target::Link`]) then the image half (a [`Target::Image`]).
    LinkedImage {
        link: Box<Target>,
        image: Box<Target>,
    },
}

/// A menu entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// Set the task status char.
    TaskStatus(char),
    Open,
    OpenInNewPane,
    Close,
    CloseOthers,
    Reveal,
    CopyPath,
    Rename,
    Delete,
    NewNote,
    /// WP-4: move the explorer file row.
    Move,
    /// WP-4: copy a vault image's bytes (MIME-typed) to the clipboard.
    CopyImage,
    /// WP-4: copy a remote image's URL.
    CopyUrl,
    /// Plan round-5 WP-8: copy the Reading selection.
    Copy,
    /// Plan round-6 WP-3: the image half of a linked image's Copy path (distinct from the link's).
    CopyImagePath,
    /// Plan round-6 WP-4: create the note an unresolved link names.
    CreateNote,
}

impl Item {
    /// Plan round-6 WP-3: items the image half of a [`Target::LinkedImage`] owns.
    pub fn is_image_item(self) -> bool {
        matches!(self, Item::CopyImage | Item::CopyImagePath)
    }
}

/// Menu messages.
#[derive(Debug, Clone)]
pub enum MenuMsg {
    /// Right-press on a target at a window position (the anchor).
    Open(Target, Point),
    /// Escape / outside click.
    Close,
    /// An item was chosen (the menu closes first).
    Choose(Item),
    /// WP-4: the off-thread Copy image finished (`Err` = status notice).
    ImageCopied(Result<String, String>),
}

/// The open menu.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextMenu {
    pub at: Point,
    pub target: Target,
}

/// The themed task status set (plan editor-feedback-round-2 WP-5's glyph map, `reading/view.rs`
/// `task_mark`): status char + short label.
pub const TASK_STATUSES: &[(char, &str)] = &[
    (' ', "To do"),
    ('x', "Done"),
    ('/', "In progress"),
    ('-', "Cancelled"),
    ('>', "Forwarded"),
    ('<', "Scheduled"),
    ('?', "Question"),
    ('!', "Important"),
    ('"', "Quote"),
    ('*', "Star"),
    ('l', "Location"),
    ('i', "Information"),
    ('S', "Savings"),
    ('I', "Idea"),
    ('f', "Fire"),
    ('k', "Key"),
    ('u', "Up"),
    ('d', "Down"),
    ('w', "Win"),
    ('p', "Pro"),
    ('c', "Con"),
    ('b', "Bookmark"),
];

/// The items of `target`'s menu, in order.
pub fn items(target: &Target) -> Vec<(Item, String)> {
    let named = |list: &[(Item, &str)]| -> Vec<(Item, String)> {
        list.iter().map(|(i, l)| (*i, (*l).to_owned())).collect()
    };
    match target {
        Target::Task { .. } => TASK_STATUSES
            .iter()
            .map(|(c, l)| (Item::TaskStatus(*c), format!("[{c}]  {l}")))
            .collect(),
        Target::Tab { .. } => named(&[
            (Item::Close, "Close"),
            (Item::CloseOthers, "Close others"),
            (Item::Reveal, "Reveal in explorer"),
            (Item::CopyPath, "Copy path"),
            (Item::Rename, "Rename"),
            (Item::Delete, "Delete note…"),
        ]),
        Target::Row { folder: false, .. } => named(&[
            (Item::Open, "Open"),
            (Item::OpenInNewPane, "Open in new pane"),
            (Item::Reveal, "Reveal"),
            (Item::CopyPath, "Copy path"),
            (Item::Rename, "Rename"),
            (Item::Move, "Move…"),
            (Item::Delete, "Delete…"),
        ]),
        // No "Open in new pane" (audit E1): it would load binary bytes into a note buffer.
        Target::Image { remote: false, .. } => named(&[
            (Item::CopyImage, "Copy image"),
            (Item::CopyPath, "Copy path"),
        ]),
        Target::Image { remote: true, .. } => named(&[(Item::CopyUrl, "Copy URL")]),
        Target::Link {
            external,
            selection,
            missing,
            ..
        } => {
            let mut v = if *selection {
                named(&[(Item::Copy, "Copy")])
            } else {
                Vec::new()
            };
            v.extend(if *external {
                named(&[(Item::Open, "Open"), (Item::CopyUrl, "Copy URL")])
            } else {
                named(&[
                    (Item::Open, "Open"),
                    (Item::OpenInNewPane, "Open in new pane"),
                    (Item::CopyPath, "Copy path"),
                ])
            });
            if *missing && !*external {
                v.push((Item::CreateNote, "Create note".to_owned()));
            }
            v
        }
        Target::Anchor { .. } => named(&[
            (Item::Open, "Open"),
            (Item::OpenInNewPane, "Open in new pane"),
        ]),
        Target::Selection { .. } => named(&[(Item::Copy, "Copy")]),
        Target::LinkedImage { link, image } => {
            let mut v = items(link);
            v.extend(items(image).into_iter().map(|(i, l)| match i {
                Item::CopyPath => (Item::CopyImagePath, "Copy image path".to_owned()),
                // A remote image's URL vs the link's URL.
                Item::CopyUrl => (Item::CopyImagePath, "Copy image URL".to_owned()),
                _ => (i, l),
            }));
            v
        }
        // Explorer ops rename / move / trash files only (no folder RPC): New note, Copy path.
        Target::Row { folder: true, .. } => {
            named(&[(Item::NewNote, "New note"), (Item::CopyPath, "Copy path")])
        }
    }
}

/// Card width.
const WIDTH: f32 = 220.0;

/// The menu layer: a full-window catcher (any click outside the card closes) holding the
/// card at the anchor.
pub fn view<'a>(m: &'a ContextMenu, t: Tokens) -> Element<'a, Message> {
    let entries = items(&m.target).into_iter().map(|(item, label)| {
        let lbl: Element<'a, Message> = match item {
            Item::TaskStatus(c) => row![
                crate::reading::view::task_box(c, 14.0, t),
                text(label).size(13)
            ]
            .spacing(8)
            .align_y(iced::Alignment::Center)
            .into(),
            _ => text(label).size(13).into(),
        };
        button(lbl)
            .padding([3, 8])
            .width(Length::Fill)
            .style(button::text)
            .on_press(Message::Menu(MenuMsg::Choose(item)))
            .into()
    });
    let card = container(column(entries).spacing(0))
        .padding(4)
        .width(WIDTH)
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
    let placed = column![
        Space::new().height(m.at.y.max(0.0)),
        row![
            Space::new().width(m.at.x.max(0.0)),
            iced::widget::opaque(card)
        ],
    ];
    iced::widget::mouse_area(container(placed).width(Length::Fill).height(Length::Fill))
        .on_press(Message::Menu(MenuMsg::Close))
        .on_right_press(Message::Menu(MenuMsg::Close))
        .into()
}

/// Wraps `content` so a right press opens `target`'s menu at the press point.
pub fn on_right<'a>(
    content: impl Into<Element<'a, Message>>,
    target: Target,
) -> Element<'a, Message> {
    crate::widget::right_click::right_click(content, open_msg(target)).into()
}

/// The right-press message for `target`: opens its menu at the press point.
pub fn open_msg(target: Target) -> impl Fn(Point) -> Message {
    move |at| Message::Menu(MenuMsg::Open(target.clone(), at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_image_menu_lists_link_items_then_image_items() {
        let t = Target::LinkedImage {
            link: Box::new(Target::Link {
                src: "n.md".into(),
                target: "Other.md".into(),
                external: false,
                view: 1,
                selection: false,
                missing: false,
            }),
            image: Box::new(Target::Image {
                src: "img/a.png".into(),
                remote: false,
            }),
        };
        let got: Vec<(Item, String)> = items(&t);
        let names: Vec<&str> = got.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(
            names,
            [
                "Open",
                "Open in new pane",
                "Copy path",
                "Copy image",
                "Copy image path"
            ]
        );
        assert_eq!(got[4].0, Item::CopyImagePath);
    }
}
