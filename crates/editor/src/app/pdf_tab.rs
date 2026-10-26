//! Plan round-15-relative-images-pdf-viewer WP-3: the PDF tab.
//!
//! A PDF tab holds no document. Its body reads and parses the file once, off the UI thread
//! ([`PdfTabMsg::Open`] → [`crate::pdf::open`]), then stacks the pages vertically at the pane
//! width. Each page is a sensor: a page coming within the near window (its anticipate margin)
//! is pinned in the shared [`ImageCache`] for this tab ([`image_cache::view_owner`]) under
//! [`page_key`] and rendered off the UI thread ([`crate::pdf::render_page`]); leaving it
//! unpins (and so releases) it. At most [`MAX_INFLIGHT`] renders run per tab, and every render
//! passes [`crate::pdf::admit`] against the bytes reserved for PDF pages (audit E1).
//!
//! [`ImageCache`]: crate::image_cache::ImageCache

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::*;
use crate::image_cache;
use crate::message::PdfTabMsg;
use crate::pdf::{self, PageSizes};
use iced::widget::image::Handle;

/// Concurrent page renders per tab (audit E1).
pub const MAX_INFLIGHT: usize = 2;

/// The cache key of page `page` (0-based) of `path` rendered `width` px wide. `#page=` never
/// occurs in an image key (a bare vault path), so the two never collide.
pub fn page_key(path: &str, page: usize, width: u32) -> String {
    format!("{path}#page={}@{width}", page + 1)
}

/// One PDF file's state (shared by every tab of it). Only `Send` data — never a renderer value
/// (audit E3).
#[derive(Debug)]
pub enum PdfDoc {
    /// Read + parse in flight.
    Loading,
    /// Parsed: the file bytes (shared with render jobs), page sizes in points, one
    /// aspect-ratio placeholder handle per page (pages of one size share one), and the last
    /// page-render failure.
    Ready {
        bytes: Arc<Vec<u8>>,
        pages: PageSizes,
        placeholders: Vec<Handle>,
        error: Option<String>,
    },
    /// The read or parse failed.
    Failed(String),
}

/// One tab's page state.
#[derive(Debug, Default)]
pub struct PdfView {
    /// The path these pages belong to.
    pub path: String,
    /// Last laid-out page width.
    pub width: f32,
    /// Pages within the near window.
    pub shown: BTreeSet<usize>,
    /// Pages pinned (or loading) for this tab, and their cache key.
    pub pages: BTreeMap<usize, String>,
    /// Pages whose render this tab started and has not seen finish.
    pub inflight: BTreeSet<usize>,
    /// Pages refused by admission or failed; retried after a page leaves view.
    pub refused: BTreeSet<usize>,
}

/// All PDF tab state.
#[derive(Debug, Default)]
pub struct PdfTabs {
    pub docs: HashMap<String, PdfDoc>,
    pub views: HashMap<u64, PdfView>,
    /// Projected bytes per PDF page key pinned or in flight (audit E1 accounting).
    pub reserved: HashMap<String, usize>,
}

impl PdfTabs {
    /// Bytes reserved for PDF pages.
    pub fn reserved_bytes(&self) -> usize {
        self.reserved.values().sum()
    }
}

/// Drops `key`'s reservation once the cache neither holds nor awaits it.
fn settle(reserved: &mut HashMap<String, usize>, cache: &image_cache::ImageCache, key: &str) {
    if !cache.contains(key) && !cache.waiting(key) {
        reserved.remove(key);
    }
}

