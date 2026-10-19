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

pub mod selection;
pub mod view;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use cv_index::Resolver;
use editor_model::render::{self, Block, Inline, RenderModel};
use iced::{Element, Subscription, Task};

use crate::daemon::DaemonClient;
use crate::image_cache::{self, ImageCache};
use crate::message::{Message, ReadingMsg, TabsMsg};
use crate::source::SourceBuffer;
use crate::theme::Tokens;
use crate::widget::selectable::SelPos;

/// Recompute debounce: a change is rendered once `edit_seq` has been stable for one tick.
pub const TICK: Duration = Duration::from_millis(250);
/// Plan round-6 WP-2: how long an armed restore guard waits for the re-created scrollable's
/// first scroll report (it arrives within a frame or two of the activation).
pub const RESTORE_GUARD: Duration = Duration::from_millis(1000);

/// A tab's display mode (new tabs open in Reading; plan editor-cutover-blockers).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

impl From<crate::ui_state::Layout> for ViewMode {
    fn from(l: crate::ui_state::Layout) -> Self {
        use crate::ui_state::Layout;
        match l {
            Layout::Source => Self::Source,
            Layout::Reading => Self::Reading,
            Layout::Split => Self::Split,
        }
    }
}

impl From<ViewMode> for crate::ui_state::Layout {
    fn from(m: ViewMode) -> Self {
        match m {
            ViewMode::Source => Self::Source,
            ViewMode::Reading => Self::Reading,
            ViewMode::Split => Self::Split,
        }
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
    /// Plan round-5 WP-7: the selectable runs in render order (copy text, select-all).
    pub runs: Vec<selection::RunInfo>,
    /// Plan round-6 WP-4: link targets (as [`render::link_targets`] lists them, the string the
    /// view hands to `OpenLink`) that do not resolve — drawn dimmed, menu offers "Create note".
    /// Empty without a daemon (no resolver: nothing is known missing).
    pub unresolved: HashSet<String>,
}

/// One tab's reading state.
#[derive(Debug, Clone, Default)]
pub struct TabReading {
    pub mode: ViewMode,
    pub doc: Option<Doc>,
    /// The `edit_seq` seen at the previous tick (debounce).
    seen_seq: Option<u64>,
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
    /// plan round-5 WP-3: a notice for the status bar, taken by `App` after each update.
    pub notice: Option<(crate::app::Severity, String)>,
    /// Plan round-6 WP-4: an unresolved vault link click `(src, target)`, taken by `App`, which
    /// warns or creates per `editor.toml` `missing_link`.
    pub unresolved_click: Option<(String, String)>,
    tabs: HashMap<String, TabReading>,
    pub images: ImageCache,
    resolver: Option<ResolverCache>,
    /// Bumped by [`ReadingStore::invalidate_resolver`]; Docs resolved under an older epoch
    /// re-render on the next tick (plan round-5 WP-2).
    resolver_epoch: u64,
    /// Render-model recomputations so far (observability + tests).
    pub recomputes: u64,
    /// Pending filename-input edits, keyed by tab path (absent = shows the stem).
    titles: HashMap<String, String>,
    /// The open find bar's query per tab path: its occurrences are painted in the reading view
    /// (plan editor-feedback-round-2 WP-4). Render blocks carry source line ranges
    /// (`RenderModel::block_lines`, plan editor-ui-round-3 WP-5) at block granularity only, so
    /// matches are still found in the rendered text.
    find: HashMap<String, String>,
    /// plan editor-ui-round-3 WP-6: the top-level block at the reading viewport's top per tab
    /// path, from the last layout probe (absent = scrolled to the top / not probed yet).
    /// Plan round-5 WP-1: keyed by (path, tab id) — two tabs on one note keep their own place.
    top: HashMap<(String, u64), usize>,
    /// Plan round-8-intake-views-anchor WP-4: `editor.toml` `scroll_anchor` as a fraction of
    /// the viewport height (0 = top; set by `App::apply_prefs`).
    anchor: f32,
    /// Plan round-8 WP-4: the block under the anchor height per tab, from the last layout
    /// probe (only kept while `anchor > 0`; at 0 it is [`Self::top_block`]).
    anchor_at: HashMap<(String, u64), usize>,
    /// Plan round-8 WP-4: tabs whose in-flight `scroll_to` probe is a mode-switch carry —
    /// placed under the anchor height instead of at the top.
    carry: HashSet<(String, u64)>,
    /// Plan round-5 WP-1: tabs just (re)activated whose first `on_scroll` (iced reports offset
    /// 0 for a re-created scrollable) must re-apply the stored place instead of overwriting it.
    /// Plan round-6 WP-2: armed at an instant and honoured only within [`RESTORE_GUARD`] — iced
    /// reports scrolling only when the content overflows, so an unexpired guard could otherwise
    /// snap the view on a much later user wheel event.
    restoring: HashMap<u64, std::time::Instant>,
    /// Plan round-6 WP-1: tabs (path, tab id) last sent to an edge by Ctrl+PgUp/PgDn (`true` =
    /// the end); a mode switch snaps the other mode to it. Cleared by the next user scroll.
    edge: HashMap<(String, u64), bool>,
    /// plan editor-ui-round-3 WP-5: the active find match per tab path, as (top-level render
    /// block, nth query occurrence inside it).
    find_active: HashMap<String, (usize, usize)>,
    /// Plan round-5 WP-7: the Reading selection `(anchor, focus)` per (path, tab id); never
    /// collapsed (an empty selection is absent). Cleared whenever the path's Doc is replaced.
    sel: HashMap<(String, u64), (SelPos, SelPos)>,
    /// Plan round-5 WP-8: (path, tab id) pairs whose Source side changed its selection more
    /// recently than the Reading side (Split: the most recent side feeds the action context).
    source_newer: std::collections::HashSet<(String, u64)>,
}

/// The byte offset where 0-based `line` starts in `text` (`text.len()` past the end).
fn line_start(text: &str, line: usize) -> usize {
    if line == 0 {
        return 0;
    }
    text.match_indices('\n')
        .nth(line - 1)
        .map_or(text.len(), |(i, _)| i + 1)
}

/// A wiki-link's displayed label (alias, else `target#fragment`).
pub fn link_label(target: &str, fragment: Option<&String>, alias: Option<&String>) -> String {
    match (alias, fragment) {
        (Some(a), _) => a.clone(),
        (None, Some(f)) if target.is_empty() => format!("#{f}"),
        (None, Some(f)) => format!("{target}#{f}"),
        (None, None) => target.to_owned(),
    }
}

/// plan editor-round-4 WP-3: the visible (painted) text of an inline, if any — text, code
/// spans, wiki-link/link labels and tags. URLs and link targets are never displayed.
pub fn inline_visible(i: &Inline) -> Option<std::borrow::Cow<'_, str>> {
    use std::borrow::Cow;
    match i {
        Inline::Text { text, .. } | Inline::Code(text) => Some(Cow::Borrowed(text)),
        Inline::WikiLink {
            target,
            fragment,
            alias,
        } => Some(Cow::Owned(link_label(
            target,
            fragment.as_ref(),
            alias.as_ref(),
        ))),
        Inline::Link { label, .. } => Some(Cow::Borrowed(label)),
        Inline::Tag(t) => Some(Cow::Owned(format!("#{t}"))),
        Inline::FootnoteRef { .. } | Inline::LineBreak => None,
    }
}

