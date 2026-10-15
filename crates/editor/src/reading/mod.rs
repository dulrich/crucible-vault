//! Reading pane and toggle/split (WP-3).
//!
//! Per-tab reading state lives in a [`ReadingStore`] keyed by vault path (mode, cached render
//! model, resolved image keys); `Tab` itself is untouched. The render model
//! (`editor_model::render`) is recomputed on a debounce tick only when the buffer's
//! `edit_seq()` (or the index generation) moved, and cached between frames. Clicks resolve
//! through a `cv_index::Resolver` built from the local index (rebuilt per index generation),
//! so links in unsaved text resolve the same way the daemon would resolve them on save.
//!
//! Image handles are pinned per tab in the [`ImageCache`]; see that module for the release points
//! (audit E2) and the handle-release vs renderer-reclamation distinction.

pub mod view;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use cv_index::Resolver;
use editor_model::render::{self, RenderModel};
use iced::{Element, Subscription, Task};

use crate::app::Tab;
use crate::daemon::DaemonClient;
use crate::image_cache::{self, ImageCache};
use crate::message::{Message, ReadingMsg, TabsMsg};
use crate::theme::Tokens;

/// Recompute debounce: a change is rendered once `edit_seq` has been stable for one tick.
pub const TICK: Duration = Duration::from_millis(250);

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
    /// The index generation links/images were resolved against.
    pub generation: Option<u64>,
    pub model: RenderModel,
    /// Image `src` → resolved vault path (`None` = unresolved); remote images are absent.
    pub images: HashMap<String, Option<String>>,
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
    tabs: HashMap<String, TabReading>,
    pub images: ImageCache,
    resolver: Option<ResolverCache>,
    /// Render-model recomputations so far (observability + tests).
    pub recomputes: u64,
    /// Pending filename-input edits, keyed by tab path (absent = shows the stem).
    titles: HashMap<String, String>,
    /// The open find bar's query per tab path: its occurrences are painted in the reading view
    /// (plan editor-feedback-round-2 WP-4; render blocks carry no source span).
    find: HashMap<String, String>,
}

/// The note stem shown in the filename input (`a/b/Note.md` → `Note`).
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

