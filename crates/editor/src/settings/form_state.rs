//! The typing-level form state (split out of `settings/form.rs`, plan tn-review-2026-10-26
//! WP-R4): [`FormState`], the active typeahead ([`ActiveTa`], [`TaTarget`]) and the draft keys.
//! Never written to a file; re-exported from [`super::form`].

use std::collections::{BTreeMap, BTreeSet};

use cv_schema::{Node, Path, Seg};

use super::form::display_order;
use super::pickers::Picker;
use crate::widget::typeahead::Typeahead;

/// Transient form UI state (never written to a file). Keyed by vault path of the file.
#[derive(Debug, Default, Clone)]
pub struct FormState {
    /// Expanded list items / map entries.
    pub expanded: BTreeSet<(String, Path)>,
    /// Typing drafts, by `(file, draft key)` (see [`draft_key`]).
    pub drafts: BTreeMap<(String, String), String>,
    /// The field whose typeahead is active (plan round-5 WP-4).
    pub typeahead: Option<ActiveTa>,
    /// The display order last shown per display-sorted list (round-13 WP-1), see
    /// [`Self::list_order`]. WP-R3 (review E9): written only in the update path, when an edit
    /// starts ([`Self::pin_order`]); the render pass only reads it.
    pub(super) orders: BTreeMap<(String, Path), Vec<usize>>,
}

/// What the active typeahead edits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TaTarget {
    /// A text field: a commit writes the value (`required`: an empty commit keeps the key).
    Text { required: bool },
    /// A chips adder: a commit adds a chip.
    Chips,
    /// A map entry's key input (the active path is the entry's [`pickers::key_ta_path`]): a
    /// commit renames the key.
    RenameKey,
    /// A map's "new key" input (the active path is the map's [`pickers::key_ta_path`]): a commit
    /// adds the key with the map's entry node.
    NewKey(&'static Node),
}

/// The active typeahead field: its file, path, picker, typed text and widget state.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveTa {
    pub file: String,
    pub path: Path,
    pub picker: Picker,
    pub target: TaTarget,
    /// The typed text (the query; a chips adder mirrors it into its chip draft).
    pub draft: String,
    pub ta: Typeahead,
}

impl ActiveTa {
    /// A typeahead on `(file, path)` whose committed value is `committed`, nothing typed yet.
    pub fn new(
        file: &str,
        path: Path,
        picker: Picker,
        target: TaTarget,
        committed: impl Into<String>,
    ) -> Self {
        Self {
            file: file.to_owned(),
            path,
            picker,
            target,
            draft: String::new(),
            ta: Typeahead::begin(committed),
        }
    }

    /// Whether this is the field at `(file, path)`.
    pub fn is(&self, file: &str, path: &Path) -> bool {
        self.file == file && self.path == *path
    }
}

/// The draft key of a widget kind at a path (`num:action[0].x`).
pub fn draft_key(kind: &str, path: &Path) -> String {
    format!("{kind}:{path}")
}

impl FormState {
    /// Whether the item at `path` is expanded.
    pub fn is_expanded(&self, file: &str, path: &Path) -> bool {
        self.expanded.contains(&(file.to_owned(), path.clone()))
    }

    /// The order to show a display-sorted list's items in. While the list is pinned (an item of
    /// it is being edited, see [`Self::pin_order`]) the order snapshotted at the edit's start is
    /// kept, so the edited item neither jumps under the cursor nor takes the widget focus to
    /// another row (iced keeps focus by tree position); otherwise it is the fresh sort of
    /// `names`. A pure read.
    pub fn list_order(
        &self,
        file: &str,
        list: &Path,
        names: &[String],
        sorted: bool,
    ) -> Vec<usize> {
        if sorted
            && let Some(order) = self.orders.get(&(file.to_owned(), list.clone()))
            && order.len() == names.len()
        {
            return order.clone();
        }
        display_order(names, sorted)
    }

    /// Pins `order` (the order shown when an edit of one of its items starts) for the list at
    /// `list`, unless it is pinned already.
    pub fn pin_order(&mut self, file: &str, list: &Path, order: Vec<usize>) {
        self.orders
            .entry((file.to_owned(), list.clone()))
            .or_insert(order);
    }

    /// Releases every pinned list order of `file`: the next render sorts afresh.
    pub fn unpin_orders(&mut self, file: &str) {
        self.orders.retain(|(f, _), _| f != file);
    }

    /// Releases every pinned list order of every file (a section switch).
    pub fn unpin_all(&mut self) {
        self.orders.clear();
    }

    /// Expands or collapses the item at `path` (navigating away from an edit: re-sorts).
    pub fn toggle(&mut self, file: &str, path: &Path) {
        self.unpin_orders(file);
        let k = (file.to_owned(), path.clone());
        if !self.expanded.remove(&k) {
            self.expanded.insert(k);
        }
    }

    /// A draft.
    pub fn draft(&self, file: &str, key: &str) -> Option<&str> {
        self.drafts
            .get(&(file.to_owned(), key.to_owned()))
            .map(String::as_str)
    }

    /// Sets (or, when `None`, clears) a draft.
    pub fn set_draft(&mut self, file: &str, key: String, v: Option<String>) {
        let k = (file.to_owned(), key);
        match v {
            Some(v) => {
                self.drafts.insert(k, v);
            }
            None => {
                self.drafts.remove(&k);
            }
        }
    }

    /// Drops every draft of `file` (a reload, or a structural edit that shifts paths).
    pub fn clear_drafts(&mut self, file: &str) {
        self.drafts.retain(|(f, _), _| f != file);
        self.unpin_orders(file);
        if self.typeahead.as_ref().is_some_and(|a| a.file == file) {
            self.typeahead = None;
        }
    }

    /// Drops all UI state of `file`.
    pub fn clear_file(&mut self, file: &str) {
        self.clear_drafts(file);
        self.expanded.retain(|(f, _)| f != file);
    }

    /// Rewrites the segment right below `prefix` in every expanded path of `file` (`None`
    /// drops the entry) — keeps expansion attached to its item across insert / remove / move /
    /// rename.
    pub fn remap(&mut self, file: &str, prefix: &Path, f: impl Fn(&Seg) -> Option<Seg>) {
        let n = prefix.0.len();
        let old = std::mem::take(&mut self.expanded);
        for (fl, p) in old {
            if fl != file || p.0.len() <= n || p.0[..n] != prefix.0[..] {
                self.expanded.insert((fl, p));
                continue;
            }
            if let Some(s) = f(&p.0[n]) {
                let mut q = p.clone();
                q.0[n] = s;
                self.expanded.insert((fl, q));
            }
        }
    }
}