/// plan editor-round-4 WP-3: the find-painted text segments of block `b` in paint order (the
/// order `view::block` draws them). Matches never cross a segment boundary.
pub fn visible_segments(b: &Block) -> Vec<String> {
    fn inl(out: &mut Vec<String>, is: &[Inline]) {
        out.extend(is.iter().filter_map(inline_visible).map(|c| c.into_owned()));
    }
    fn go(out: &mut Vec<String>, b: &Block) {
        match b {
            Block::Heading { inlines, .. } | Block::Paragraph(inlines) => inl(out, inlines),
            Block::List { items, .. } => {
                for it in items {
                    it.children.iter().for_each(|c| go(out, c));
                }
            }
            Block::Table { header, rows, .. } => {
                header.iter().for_each(|c| inl(out, c));
                for r in rows {
                    r.iter().for_each(|c| inl(out, c));
                }
            }
            Block::Callout { children, .. } | Block::BlockQuote(children) => {
                children.iter().for_each(|c| go(out, c));
            }
            Block::CodeBlock { text, .. } => out.push(text.clone()),
            Block::Transclusion {
                content: Some(bs), ..
            } => bs.iter().for_each(|c| go(out, c)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    go(&mut out, b);
    out
}

/// plan editor-round-4 WP-3: every visible occurrence of `q` in the reading view, in order, as
/// (top-level block, nth occurrence inside it) — the Reading-mode find bar's match list.
pub fn visible_occurrences(model: &RenderModel, q: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (i, b) in model.blocks.iter().enumerate() {
        let n: usize = visible_segments(b)
            .iter()
            .map(|s| editor_model::find::matches(s, q).len())
            .sum();
        out.extend((0..n).map(|k| (i, k)));
    }
    out
}

/// plan editor-round-4 WP-3: byte ranges of a block's source that Reading never displays —
/// md-link destinations `](url "title")`, image `![alt](src)` wholes, wiki-link targets hidden
/// by an alias (`[[target|`), `[[`/`]]` brackets, embeds `![[…]]` and code-fence lines.
pub fn invisible_ranges(src: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    // Code-fence lines (the info string is not painted; the fence is markup).
    let mut at = 0;
    for line in src.split_inclusive('\n') {
        let t = line.trim_start();
        let t = t.trim_start_matches(['>', ' ']);
        if t.starts_with("```") || t.starts_with("~~~") {
            out.push(at..at + line.len());
        }
        at += line.len();
    }
    // Wiki-links and embeds.
    let mut i = 0;
    while let Some(off) = src[i..].find("[[") {
        let s = i + off;
        let Some(len) = src[s..].find("]]") else {
            break;
        };
        let e = s + len + 2;
        let embed = s > 0 && bytes[s - 1] == b'!';
        if embed {
            out.push(s - 1..e);
        } else {
            match src[s..e].find('|') {
                Some(p) => out.push(s..s + p + 1),
                None => out.push(s..s + 2),
            }
            out.push(e - 2..e);
        }
        i = e;
    }
    // Md-link destinations (and whole images).
    let mut i = 0;
    while let Some(off) = src[i..].find("](") {
        let s = i + off;
        let mut depth = 0usize;
        let mut end = src.len();
        for (k, c) in src[s + 1..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = s + 1 + k + 1;
                        break;
                    }
                }
                '\n' => {
                    end = s + 1 + k;
                    break;
                }
                _ => {}
            }
        }
        let open = src[..s].rfind('[');
        match open {
            Some(o) if o > 0 && bytes[o - 1] == b'!' => out.push(o - 1..end),
            _ => out.push(s..end),
        }
        i = end.max(s + 2);
    }
    out
}