/// The widget id of `path`'s filename input (blur polling).
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
            }
        }
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
    pub fn doc(&self, path: &str) -> Option<&Doc> {
        self.tabs.get(path).and_then(|t| t.doc.as_ref())
    }

    /// A tab closed: forget its state and release its image pins (E2).
    pub fn on_tab_closed(&mut self, path: &str) {
        self.tabs.remove(path);
        self.titles.remove(path);
        self.find.remove(path);
        self.images.release_owner(path);
    }

    /// The debounce tick while any tab shows a reading pane, plus the title editor's Escape.
    /// Ctrl+E is no longer listened to here (plan editor-settings-tab WP-4, audit E1): "Toggle
    /// source / reading view" is an ordinary client command resolved by the effective keymap
    /// in `App::on_chord`, so it can be rebound.
    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = Vec::new();
        if !self.titles.is_empty() {
            // The focused text input captures Escape, so listen to captured events too.
            subs.push(iced::event::listen_with(escape));
        }
        if self.tabs.values().any(|t| t.mode.shows_reading()) {
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
        tab: &Tab,
        mode: ViewMode,
        render_now: bool,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) {
        if render_now {
            self.set_mode(tab, mode, daemon);
        } else {
            let entry = self.tabs.entry(tab.path.clone()).or_default();
            entry.mode = mode;
        }
    }

    fn set_mode(&mut self, tab: &Tab, mode: ViewMode, daemon: Option<&Arc<dyn DaemonClient>>) {
        let entry = self.tabs.entry(tab.path.clone()).or_default();
        entry.mode = mode;
        if mode.shows_reading() {
            if entry.doc.is_none() {
                self.recompute(tab, daemon);
            }
        } else {
            // Leaving reading/split: drop the model and its image pins (E2).
            entry.doc = None;
            entry.seen_seq = None;
            self.images.release_owner(&tab.path);
        }
    }

    /// Re-lowers `tab`'s buffer, fills transclusions (depth 1), resolves images, and releases
    /// image pins the new model no longer references (E2).
    pub fn recompute(&mut self, tab: &Tab, daemon: Option<&Arc<dyn DaemonClient>>) {
        self.recomputes += 1;
        let seq = tab.source.edit_seq();
        if let Some(d) = daemon {
            self.refresh_resolver(d.as_ref());
        }
        let generation = daemon.and_then(|d| d.index().generation());
        let mut model = render::lower(&tab.source.text());
        let mut fetch = |target: &str| {
            let d = daemon?;
            let path = self.resolve(&tab.path, target)?;
            let (bytes, _) = d.read_file(&path).ok()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        };
        render::transclude(&mut model, &mut fetch);
        let images: HashMap<String, Option<String>> = render::images(&model)
            .into_iter()
            .filter(|(_, remote)| !remote)
            .map(|(src, _)| {
                let target = src.replace("%20", " ");
                let key = self.resolve(&tab.path, &target);
                (src, key)
            })
            .collect();
        let keep: HashSet<String> = images.values().flatten().cloned().collect();
        self.images.retain_owner(&tab.path, &keep);
        let entry = self.tabs.entry(tab.path.clone()).or_default();
        entry.seen_seq = Some(seq);
        entry.doc = Some(Doc {
            seq,
            generation,
            model,
            images,
        });
    }

    /// Debounce: re-render `tab` once its `edit_seq` held still for a tick (or the index moved).
    fn tick(&mut self, tab: &Tab, daemon: Option<&Arc<dyn DaemonClient>>) {
        let generation = daemon.and_then(|d| d.index().generation());
        let Some(entry) = self.tabs.get_mut(&tab.path) else {
            return;
        };
        if !entry.mode.shows_reading() {
            return;
        }
        let seq = tab.source.edit_seq();
        let stale = match &entry.doc {
            None => true,
            Some(d) => d.seq != seq || d.generation != generation,
        };
        if !stale {
            return;
        }
        if entry.seen_seq == Some(seq) || entry.doc.is_none() {
            self.recompute(tab, daemon);
        } else {
            entry.seen_seq = Some(seq);
        }
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
        if render::is_remote(target) {
            return None;
        }
        let d = daemon?;
        self.refresh_resolver(d.as_ref());
        let target = target.split('#').next().unwrap_or(target);
        let target = target.replace("%20", " ");
        // `[[#h]]` resolves to the note itself; keep it (focuses the same tab).
        self.resolve(src, &target)
            .map(|p| Message::Tabs(TabsMsg::Open(p)))
    }

    /// `tab` shows image `key`: pins it, and starts an off-thread load on a miss.
    fn image_shown(
        &mut self,
        tab: &str,
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
        if self.images.show(tab, key) {
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

/// The reading update.
pub fn update(
    store: &mut ReadingStore,
    msg: ReadingMsg,
    tabs: &[&Tab],
    active: Option<&Tab>,
    daemon: Option<&Arc<dyn DaemonClient>>,
) -> Task<Message> {
    match msg {
        ReadingMsg::CycleMode => {
            if let Some(tab) = active {
                let next = store.mode(&tab.path).toggle();
                store.set_mode(tab, next, daemon);
            }
        }
        ReadingMsg::SetMode { path, mode } => {
            if let Some(tab) = tabs.iter().find(|t| t.path == path) {
                store.set_mode(tab, mode, daemon);
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
        // Buffer edits and renames need the app (tabs, explorer, saves): handled in `App`.
        ReadingMsg::ToggleTask { .. }
        | ReadingMsg::TitleSubmit { .. }
        | ReadingMsg::TitleFocus { .. }
        | ReadingMsg::TitleSaved { .. } => {}
        ReadingMsg::OpenLink { src, target } => {
            if let Some(m) = store.click(&src, &target, daemon) {
                return Task::done(m);
            }
        }
        ReadingMsg::ImageShown { tab, key, width } => {
            return store.image_shown(&tab, &key, width, daemon);
        }
        ReadingMsg::ImageHidden { tab, key } => store.images.hide(&tab, &key),
        ReadingMsg::ImageLoaded { key, result } => {
            if let Err(e) = &result {
                tracing::debug!(error = %e, "image load failed");
            }
            store.images.loaded(&key, result.ok());
        }
    }
    Task::none()
}

/// The active tab's body: source, reading, or split (source left, reading right).
pub fn tab_body<'a>(
    store: &'a ReadingStore,
    views: &'a crate::views::ViewStore,
    tab: &'a Tab,
    t: Tokens,
    mode: cv_protocol::Mode,
    font_size: f32,
    keymap: &'a crate::palette::Keymap,
) -> Element<'a, Message> {
    use iced::Length;
    use iced::widget::{container, row};
    // plan editor-settings-tab WP-2 (audit M1): `editor.toml` `font_size` drives both panes.
    let scale = font_size / crate::theme::BASE_TEXT_SIZE;
    match store.mode(&tab.path) {
        ViewMode::Source => crate::source::view(&tab.source, t, mode, font_size, keymap),
        ViewMode::Reading => view::reading(store, views, &tab.path, t, scale),
        ViewMode::Split => row![
            container(crate::source::view(&tab.source, t, mode, font_size, keymap))
                .width(Length::FillPortion(1)),
            container(view::reading(store, views, &tab.path, t, scale))
                .width(Length::FillPortion(1)),
        ]
        .spacing(12)
        .into(),
    }
}

#[cfg(test)]
mod tests;
