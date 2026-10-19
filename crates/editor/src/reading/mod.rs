//! Reading pane and toggle/split (WP-3).
//!
//! Per-tab reading state lives in a [`ReadingStore`] keyed by vault path (mode, cached render
//! model, resolved image keys), one entry per document (plan editor-feedback-round-2 WP-7). The render model
//! (`editor_model::render`) is recomputed on a debounce tick only when the buffer's
//! `edit_seq()` (or the index generation) moved, and cached between frames. Clicks resolve
//! through a `cv_index::Resolver` built from the local index (rebuilt per index generation),
//! so links in unsaved text resolve the same way the daemon would resolve them on save.
//!
//! Image handles are pinned per tab in the [`ImageCache`]; see that module for the release points
//! (audit E2) and the handle-release vs renderer-reclamation distinction.

mod paths;
mod place;
pub mod selection;
mod source_map;
pub mod view;

pub use paths::{missing_note_path, rename_target};
#[cfg(test)]
pub(crate) use place::{INTENTS, probe_op};
pub use place::{Probe, ScrollIntent, probe};
pub use source_map::{invisible_ranges, source_counterpart};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use cv_index::Resolver;
/// The note stem (breadcrumb filename input) and link-target decoding: the shared vault-path
/// helpers (WP-R10).
pub use cv_text::vpath::{percent_decode, stem};
use editor_model::render::{self, RenderModel};
use editor_model::visible::VisRun;
use iced::{Element, Subscription, Task};

use crate::daemon::DaemonClient;
use crate::image_cache::{self, ImageCache};
use crate::message::{Message, ReadingMsg, TabsMsg};
use crate::source::SourceBuffer;
use crate::theme::Tokens;
use crate::widget::selectable::Selection;

/// Recompute debounce: a change is rendered once `edit_seq` has been stable for one tick.
pub const TICK: Duration = Duration::from_millis(250);
/// Plan round-6 WP-2: how long an armed restore guard waits for the re-created scrollable's
/// first scroll report (it arrives within a frame or two of the activation).
pub const RESTORE_GUARD: Duration = Duration::from_millis(1000);

/// A tab's display mode: source / reading / split (D9). New tabs open in Reading (plan
/// editor-cutover-blockers). Persisted per path in `editor.json` (`ui_state::UiState::modes`)
/// as `"source"` / `"reading"` / `"split"` (the spellings of the former `ui_state::Layout`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    Source,
    #[default]
    Reading,
    /// Source left, reading right.
    Split,
}

impl ViewMode {
    /// Source → Reading → Split → Source.
    pub fn next(self) -> Self {
        match self {
            Self::Source => Self::Reading,
            Self::Reading => Self::Split,
            Self::Split => Self::Source,
        }
    }

    /// Ctrl+E (editor-usability-round WP-3): Source ↔ Reading; Split goes to Source.
    pub fn toggle(self) -> Self {
        match self {
            Self::Source => Self::Reading,
            Self::Reading | Self::Split => Self::Source,
        }
    }

    pub fn shows_reading(self) -> bool {
        self != Self::Source
    }

    /// The source buffer is on screen (Source or Split) and needs its shaped `Content`.
    pub fn shows_source(self) -> bool {
        self != Self::Reading
    }
}

/// A rendered document.
#[derive(Debug, Clone)]
pub struct Doc {
    /// The buffer `edit_seq()` this was rendered from.
    pub seq: u64,
    /// The buffer `epoch()` (wholesale reload counter) this was rendered from: an external
    /// reload (e.g. a daemon-origin lint write) replaces the text without bumping `edit_seq`
    /// (plan editor-ui-round-3 WP-2).
    pub epoch: u64,
    /// The index generation links/images were resolved against.
    pub generation: Option<u64>,
    /// The [`ReadingStore`] resolver epoch this was resolved against (plan round-5 WP-2): a
    /// file created/deleted/renamed keeps the index generation, so this marks the Doc stale.
    pub resolver_epoch: u64,
    pub model: RenderModel,
    /// Image `src` → resolved vault path (`None` = unresolved); remote images are absent.
    pub images: HashMap<String, Option<String>>,
    /// The visible runs in render order (WP-R17: the one walk — the view draws them, the
    /// selection copies them, find counts over them).
    pub runs: Vec<VisRun>,
    /// Plan round-6 WP-4: link targets (as [`render::link_targets`] lists them, the string the
    /// view hands to `OpenLink`) that do not resolve — drawn dimmed, menu offers "Create note".
    /// Empty without a daemon (no resolver: nothing is known missing).
    pub unresolved: HashSet<String>,
}