/// plan editor-round-4 WP-3: the rendered counterpart (top-level block, nth visible
/// occurrence) of the source match `r` of `q` in `text`; `None` = outside every block, or an
/// invisible match (no rendered counterpart).
pub fn source_counterpart(
    model: &RenderModel,
    text: &str,
    q: &str,
    r: &std::ops::Range<usize>,
) -> Option<(usize, usize)> {
    let (line, _) = editor_model::find::line_col(text, r.start);
    let b = model.block_at_line(line)?;
    let lines = &model.block_lines[b];
    let from = line_start(text, lines.start);
    let to = line_start(text, lines.end).max(from);
    let src = &text[from..to];
    let rel = r.start.checked_sub(from)?;
    let hidden = invisible_ranges(src);
    let visible =
        |m: &std::ops::Range<usize>| !hidden.iter().any(|h| m.start < h.end && h.start < m.end);
    let ms = editor_model::find::matches(src, q);
    let idx = ms.iter().position(|m| m.start >= rel)?;
    if !visible(&ms[idx]) {
        return None;
    }
    Some((b, ms[..idx].iter().filter(|m| visible(m)).count()))
}

/// The note stem shown in the breadcrumb filename input (`a/b/Note.md` → `Note`).
pub fn stem(path: &str) -> &str {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    leaf.strip_suffix(".md").unwrap_or(leaf)
}

