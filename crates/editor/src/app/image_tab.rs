//! Plan round-14 WP-3: image tabs and the system opener for files the editor cannot show.
//!
//! An image tab holds no document: its body is a sensor over the shared [`ImageCache`] entry
//! keyed by the vault path (pinned per tab id, exactly as Reading pins an embed), decoded off
//! the UI thread by [`image_cache::load_task`]. A decode failure is shown in the tab.
//!
//! [`ImageCache`]: crate::image_cache::ImageCache

use super::*;
use crate::image_cache;
use crate::message::ImageTabMsg;
use cv_text::file_kind::{self, FileKind};

/// A file read made while classifying it (bytes, content hash).
type Prefetched = (Vec<u8>, crate::daemon::ContentHash);

impl App {
    /// The kind of `path` for [`Self::open_into`], plus its bytes when classifying had to read
    /// them (unknown extension: NUL sniff), so the text open reuses that read.
    pub(super) fn classify_open(
        &self,
        path: &str,
    ) -> Result<(FileKind, Option<Prefetched>), DaemonError> {
        if let Some(k) = file_kind::by_extension(path) {
            return Ok((k, None));
        }
        if self.docs.contains_key(path) {
            return Ok((FileKind::Text, None));
        }
        let Some(d) = &self.daemon else {
            return Err(DaemonError::Disconnected);
        };
        let read = d.read_file(path)?;
        Ok((file_kind::sniff(&read.0), Some(read)))
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
        if let Err(e) = crate::open_url::open_file(&self.prefs.open_url_cmd, &abs) {
            self.status.set_error(e);
        }
    }

    pub(super) fn update_image_tab(&mut self, m: ImageTabMsg) -> Task<Message> {
        match m {
            ImageTabMsg::Shown { path, view, width } => {
                if self.image_errors.contains_key(&path) {
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
                        |key, result| Message::ImageTab(ImageTabMsg::Loaded { key, result }),
                    );
                }
            }
            ImageTabMsg::Hidden { path, view } => {
                self.reading
                    .images
                    .hide(&image_cache::view_owner(&path, view), &path);
            }
            ImageTabMsg::Loaded { key, result } => match result {
                Ok(d) => {
                    self.image_errors.remove(&key);
                    self.reading.images.loaded(&key, Some(d));
                }
                Err(e) => {
                    tracing::debug!(error = %e, "image tab decode failed");
                    self.reading.images.loaded(&key, None);
                    self.image_errors.insert(key, e);
                }
            },
        }
        Task::none()
    }

    /// An image tab closed: its pin goes (and a cached failure, when no tab shows it any more).
    pub(super) fn image_tab_closed(&mut self, path: &str, view: u64) {
        self.reading
            .images
            .hide(&image_cache::view_owner(path, view), path);
        if !self.path_open(path) {
            self.image_errors.remove(path);
        }
    }

    /// The image tab body: the image scaled to fit the pane, a loading line, or the failure.
    pub(super) fn image_body<'a>(&'a self, path: &'a str, view: u64) -> Element<'a, Message> {
        use iced::widget::{image, sensor};
        let t = self.tokens;
        let name = path.rsplit('/').next().unwrap_or(path);
        let content: Element<'a, Message> =
            match (self.reading.images.get(path), self.image_errors.get(path)) {
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
