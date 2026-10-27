//! The per-app view cache: [`ViewStore`], its [`Entry`]/[`State`], the [`update`] reducer and the
//! [`Effect`]s the app performs (split out of `views/mod.rs`, plan tn-review-2026-10-26 WP-R4).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::decode::{Decoded, decode};
use super::{Key, list_id};
use crate::message::ViewsMsg;
use crate::ui_state::{ViewSortPref, view_sort_key};
use crate::widget::virtual_list::ListWindow;

/// A cache entry's state.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Loading,
    Ready(Decoded),
    Error(String),
}

/// One cached result.
#[derive(Debug, Clone)]
pub struct Entry {
    pub state: State,
    /// A `view_run` is outstanding (dedupe).
    pub in_flight: bool,
    /// Invalidated since the last result: the next `Need` re-requests.
    pub stale: bool,
    /// Bumped on invalidation; part of the fence sensor key.
    pub epoch: u64,
    pub window: ListWindow,
}

impl Entry {
    /// WP-R3 (review E10): a loaded view with a `select_by` property (the one predicate).
    fn is_select_ready(&self) -> bool {
        matches!(&self.state, State::Ready(d) if d.select_by.is_some())
    }
}

/// The per-app view cache.
#[derive(Debug, Default)]
pub struct ViewStore {
    pub entries: HashMap<Key, Entry>,
    /// Plan round-9-feedback-todos WP-5: ids of the views whose "Only open" toggle is on
    /// (persisted as `UiState.view_filters`).
    pub filters: BTreeSet<String>,
    /// Plan round-11 WP-5: column-header sorts keyed by [`view_sort_key`]
    /// (persisted as `UiState.view_sorts`).
    pub sorts: BTreeMap<String, ViewSortPref>,
    /// Plan round-13 WP-4: the in-place selection of each host note (the clicked serial row's
    /// name). In memory only; dropped with the host's tab.
    pub selections: BTreeMap<String, String>,
}

/// The next header-click state of a column: none/other → ASC, ASC → DESC, DESC → cleared.
pub fn next_sort(current: Option<&ViewSortPref>, column: &str) -> Option<ViewSortPref> {
    match current {
        Some(c) if c.column == column && c.desc => None,
        Some(c) if c.column == column => Some(ViewSortPref {
            column: column.to_owned(),
            desc: true,
        }),
        _ => Some(ViewSortPref {
            column: column.to_owned(),
            desc: false,
        }),
    }
}

impl ViewStore {
    /// The active header sort of `(id, host)`.
    pub fn sort_of(&self, id: &str, host: Option<&str>) -> Option<&ViewSortPref> {
        self.sorts.get(&view_sort_key(id, host))
    }

    /// [`Self::sort_of`] as the wire type.
    fn wire_sort(&self, id: &str, host: Option<&str>) -> Option<cv_protocol::ViewSort> {
        self.sort_of(id, host).map(|s| cv_protocol::ViewSort {
            property: s.column.clone(),
            direction: if s.desc {
                cv_protocol::ViewSortDir::Desc
            } else {
                cv_protocol::ViewSortDir::Asc
            },
        })
    }

    /// Plan round-13 WP-4: the `select_by` property of the loaded view `(id, host)`.
    pub(super) fn select_by(&self, id: &str, host: Option<&str>) -> Option<&str> {
        match self.get(id, host).map(|e| &e.state) {
            Some(State::Ready(d)) => d.select_by.as_deref(),
            _ => None,
        }
    }

    /// Plan round-13 WP-4: the selection to send with `view_run(id, host)` — the host's selected
    /// value under the view's `select_by` property; `None` for any other view.
    fn wire_select(&self, id: &str, host: Option<&str>) -> Option<cv_protocol::ViewSelect> {
        let property = self.select_by(id, host)?;
        let value = self.selections.get(host?)?;
        Some(cv_protocol::ViewSelect {
            property: property.to_owned(),
            value: value.clone(),
        })
    }

    /// Plan round-13 WP-4: whether note `host` embeds a loaded `select_by` view (its sources
    /// sections' serial rows are then clickable).
    pub fn host_selects(&self, host: &str) -> bool {
        self.entries
            .iter()
            .any(|((_, h), e)| h.as_deref() == Some(host) && e.is_select_ready())
    }

