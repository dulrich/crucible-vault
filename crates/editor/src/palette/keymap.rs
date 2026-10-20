//! The effective keymap: the editor's bound payload ([`Bound`]), its bindables and the
//! thin adapter over the pure conflict-resolution core ([`editor_model::keymap`]) — plan
//! `editor-feedback-round-2` WP-1 "Hotkey conflict policy", which replaced `editor-settings-tab`'s
//! "override > editor default > config action keys": every bindable claims its chords (an
//! `editor.toml` `[keys]` override replaces the command's editor default; `[]` releases it), and
//! a chord claimed by two or more distinct bindables is bound to none of them. Every key handler
//! (`App::on_chord`, the focused source editor's `key_binding`) decides from it.

use std::collections::BTreeMap;

use cv_protocol::ActionInfo;
use editor_model::keymap::{Binding, ClaimKind, Claimant};

use super::commands::{ClientCmd, DEFAULT_ACTIONS, PERIOD_OPEN, Preset, table};
use super::{Entry, EntryId, bookmark_id};

/// What a bound chord runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Bound {
    Client(ClientCmd),
    /// A default action preset (`period.open` / `lint-note`): fetches the action list first.
    Preset(Preset),
    /// A config action (`actions.toml` `keys`).
    Action(String),
    /// WP-9: open the bookmark at this path (`bookmarks.toml` `keys`).
    Bookmark(String),
}

impl Binding for Bound {
    /// Chords bound to a client command, preset or bookmark are handed to the app (Alt+digit
    /// would otherwise insert the digit).
    fn routed(&self) -> bool {
        matches!(
            self,
            Bound::Client(_) | Bound::Preset(_) | Bound::Bookmark(_)
        )
    }

    fn action_id(&self) -> Option<&str> {
        match self {
            Bound::Action(id) => Some(id),
            _ => None,
        }
    }

    fn default_claimants() -> Vec<Claimant<Self>> {
        claimants(&[], &[])
    }
}

/// The effective keymap: see [`editor_model::keymap::Keymap`].
pub type Keymap = editor_model::keymap::Keymap<Bound>;

/// Where a Hotkeys row's chords come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    /// The editor default.
    Default,
    /// An `editor.toml` `[keys]` override.
    EditorToml,
    /// The action's `keys` in `actions.toml`.
    ActionsToml,
    /// WP-9: the bookmark's `keys` in `bookmarks.toml`.
    BookmarksToml,
}

impl KeySource {
    /// The Hotkeys page label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::EditorToml => "editor.toml",
            Self::ActionsToml => "actions.toml",
            Self::BookmarksToml => "bookmarks.toml",
        }
    }
}

/// One editor-owned bindable command: a client command or a default-action preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bindable {
    /// The `[keys]` id ([`ClientCmd::id`] / [`Preset::id`]).
    pub id: String,
    pub title: String,
    pub run: Bound,
    /// The editor default chords.
    pub defaults: Vec<String>,
    /// Save / Undo / Redo: their chords are `text_editor` editing chords (not rebindable).
    pub fixed: bool,
}

/// Every editor-owned bindable command, in palette order (client commands, then the
/// non-period default-action presets; the period presets are the `open_period.*` commands).
pub fn bindables() -> Vec<Bindable> {
    let mut v: Vec<Bindable> = table()
        .iter()
        .map(|s| Bindable {
            id: s.id.clone(),
            title: s.title.clone(),
            run: match s.cmd {
                ClientCmd::OpenPeriod(p) => Bound::Preset(Preset {
                    action: PERIOD_OPEN,
                    period: Some(p),
                }),
                c => Bound::Client(c),
            },
            defaults: s.keys.clone(),
            fixed: s.fixed,
        })
        .collect();
    v.extend(
        DEFAULT_ACTIONS
            .iter()
            .filter(|(_, p)| p.period.is_none())
            .map(|&(k, p)| Bindable {
                id: p.id(),
                title: format!("Run {} (default chord)", p.action),
                run: Bound::Preset(p),
                defaults: vec![k.to_owned()],
                fixed: false,
            }),
    );
    v
}

/// The claimants in claim order: bindables, config actions, bookmarks.
fn claimants(
    actions: &[ActionInfo],
    bookmarks: &[crate::bookmarks::Bookmark],
) -> Vec<Claimant<Bound>> {
    let mut v: Vec<Claimant<Bound>> = bindables()
        .into_iter()
        .map(|b| Claimant {
            id: b.id,
            keys: b.defaults,
            run: b.run,
            kind: ClaimKind::Bindable { fixed: b.fixed },
        })
        .collect();
    v.extend(actions.iter().map(|a| Claimant {
        id: a.id.clone(),
        keys: a.keys.clone(),
        run: Bound::Action(a.id.clone()),
        kind: ClaimKind::Config,
    }));
    // Bookmarks (WP-9): their record's `keys`, checked like an action's.
    v.extend(bookmarks.iter().map(|b| Claimant {
        id: bookmark_id(&b.path),
        keys: b.keys.clone(),
        run: Bound::Bookmark(b.path.clone()),
        kind: ClaimKind::Config,
    }));
    v
}

/// Resolves the effective keymap from `[keys]` `overrides` (id → chords; `[]` unbinds), the
/// editor defaults and the config actions' `keys`. Malformed and reserved
/// ([`super::chord::text_editing`]) chords become notices; the same chord listed twice by one
/// bindable is collapsed; a chord claimed by two or more distinct bindables is registered for
/// none of them, every claimant is marked [`Keymap::conflicted`] and gets a notice naming the
/// chord.
pub fn resolve(overrides: &BTreeMap<String, Vec<String>>, actions: &[ActionInfo]) -> Keymap {
    resolve_all(overrides, actions, &[])
}

/// [`resolve`] with the bookmarks' chords as claimants too (plan editor-ui-round-3 WP-9): the
/// same conflict policy covers them against every other claimant.
pub fn resolve_all(
    overrides: &BTreeMap<String, Vec<String>>,
    actions: &[ActionInfo],
    bookmarks: &[crate::bookmarks::Bookmark],
) -> Keymap {
    editor_model::keymap::resolve(overrides, &claimants(actions, bookmarks))
}

/// `entries` with each entry's chords taken from the effective keymap (an action entry shows
/// its default-action preset chords first, as `build_entries` does).
pub fn apply_keys(entries: &mut [Entry], km: &Keymap) {
    for e in entries {
        let get = |id: &str| km.chords.get(id).cloned().unwrap_or_default();
        e.keys = match &e.id {
            EntryId::Client(c) => {
                if km.chords.contains_key(&c.id()) {
                    get(&c.id())
                } else {
                    continue;
                }
            }
            EntryId::Action(a) => {
                let mut k = get(&format!("preset.{a}"));
                k.extend(get(a));
                k
            }
            EntryId::Bookmark(p) => get(&bookmark_id(p)),
        };
    }
}
