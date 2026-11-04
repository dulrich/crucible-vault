//! Reading embed renderers (split out of `reading/view.rs`, plan tn-review-2026-10-26 WP-R4):
//! the view fence, file card, inline PDF page, and image / linked-image blocks.

use super::*;
use editor_model::render::RemoteCardKind;

/// The `crucible-view` fence hook: the view `<file>#<name>` with `this` = the embedding note.
pub(super) fn view_fence<'a>(cx: &Ctx<'a, '_>, file: &str, name: &str) -> Element<'a, Message> {
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
pub(super) fn file_card<'a>(cx: &Ctx<'a, '_>, target: &'a str) -> Element<'a, Message> {
    let t = cx.t;
    let name = target.rsplit('/').next().unwrap_or(target);
    let open = Message::Reading(ReadingMsg::OpenLink {
        src: cx.path.to_owned(),
        target: target.to_owned(),
        fragment: None,
    });
    // Plan remote-media-sprint WP-5: a local audio/video file is a media card (♪/▶ + name).
    let label = match media_glyph(target) {
        Some(g) => format!("{g} {name}"),
        None => name.to_owned(),
    };
    let card = row![
        text(label)
            .size(cx.sz(14.0))
            .color(t.text.to_color())
            .shaping(iced::widget::text::Shaping::Advanced),
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
pub(super) fn pdf_embed<'a>(
    cx: &Ctx<'a, '_>,
    target: &'a str,
    page: u32,
    width: Option<u32>,
) -> Element<'a, Message> {
    use crate::pdf::store::{Consumer, Doc as PdfDoc, PageKey};
    let Some(Some(pdf)) = cx.doc.pdfs.get(target) else {
        return file_card(cx, target);
    };
    let pk = PageKey::new(pdf, (page as usize).saturating_sub(1));
    let page0 = pk.page;
    // WP-R2: a render failure of the page (any surface's) shows the card.
    if cx.pdfs.page_failed(&pk).is_some() {
        return file_card(cx, target);
    }
    let (size, placeholder) = match cx.pdfs.doc(pdf) {
        Some(PdfDoc::Failed(_)) => return file_card(cx, target),
        Some(PdfDoc::Ready {
            pages,
            placeholders,
            ..
        }) => match pages.get(page0) {
            Some(&size) => (Some(size), placeholders.get(page0)),
            None => return file_card(cx, target),
        },
        Some(PdfDoc::Loading) | None => (None, None),
    };
    let me = Consumer::Embed {
        note: cx.path.to_owned(),
        view: cx.view,
    };
    let pinned = cx.pdfs.held(&me, &pk).and_then(|k| cx.store.images.get(k));
    let fill = |h: &iced::widget::image::Handle| -> Element<'a, Message> {
        let img = crate::pdf::view::page_image(h.clone());
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
    let shown = (note.clone(), pk.clone());
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
        .key(format!("{note}\u{1}{view}\u{1}{}", pk.id()))
        .anticipate(crate::image_cache::sensor_margin(est_w, est_h))
        .on_show(move |size| {
            Message::PdfTab(PdfTabMsg::EmbedShown {
                note: shown.0.clone(),
                view,
                page: shown.1.clone(),
                width: size.width,
            })
        })
        .on_hide(Message::PdfTab(PdfTabMsg::EmbedHidden {
            note: hidden.0,
            view,
            page: hidden.1,
        }))
        .into()
}

pub(super) fn image_block<'a>(
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
    if remote && link.is_none() {
        // Plan remote-media-sprint WP-5: a non-image remote reference is a card by kind.
        if let Some(kind) = RemoteCardKind::from_url(src) {
            return remote_card(cx, src, kind);
        }
    }
    if !remote && media_glyph(src).is_some() {
        // WP-5: `![](local.mp4)` is the same media card as `![[local.mp4]]`.
        return file_card(cx, src);
    }
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

/// Plan round-6 WP-3: the left-click message of a linked image with `href` in note `path`.
pub(super) fn linked_image_open(path: &str, href: &str) -> Message {
    link_msg(path)(href_target(href))
}

