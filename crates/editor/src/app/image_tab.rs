//! Plan round-14 WP-3: image tabs and the system opener for files the editor cannot show.
//!
//! An image tab holds no document: its body is a sensor over the shared [`ImageCache`] entry
//! keyed by the vault path (pinned per tab id, exactly as Reading pins an embed), decoded off
//! the UI thread by [`image_cache::load_task`]. A decode failure is shown in the tab.
//!
//! [`ImageCache`]: crate::image_cache::ImageCache

use super::*;
use crate::image_cache;
use crate::message::{ImageTabMsg, MediaKey};
use cv_text::file_kind::{self, FileKind};

impl App {
    /// The kind of `path` for [`Self::open_into`]: by extension, else a NUL sniff of the file's
    /// bytes (the text open reads the file again itself).
    pub(super) fn classify_open(&self, path: &str) -> Result<FileKind, DaemonError> {
        if let Some(k) = file_kind::by_extension(path) {
            return Ok(k);
        }
        if self.docs.contains_key(path) {
            return Ok(FileKind::Text);
        }
        let Some(d) = &self.daemon else {
            return Err(DaemonError::Disconnected);
        };
        let read = d.read_file(path)?;
        Ok(file_kind::sniff(&read.0))
    }

    /// Opens vault file `path` with the system opener (`open_url_cmd`) by its absolute path
    /// inside the vault root; a spawn failure is a status-bar error.
    pub(super) fn open_external(&mut self, path: &str) {
        let Some(d) = &self.daemon else {
            self.status
                .set_error(open_error(&DaemonError::Disconnected));
            return;
        };
        let abs = d.vault_root().join(path);
        self.sink.opened_file(&abs.to_string_lossy());
        if let Err(e) =
            crate::open_url::open_file(&self.prefs.open_url_cmd, &abs, &self.opener_notices.tx)
        {
            self.status.set_error(e);
        }
    }

    pub(super) fn update_image_tab(&mut self, m: ImageTabMsg) -> Task<Message> {
        match m {
            ImageTabMsg::Shown { path, view, width } => {
                if self.reading.images.failure(&path).is_some() {
                    return Task::none();
                }
                let Some(d) = &self.daemon else {
                    return Task::none();
                };
                let owner = image_cache::view_owner(&path, view);
                if self.reading.images.show(&owner, &path) {
                    return image_cache::load_task(
                        Arc::clone(d),
                        path,
                        image_cache::decode_width(width),
                        |key, result| Message::MediaLoaded {
                            key: MediaKey::Image(key),
                            result,
                        },
                    );
                }
            }
            ImageTabMsg::Hidden { path, view } => {
                self.reading
                    .images
                    .hide(&image_cache::view_owner(&path, view), &path);
            }
        }
        Task::none()
    }