/// One document's Reading state (WP-R15 SP2): everything per note path, so a close is one
/// `remove` and a rename one re-key ([`ReadingStore::remove`], [`ReadingStore::rekey`]).
#[derive(Debug, Clone, Default)]
pub struct DocReading {
    pub mode: ViewMode,
    pub doc: Option<Doc>,
    /// The `edit_seq` seen at the previous tick (debounce).
    seen_seq: Option<u64>,
    /// The pending filename-input edit (absent = shows the stem).
    title: Option<String>,
    /// Plan round-5 WP-1: per-tab (view) place and selection — two tabs on one note keep
    /// their own; keyed by tab id.
    views: HashMap<u64, ViewPlace>,
}

/// One tab's (view's) Reading place on a document (plan round-5 WP-1 "per-tab Reading
/// state"; WP-R15 SP2: the per-(path, view) maps collapsed into one).
#[derive(Debug, Clone, Default)]
struct ViewPlace {
    /// plan editor-ui-round-3 WP-6: the top-level block at the viewport's top, from the last
    /// layout probe (`None` = scrolled to the top / not probed yet).
    top: Option<usize>,
    /// Plan round-8 WP-4: the block under the anchor height, from the last layout probe (only
    /// kept while `anchor > 0`; at 0 it is `top`).
    anchor_at: Option<usize>,
    /// Plan round-8 WP-4: the in-flight `scroll_to` probe is a mode-switch carry — placed under
    /// the anchor height instead of at the top.
    carry: bool,
    /// Plan round-6 WP-1: last sent to an edge by Ctrl+PgUp/PgDn (`true` = the end); a mode
    /// switch snaps the other mode to it. Cleared by the next user scroll.
    edge: Option<bool>,
    /// Plan round-5 WP-7: the Reading selection; never collapsed. Cleared whenever the Doc is
    /// replaced.
    sel: Option<Selection>,
    /// Plan round-5 WP-8: the Source side changed its selection more recently than the Reading
    /// side (Split: the most recent side feeds the action context).
    source_newer: bool,
    /// Plan round-5 WP-1: just (re)activated — the first `on_scroll` (iced reports offset 0
    /// for a re-created scrollable) must re-apply the stored place instead of overwriting it.
    /// Plan round-6 WP-2: armed at an instant and honoured only within [`RESTORE_GUARD`].
    restoring: Option<std::time::Instant>,
}

#[derive(Debug)]
struct ResolverCache {
    generation: Option<u64>,
    resolver: Resolver,
    ids: HashMap<String, i64>,
    paths: HashMap<i64, String>,
}

/// All reading state.
#[derive(Debug, Default)]
pub struct ReadingStore {
    /// WP-R15 (SP2): per-document state, one entry per open note path.
    docs: HashMap<String, DocReading>,
    pub images: ImageCache,
    resolver: Option<ResolverCache>,
    /// Bumped by [`ReadingStore::invalidate_resolver`]; Docs resolved under an older epoch
    /// re-render on the next tick (plan round-5 WP-2).
    resolver_epoch: u64,
    /// Render-model recomputations so far (observability + tests).
    pub recomputes: u64,
    /// Plan round-8-intake-views-anchor WP-4: `editor.toml` `scroll_anchor` as a fraction of
    /// the viewport height (0 = top; set by `App::apply_prefs`).
    anchor: f32,
}

/// The widget id of `path`'s breadcrumb filename input (blur polling).
pub fn title_id(path: &str) -> iced::widget::Id {
    iced::widget::Id::from(format!("reading-title:{path}"))
}

impl ReadingStore {
    /// The store with the default image budget (`CV_IMAGE_CACHE_MB`).
    pub fn new() -> Self {
        Self::default()
    }

    /// A store with an explicit image budget (tests).
    pub fn with_image_budget(bytes: usize) -> Self {
        Self {
            images: ImageCache::with_budget(bytes),
            ..Self::default()
        }
    }

    /// `path`'s document state (created on first use).
    fn entry(&mut self, path: &str) -> &mut DocReading {
        self.docs.entry(path.to_owned()).or_default()
    }

    /// Tab `view`'s place on `path`.
    fn place(&self, path: &str, view: u64) -> Option<&ViewPlace> {
        self.docs.get(path)?.views.get(&view)
    }

    /// Tab `view`'s place on `path`, mutably (created on first use).
    fn place_mut(&mut self, path: &str, view: u64) -> &mut ViewPlace {
        self.entry(path).views.entry(view).or_default()
    }