    /// WP-R3 (review E10): marks the entry at `key` stale and bumps its epoch (a shown fence asks
    /// again). `Loaded` re-bumps the epoch of a stale entry, so an in-flight run is covered too.
    fn invalidate_entry(&mut self, key: &Key) {
        if let Some(e) = self.entries.get_mut(key) {
            e.stale = true;
            e.epoch = e.epoch.wrapping_add(1);
        }
    }

    /// WP-R3 (review E10): claims the idle entry at `key` for a run (`false` while one is
    /// outstanding or the entry is absent).
    fn claim(&mut self, key: &Key) -> bool {
        match self.entries.get_mut(key) {
            Some(e) if !e.in_flight => {
                e.in_flight = true;
                e.stale = false;
                true
            }
            _ => false,
        }
    }

    /// WP-R3 (review E10): the run request for the claimed view `id` in `host`.
    fn run_effect(&self, id: String, host: Option<String>) -> Effect {
        let sort = self.wire_sort(&id, host.as_deref());
        let select = self.wire_select(&id, host.as_deref());
        Effect::Run {
            id,
            host,
            sort,
            select,
        }
    }

    /// Plan round-13 WP-4: toggles `host`'s selection to `value` (the same value clears it) and
    /// re-runs its `select_by` views: the first idle one now, the rest through their bumped
    /// epoch (a shown fence asks again).
    fn select(&mut self, host: String, value: String) -> Effect {
        if self.selections.get(&host) == Some(&value) {
            self.selections.remove(&host);
        } else {
            self.selections.insert(host.clone(), value);
        }
        let mut ids: Vec<String> = self
            .entries
            .iter()
            .filter(|((_, h), e)| h.as_deref() == Some(host.as_str()) && e.is_select_ready())
            .map(|((id, _), _)| id.clone())
            .collect();
        ids.sort();
        let mut run = None;
        for id in ids {
            let key = (id.clone(), Some(host.clone()));
            self.invalidate_entry(&key);
            if run.is_none() && self.claim(&key) {
                run = Some(id);
            }
        }
        match run {
            Some(id) => self.run_effect(id, Some(host)),
            None => Effect::None,
        }
    }

    /// Plan round-5 WP-1: re-applies the stored scroll window of every virtual list hosted in
    /// note `host` (its scrollable state is positional and lost while the tab was hidden).
    pub fn restore<T: Send + 'static>(&self, host: &str) -> Vec<iced::Task<T>> {
        self.entries
            .iter()
            .filter(|((_, h), e)| h.as_deref() == Some(host) && e.window.offset > 0.0)
            .map(|((id, _), e)| {
                iced::widget::operation::scroll_to(
                    list_id(id, host),
                    iced::widget::scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(e.window.offset),
                    },
                )
            })
            .collect()
    }
}

/// What the app must do after [`update`].
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    None,
    /// Run `view_run(id, host)` off the UI thread.
    Run {
        id: String,
        host: Option<String>,
        /// The column-header sort to send with the request.
        sort: Option<cv_protocol::ViewSort>,
        /// Plan round-13 WP-4: the host's selection, for a `select_by` view.
        select: Option<cv_protocol::ViewSelect>,
    },
    /// Open `path` in the focused pane, cursor at 1-based `line` when set.
    Open {
        path: String,
        line: Option<i64>,
    },
    /// Plan round-6-feedback-todos WP-6: run `tracker_item_set_status` off the UI thread.
    SetStatus {
        key: views::result::TrackerKey,
        status: Option<char>,
    },
    /// Plan round-8-intake-views-anchor WP-3: set frontmatter boolean `key` of `path`.
    SetBool {
        path: String,
        key: String,
        value: bool,
    },
    /// A failure notice for the status bar.
    Notice(String),
}

impl ViewStore {
    /// The entry for `(id, host)`.
    pub fn get(&self, id: &str, host: Option<&str>) -> Option<&Entry> {
        self.entries.get(&(id.to_owned(), host.map(str::to_owned)))
    }

    /// `views_invalidated`: entries whose id is in `ids` (all when empty) go stale and bump
    /// their epoch, so shown fences request again.
    pub fn invalidate(&mut self, ids: &[String]) {
        for ((id, _), e) in &mut self.entries {
            if ids.is_empty() || ids.contains(id) {
                e.stale = true;
                e.epoch = e.epoch.wrapping_add(1);
            }
        }
    }