/// The rename target for a submitted filename: `None` when unchanged or empty.
pub fn rename_target(path: &str, input: &str) -> Option<String> {
    let input = input.trim();
    let input = input.strip_suffix(".md").unwrap_or(input);
    if input.is_empty() || input == stem(path) {
        return None;
    }
    let ext = if path.ends_with(".md") { ".md" } else { "" };
    Some(match path.rsplit_once('/') {
        Some((dir, _)) => format!("{dir}/{input}{ext}"),
        None => format!("{input}{ext}"),
    })
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

    /// `path`'s mode.
    pub fn mode(&self, path: &str) -> ViewMode {
        self.tabs.get(path).map(|t| t.mode).unwrap_or_default()
    }

    /// Sets (or clears, `None`/empty) the find query painted in `path`'s reading view.
    pub fn set_find(&mut self, path: &str, query: Option<String>) {
        match query.filter(|q| !q.is_empty()) {
            Some(q) => {
                self.find.insert(path.to_owned(), q);
            }
            None => {
                self.find.remove(path);
                self.find_active.remove(path);
            }
        }
    }

    /// Sets (or clears) the active find match for `path`'s reading view from the current match's
    /// source byte range `r` in `text` (Source/Split): the top-level block containing its first
    /// line (block-granularity provenance, `RenderModel::block_lines`), and its rendered
    /// counterpart's ordinal = the number of *visible* source matches before it in that block's
    /// source lines (plan editor-round-4 WP-3). When the active source match is itself
    /// invisible ([`invisible_ranges`]: a URL, link target, markup) it has no rendered
    /// counterpart and nothing is drawn strong (audit E2).
    pub fn set_find_active(&mut self, path: &str, text: &str, r: Option<std::ops::Range<usize>>) {
        let hit = r.and_then(|r| {
            let q = self.find.get(path)?;
            let model = &self.tabs.get(path)?.doc.as_ref()?.model;
            source_counterpart(model, text, q, &r)
        });
        self.set_find_active_at(path, hit);
    }

    /// Sets (or clears) the active find occurrence of `path`'s reading view directly (Reading
    /// mode steps through visible occurrences, plan editor-round-4 WP-3).
    pub fn set_find_active_at(&mut self, path: &str, hit: Option<(usize, usize)>) {
        match hit {
            Some(h) => {
                self.find_active.insert(path.to_owned(), h);
            }
            None => {
                self.find_active.remove(path);
            }
        }
    }

    /// The active find match of `path`'s reading view (block, nth occurrence in it).
    pub fn find_active(&self, path: &str) -> Option<(usize, usize)> {
        self.find_active.get(path).copied()
    }

    /// The find query painted in `path`'s reading view.
    pub fn find_query(&self, path: &str) -> Option<&str> {
        self.find.get(path).map(String::as_str)
    }

    /// The filename input's current text for `path`.
    pub fn title<'a>(&'a self, path: &'a str) -> &'a str {
        self.titles
            .get(path)
            .map_or_else(|| stem(path), String::as_str)
    }

    /// Takes (clears) `path`'s pending filename edit.
    pub fn take_title(&mut self, path: &str) -> Option<String> {
        self.titles.remove(path)
    }

    /// `path`'s rendered doc.
    /// plan editor-ui-round-3 WP-6: the top-level block at the reading viewport's top, from
    /// the last layout probe (`None` = at the top / not probed).
    pub fn top_block(&self, path: &str, view: u64) -> Option<usize> {
        self.top.get(&(path.to_owned(), view)).copied()
    }

    /// Plan round-8 WP-4: sets the `scroll_anchor` fraction (clamped to 0–1).
    pub fn set_anchor(&mut self, a: f32) {
        self.anchor = if a.is_finite() {
            a.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if self.anchor <= 0.0 {
            self.anchor_at.clear();
        }
    }

    /// Plan round-8 WP-4: the block under the anchor height (the mode-switch origin); with
    /// anchor 0 (or no anchored probe yet) exactly [`Self::top_block`].
    pub fn anchor_block(&self, path: &str, view: u64) -> Option<usize> {
        if self.anchor > 0.0
            && let Some(b) = self.anchor_at.get(&(path.to_owned(), view))
        {
            return Some(*b);
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
        self.set_top_block(path, view, block);
        if self.anchor > 0.0 {
            self.carry.insert((path.to_owned(), view));
        }
        probe(path, view, blocks, Some(block))
    }

    /// plan editor-ui-round-3 WP-6: records the intended top block of a carry (the scroll task
    /// is in flight; its probe confirms it).
    pub fn set_top_block(&mut self, path: &str, view: u64, block: usize) {
        self.top.insert((path.to_owned(), view), block);
    }

    /// Plan round-6 WP-1: tab `view` on `path` went to its start (`false`) / end (`true`).
    pub fn set_edge(&mut self, path: &str, view: u64, bottom: bool) {
        self.edge.insert((path.to_owned(), view), bottom);
    }

    /// Plan round-6 WP-1: the edge tab `view` on `path` was last sent to, if not scrolled since.
    pub fn edge(&self, path: &str, view: u64) -> Option<bool> {
        self.edge.get(&(path.to_owned(), view)).copied()
    }

    /// Plan round-6 WP-1: a user scroll left the edge.
    pub fn clear_edge(&mut self, path: &str, view: u64) {
        self.edge.remove(&(path.to_owned(), view));
    }

    /// plan editor-ui-round-3 WP-6: the reading view is (being scrolled) at its top.
    pub fn clear_top_block(&mut self, path: &str, view: u64) {
        self.top.remove(&(path.to_owned(), view));
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
        self.restoring.insert(view, std::time::Instant::now());
        Some(probe(path, view, blocks, Some(b)))
    }

    /// Plan round-5 WP-1: the restore guard of tab `view` is armed.
    pub fn is_restoring(&self, view: u64) -> bool {
        self.restoring
            .get(&view)
            .is_some_and(|at| at.elapsed() < RESTORE_GUARD)
    }

    /// Test hook: backdates tab `view`'s restore guard by `by`.
    #[cfg(test)]
    pub(crate) fn age_restore_guard(&mut self, view: u64, by: std::time::Duration) {
        if let Some(at) = self.restoring.get_mut(&view) {
            *at = at.checked_sub(by).unwrap_or(*at);
        }
    }

    pub fn doc(&self, path: &str) -> Option<&Doc> {
        self.tabs.get(path).and_then(|t| t.doc.as_ref())
    }

    /// Plan round-5 WP-7: tab `view`'s Reading selection of `path` as `(anchor, focus)`.
    pub fn selection(&self, path: &str, view: u64) -> Option<(SelPos, SelPos)> {
        self.sel.get(&(path.to_owned(), view)).copied()
    }

    /// Sets (or clears: `None` or collapsed) tab `view`'s Reading selection of `path`.
    pub fn set_selection(&mut self, path: &str, view: u64, sel: Option<(SelPos, SelPos)>) {
        match sel.filter(|(a, f)| a != f) {
            Some(s) => {
                self.source_newer.remove(&(path.to_owned(), view));
                self.sel.insert((path.to_owned(), view), s);
            }
            None => {
                self.sel.remove(&(path.to_owned(), view));
            }
        }
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
        let (a, f) = self.selection(path, view)?;
        let text = selection::copy_text(&self.doc(path)?.runs, a, f);
        (!text.is_empty()).then_some(text)
    }

    /// Plan round-5 WP-8: the Source side of tab `view` of `path` changed its selection (it is
    /// now the most recent side in Split).
    pub fn note_source_selection(&mut self, path: &str, view: u64) {
        self.source_newer.insert((path.to_owned(), view));
    }

    /// Plan round-5 WP-8: true when the Source side selected more recently than the Reading
    /// side of tab `view` of `path`.
    pub fn source_selected_last(&self, path: &str, view: u64) -> bool {
        self.source_newer.contains(&(path.to_owned(), view))
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
        self.sel.retain(|(p, _), _| p != path);
    }

    /// A tab closed: forget its state and release its image pins (E2).
    pub fn on_tab_closed(&mut self, path: &str) {
        self.tabs.remove(path);
        self.titles.remove(path);
        self.find.remove(path);
        self.find_active.remove(path);
        self.top.retain(|(p, _), _| p != path);
        self.clear_selections(path);
        self.images.release_owner(path);
    }

    /// A filename edit is pending (the breadcrumb's input holds unsubmitted text).
    pub fn title_pending(&self) -> bool {
        !self.titles.is_empty()
    }

    /// The Escape listener runs: a filename edit is pending, in any view mode.
    pub fn listens_escape(&self) -> bool {
        self.title_pending()
    }

    /// The tick runs: some tab shows a reading pane (render debounce) or a filename edit is
    /// pending (the breadcrumb's blur poll) — in any view mode, Source-only included (plan
    /// editor-feedback-round-2 WP-6).
    pub fn ticks(&self) -> bool {
        self.title_pending() || self.tabs.values().any(|t| t.mode.shows_reading())
    }

    /// The debounce / blur-poll tick, plus the filename editor's Escape (see [`Self::ticks`],
    /// [`Self::listens_escape`]). Ctrl+E is no longer listened to here (plan
    /// editor-settings-tab WP-4, audit E1): "Toggle source / reading view" is an ordinary
    /// client command resolved by the effective keymap in `App::on_chord`, so it can be
    /// rebound.
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = Vec::new();
        if self.listens_escape() {
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
        tab: &SourceBuffer,
        mode: ViewMode,
        render_now: bool,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) {
        if render_now {
            self.set_mode(tab, mode, daemon);
        } else {
            let entry = self.tabs.entry(tab.path().to_owned()).or_default();
            entry.mode = mode;
        }
    }

    fn set_mode(
        &mut self,
        tab: &SourceBuffer,
        mode: ViewMode,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) {
        let entry = self.tabs.entry(tab.path().to_owned()).or_default();
        entry.mode = mode;
        if mode.shows_reading() {
            if entry.doc.is_none() {
                self.recompute(tab, daemon);
            }
        } else {
            // Leaving reading/split: drop the model and its image pins (E2).
            entry.doc = None;
            let path = tab.path();
            self.top.retain(|(p, _), _| p != path);
            self.sel.retain(|(p, _), _| p != path);
            entry.seen_seq = None;
            self.images.release_owner(tab.path());
        }
    }

    /// Re-lowers `tab`'s buffer, fills transclusions (depth 1), resolves images, and releases
    /// image pins the new model no longer references (E2).
    pub fn recompute(&mut self, tab: &SourceBuffer, daemon: Option<&Arc<dyn DaemonClient>>) {
        self.recomputes += 1;
        let seq = tab.edit_seq();
        if let Some(d) = daemon {
            self.refresh_resolver(d.as_ref());
        }
        let generation = daemon.and_then(|d| d.index().generation());
        let mut model = render::lower(&tab.text());
        let mut fetch = |target: &str| {
            let d = daemon?;
            let path = self.resolve(tab.path(), target)?;
            let (bytes, _) = d.read_file(&path).ok()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        };
        render::transclude(&mut model, &mut fetch);
        let images: HashMap<String, Option<String>> = render::images(&model)
            .into_iter()
            .filter(|(_, remote)| !remote)
            .map(|(src, _)| {
                let target = percent_decode(&src);
                let key = self.resolve(tab.path(), &target);
                (src, key)
            })
            .collect();
        let unresolved: HashSet<String> = if daemon.is_some() {
            render::link_targets(&model)
                .into_iter()
                .filter(|t| {
                    let bare = t.split('#').next().unwrap_or(t);
                    self.resolve(tab.path(), &percent_decode(bare)).is_none()
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
        self.images.retain_owner(tab.path(), &keep);
        // Plan round-5 WP-7: run indices / byte offsets shift with a new Doc.
        self.clear_selections(tab.path());
        let runs = selection::runs(&model);
        let entry = self.tabs.entry(tab.path().to_owned()).or_default();
        entry.seen_seq = Some(seq);
        entry.doc = Some(Doc {
            seq,
            epoch: tab.epoch(),
            generation,
            resolver_epoch: self.resolver_epoch,
            model,
            images,
            runs,
            unresolved,
        });
    }

    /// Debounce: re-render `tab` once its `edit_seq` held still for a tick (or the index moved).
    fn tick(&mut self, tab: &SourceBuffer, daemon: Option<&Arc<dyn DaemonClient>>) {
        let generation = daemon.and_then(|d| d.index().generation());
        let Some(entry) = self.tabs.get_mut(tab.path()) else {
            return;
        };
        if !entry.mode.shows_reading() {
            return;
        }
        let seq = tab.edit_seq();
        let resolver_epoch = self.resolver_epoch;
        let stale = match &entry.doc {
            None => true,
            Some(d) => {
                d.seq != seq
                    || d.epoch != tab.epoch()
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
            .is_some_and(|d| d.epoch != tab.epoch() || d.resolver_epoch != resolver_epoch);
        if reloaded || entry.seen_seq == Some(seq) || entry.doc.is_none() {
            self.recompute(tab, daemon);
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

/// Decodes every `%xx` escape in a link/image target (plan round-5 WP-2; was `%20`-only).
/// Invalid escapes stay verbatim; a result that is not UTF-8 leaves the input unchanged.
/// Plan round-6 WP-4: the vault path a missing link `target` creates — `#fragment` stripped and
/// percent-decoded (as [`ReadingStore::resolve_link`]); a target with a `/` is a vault path, a
/// bare one goes into `folder` (the explorer's New-note folder; `""` = vault root); `.md` is
/// appended when missing.
pub fn missing_note_path(target: &str, folder: &str) -> String {
    let bare = target.split('#').next().unwrap_or(target);
    let name = percent_decode(bare);
    let name = name.trim().trim_start_matches('/');
    let mut path = if name.contains('/') || folder.is_empty() {
        name.to_owned()
    } else {
        format!("{}/{name}", folder.trim_end_matches('/'))
    };
    if !path.to_ascii_lowercase().ends_with(".md") {
        path.push_str(".md");
    }
    path
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let (Some(h), Some(l)) = (
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(u8::try_from(h * 16 + l).unwrap_or(b'%'));
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
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

/// The reading update.
pub fn update(
    store: &mut ReadingStore,
    msg: ReadingMsg,
    tabs: &[&SourceBuffer],
    active: Option<&SourceBuffer>,
    daemon: Option<&Arc<dyn DaemonClient>>,
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
                let again = store
                    .restoring
                    .remove(&view)
                    .is_some_and(|at| at.elapsed() < RESTORE_GUARD)
                    .then(|| store.top_block(&path, view))
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
            match scroll_to {
                Some(b) => {
                    store.top.insert((path.clone(), view), b);
                    let key = (path.clone(), view);
                    let carry = store.carry.remove(&key);
                    if let Some(y) = p.block_offset(b) {
                        // Plan round-8 WP-4: a carry puts the block under the anchor height,
                        // clamped to the scrollable's range here (anchor 0 = today's `y`).
                        let y = if carry && store.anchor > 0.0 {
                            store.anchor_at.insert(key, b);
                            p.anchored_offset(y, store.anchor)
                        } else {
                            y
                        };
                        return ScrollIntent::ScrollTo { path, view, y }.task();
                    }
                }
                None => match p.top_block() {
                    Some(b) => {
                        if store.anchor > 0.0
                            && let Some(a) = p.anchor_block(store.anchor)
                        {
                            store.anchor_at.insert((path.clone(), view), a);
                        }
                        store.top.insert((path, view), b);
                    }
                    None => {
                        store.anchor_at.remove(&(path.clone(), view));
                        store.top.remove(&(path, view));
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
                .titles
                .keys()
                .map(|p| {
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
            store.titles.insert(path, value);
        }
        ReadingMsg::TitleRevert => store.titles.clear(),
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
                store.unresolved_click = Some((src, target));
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
                store.notice = Some((crate::app::Severity::Warning, image_failure_text(&key, &e)));
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
/// the showing tab's id (per-view image pins, plan editor-feedback-round-2 WP-7).
#[allow(clippy::too_many_arguments)] // one render site; a context struct would only rename these
pub fn tab_body<'a>(
    store: &'a ReadingStore,
    views: &'a crate::views::ViewStore,
    tab: &'a SourceBuffer,
    view: u64,
    t: Tokens,
    font_size: f32,
    keymap: &'a crate::palette::Keymap,
    focused: bool,
    link: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    use iced::Length;
    use iced::widget::{container, row};
    // plan editor-settings-tab WP-2 (audit M1): `editor.toml` `font_size` drives both panes.
    let scale = view::reading_scale(font_size);
    match store.mode(tab.path()) {
        ViewMode::Source => crate::source::view(tab, t, font_size, keymap, link),
        ViewMode::Reading => view::reading(store, views, tab.path(), view, t, scale, focused),
        ViewMode::Split => row![
            container(crate::source::view(tab, t, font_size, keymap, link))
                .width(Length::FillPortion(1)),
            container(view::reading(
                store,
                views,
                tab.path(),
                view,
                t,
                scale,
                focused
            ))
            .width(Length::FillPortion(1)),
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

/// plan editor-ui-round-3 WP-6: measured reading layout — the scrollable's offset and each
/// top-level block's top, all in absolute layout coordinates (audit E1: measured, never a
/// proportional estimate).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Probe {
    /// The scrollable's vertical scroll offset.
    pub offset_y: f32,
    /// The top of the scrollable's content.
    pub content_y: f32,
    /// Plan round-8 WP-4: the scrollable's viewport height.
    pub viewport_h: f32,
    /// Plan round-8 WP-4: the scrollable's content height.
    pub content_h: f32,
    /// The top of each top-level block (`None` = not laid out / not found).
    pub block_ys: Vec<Option<f32>>,
}

impl Probe {
    /// The block at the viewport top: the last block whose top is at or above the offset
    /// (1px slack), else the first laid-out block.
    pub fn top_block(&self) -> Option<usize> {
        self.block_at(0.0)
    }

    /// Plan round-8 WP-4: the block under the anchor height `a` (fraction of the viewport):
    /// the last block whose content-relative top is at or above `offset + a × viewport_h`.
    /// `a = 0` is exactly [`Self::top_block`].
    pub fn anchor_block(&self, a: f32) -> Option<usize> {
        self.block_at(a * self.viewport_h)
    }

    fn block_at(&self, dy: f32) -> Option<usize> {
        let rel = |y: f32| y - self.content_y;
        let mut first = None;
        let mut top = None;
        for (i, y) in self.block_ys.iter().enumerate() {
            let Some(y) = *y else { continue };
            first.get_or_insert(i);
            if rel(y) <= self.offset_y + dy + 1.0 {
                top = Some(i);
            }
        }
        top.or(first)
    }

    /// Plan round-8 WP-4: the scroll offset that puts content-relative `block_y` at the anchor
    /// height `a`, clamped to `[0, content_h − viewport_h]`.
    pub fn anchored_offset(&self, block_y: f32, a: f32) -> f32 {
        let max = (self.content_h - self.viewport_h).max(0.0);
        (block_y - a * self.viewport_h).clamp(0.0, max)
    }

    /// The scroll offset that puts block `b`'s top at the viewport top.
    pub fn block_offset(&self, b: usize) -> Option<f32> {
        let y = (*self.block_ys.get(b)?)?;
        Some((y - self.content_y).max(0.0))
    }
}

/// The widget operation behind [`Probe`]: reads the reading scrollable and the id'd block
/// containers of one path.
pub(crate) struct ProbeOp {
    scroll: iced::widget::Id,
    blocks: Vec<iced::widget::Id>,
    out: Probe,
    found: bool,
}

impl iced::advanced::widget::Operation<Probe> for ProbeOp {
    fn traverse(
        &mut self,
        operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<Probe>),
    ) {
        operate(self);
    }

    fn container(&mut self, id: Option<&iced::widget::Id>, bounds: iced::Rectangle) {
        if let Some(id) = id
            && let Some(i) = self.blocks.iter().position(|b| b == id)
        {
            self.out.block_ys[i] = Some(bounds.y);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: iced::Rectangle,
        content_bounds: iced::Rectangle,
        translation: iced::Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&self.scroll) {
            self.out.offset_y = translation.y;
            self.out.content_y = content_bounds.y;
            self.out.viewport_h = bounds.height;
            self.out.content_h = content_bounds.height;
            self.found = true;
        }
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<Probe> {
        if self.found {
            iced::advanced::widget::operation::Outcome::Some(self.out.clone())
        } else {
            iced::advanced::widget::operation::Outcome::None
        }
    }
}

/// Plan round-7 WP-4: builds the [`ProbeOp`] for `path`'s reading layout in tab `view`
/// (`blocks` top-level blocks) — shared by [`probe`] and the headless test harness.
pub(crate) fn probe_op(path: &str, view: u64, blocks: usize) -> ProbeOp {
    ProbeOp {
        scroll: crate::find::reading_scroll_id(path, view),
        blocks: (0..blocks).map(|i| block_id(path, view, i)).collect(),
        out: Probe {
            block_ys: vec![None; blocks],
            ..Probe::default()
        },
        found: false,
    }
}

/// plan editor-ui-round-3 WP-6: probes `path`'s reading layout (`blocks` top-level blocks),
/// answering with [`ReadingMsg::Probed`] (`scroll_to` passed through).
pub fn probe(path: &str, view: u64, blocks: usize, scroll_to: Option<usize>) -> Task<Message> {
    ScrollIntent::Probe {
        path: path.to_owned(),
        view,
        blocks,
        scroll_to,
    }
    .task()
}

/// Plan round-7 WP-4 (audit E1): one programmatic scroll or layout probe of tab `view`'s
/// reading scrollable on `path`, as a plain value. Every editor-issued reading scroll goes
/// through [`ScrollIntent::task`], so the headless tests can apply the same intents to a
/// laid-out tree (the returned `Task` is opaque outside the runtime).
#[derive(Debug, Clone, PartialEq)]
pub enum ScrollIntent {
    /// Snap to a relative vertical offset (0 = top, 1 = bottom).
    SnapTo { path: String, view: u64, y: f32 },
    /// Scroll to an absolute vertical offset.
    ScrollTo { path: String, view: u64, y: f32 },
    /// PageUp / PageDown ([`crate::page::page`]).
    Page { path: String, view: u64, down: bool },
    /// One line step of `dy` pixels ([`crate::page::step`]).
    Step { path: String, view: u64, dy: f32 },
    /// A layout probe answered by [`ReadingMsg::Probed`].
    Probe {
        path: String,
        view: u64,
        blocks: usize,
        scroll_to: Option<usize>,
    },
    /// Plan round-7 WP-4 (attempt 2): re-sends the source editor's viewport report after the
    /// next layout (pass 2 of a Reading → Source line carry, [`SourceBuffer::set_visible_lines`]).
    ///
    /// [`SourceBuffer::set_visible_lines`]: crate::source::SourceBuffer::set_visible_lines
    SourceRelayout { path: String, lines: usize },
}

#[cfg(test)]
thread_local! {
    /// Test hook: every intent turned into a task on this thread, in order.
    pub(crate) static INTENTS: std::cell::RefCell<Vec<ScrollIntent>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl ScrollIntent {
    /// The runtime task carrying out this intent.
    pub fn task(self) -> Task<Message> {
        #[cfg(test)]
        INTENTS.with(|i| i.borrow_mut().push(self.clone()));
        let id = |path: &str, view| crate::find::reading_scroll_id(path, view);
        match self {
            Self::SnapTo { path, view, y } => iced::widget::operation::snap_to(
                id(&path, view),
                iced::widget::scrollable::RelativeOffset { x: 0.0, y },
            ),
            Self::ScrollTo { path, view, y } => iced::widget::operation::scroll_to(
                id(&path, view),
                iced::widget::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
            ),
            Self::Page { path, view, down } => crate::page::page(id(&path, view), down),
            Self::Step { path, view, dy } => crate::page::step(id(&path, view), dy),
            Self::SourceRelayout { path, lines } => {
                Task::done(Message::Source(crate::message::SourceMsg::Viewport {
                    path,
                    lines,
                }))
            }
            Self::Probe {
                path,
                view,
                blocks,
                scroll_to,
            } => {
                let op = probe_op(&path, view, blocks);
                iced::advanced::widget::operate(op).map(move |p: Probe| {
                    Message::Reading(ReadingMsg::Probed {
                        path: path.clone(),
                        view,
                        probe: Some(p),
                        scroll_to,
                    })
                })
            }
        }
    }
}

#[cfg(test)]
mod selection_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wp8_tests;
