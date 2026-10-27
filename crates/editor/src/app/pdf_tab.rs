//! Plan round-15 WP-3/WP-4, WP-R2 (review E1, E14): the PDF message glue. PDF tabs and inline
//! embeds are consumers of the one store ([`crate::pdf::store`]); this module turns their
//! sensor messages into store calls, runs the store's one pump ([`App::pump_pdf`]) and turns
//! its jobs into off-thread opens ([`crate::pdf::open`]) and renders
//! ([`crate::pdf::render_page`]), whose completion is the App-level
//! [`Message::MediaLoaded`]. Drawing is [`crate::pdf::view`].

use std::collections::HashSet;

use super::*;
use crate::message::{MediaKey, PdfTabMsg};
use crate::pdf::store::{Consumer, Doc, PageKey};
use crate::pdf::{self, view as pdf_view};

impl App {
    pub(super) fn update_pdf_tab(&mut self, m: PdfTabMsg) -> Task<Message> {
        match m {
            PdfTabMsg::Open { path } => return self.open_pdf(path),
            PdfTabMsg::Opened { path, result } => {
                if !self.pdf_needed(&path) {
                    self.pdf.remove_doc(&path);
                    return Task::none();
                }
                if let Err(e) = &result {
                    tracing::debug!(error = %e, "pdf open failed");
                }
                self.pdf.opened(&path, result, pdf_view::placeholders);
                return self.pump_pdf();
            }
            PdfTabMsg::PageShown { view, page, width } => {
                let path = page.path.clone();
                self.pdf.show(Consumer::Tab(view), page, width);
                let pump = self.pump_pdf();
                return Task::batch([pump, self.pdf_apply_goto(&path)]);
            }
            PdfTabMsg::PageHidden { view, page } => {
                self.pdf
                    .hide(&Consumer::Tab(view), &page, &mut self.reading.images);
                return self.pump_pdf();
            }
            PdfTabMsg::OpenExternal(path) => self.open_external(&path),
            PdfTabMsg::EmbedShown {
                note,
                view,
                page,
                width,
            } => {
                if !self.reading.pdf_embed_known(&self.docs, &note, &page.path) {
                    return Task::none();
                }
                self.pdf.show(Consumer::Embed { note, view }, page, width);
                return self.pump_pdf();
            }
            PdfTabMsg::EmbedHidden { note, view, page } => {
                self.pdf.hide(
                    &Consumer::Embed { note, view },
                    &page,
                    &mut self.reading.images,
                );
                return self.pump_pdf();
            }
            // The existing open rules (focus an open tab, else a new one), then the scroll.
            PdfTabMsg::EmbedOpen { path, page } => {
                self.pdf.goto.insert(path.clone(), page);
                let open = self.update_inner(Message::Tabs(TabsMsg::Open(path.clone())));
                return Task::batch([open, self.pdf_apply_goto(&path)]);
            }
        }
        Task::none()
    }

    /// Reads and parses `path` once, off the UI thread.
    fn open_pdf(&mut self, path: String) -> Task<Message> {
        let Some(d) = &self.daemon else {
            return Task::none();
        };
        if !self.pdf.begin_open(&path) {
            return Task::none();
        }
        let d = Arc::clone(d);
        let p = path.clone();
        Task::perform(
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
        )
    }

    /// The one pump ([`crate::pdf::store::PdfStore::pump`]) over every tab and embed consumer:
    /// its renders become tasks completing as [`Message::MediaLoaded`], its unknown documents
    /// are opened.
    pub(super) fn pump_pdf(&mut self) -> Task<Message> {
        let out = self.pdf.pump(&mut self.reading.images);
        let mut tasks: Vec<Task<Message>> =
            out.open.into_iter().map(|p| self.open_pdf(p)).collect();
        for job in out.jobs {
            let pdf::store::Job {
                page,
                width,
                key,
                bytes,
            } = job;
            let index = page.page;
            tasks.push(Task::perform(
                async move {
                    tokio::task::spawn_blocking(move || pdf::render_page(&bytes, index, width))
                        .await
                        .unwrap_or_else(|_| Err("render task failed".to_owned()))
                },
                move |result| Message::MediaLoaded {
                    key: MediaKey::Page(page.clone(), key.clone()),
                    result,
                },
            ));
        }
        Task::batch(tasks)
    }

    /// Whether a PDF document must stay loaded: a tab shows it, or an open note embeds it.
    fn pdf_needed(&self, path: &str) -> bool {
        self.path_open(path)
            || self.docs.values().any(|od| {
                od.reading
                    .doc
                    .as_ref()
                    .is_some_and(|d| d.pdfs.values().flatten().any(|p| p == path))
            })
    }

    /// Drops parsed documents nothing needs any more — the one document-removal rule.
    pub(super) fn sweep_pdf_docs(&mut self) {
        let stale: Vec<String> = self
            .pdf
            .doc_paths()
            .filter(|(p, d)| !matches!(d, Doc::Loading) && !self.pdf_needed(p))
            .map(|(p, _)| p.clone())
            .collect();
        for p in stale {
            self.pdf.remove_doc(&p);
        }
    }

    /// Note `note` closed or moved away: its inline embeds go.
    pub(super) fn pdf_note_closed(&mut self, note: &str) {
        self.pdf.retain(
            &mut self.reading.images,
            |c, _| !matches!(c, Consumer::Embed { note: n, .. } if n == note),
        );
    }

    /// Plan round-15 WP-4 (audit E4), WP-R2: after a Reading re-lower, each note's embeds keep
    /// only the pages its current render model embeds (none when it is closed or not shown
    /// in Reading).
    pub(crate) fn prune_pdf_embeds(&mut self) {
        let notes: HashSet<String> = self.pdf.embed_notes().map(str::to_owned).collect();
        for note in notes {
            let live: HashSet<PageKey> = if self.reading.mode(&self.docs, &note).shows_reading() {
                self.reading
                    .doc(&self.docs, &note)
                    .map(|d| d.pdf_pages.clone())
                    .unwrap_or_default()
            } else {
                HashSet::new()
            };
            self.pdf
                .retain_embeds(&note, &live, &mut self.reading.images);
        }
    }

    /// Plan round-15 WP-4: scrolls a PDF tab of `path` to the page an embed click asked for,
    /// once a tab has a laid-out width and the document is parsed (else the request stays
    /// pending and the next `PageShown` retries).
    fn pdf_apply_goto(&mut self, path: &str) -> Task<Message> {
        let Some(&page) = self.pdf.goto.get(path) else {
            return Task::none();
        };
        let Some(pages) = self.pdf.pages(path) else {
            return Task::none();
        };
        let ids: Vec<u64> = self
            .panes
            .iter()
            .flat_map(|(_, ps)| ps.tabs.iter())
            .filter(
                |t| matches!(&t.kind, TabKind::File { viewer: Viewer::Pdf, path: p } if p == path),
            )
            .map(|t| t.id)
            .collect();
        let Some((view, width)) = ids.into_iter().find_map(|id| {
            let w = self.pdf.tab_width(id, path);
            (w > 0.0).then_some((id, w))
        }) else {
            return Task::none();
        };
        let y = pdf_view::page_offset(pages, width, page);
        self.pdf.goto.remove(path);
        iced::widget::operation::scroll_to(
            pdf_view::scroll_id(view),
            iced::widget::scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        )
    }
}