    /// `path`'s mode.
    pub fn mode(&self, path: &str) -> ViewMode {
        self.docs.get(path).map(|t| t.mode).unwrap_or_default()
    }

    /// The filename input's current text for `path`.
    pub fn title<'a>(&'a self, path: &'a str) -> &'a str {
        self.docs
            .get(path)
            .and_then(|d| d.title.as_deref())
            .unwrap_or_else(|| stem(path))
    }

    /// Takes (clears) `path`'s pending filename edit.
    pub fn take_title(&mut self, path: &str) -> Option<String> {
        self.docs.get_mut(path)?.title.take()
    }

    /// plan editor-ui-round-3 WP-6: the top-level block at the reading viewport's top, from
    /// the last layout probe (`None` = at the top / not probed).
    pub fn top_block(&self, path: &str, view: u64) -> Option<usize> {
        self.place(path, view)?.top
    }

    /// Plan round-8 WP-4: sets the `scroll_anchor` fraction (clamped to 0–1).
    pub fn set_anchor(&mut self, a: f32) {
        self.anchor = if a.is_finite() {
            a.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if self.anchor <= 0.0 {
            for d in self.docs.values_mut() {
                d.views.values_mut().for_each(|v| v.anchor_at = None);
            }
        }
    }

    /// Plan round-8 WP-4: the block under the anchor height (the mode-switch origin); with
    /// anchor 0 (or no anchored probe yet) exactly [`Self::top_block`].
    pub fn anchor_block(&self, path: &str, view: u64) -> Option<usize> {
        if self.anchor > 0.0
            && let Some(b) = self.place(path, view).and_then(|v| v.anchor_at)
        {
            return Some(b);
        }
        self.top_block(path, view)
    }

    /// Plan round-8 WP-4: a mode-switch carry of `block` — probes, then scrolls the block to
    /// the anchor height (the top at anchor 0, exactly as [`probe`] with `Some(block)`).
    pub fn carry_to(
        &mut self,
        path: &str,
        view: u64,
        blocks: usize,
        block: usize,
    ) -> Task<Message> {
        let anchored = self.anchor > 0.0;
        let v = self.place_mut(path, view);
        v.top = Some(block);
        v.carry |= anchored;
        probe(path, view, blocks, Some(block))
    }

    /// plan editor-ui-round-3 WP-6: records the intended top block of a carry (the scroll task
    /// is in flight; its probe confirms it).
    pub fn set_top_block(&mut self, path: &str, view: u64, block: usize) {
        self.place_mut(path, view).top = Some(block);
    }

    /// Plan round-6 WP-1: tab `view` on `path` went to its start (`false`) / end (`true`).
    pub fn set_edge(&mut self, path: &str, view: u64, bottom: bool) {
        self.place_mut(path, view).edge = Some(bottom);
    }

    /// Plan round-6 WP-1: the edge tab `view` on `path` was last sent to, if not scrolled since.
    pub fn edge(&self, path: &str, view: u64) -> Option<bool> {
        self.place(path, view)?.edge
    }

    /// Plan round-6 WP-1: a user scroll left the edge.
    pub fn clear_edge(&mut self, path: &str, view: u64) {
        if let Some(v) = self.docs.get_mut(path).and_then(|d| d.views.get_mut(&view)) {
            v.edge = None;
        }
    }

    /// plan editor-ui-round-3 WP-6: the reading view is (being scrolled) at its top.
    pub fn clear_top_block(&mut self, path: &str, view: u64) {
        if let Some(v) = self.docs.get_mut(path).and_then(|d| d.views.get_mut(&view)) {
            v.top = None;
        }
    }

    /// Plan round-5 WP-1: tab `view` showing `path` was (re)activated — restores its stored
    /// top block (probe → `scroll_to`), and arms the guard so the re-created scrollable's first
    /// `on_scroll` (offset 0) re-applies it instead of overwriting it. `None` when the tab has
    /// no stored place (it starts at the top anyway) or shows no reading view.
    pub fn restore(&mut self, path: &str, view: u64) -> Option<Task<Message>> {
        if !self.mode(path).shows_reading() {
            return None;
        }
        let b = self.top_block(path, view)?;
        let blocks = self.doc(path)?.model.blocks.len();
        self.place_mut(path, view).restoring = Some(std::time::Instant::now());
        Some(probe(path, view, blocks, Some(b)))
    }

    /// Plan round-5 WP-1: the restore guard of tab `view` is armed.
    pub fn is_restoring(&self, view: u64) -> bool {
        self.docs
            .values()
            .filter_map(|d| d.views.get(&view)?.restoring)
            .any(|at| at.elapsed() < RESTORE_GUARD)
    }

    /// Test hook: backdates tab `view`'s restore guard by `by`.
    #[cfg(test)]
    pub(crate) fn age_restore_guard(&mut self, view: u64, by: std::time::Duration) {
        for d in self.docs.values_mut() {
            if let Some(at) = d.views.get_mut(&view).and_then(|v| v.restoring.as_mut()) {
                *at = at.checked_sub(by).unwrap_or(*at);
            }
        }
    }

    /// Test hook: `path`'s rendered doc, mutably.
    #[cfg(test)]
    pub(crate) fn doc_mut(&mut self, path: &str) -> Option<&mut Doc> {
        self.docs.get_mut(path)?.doc.as_mut()
    }

    /// Tests (WP-R15 SP2): per-document Reading entries held (open documents + leaks).
    #[cfg(test)]
    pub(crate) fn doc_entries(&self) -> usize {
        self.docs.len()
    }

    pub fn doc(&self, path: &str) -> Option<&Doc> {
        self.docs.get(path).and_then(|t| t.doc.as_ref())
    }

    /// Plan round-5 WP-7: tab `view`'s Reading selection of `path`.
    pub fn selection(&self, path: &str, view: u64) -> Option<Selection> {
        self.place(path, view)?.sel
    }

    /// Sets (or clears: `None` or collapsed) tab `view`'s Reading selection of `path`.
    pub fn set_selection(&mut self, path: &str, view: u64, sel: Option<Selection>) {
        let sel = sel.filter(|s| !s.is_empty());
        let v = self.place_mut(path, view);
        if sel.is_some() {
            v.source_newer = false;
        }
        v.sel = sel;
    }

    /// Ctrl+A: selects `path`'s whole rendered document in tab `view`; false when nothing is
    /// rendered.
    pub fn select_all(&mut self, path: &str, view: u64) -> bool {
        let all = self.doc(path).and_then(|d| selection::all(&d.runs));
        self.set_selection(path, view, all);
        self.selection(path, view).is_some()
    }

    /// Plan round-5 WP-7 (the WP-8 accessor): the copy text of tab `view`'s Reading selection
    /// of `path` — `None` when nothing (or only an empty range) is selected.
    pub fn selection_text(&self, path: &str, view: u64) -> Option<String> {
        let sel = self.selection(path, view)?;
        let text = selection::copy_text(&self.doc(path)?.runs, sel);
        (!text.is_empty()).then_some(text)
    }

    /// Plan round-5 WP-8: the Source side of tab `view` of `path` changed its selection (it is
    /// now the most recent side in Split).
    pub fn note_source_selection(&mut self, path: &str, view: u64) {
        self.place_mut(path, view).source_newer = true;
    }

    /// Plan round-5 WP-8: true when the Source side selected more recently than the Reading
    /// side of tab `view` of `path`.
    pub fn source_selected_last(&self, path: &str, view: u64) -> bool {
        self.place(path, view).is_some_and(|v| v.source_newer)
    }

    /// Plan round-5 WP-8: the vault path a link `target` in `src` resolves to (the
    /// [`ReadingMsg::OpenLink`] resolver; `None` for remote / unresolved / no daemon).
    pub fn resolve_link(
        &mut self,
        src: &str,
        target: &str,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) -> Option<String> {
        if render::is_remote(target) {
            return None;
        }
        let d = daemon?;
        self.refresh_resolver(d.as_ref());
        let target = target.split('#').next().unwrap_or(target);
        let target = percent_decode(target);
        // `[[#h]]` resolves to the note itself; keep it (focuses the same tab).
        self.resolve(src, &target)
    }

    /// Drops every tab's selection of `path` (its Doc was replaced or dropped).
    fn clear_selections(&mut self, path: &str) {
        if let Some(d) = self.docs.get_mut(path) {
            d.views.values_mut().for_each(|v| v.sel = None);
        }
    }

    /// The last tab of `path` closed: forget its state (one `remove`) and release its image
    /// pins (E2).
    pub fn remove(&mut self, path: &str) {
        self.docs.remove(path);
        self.images.release_owner(path);
    }

    /// `from` was renamed to `to`: its state moves (one re-key); the `from` image pins are
    /// released (the next render re-pins under `to`).
    pub fn rekey(&mut self, from: &str, to: &str) {
        if let Some(mut d) = self.docs.remove(from) {
            d.title = None;
            self.docs.insert(to.to_owned(), d);
        }
        self.images.release_owner(from);
    }

    /// A filename edit is pending (the breadcrumb's input holds unsubmitted text); the Escape
    /// listener runs then, in any view mode.
    pub fn title_pending(&self) -> bool {
        self.docs.values().any(|d| d.title.is_some())
    }

    /// The tick runs: some tab shows a reading pane (render debounce) or a filename edit is
    /// pending (the breadcrumb's blur poll) — in any view mode, Source-only included (plan
    /// editor-feedback-round-2 WP-6).
    pub fn ticks(&self) -> bool {
        self.title_pending() || self.docs.values().any(|t| t.mode.shows_reading())
    }

    /// The debounce / blur-poll tick, plus the filename editor's Escape (see [`Self::ticks`],
    /// [`Self::title_pending`]). Ctrl+E is no longer listened to here (plan
    /// editor-settings-tab WP-4, audit E1): "Toggle source / reading view" is an ordinary
    /// client command resolved by the effective keymap in `App::on_chord`, so it can be
    /// rebound.
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = Vec::new();
        if self.title_pending() {
            // The focused text input captures Escape, so listen to captured events too.
            subs.push(iced::event::listen_with(escape));
        }
        if self.ticks() {
            subs.push(iced::time::every(TICK).map(|_| Message::Reading(ReadingMsg::Tick)));
        }
        Subscription::batch(subs)
    }