/// One placeholder handle per distinct page size.
fn placeholders(pages: &PageSizes) -> Vec<Handle> {
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

impl App {
    pub(super) fn update_pdf_tab(&mut self, m: PdfTabMsg) -> Task<Message> {
        match m {
            PdfTabMsg::Open { path } => {
                if self.pdf.docs.contains_key(&path) {
                    return Task::none();
                }
                let Some(d) = &self.daemon else {
                    return Task::none();
                };
                self.pdf.docs.insert(path.clone(), PdfDoc::Loading);
                let d = Arc::clone(d);
                let p = path.clone();
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let (bytes, _) = d.read_file(&p).map_err(|e| format!("read: {e}"))?;
                            let bytes = Arc::new(bytes);
                            pdf::open(&bytes).map(|sizes| (bytes, sizes))
                        })
                        .await
                        .unwrap_or_else(|_| Err("open task failed".to_owned()))
                    },
                    move |result| Message::PdfTab(PdfTabMsg::Opened { path, result }),
                );
            }
            PdfTabMsg::Opened { path, result } => {
                if !self.path_open(&path) {
                    self.pdf.docs.remove(&path);
                    return Task::none();
                }
                let doc = match result {
                    Ok((bytes, pages)) => PdfDoc::Ready {
                        placeholders: placeholders(&pages),
                        bytes,
                        pages,
                        error: None,
                    },
                    Err(e) => {
                        tracing::debug!(error = %e, "pdf open failed");
                        PdfDoc::Failed(e)
                    }
                };
                self.pdf.docs.insert(path, doc);
            }
            PdfTabMsg::PageShown {
                path,
                view,
                page,
                width,
            } => {
                let v = self.pdf.views.entry(view).or_default();
                if v.path != path {
                    v.path.clone_from(&path);
                }
                v.width = width;
                v.shown.insert(page);
                return self.pump_pdf(view);
            }
            PdfTabMsg::PageHidden { path, view, page } => {
                let Some(v) = self.pdf.views.get_mut(&view) else {
                    return Task::none();
                };
                v.shown.remove(&page);
                v.refused.clear();
                if let Some(key) = v.pages.remove(&page) {
                    let cache = &mut self.reading.images;
                    cache.hide(&image_cache::view_owner(&path, view), &key);
                    settle(&mut self.pdf.reserved, cache, &key);
                }
                return self.pump_pdf(view);
            }
            PdfTabMsg::Rendered {
                path,
                view,
                page,
                key,
                result,
            } => {
                let cache = &mut self.reading.images;
                let failed = match result {
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
                settle(&mut self.pdf.reserved, cache, &key);
                if let Some(e) = failed {
                    if let Some(PdfDoc::Ready { error, .. }) = self.pdf.docs.get_mut(&path) {
                        *error = Some(format!("page {}: {e}", page + 1));
                    }
                    if let Some(v) = self.pdf.views.get_mut(&view)
                        && v.pages.get(&page) == Some(&key)
                    {
                        v.pages.remove(&page);
                        v.refused.insert(page);
                    }
                }
                if let Some(v) = self.pdf.views.get_mut(&view) {
                    v.inflight.remove(&page);
                }
                return self.pump_pdf(view);
            }
            PdfTabMsg::OpenExternal(path) => self.open_external(&path),
        }
        Task::none()
    }

    /// Starts renders for `view`'s shown pages that have none, within [`MAX_INFLIGHT`] and
    /// the admission rule ([`pdf::admit`], audit E1).
    fn pump_pdf(&mut self, view: u64) -> Task<Message> {
        let PdfTabs {
            docs,
            views,
            reserved,
        } = &mut self.pdf;
        let Some(v) = views.get_mut(&view) else {
            return Task::none();
        };
        let Some(PdfDoc::Ready { bytes, pages, .. }) = docs.get(&v.path) else {
            return Task::none();
        };
        let cache = &mut self.reading.images;
        let owner = image_cache::view_owner(&v.path, view);
        let wanted: Vec<usize> = v
            .shown
            .iter()
            .copied()
            .filter(|p| !v.pages.contains_key(p) && !v.refused.contains(p))
            .collect();
        let mut tasks = Vec::new();
        for page in wanted {
            if v.inflight.len() >= MAX_INFLIGHT {
                break;
            }
            let Some(&size) = pages.get(page) else {
                continue;
            };
            let used: usize = reserved.values().sum();
            let Some(width) = pdf::admit(size, v.width, used, cache.budget()) else {
                v.refused.insert(page);
                continue;
            };
            let key = page_key(&v.path, page, width);
            let start = cache.show(&owner, &key);
            v.pages.insert(page, key.clone());
            if start {
                reserved.insert(key.clone(), pdf::projected_bytes(size, width));
                v.inflight.insert(page);
                let bytes = Arc::clone(bytes);
                let path = v.path.clone();
                tasks.push(Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || pdf::render_page(&bytes, page, width))
                            .await
                            .unwrap_or_else(|_| Err("render task failed".to_owned()))
                    },
                    move |result| {
                        Message::PdfTab(PdfTabMsg::Rendered {
                            path,
                            view,
                            page,
                            key,
                            result,
                        })
                    },
                ));
            }
        }
        Task::batch(tasks)
    }

    /// Tab `view` of `path` closed or moved away: every page pin it holds goes.
    pub(super) fn pdf_view_closed(&mut self, path: &str, view: u64) {
        let Some(v) = self.pdf.views.remove(&view) else {
            return;
        };
        let cache = &mut self.reading.images;
        let owner = image_cache::view_owner(path, view);
        for key in v.pages.values() {
            cache.hide(&owner, key);
            settle(&mut self.pdf.reserved, cache, key);
        }
    }

    /// The PDF at `from` moved to `to` (tabs `ids`, already retargeted by
    /// [`Self::retarget_image`]): the old page pins go, the parsed document moves with the
    /// file. Each page sensor's key carries the path, so the change re-fires `PageShown`.
    pub(super) fn retarget_pdf(&mut self, from: &str, to: &str, ids: &[u64]) {
        for &id in ids {
            self.pdf_view_closed(from, id);
        }
        if let Some(doc) = self.pdf.docs.remove(from) {
            self.pdf.docs.insert(to.to_owned(), doc);
        }
    }

    /// The PDF tab body: a "page N / M" bar with "Open externally", then the pages.
    pub(super) fn pdf_body<'a>(&'a self, path: &'a str, view: u64) -> Element<'a, Message> {
        use iced::widget::{Column, image, scrollable, sensor};
        let t = self.tokens;
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
        let (pages, placeholders, error) = match self.pdf.docs.get(path) {
            Some(PdfDoc::Ready {
                pages,
                placeholders,
                error,
                ..
            }) => (pages, placeholders, error),
            Some(PdfDoc::Failed(e)) => {
                return centred(
                    iced::widget::column![faint(format!("Cannot show {name}: {e}")), external]
                        .spacing(8)
                        .into(),
                );
            }
            Some(PdfDoc::Loading) | None => {
                let open = Message::PdfTab(PdfTabMsg::Open {
                    path: path.to_owned(),
                });
                return sensor(centred(faint(format!("Loading {name}…")).into()))
                    .key_ref(path)
                    .on_show(move |_| open.clone())
                    .into();
            }
        };
        let v = self.pdf.views.get(&view);
        let width = v.map_or(0.0, |v| v.width);
        let at = v.and_then(|v| v.shown.first()).map_or(1, |p| p + 1);
        let mut bar = row![
            faint(format!("page {at} / {}", pages.len())),
            Space::new().width(Length::Fill),
            external
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center)
        .padding(4);
        if let Some(e) = error {
            bar = bar.push(faint(format!("({e})")));
        }
        let mut col = Column::new().spacing(8).padding(8).width(Length::Fill);
        for (i, &(pw, ph)) in pages.iter().enumerate() {
            let handle = v
                .and_then(|v| v.pages.get(&i))
                .and_then(|k| self.reading.images.get(k))
                .or_else(|| placeholders.get(i));
            let body: Element<'a, Message> = match handle {
                Some(h) => image(h.clone()).width(Length::Fill).into(),
                None => Space::new().width(Length::Fill).height(100).into(),
            };
            let est_w = if width > 0.0 { width } else { 800.0 };
            let est_h = if pw > 0.0 { est_w * ph / pw } else { est_w };
            let shown = path.to_owned();
            col = col.push(
                sensor(body)
                    .key(format!("{path}#page={i}"))
                    .anticipate(image_cache::sensor_margin(est_w, est_h))
                    .on_show(move |size| {
                        Message::PdfTab(PdfTabMsg::PageShown {
                            path: shown.clone(),
                            view,
                            page: i,
                            width: size.width,
                        })
                    })
                    .on_hide(Message::PdfTab(PdfTabMsg::PageHidden {
                        path: path.to_owned(),
                        view,
                        page: i,
                    })),
            );
        }
        iced::widget::column![bar, scrollable(col).height(Length::Fill)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}
