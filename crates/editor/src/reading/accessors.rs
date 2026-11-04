//! `ReadingStore` place and selection accessors (split out of `reading/mod.rs`, plan
//! tn-review-2026-10-26 WP-R4): the per-tab block/edge/restore place state and the Reading
//! selection, each a thin read or write of the owning [`DocReading`].

use iced::Task;

use super::{Doc, Docs, RESTORE_GUARD, ReadingStore, probe, rd, rd_mut, selection};
use crate::message::Message;
use crate::widget::selectable::Selection;

impl ReadingStore {
    /// plan editor-ui-round-3 WP-6: the top-level block at the reading viewport's top, from
    /// the last layout probe (`None` = at the top / not probed).
    pub fn top_block(&self, docs: &Docs, path: &str, view: u64) -> Option<usize> {
        rd(docs, path)?.place(view)?.top
    }

    /// Plan round-8 WP-4: sets the `scroll_anchor` fraction (clamped to 0–1).
    pub fn set_anchor(&mut self, docs: &mut Docs, a: f32) {
        self.anchor = if a.is_finite() {
            a.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if self.anchor <= 0.0 {
            for d in docs.values_mut() {
                d.reading
                    .views
                    .values_mut()
                    .for_each(|v| v.anchor_at = None);
            }
        }
    }

    /// Plan round-8 WP-4: the block under the anchor height (the mode-switch origin); with
    /// anchor 0 (or no anchored probe yet) exactly [`Self::top_block`].
    pub fn anchor_block(&self, docs: &Docs, path: &str, view: u64) -> Option<usize> {
        if self.anchor > 0.0
            && let Some(b) = rd(docs, path)
                .and_then(|d| d.place(view))
                .and_then(|v| v.anchor_at)
        {
            return Some(b);
        }
        self.top_block(docs, path, view)
    }

    /// Plan round-8 WP-4: a mode-switch carry of `block` — probes, then scrolls the block to
    /// the anchor height (the top at anchor 0, exactly as [`probe`] with `Some(block)`).
    pub fn carry_to(
        &mut self,
        docs: &mut Docs,
        path: &str,
        view: u64,
        blocks: usize,
        block: usize,
    ) -> Task<Message> {
        let anchored = self.anchor > 0.0;
        if let Some(d) = rd_mut(docs, path) {
            let v = d.place_mut(view);
            v.top = Some(block);
            v.carry |= anchored;
        }
        probe(path, view, blocks, Some(block))
    }

    /// plan editor-ui-round-3 WP-6: records the intended top block of a carry (the scroll task
    /// is in flight; its probe confirms it).
    pub fn set_top_block(&mut self, docs: &mut Docs, path: &str, view: u64, block: usize) {
        if let Some(d) = rd_mut(docs, path) {
            d.place_mut(view).top = Some(block);
        }
    }

    /// Plan round-6 WP-1: tab `view` on `path` went to its start (`false`) / end (`true`).
    pub fn set_edge(&mut self, docs: &mut Docs, path: &str, view: u64, bottom: bool) {
        if let Some(d) = rd_mut(docs, path) {
            d.place_mut(view).edge = Some(bottom);
        }
    }

    /// Plan round-6 WP-1: the edge tab `view` on `path` was last sent to, if not scrolled since.
    pub fn edge(&self, docs: &Docs, path: &str, view: u64) -> Option<bool> {
        rd(docs, path)?.place(view)?.edge
    }

    /// Plan round-6 WP-1: a user scroll left the edge.
    pub fn clear_edge(&mut self, docs: &mut Docs, path: &str, view: u64) {
        if let Some(v) = rd_mut(docs, path).and_then(|d| d.views.get_mut(&view)) {
            v.edge = None;
        }
    }

    /// plan editor-ui-round-3 WP-6: the reading view is (being scrolled) at its top.
    pub fn clear_top_block(&mut self, docs: &mut Docs, path: &str, view: u64) {
        if let Some(v) = rd_mut(docs, path).and_then(|d| d.views.get_mut(&view)) {
            v.top = None;
        }
    }

    /// Plan round-5 WP-1: tab `view` showing `path` was (re)activated — restores its stored
    /// top block (probe → `scroll_to`), and arms the guard so the re-created scrollable's first
    /// `on_scroll` (offset 0) re-applies it instead of overwriting it. `None` when the tab has
    /// no stored place (it starts at the top anyway) or shows no reading view.
    pub fn restore(&mut self, docs: &mut Docs, path: &str, view: u64) -> Option<Task<Message>> {
        if !self.mode(docs, path).shows_reading() {
            return None;
        }
        let b = self.top_block(docs, path, view)?;
        let blocks = self.doc(docs, path)?.model.blocks.len();
        rd_mut(docs, path)?.place_mut(view).restoring = Some(std::time::Instant::now());
        Some(probe(path, view, blocks, Some(b)))
    }

    /// Plan round-5 WP-1: the restore guard of tab `view` is armed.
    pub fn is_restoring(&self, docs: &Docs, view: u64) -> bool {
        docs.values()
            .filter_map(|d| d.reading.views.get(&view)?.restoring)
            .any(|at| at.elapsed() < RESTORE_GUARD)
    }

    /// Test hook: backdates tab `view`'s restore guard by `by`.
    #[cfg(test)]
    pub(crate) fn age_restore_guard(
        &mut self,
        docs: &mut Docs,
        view: u64,
        by: std::time::Duration,
    ) {
        for d in docs.values_mut() {
            if let Some(at) = d
                .reading
                .views
                .get_mut(&view)
                .and_then(|v| v.restoring.as_mut())
            {
                *at = at.checked_sub(by).unwrap_or(*at);
            }
        }
    }

    /// Test hook: `path`'s rendered doc, mutably.
    #[cfg(test)]
    pub(crate) fn doc_mut<'a>(&mut self, docs: &'a mut Docs, path: &str) -> Option<&'a mut Doc> {
        rd_mut(docs, path)?.doc.as_mut()
    }

    /// Tests: per-document Reading states held (one per open document).
    #[cfg(test)]
    pub(crate) fn doc_entries(&self, docs: &Docs) -> usize {
        docs.len()
    }

    pub fn doc<'a>(&self, docs: &'a Docs, path: &str) -> Option<&'a Doc> {
        rd(docs, path).and_then(|t| t.doc.as_ref())
    }

    /// Plan round-5 WP-7: tab `view`'s Reading selection of `path`.
    pub fn selection(&self, docs: &Docs, path: &str, view: u64) -> Option<Selection> {
        rd(docs, path)?.place(view)?.sel
    }

    /// plan round-21 WP-2: tab `view`'s just-copied code block (run index) of `path`.
    pub fn code_copied(&self, docs: &Docs, path: &str, view: u64) -> Option<usize> {
        rd(docs, path)
            .and_then(|d| d.place(view))
            .and_then(|v| v.code_copied)
    }

    /// Sets (or clears: `None` or collapsed) tab `view`'s Reading selection of `path`.
    pub fn set_selection(
        &mut self,
        docs: &mut Docs,
        path: &str,
        view: u64,
        sel: Option<Selection>,
    ) {
        let sel = sel.filter(|s| !s.is_empty());
        if let Some(d) = rd_mut(docs, path) {
            let v = d.place_mut(view);
            if sel.is_some() {
                v.source_newer = false;
            }
            v.sel = sel;
        }
    }

    /// Ctrl+A: selects `path`'s whole rendered document in tab `view`; false when nothing is
    /// rendered.
    pub fn select_all(&mut self, docs: &mut Docs, path: &str, view: u64) -> bool {
        let all = self.doc(docs, path).and_then(|d| selection::all(&d.runs));
        self.set_selection(docs, path, view, all);
        self.selection(docs, path, view).is_some()
    }

    /// Plan round-5 WP-7 (the WP-8 accessor): the copy text of tab `view`'s Reading selection
    /// of `path` — `None` when nothing (or only an empty range) is selected.
    pub fn selection_text(&self, docs: &Docs, path: &str, view: u64) -> Option<String> {
        let sel = self.selection(docs, path, view)?;
        let text = selection::copy_text(&self.doc(docs, path)?.runs, sel);
        (!text.is_empty()).then_some(text)
    }

    /// Plan round-5 WP-8: the Source side of tab `view` of `path` changed its selection (it is
    /// now the most recent side in Split).
    pub fn note_source_selection(&mut self, docs: &mut Docs, path: &str, view: u64) {
        if let Some(d) = rd_mut(docs, path) {
            d.place_mut(view).source_newer = true;
        }
    }

    /// Plan round-5 WP-8: true when the Source side selected more recently than the Reading
    /// side of tab `view` of `path`.
    pub fn source_selected_last(&self, docs: &Docs, path: &str, view: u64) -> bool {
        rd(docs, path)
            .and_then(|d| d.place(view))
            .is_some_and(|v| v.source_newer)
    }
}