    /// A tab opened in `mode`: creates its entry and, when it shows a reading pane, renders it
    /// now (`render_now`, the open path) or on the next debounce tick (the restore path, so
    /// startup does not render every restored tab inline). Audit E1: no tab is left at
    /// "Rendering…" without a Ctrl+E.
    pub fn register(
        &mut self,
        doc: &SourceBuffer,
        mode: ViewMode,
        render_now: bool,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) {
        if render_now {
            self.set_mode(doc, mode, daemon);
        } else {
            self.entry(doc.path()).mode = mode;
        }
    }

    fn set_mode(
        &mut self,
        doc: &SourceBuffer,
        mode: ViewMode,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) {
        let entry = self.entry(doc.path());
        entry.mode = mode;
        if mode.shows_reading() {
            if entry.doc.is_none() {
                self.recompute(doc, daemon);
            }
        } else {
            // Leaving reading/split: drop the model, places, selections and image pins (E2).
            entry.doc = None;
            entry.seen_seq = None;
            entry.views.values_mut().for_each(|v| {
                v.top = None;
                v.sel = None;
            });
            self.images.release_owner(doc.path());
        }
    }

    /// Re-lowers `doc`'s buffer, fills transclusions (depth 1), resolves images, and releases
    /// image pins the new model no longer references (E2).
    pub fn recompute(&mut self, doc: &SourceBuffer, daemon: Option<&Arc<dyn DaemonClient>>) {
        self.recomputes += 1;
        let seq = doc.edit_seq();
        if let Some(d) = daemon {
            self.refresh_resolver(d.as_ref());
        }
        let generation = daemon.and_then(|d| d.index().generation());
        let mut model = render::lower(&doc.text());
        let mut fetch = |target: &str| {
            let d = daemon?;
            let path = self.resolve(doc.path(), target)?;
            let (bytes, _) = d.read_file(&path).ok()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        };
        render::transclude(&mut model, &mut fetch);
        let images: HashMap<String, Option<String>> = render::images(&model)
            .into_iter()
            .filter(|(_, remote)| !remote)
            .map(|(src, _)| {
                let target = percent_decode(&src);
                let key = self.resolve(doc.path(), &target);
                (src, key)
            })
            .collect();
        let unresolved: HashSet<String> = if daemon.is_some() {
            render::link_targets(&model)
                .into_iter()
                .filter(|t| {
                    let bare = t.split('#').next().unwrap_or(t);
                    self.resolve(doc.path(), &percent_decode(bare)).is_none()
                })
                .collect()
        } else {
            HashSet::new()
        };
        let keep: HashSet<String> = images.values().flatten().cloned().collect();
        if let Some(d) = daemon {
            // Plan round-5 WP-2: header-read sizes so placeholders take the image's height.
            for key in &keep {
                self.images.note_dims(key, d.vault_root());
            }
        }
        self.images.retain_owner(doc.path(), &keep);
        // Plan round-5 WP-7: run indices / byte offsets shift with a new Doc.
        self.clear_selections(doc.path());
        let runs = editor_model::visible::visible_runs(&model);
        let resolver_epoch = self.resolver_epoch;
        let entry = self.entry(doc.path());
        entry.seen_seq = Some(seq);
        entry.doc = Some(Doc {
            seq,
            epoch: doc.epoch(),
            generation,
            resolver_epoch,
            model,
            images,
            runs,
            unresolved,
        });
    }

