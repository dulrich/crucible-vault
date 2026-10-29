//! WP-R2 (review E14): the PDF drawing — the page image, placeholders, the PDF tab body and
//! its layout constants (shared by the body and [`page_offset`], review E12a).

use std::collections::HashMap;

use iced::widget::image::Handle;
use iced::widget::{Column, Space, button, container, row, sensor, text};
use iced::{Element, Length};

use super::PageSizes;
use super::store::{Consumer, Doc, PageKey, PdfStore};
use crate::image_cache::{self, ImageCache};
use crate::message::{Message, PdfTabMsg};
use crate::theme::Tokens;

/// The PDF tab's page column padding, px.
pub const PAGE_PAD: f32 = 8.0;
/// The gap between pages in the PDF tab, px.
pub const PAGE_GAP: f32 = 8.0;

/// WP-R1 (review P1): a page bitmap (or its placeholder) drawn expanding to its slot width,
/// whatever its pixel width — iced's non-expanding image keeps a narrower bitmap at its pixel
/// size (`iced_widget-0.14.2/src/image.rs:185-235`), while expanding it scales to the slot with
/// the page's aspect, so a placeholder and any render of the page lay out at the same height.
pub fn page_image(h: Handle) -> iced::widget::Image<Handle> {
    iced::widget::image(h)
        .width(iced::Length::Fill)
        .expand(true)
}

/// One placeholder handle per distinct page size.
pub fn placeholders(pages: &PageSizes) -> Vec<Handle> {
    let mut by_size: HashMap<(u32, u32), Handle> = HashMap::new();
    let side = |v: f32| (v.round().max(1.0)) as u32;
    pages
        .iter()
        .map(|&(w, h)| {
            let (w, h) = (side(w), side(h));
            by_size
                .entry((w, h))
                .or_insert_with(|| {
                    image_cache::Dims::new(w, h)
                        .map_or_else(|| Handle::from_rgba(1, 1, vec![0; 4]), |d| d.placeholder)
                })
                .clone()
        })
        .collect()
}

/// The scrollable id of PDF tab `view`'s page column.
pub fn scroll_id(view: u64) -> iced::widget::Id {
    iced::widget::Id::from(format!("pdf-pages:{view}"))
}

/// The scroll offset of the top of 0-based `page` in a column of pages `width` px wide
/// ([`PAGE_PAD`] then each page and a [`PAGE_GAP`], as [`tab_body`] lays them out).
pub fn page_offset(pages: &PageSizes, width: f32, page: usize) -> f32 {
    pages.iter().take(page).fold(PAGE_PAD, |y, &(pw, ph)| {
        y + if pw > 0.0 { width * ph / pw } else { width } + PAGE_GAP
    })
}

/// The PDF tab body: a "page N / M" bar with "Open externally", then the pages.
pub fn tab_body<'a>(
    store: &'a PdfStore<Handle>,
    images: &'a ImageCache,
    t: Tokens,
    path: &'a str,
    view: u64,
) -> Element<'a, Message> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let faint = |s: String| text(s).color(t.text_faint.to_color());
    let external = button(text("Open externally"))
        .style(button::secondary)
        .on_press(Message::PdfTab(PdfTabMsg::OpenExternal(path.to_owned())));
    let centred = |e: Element<'a, Message>| -> Element<'a, Message> {
        container(e)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(8)
            .into()
    };
    let (pages, placeholders, failed) = match store.doc(path) {
        Some(Doc::Ready {
            pages,
            placeholders,
            failed,
            ..
        }) => (pages, placeholders, failed),
        Some(Doc::Failed(e)) => {
            return centred(
                iced::widget::column![faint(format!("Cannot show {name}: {e}")), external]
                    .spacing(8)
                    .into(),
            );
        }
        Some(Doc::Loading) | None => {
            let open = Message::PdfTab(PdfTabMsg::Open {
                path: path.to_owned(),
            });
            return sensor(centred(faint(format!("Loading {name}…")).into()))
                .key_ref(path)
                .on_show(move |_| open.clone())
                .into();
        }
    };
    let me = Consumer::Tab(view);
    let v = store.pages_of(&me, path);
    let width = store.tab_width(view, path);
    let at = v.and_then(|v| v.shown.keys().next()).map_or(1, |p| p + 1);
    let mut bar = row![
        faint(format!("page {at} / {}", pages.len())),
        Space::new().width(Length::Fill),
        external
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center)
    .padding(4);
    if let Some((p, e)) = failed.iter().next() {
        bar = bar.push(faint(format!("(page {}: {e})", p + 1)));
    }
    let mut col = Column::new()
        .spacing(PAGE_GAP)
        .padding(PAGE_PAD)
        .width(Length::Fill);
    for (i, &(pw, ph)) in pages.iter().enumerate() {
        let pk = PageKey::new(path, i);
        let handle = store
            .held(&me, &pk)
            .and_then(|k| images.get(k))
            .or_else(|| placeholders.get(i));
        let body: Element<'a, Message> = match handle {
            Some(h) => page_image(h.clone()).into(),
            None => Space::new().width(Length::Fill).height(100).into(),
        };
        let est_w = if width > 0.0 { width } else { 800.0 };
        let est_h = if pw > 0.0 { est_w * ph / pw } else { est_w };
        let shown = pk.clone();
        col = col.push(
            sensor(body)
                .key(pk.id())
                .anticipate(image_cache::sensor_margin(est_w, est_h))
                .on_show(move |size| {
                    Message::PdfTab(PdfTabMsg::PageShown {
                        view,
                        page: shown.clone(),
                        width: size.width,
                    })
                })
                .on_hide(Message::PdfTab(PdfTabMsg::PageHidden { view, page: pk })),
        );
    }
    iced::widget::column![
        bar,
        crate::theme::gutter_scroll(col)
            .id(scroll_id(view))
            .height(Length::Fill)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