    /// WP-R2 (review E2): the one media completion, by key. It updates the cache, records a
    /// failure every surface reads (images: [`image_cache::ImageCache::failure`]; PDF pages:
    /// [`crate::pdf::store::PdfStore::page_failed`]) and re-pumps every consumer of the key.
    /// Images keep one shared decode per path (decoded at the first shower's width bucket;
    /// a wider later view scales it up) — see the WP-R2 report.
    pub(super) fn media_loaded(
        &mut self,
        key: MediaKey,
        result: Result<image_cache::Decoded, String>,
    ) -> Task<Message> {
        let cache = &mut self.reading.images;
        match key {
            MediaKey::Image(key) => match result {
                Ok(d) => {
                    cache.loaded(&key, Some(d));
                }
                Err(e) => {
                    tracing::debug!(error = %e, "image load failed");
                    cache.loaded(&key, None);
                    // A Reading embed's failure is a notice; an image tab shows it in place.
                    if !self.path_open(&key) {
                        self.status.notify(
                            Severity::Warning,
                            crate::reading::image_failure_text(&key, &e),
                        );
                    }
                    self.reading.images.fail(&key, e);
                }
            },
            MediaKey::Page(page, key) => {
                let error = match result {
                    Ok(d) => {
                        cache.loaded(&key, Some(d));
                        None
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "pdf page render failed");
                        cache.loaded(&key, None);
                        Some(e)
                    }
                };
                self.pdf.rendered(&page, &key, error, cache);
                return self.pump_pdf();
            }
        }
        Task::none()
    }

    /// Plan round-15 WP-2 / WP-R2 (review E6): the file shown at `from` moved to `to`. Every
    /// file tab of `from` keeps its viewer and id ([`Tab::retarget`]) and its history entries
    /// move; the `from` pins are released (an image failure and a parsed PDF move with the
    /// file). The new pin comes from the tab's sensor (its key carries the path).
    pub(super) fn retarget_file(&mut self, from: &str, to: &str) {
        let tabs: Vec<(Viewer, u64)> = self
            .panes
            .iter()
            .flat_map(|(_, ps)| ps.tabs.iter())
            .filter_map(|t| match &t.kind {
                TabKind::File { viewer, path } if path == from => Some((*viewer, t.id)),
                _ => None,
            })
            .collect();
        for (_, ps) in self.panes.iter_mut() {
            for t in ps.tabs.iter_mut().filter(|t| t.shows(from, &self.docs)) {
                t.retarget(to);
            }
            ps.history.retarget(from, to);
        }
        for &(viewer, id) in &tabs {
            self.file_view_released(viewer, from, id);
        }
        if let Some(e) = self.reading.images.take_failure(from) {
            self.reading.images.fail(to, e);
        }
        self.pdf.renamed(from, to, &mut self.reading.images);
    }

    /// File tab `view` (`viewer`) no longer shows `path`: its pins go.
    fn file_view_released(&mut self, viewer: Viewer, path: &str, view: u64) {
        match viewer {
            Viewer::Image => self
                .reading
                .images
                .hide(&image_cache::view_owner(path, view), path),
            Viewer::Pdf => self.pdf.view_closed(view, &mut self.reading.images),
        }
    }

    /// A file tab closed: its pins go (and an image failure, when no tab shows it any more);
    /// parsed PDFs are left to [`Self::sweep_pdf_docs`], the one removal rule.
    pub(super) fn file_tab_closed(&mut self, viewer: Viewer, path: &str, view: u64) {
        self.file_view_released(viewer, path, view);
        if viewer == Viewer::Image && !self.path_open(path) {
            self.reading.images.take_failure(path);
        }
    }

    /// The image tab body: the image scaled to fit the pane, a loading line, or the failure.
    pub(super) fn image_body<'a>(&'a self, path: &'a str, view: u64) -> Element<'a, Message> {
        use iced::widget::{image, sensor};
        let t = self.tokens;
        let name = path.rsplit('/').next().unwrap_or(path);
        let content: Element<'a, Message> = match (
            self.reading.images.get(path),
            self.reading.images.failure(path),
        ) {
            (Some(h), _) => image(h.clone())
                .content_fit(iced::ContentFit::ScaleDown)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            (None, Some(e)) => text(format!("Cannot show {name}: {e}"))
                .color(t.text_faint.to_color())
                .into(),
            (None, None) => text(format!("Loading {name}…"))
                .color(t.text_faint.to_color())
                .into(),
        };
        let shown = path.to_owned();
        let body = container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(8);
        // Plan round-22 WP-3: right-click the image for Copy image / Copy path.
        let target = crate::context_menu::Target::Image {
            src: path.to_owned(),
            remote: false,
        };
        let body = crate::context_menu::on_right(body, target);
        sensor(body)
            .key_ref(path)
            .on_show(move |size| {
                Message::ImageTab(ImageTabMsg::Shown {
                    path: shown.clone(),
                    view,
                    width: size.width,
                })
            })
            .on_hide(Message::ImageTab(ImageTabMsg::Hidden {
                path: path.to_owned(),
                view,
            }))
            .into()
    }
}