    /// Debounce: re-render `doc` once its `edit_seq` held still for a tick (or the index moved).
    fn tick(&mut self, doc: &SourceBuffer, daemon: Option<&Arc<dyn DaemonClient>>) {
        let generation = daemon.and_then(|d| d.index().generation());
        let Some(entry) = self.docs.get_mut(doc.path()) else {
            return;
        };
        if !entry.mode.shows_reading() {
            return;
        }
        let seq = doc.edit_seq();
        let resolver_epoch = self.resolver_epoch;
        let stale = match &entry.doc {
            None => true,
            Some(d) => {
                d.seq != seq
                    || d.epoch != doc.epoch()
                    || d.generation != generation
                    || d.resolver_epoch != resolver_epoch
            }
        };
        if !stale {
            return;
        }
        // A reload (epoch moved) or a vault membership change is a settled replacement: render
        // it now, no debounce.
        let reloaded = entry
            .doc
            .as_ref()
            .is_some_and(|d| d.epoch != doc.epoch() || d.resolver_epoch != resolver_epoch);
        if reloaded || entry.seen_seq == Some(seq) || entry.doc.is_none() {
            self.recompute(doc, daemon);
        } else {
            entry.seen_seq = Some(seq);
        }
    }

    /// Drops the cached resolver: a file was created, deleted or renamed. Incremental indexing
    /// keeps the generation, so a generation-keyed cache alone never sees new files (a
    /// localized attachment written after the first render stayed unresolved = broken image).
    /// Plan round-5 WP-2: it also marks every rendered Doc stale (a Doc rendered before its
    /// attachment was resolvable re-renders with the image on the next tick).
    pub fn invalidate_resolver(&mut self) {
        self.resolver = None;
        self.resolver_epoch += 1;
    }