    /// The host note `from` was renamed/moved to `to` (plan editor-feedback-round-2 WP-8): its
    /// hosted entries move to `to` keeping their shown state and scroll window (no flash), go
    /// stale and bump their epoch so the fence re-runs against `to`. A run in flight for `from`
    /// is abandoned (its `Loaded` finds no `from` entry).
    pub fn rehost(&mut self, from: &str, to: &str) {
        let keys: Vec<Key> = self
            .entries
            .keys()
            .filter(|(_, h)| h.as_deref() == Some(from))
            .cloned()
            .collect();
        for k in keys {
            if let Some(mut e) = self.entries.remove(&k) {
                e.in_flight = false;
                e.stale = true;
                e.epoch = e.epoch.wrapping_add(1);
                self.entries.insert((k.0, Some(to.to_owned())), e);
            }
        }
        if let Some(v) = self.selections.remove(from) {
            self.selections.insert(to.to_owned(), v);
        }
    }

    /// A tab closed: drop the results it hosted.
    pub fn on_tab_closed(&mut self, path: &str) {
        self.entries.retain(|(_, h), _| h.as_deref() != Some(path));
        self.selections.remove(path);
    }
}

/// Applies `m`; the caller performs the returned [`Effect`].
pub fn update(store: &mut ViewStore, m: ViewsMsg) -> Effect {
    match m {
        ViewsMsg::Need { id, host } => {
            let e = store
                .entries
                .entry((id.clone(), host.clone()))
                .or_insert_with(|| Entry {
                    state: State::Loading,
                    in_flight: false,
                    stale: true,
                    epoch: 0,
                    window: ListWindow::default(),
                });
            if !e.stale || !store.claim(&(id.clone(), host.clone())) {
                return Effect::None;
            }
            store.run_effect(id, host)
        }
        ViewsMsg::SortBy { id, host, column } => {
            let key = view_sort_key(&id, host.as_deref());
            match next_sort(store.sorts.get(&key), &column) {
                Some(s) => store.sorts.insert(key, s),
                None => store.sorts.remove(&key),
            };
            let key = (id.clone(), host.clone());
            store.invalidate_entry(&key);
            if store.claim(&key) {
                store.run_effect(id, host)
            } else {
                Effect::None
            }
        }
        ViewsMsg::Loaded { id, host, result } => {
            if let Some(e) = store.entries.get_mut(&(id, host)) {
                e.in_flight = false;
                if e.stale {
                    // Invalidated while in flight (WP-8): the fence's `Need` for the bumped
                    // epoch was refused then, so bump again to make a shown fence re-request.
                    e.epoch = e.epoch.wrapping_add(1);
                }
                e.state = match result.and_then(|v| decode(&v).map_err(|e| e.to_string())) {
                    Ok(d) => State::Ready(d),
                    Err(err) => State::Error(err),
                };
            }
            Effect::None
        }
        ViewsMsg::Scrolled { id, host, window } => {
            if let Some(e) = store.entries.get_mut(&(id, host)) {
                e.window = window;
            }
            Effect::None
        }
        ViewsMsg::Open { path, line } => Effect::Open { path, line },
        ViewsMsg::SetStatus { key, status } => match views::result::TrackerKey::decode(&key) {
            Some(key) => Effect::SetStatus { key, status },
            None => Effect::Notice("status toggle: malformed row key".into()),
        },
        // Success needs nothing: the refresh arrives as `views_invalidated`.
        ViewsMsg::StatusSet(Ok(())) => Effect::None,
        ViewsMsg::StatusSet(Err(e)) => Effect::Notice(format!("status change failed: {e}")),
        ViewsMsg::SetBool { path, key, value } => Effect::SetBool { path, key, value },
        // Success needs nothing: the refresh arrives with the file change.
        ViewsMsg::BoolSet(Ok(())) => Effect::None,
        ViewsMsg::ToggleOpenOnly { id } => {
            if !store.filters.remove(&id) {
                store.filters.insert(id);
            }
            Effect::None
        }
        ViewsMsg::BoolSet(Err(e)) => Effect::Notice(format!("property change failed: {e}")),
        ViewsMsg::Select { host, value } => store.select(host, value),
    }
}