/// Plan round-6 WP-3: wraps an image of `[![alt](src)](href)` so a left click opens `href`
/// (no padding: layout unchanged) and a right click offers the link then the image items.
pub(super) fn linked_image<'a>(
    cx: &Ctx<'a, '_>,
    img: Element<'a, Message>,
    href: &str,
    src: &str,
    remote: bool,
) -> Element<'a, Message> {
    let open = linked_image_open(cx.path, href);
    let (target, external) = match href_target(href) {
        LinkTarget::Vault { target, .. } => (target, false),
        // `href_target` only yields Vault / External.
        LinkTarget::External(u) | LinkTarget::Copy(u) => (u, true),
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
    // Plan remote-media-sprint WP-5: a video href gets a ▶ badge over the thumbnail.
    let img: Element<'a, Message> = if is_video_href(href) {
        iced::widget::Stack::new()
            .push(img)
            .push(
                container(
                    text("▶")
                        .size(cx.sz(28.0))
                        .color(iced::Color::WHITE)
                        .shaping(iced::widget::text::Shaping::Advanced),
                )
                .center(Length::Fill),
            )
            .into()
    } else {
        img
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
pub(in crate::reading) fn sized_image<'a, M: 'a, R>(
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

/// Plan remote-media-sprint WP-5: whether a linked image's `href` is a video (the ▶ badge rule).
pub(super) fn is_video_href(href: &str) -> bool {
    matches!(
        cv_text::classify_remote(href),
        cv_text::RemoteKind::VideoPage { .. } | cv_text::RemoteKind::VideoFile
    )
}

/// The URL host minus a leading `www.` (the whole string when it has no scheme).
fn card_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit('@').next().unwrap_or(host);
    host.strip_prefix("www.").unwrap_or(host)
}

/// Plan remote-media-sprint WP-5: the card label of a remote non-image reference. Glyph coverage:
/// the bundled font (Inter) has none of ▶ ♪ 𝕏 ⧉; ▶ and ♪ come from common system fallbacks, but
/// 𝕏 and especially ⧉ (one installed font on the dev host) are not reliable, so those two use
/// plain text.
pub(super) fn remote_card_text(kind: RemoteCardKind, url: &str) -> String {
    let host = card_host(url);
    match kind {
        RemoteCardKind::Video => format!("▶ video · {host}"),
        RemoteCardKind::Post => "X post".to_owned(),
        RemoteCardKind::Audio => format!("♪ audio · {host}"),
        RemoteCardKind::Pdf => format!("pdf · {host}"),
        RemoteCardKind::Embed => format!("embed · {host}"),
    }
}

/// The label for a URL that classifies as a card kind (`None` = image/unknown: the placeholder).
#[cfg(test)]
pub(super) fn remote_card_label(url: &str) -> Option<String> {
    RemoteCardKind::from_url(url).map(|k| remote_card_text(k, url))
}

/// Plan remote-media-sprint WP-5: ♪ / ▶ for a local audio / video file name, else `None`.
pub(super) fn media_glyph(target: &str) -> Option<&'static str> {
    const AUDIO: &[&str] = &["mp3", "m4a", "wav", "flac", "ogg", "oga", "opus", "aac"];
    const VIDEO: &[&str] = &["webm", "mp4", "m4v", "mkv", "mov", "avi", "3gp"];
    let name = target.rsplit('/').next().unwrap_or(target);
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    if AUDIO.contains(&ext.as_str()) {
        Some("♪")
    } else if VIDEO.contains(&ext.as_str()) {
        Some("▶")
    } else {
        None
    }
}

/// Plan remote-media-sprint WP-5: a card for a remote non-image reference. Left click opens the
/// URL; right click offers Copy URL (the remote-image menu target).
pub(super) fn remote_card<'a>(
    cx: &Ctx<'a, '_>,
    url: &'a str,
    kind: RemoteCardKind,
) -> Element<'a, Message> {
    let t = cx.t;
    let label = boxed(
        text(remote_card_text(kind, url))
            .size(cx.sz(13.0))
            .color(t.text_faint.to_color())
            .shaping(iced::widget::text::Shaping::Advanced)
            .into(),
        t,
        t.line,
    );
    let target = crate::context_menu::Target::Image {
        src: url.to_owned(),
        remote: true,
    };
    crate::context_menu::on_right(
        button(label)
            .padding(0)
            .style(button::text)
            .on_press(Message::OpenUrl(url.to_owned())),
        target,
    )
}