    /// Plan round-6 WP-2: vault file `path` changed (any kind) — only its own header-read size
    /// is dropped; every other image keeps its placeholder size across resolver invalidation.
    pub fn file_changed(&mut self, path: &str) {
        self.images.forget_dims_of(path);
    }

    fn refresh_resolver(&mut self, d: &dyn DaemonClient) {
        let generation = d.index().generation();
        if self
            .resolver
            .as_ref()
            .is_some_and(|r| r.generation == generation && generation.is_some())
        {
            return;
        }
        // One scan (the per-folder `subfolders` walk was quadratic over folders).
        let rows = d.index().with_query(|q| q.file_paths());
        match rows {
            Ok(rows) => {
                let mut resolver = Resolver::default();
                let mut ids = HashMap::new();
                let mut paths = HashMap::new();
                for (id, path) in rows {
                    resolver.add(id, &path);
                    ids.insert(path.clone(), id);
                    paths.insert(id, path);
                }
                self.resolver = Some(ResolverCache {
                    generation,
                    resolver,
                    ids,
                    paths,
                });
            }
            Err(e) => tracing::warn!(error = %e, "reading: resolver rebuild failed"),
        }
    }

    /// Resolves `target` written in `src` to a vault path.
    fn resolve(&self, src: &str, target: &str) -> Option<String> {
        let rc = self.resolver.as_ref()?;
        let src_id = rc.ids.get(src).copied().unwrap_or(-1);
        let id = rc.resolver.resolve(src, src_id, target)?;
        rc.paths.get(&id).cloned()
    }

    /// The message a link click produces: `TabsMsg::Open(path)` when it resolves, else `None`
    /// (unresolved links and URLs with a scheme do not open).
    pub fn click(
        &mut self,
        src: &str,
        target: &str,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) -> Option<Message> {
        self.resolve_link(src, target, daemon)
            .map(|p| Message::Tabs(TabsMsg::Open(p)))
    }

    /// `tab` shows image `key`: pins it, and starts an off-thread load on a miss.
    fn image_shown(
        &mut self,
        tab: &str,
        view: u64,
        key: &str,
        width: f32,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) -> Task<Message> {
        // Only keys this tab's current model resolved are loadable (remote URLs never are).
        let known = self
            .doc(tab)
            .is_some_and(|d| d.images.values().flatten().any(|k| k == key));
        if !known || !self.mode(tab).shows_reading() {
            return Task::none();
        }
        let Some(d) = daemon else {
            return Task::none();
        };
        if self.images.show(&image_cache::view_owner(tab, view), key) {
            image_cache::load_task(
                Arc::clone(d),
                key.to_owned(),
                image_cache::decode_width(width),
                |key, result| Message::Reading(ReadingMsg::ImageLoaded { key, result }),
            )
        } else {
            Task::none()
        }
    }
}

fn escape(e: iced::Event, _: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    match e {
        iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            ..
        }) => Some(Message::Reading(ReadingMsg::TitleRevert)),
        _ => None,
    }
}

/// What [`update`] asks the app to do (WP-R15 SP5: the one effect shape `App` drains, as
/// `views::Effect`; replaces the store's outbox fields).
pub enum Effect {
    /// Run this task (probes, scrolls, image loads, clipboard, a resolved link's open).
    Task(Task<Message>),
    /// plan round-5 WP-3: a status-bar notice.
    Notice(crate::app::Severity, String),
    /// Plan round-6 WP-4: an unresolved vault link `target` was clicked; `App` warns or
    /// creates it per `editor.toml` `missing_link`.
    MissingLink(String),
}

/// The reading update.
pub fn update(
    store: &mut ReadingStore,
    msg: ReadingMsg,
    tabs: &[&SourceBuffer],
    active: Option<&SourceBuffer>,
    daemon: Option<&Arc<dyn DaemonClient>>,
) -> Vec<Effect> {
    let mut fx = Vec::new();
    let task = update_store(store, msg, tabs, active, daemon, &mut fx);
    fx.push(Effect::Task(task));
    fx
}

fn update_store(
    store: &mut ReadingStore,
    msg: ReadingMsg,
    tabs: &[&SourceBuffer],
    active: Option<&SourceBuffer>,
    daemon: Option<&Arc<dyn DaemonClient>>,
    fx: &mut Vec<Effect>,
) -> Task<Message> {
    match msg {
        ReadingMsg::CycleMode => {
            if let Some(tab) = active {
                let next = store.mode(tab.path()).toggle();
                store.set_mode(tab, next, daemon);
            }
        }
        ReadingMsg::SetMode { path, mode } => {
            if let Some(tab) = tabs.iter().find(|t| t.path() == path) {
                store.set_mode(tab, mode, daemon);
            }
        }
        ReadingMsg::Scrolled { path, view, y } => {
            // Plan round-6 WP-1: a report away from the recorded edge (never the re-created
            // scrollable's first report, nor a snap landing on the edge) clears it. NaN (no
            // scroll range) keeps it.
            if !store.is_restoring(view)
                && let Some(bottom) = store.edge(&path, view)
                && (if bottom { y < 0.999 } else { y > 0.001 })
            {
                store.clear_edge(&path, view);
            }
            if let Some(blocks) = store.doc(&path).map(|d| d.model.blocks.len()) {
                // Plan round-5 WP-1: the first scroll report after a (re)activation is the
                // re-created scrollable's offset 0 — re-apply the stored place instead.
                // Plan round-6 WP-2: an expired guard is dropped without re-applying.
                let v = store.place_mut(&path, view);
                let again = v
                    .restoring
                    .take()
                    .is_some_and(|at| at.elapsed() < RESTORE_GUARD)
                    .then_some(v.top)
                    .flatten();
                return probe(&path, view, blocks, again);
            }
        }
        ReadingMsg::Probed {
            path,
            view,
            probe,
            scroll_to,
        } => {
            let Some(p) = probe else {
                return Task::none();
            };
            let anchor = store.anchor;
            let v = store.place_mut(&path, view);
            match scroll_to {
                Some(b) => {
                    v.top = Some(b);
                    let carry = std::mem::take(&mut v.carry);
                    if let Some(y) = p.block_offset(b) {
                        // Plan round-8 WP-4: a carry puts the block under the anchor height,
                        // clamped to the scrollable's range here (anchor 0 = today's `y`).
                        let y = if carry && anchor > 0.0 {
                            v.anchor_at = Some(b);
                            p.anchored_offset(y, anchor)
                        } else {
                            y
                        };
                        return ScrollIntent::ScrollTo { path, view, y }.task();
                    }
                }
                None => match p.anchor_block(0.0) {
                    Some(b) => {
                        if anchor > 0.0
                            && let Some(a) = p.anchor_block(anchor)
                        {
                            v.anchor_at = Some(a);
                        }
                        v.top = Some(b);
                    }
                    None => {
                        v.anchor_at = None;
                        v.top = None;
                    }
                },
            }
        }
        ReadingMsg::Tick => {
            for tab in tabs {
                store.tick(tab, daemon);
            }
            // Blur poll: a pending filename edit whose input lost focus is submitted.
            let polls: Vec<Task<Message>> = store
                .docs
                .iter()
                .filter(|(_, d)| d.title.is_some())
                .map(|(p, _)| {
                    let path = p.clone();
                    iced::widget::operation::is_focused(title_id(p)).map(move |focused| {
                        Message::Reading(ReadingMsg::TitleFocus {
                            path: path.clone(),
                            focused,
                        })
                    })
                })
                .collect();
            return Task::batch(polls);
        }
        ReadingMsg::TitleInput { path, value } => {
            store.entry(&path).title = Some(value);
        }
        ReadingMsg::TitleRevert => store.docs.values_mut().for_each(|d| d.title = None),
        ReadingMsg::Select { path, view, sel } => store.set_selection(&path, view, sel),
        // Plan round-5 WP-7: a finished selection gesture goes to PRIMARY (as Source does).
        ReadingMsg::SelectDone { path, view } => {
            if let Some(text) = store.selection_text(&path, view) {
                return iced::clipboard::write_primary(text);
            }
        }
        // Buffer edits and renames need the app (tabs, explorer, saves): handled in `App`.
        ReadingMsg::ToggleTask { .. }
        | ReadingMsg::ToggleProp { .. }
        | ReadingMsg::GoToEdge { .. }
        | ReadingMsg::TitleSubmit { .. }
        | ReadingMsg::TitleFocus { .. } => {}
        ReadingMsg::OpenLink { src, target } => {
            if let Some(m) = store.click(&src, &target, daemon) {
                return Task::done(m);
            }
            // plan round-5 WP-3: an unresolved vault link says so instead of doing nothing;
            // plan round-6 WP-4: `App` warns or creates it per `missing_link`.
            if daemon.is_some() && !render::is_remote(&target) {
                fx.push(Effect::MissingLink(target));
            }
        }
        ReadingMsg::ImageShown {
            tab,
            view,
            key,
            width,
        } => {
            return store.image_shown(&tab, view, &key, width, daemon);
        }
        ReadingMsg::ImageHidden { tab, view, key } => {
            store
                .images
                .hide(&image_cache::view_owner(&tab, view), &key);
        }
        ReadingMsg::ImageLoaded { key, result } => {
            let failed = result.as_ref().err().cloned();
            store.images.loaded(&key, result.ok());
            if let Some(e) = failed {
                tracing::debug!(error = %e, "image load failed");
                fx.push(Effect::Notice(
                    crate::app::Severity::Warning,
                    image_failure_text(&key, &e),
                ));
            }
        }
    }
    Task::none()
}

/// The image-failure notice: the image's basename and a one-line, short reason.
pub fn image_failure_text(key: &str, err: &str) -> String {
    let base = key.rsplit('/').next().unwrap_or(key);
    let reason: String = err.lines().next().unwrap_or("").chars().take(60).collect();
    format!("image failed to load: {base} ({reason})")
}

/// The active tab's body: source, reading, or split (source left, reading right). `view` is
/// the showing tab's id (per-view image pins, plan editor-feedback-round-2 WP-7). `find` is the
/// focused pane's find bar when this is its active tab: both sides derive their find paint from
/// it at view time (WP-R17 SP6).
#[allow(clippy::too_many_arguments)] // one render site; a context struct would only rename these
pub fn tab_body<'a>(
    store: &'a ReadingStore,
    views: &'a crate::views::ViewStore,
    doc: &'a SourceBuffer,
    view: u64,
    t: Tokens,
    font_size: f32,
    keymap: &'a crate::palette::Keymap,
    focused: bool,
    find: Option<&'a crate::find::FindBar>,
    link: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    use iced::Length;
    use iced::widget::{container, row};
    // plan editor-settings-tab WP-2 (audit M1): `editor.toml` `font_size` drives both panes.
    let scale = view::reading_scale(font_size);
    let paint = find.and_then(crate::find::FindBar::reading_paint);
    let reading = || view::reading(store, views, doc.path(), view, t, scale, focused, paint);
    match store.mode(doc.path()) {
        ViewMode::Source => crate::source::view(doc, t, font_size, keymap, find, link),
        ViewMode::Reading => reading(),
        ViewMode::Split => row![
            container(crate::source::view(doc, t, font_size, keymap, find, link))
                .width(Length::FillPortion(1)),
            container(reading()).width(Length::FillPortion(1)),
        ]
        .spacing(12)
        .into(),
    }
}

/// plan editor-ui-round-3 WP-6: the widget id of top-level reading block `i` of `path` in tab
/// `view` (plan round-5 WP-1: per tab, so a probe never mixes two panes showing one note).
pub fn block_id(path: &str, view: u64, i: usize) -> iced::widget::Id {
    iced::widget::Id::from(format!("reading-block:{view}:{path}:{i}"))
}

#[cfg(test)]
mod selection_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wp8_tests;
