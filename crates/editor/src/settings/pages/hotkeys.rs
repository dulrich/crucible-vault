//! The Hotkeys page (plan `editor-settings-tab` WP-4): one table of every bindable command —
//! client commands, default-action presets and the loaded config actions — with its effective
//! chord(s), where they come from, a "record chord" capture, Clear and Reset-to-default, and the
//! resolver's conflicts inline.
//!
//! Edits never write anything by themselves: a client command / preset edit goes into the
//! `editor.toml` [`ConfigFile`] `[keys]` table, a config action's edit into its `keys` in the
//! `actions.toml` [`ConfigFile`] — both through the WP-2 save loop (the user presses Save).
//!
//! Crucible reference: Obsidian's own Hotkeys settings tab (Crucible registered commands and let
//! Obsidian bind them, `https://github.com/dulrich/obsidian-crucible/blob/master/src/main.ts` command registration). The
//! Obsidian-induced part — a separate hotkeys store outside the vault config — is dropped: the
//! keymap is the vault's `editor.toml` + `actions.toml`.

use std::sync::Arc;

use cv_protocol::ActionInfo;
use cv_schema::{ConfigDoc, Path};
use iced::widget::{column, container, row, text, text_input};
use iced::{Element, Length};

use super::PageCx;
use crate::icons::Icon;
use crate::message::Message;
use crate::palette::chord::{self, Chord};
use crate::palette::{KeySource, Keymap, bindables};
use crate::settings::editor_prefs::EDITOR_PATH;
use crate::settings::widgets::{small, text_button};
use crate::settings::{DocEdit, ScalarValue, SettingsMsg, file_header, save_bar};
use crate::theme::{Tokens, icon_button};

/// The `actions.toml` vault path.
pub const ACTIONS_PATH: &str = cv_schema::config_path!("actions.toml");

/// The page intro line.
pub const INTRO: &str = "Every bindable command. Record: the next Ctrl/Alt chord becomes its chord (Esc cancels). Editor commands are stored in editor.toml [keys], config actions in their actions.toml keys, bookmarks in their bookmarks.toml keys; every change saves automatically.";

/// Which row a Hotkeys edit targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A client command or preset (`[keys]` id).
    Command(String),
    /// A config action (its id).
    Action(String),
    /// Plan editor-ui-round-3 WP-9: a bookmark (its path); its chord is written to the
    /// bookmark's `keys` through the shared `bookmarks.toml` CAS write right away.
    Bookmark(String),
}

impl Target {
    /// The keymap id of the row.
    pub fn id(&self) -> String {
        match self {
            Target::Command(i) | Target::Action(i) => i.clone(),
            Target::Bookmark(p) => crate::palette::bookmark_id(p),
        }
    }
}

/// Hotkeys page messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyMsg {
    /// Start recording a chord for the row.
    Record(Target),
    /// Stop recording.
    Cancel,
    /// Unbind the row (`[]` for a command; drop `keys` for an action).
    Clear(Target),
    /// Drop the command's `[keys]` override (back to the editor default).
    Reset(String),
    /// Plan round-10 WP-5: the text filter's new value.
    Filter(String),
    /// Start "Find by chord": the next Ctrl/Alt chord becomes the chord filter (never an edit).
    FindChord,
    /// Clear both the text and the chord filter.
    ClearFilter,
}

/// The Hotkeys page state: the capture, its last note, and snapshots of the effective keymap
/// and the loaded action list (refreshed by the app whenever the palette rebuilds them).
#[derive(Debug, Clone, Default)]
pub struct HotkeyState {
    pub recording: Option<Target>,
    pub note: Option<String>,
    pub keymap: Arc<Keymap>,
    pub actions: Vec<ActionInfo>,
    /// WP-9: the bookmarks snapshot (their rows follow the config actions).
    pub bookmarks: Vec<crate::bookmarks::Bookmark>,
    /// Round-10 WP-5: the text filter (command title / id, case-insensitive substring).
    pub filter: String,
    /// The chord filter: only rows bound to this chord show.
    pub chord_filter: Option<Chord>,
    /// "Find by chord" is waiting for a chord (distinct from per-row `recording`: the chord
    /// is routed to the filter and never reaches the owner-reject / write path).
    pub searching: bool,
}

impl HotkeyState {
    /// Starts a capture.
    pub fn record(&mut self, t: Target) {
        self.recording = Some(t);
        self.searching = false;
        self.note = None;
    }

    /// Cancels a capture (per-row or the chord search).
    pub fn cancel(&mut self) {
        self.recording = None;
        self.searching = false;
    }

    /// Starts "Find by chord".
    pub fn find_chord(&mut self) {
        self.recording = None;
        self.searching = true;
        self.note = None;
    }

    /// Whether any capture (per-row or chord search) is waiting for a chord.
    pub fn capturing(&self) -> bool {
        self.recording.is_some() || self.searching
    }

    /// The chord search receives `c`: it becomes the chord filter.
    pub fn search_chord(&mut self, c: &Chord) {
        self.searching = false;
        self.chord_filter = Some(c.clone());
    }

    /// Clears the text and chord filters.
    pub fn clear_filter(&mut self) {
        self.filter.clear();
        self.chord_filter = None;
        self.searching = false;
    }
}

/// The rows the filters leave: the text filter (title or id, case-insensitive substring) and,
/// when set, only rows bound to the chord filter (its effective chords, unsaved edits or claims).
pub fn filtered(st: &HotkeyState, rows: Vec<HotkeyRow>) -> Vec<HotkeyRow> {
    let q = st.filter.trim().to_lowercase();
    let claimed = st
        .chord_filter
        .as_ref()
        .and_then(|c| st.keymap.claims.get(c));
    rows.into_iter()
        .filter(|r| {
            q.is_empty()
                || r.title.to_lowercase().contains(&q)
                || r.target.id().to_lowercase().contains(&q)
        })
        .filter(|r| {
            let Some(c) = &st.chord_filter else {
                return true;
            };
            claimed.is_some_and(|ids| ids.contains(&r.target.id()))
                || r.chords.iter().any(|k| chord::parse(k).as_ref() == Some(c))
        })
        .collect()
}

/// The "unbound" line shown when a chord filter leaves no row.
pub fn unbound_line(st: &HotkeyState, shown: &[HotkeyRow]) -> Option<String> {
    let c = st.chord_filter.as_ref()?;
    shown.is_empty().then(|| format!("{c}: unbound"))
}

/// One table row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyRow {
    pub target: Target,
    pub title: String,
    pub chords: Vec<String>,
    pub source: KeySource,
    /// Text-editing commands (Save / Undo / Redo): shown, not editable.
    pub fixed: bool,
    pub notices: Vec<String>,
    /// One of its chords is claimed by another bindable too, so it is registered for none
    /// (plan `editor-feedback-round-2` WP-1 conflict policy).
    pub conflicted: bool,
}

fn keys_path(id: &str) -> Path {
    Path::root().key("keys").key(id)
}

/// The `[[action]]` index whose `id` is `id` in the `actions.toml` doc.
pub fn action_index(doc: &ConfigDoc, id: &str) -> Option<usize> {
    let list = Path::root().key("action");
    (0..doc.len(&list)).find(|&i| doc.get_str(&list.index(i).key("id")) == Some(id))
}

/// The table: editor commands then config actions. `actions_doc` (the loaded `actions.toml`)
/// shows an action's unsaved chord edits; otherwise its `ActionInfo.keys` as bound.
pub fn rows(st: &HotkeyState, actions_doc: Option<&ConfigDoc>) -> Vec<HotkeyRow> {
    let km = &st.keymap;
    let notices = |id: &str| km.row_notices.get(id).cloned().unwrap_or_default();
    let mut v: Vec<HotkeyRow> = bindables()
        .into_iter()
        .map(|b| HotkeyRow {
            chords: km.chords.get(&b.id).cloned().unwrap_or_default(),
            source: if km.overridden.contains(&b.id) {
                KeySource::EditorToml
            } else {
                KeySource::Default
            },
            notices: notices(&b.id),
            conflicted: km.conflicted.contains(&b.id),
            fixed: b.fixed,
            title: b.title,
            target: Target::Command(b.id),
        })
        .collect();
    v.extend(st.actions.iter().map(|a| {
        let pending = actions_doc.and_then(|d| {
            let i = action_index(d, &a.id)?;
            Some(
                d.get_str_list(&Path::root().key("action").index(i).key("keys"))
                    .unwrap_or_default(),
            )
        });
        let chords = match pending {
            Some(p) if p != a.keys => p,
            _ => km.chords.get(&a.id).cloned().unwrap_or_default(),
        };
        HotkeyRow {
            target: Target::Action(a.id.clone()),
            title: a.title.clone(),
            chords,
            source: KeySource::ActionsToml,
            fixed: false,
            notices: notices(&a.id),
            conflicted: km.conflicted.contains(&a.id),
        }
    }));
    v.extend(st.bookmarks.iter().map(|b| {
        let id = crate::palette::bookmark_id(&b.path);
        HotkeyRow {
            target: Target::Bookmark(b.path.clone()),
            title: format!("Open bookmark: {}", b.name),
            chords: km.chords.get(&id).cloned().unwrap_or_default(),
            source: KeySource::BookmarksToml,
            fixed: false,
            notices: notices(&id),
            conflicted: km.conflicted.contains(&id),
        }
    }));
    v
}

/// The bindable other than `t` that already claims `c` — its row title — counting the
/// effective keymap's claims (bound or conflicted) and the unsaved chord edits `rows` shows.
/// Interactive assignment of such a chord is refused (plan `editor-feedback-round-2` WP-1).
pub fn owner(
    st: &HotkeyState,
    actions_doc: Option<&ConfigDoc>,
    t: &Target,
    c: &Chord,
) -> Option<String> {
    let id_of = Target::id;
    let me = id_of(t);
    let rows = rows(st, actions_doc);
    let claimed = st.keymap.claims.get(c);
    rows.into_iter()
        .filter(|r| r.target != *t && id_of(&r.target) != me)
        .find(|r| {
            claimed.is_some_and(|ids| ids.contains(&id_of(&r.target)))
                || r.chords.iter().any(|k| chord::parse(k).as_ref() == Some(c))
        })
        .map(|r| r.title)
}

/// Whether `c` may be bound at all (`Err`: the note to show).
pub fn check(c: &Chord) -> Result<(), String> {
    if chord::text_editing(c) {
        return Err(format!("{c} is reserved (not bound)"));
    }
    if !c.command && !c.alt {
        return Err(format!("{c} needs Ctrl or Alt"));
    }
    Ok(())
}

/// The document edit a capture of `c` for `t` makes: `(file, edit)`. `Err` = a note to show
/// (reserved chord, unknown action).
pub fn record_edit(
    t: &Target,
    c: &Chord,
    actions_doc: Option<&ConfigDoc>,
) -> Result<(String, DocEdit), String> {
    check(c)?;
    let list = ScalarValue::StrList(vec![c.to_string()]);
    match t {
        Target::Command(id) => Ok((EDITOR_PATH.to_owned(), DocEdit::Set(keys_path(id), list))),
        Target::Action(id) => {
            let i = action_doc_index(actions_doc, id)?;
            Ok((
                ACTIONS_PATH.to_owned(),
                DocEdit::Set(Path::root().key("action").index(i).key("keys"), list),
            ))
        }
        Target::Bookmark(_) => Err("bookmark chords are written through bookmarks.toml".into()),
    }
}

fn action_doc_index(doc: Option<&ConfigDoc>, id: &str) -> Result<usize, String> {
    let doc = doc.ok_or_else(|| "actions.toml is not loaded".to_owned())?;
    action_index(doc, id).ok_or_else(|| format!("action \"{id}\" is not in actions.toml"))
}

/// The edit Clear makes: a command gets `[]` (unbound), an action loses its `keys` (`None` when
/// it has none).
pub fn clear_edit(
    t: &Target,
    actions_doc: Option<&ConfigDoc>,
) -> Result<Option<(String, DocEdit)>, String> {
    match t {
        Target::Command(id) => Ok(Some((
            EDITOR_PATH.to_owned(),
            DocEdit::Set(keys_path(id), ScalarValue::StrList(Vec::new())),
        ))),
        Target::Action(id) => {
            let i = action_doc_index(actions_doc, id)?;
            let p = Path::root().key("action").index(i).key("keys");
            Ok(actions_doc
                .and_then(|d| d.get(&p))
                .map(|_| (ACTIONS_PATH.to_owned(), DocEdit::Remove(p))))
        }
        Target::Bookmark(_) => Err("bookmark chords are written through bookmarks.toml".into()),
    }
}

/// The edit Reset makes: drop the `[keys]` override (`None` when there is none).
pub fn reset_edit(id: &str, editor_doc: Option<&ConfigDoc>) -> Option<(String, DocEdit)> {
    let p = keys_path(id);
    editor_doc
        .and_then(|d| d.get(&p))
        .map(|_| (EDITOR_PATH.to_owned(), DocEdit::Remove(p)))
}

fn hk(m: HotkeyMsg) -> Message {
    Message::Settings(SettingsMsg::Hotkey(m))
}

fn row_view<'a>(r: HotkeyRow, recording: bool, editable: bool, t: Tokens) -> Element<'a, Message> {
    let chords = if recording {
        text("press a chord… (Esc cancels)")
            .size(14)
            .color(t.accent_text.to_color())
    } else if r.chords.is_empty() {
        text("—").size(14).color(t.text_faint.to_color())
    } else {
        text(r.chords.join("  ")).size(14)
    };
    let title: Element<'a, Message> = if r.conflicted {
        row![
            text(r.title).size(14),
            text("conflict").size(12).color(t.warn_text.to_color()),
        ]
        .spacing(6)
        .width(Length::Fixed(280.0))
        .into()
    } else {
        text(r.title).size(14).width(Length::Fixed(280.0)).into()
    };
    let controls: Element<'a, Message> = if r.fixed {
        small("text-editing (fixed)", t)
    } else {
        let can = editable && !recording;
        let mut ctl = row![
            icon_button(
                Icon::Keyboard,
                can.then(|| hk(HotkeyMsg::Record(r.target.clone()))),
                t
            ),
            icon_button(
                Icon::X,
                can.then(|| hk(HotkeyMsg::Clear(r.target.clone()))),
                t
            ),
        ]
        .spacing(6);
        if let Target::Command(id) = &r.target {
            let reset = can && r.source == KeySource::EditorToml;
            ctl = ctl.push(text_button(
                "Reset",
                reset.then(|| hk(HotkeyMsg::Reset(id.clone()))),
                t,
            ));
        }
        ctl.into()
    };
    let mut col = column![
        row![
            title,
            container(chords).width(Length::Fixed(220.0)),
            container(small(r.source.label(), t)).width(Length::Fixed(100.0)),
            controls,
        ]
        .spacing(10)
        .height(crate::theme::CONTROL)
        .align_y(iced::Alignment::Center),
    ];
    for n in r.notices {
        col = col.push(text(n).size(12).color(t.warn_text.to_color()));
    }
    col.into()
}

/// The fixed-height notice strip shown above the Hotkeys page's scrollable (always present on
/// the page, empty when there is no note), so the conflict / "already assigned" / reserved
/// notice is visible wherever the list is scrolled and never resizes it.
pub fn notice_bar(st: &HotkeyState, t: Tokens) -> Element<'_, Message> {
    let n = st.note.as_deref().unwrap_or("");
    container(
        text(n)
            .size(13)
            .color(t.warn_text.to_color())
            .wrapping(iced::widget::text::Wrapping::None)
            .align_y(iced::alignment::Vertical::Center)
            .height(Length::Fill),
    )
    .height(crate::theme::CONTROL)
    .width(Length::Fill)
    .padding([0, 16])
    .clip(true)
    .into()
}

/// The search strip above the list: text filter, "Find by chord", the active chord and clear.
fn search_bar<'a>(st: &HotkeyState, t: Tokens) -> Element<'a, Message> {
    let find = if st.searching {
        text("press a chord… (Esc cancels)")
            .size(14)
            .color(t.accent_text.to_color())
            .into()
    } else {
        text_button("Find by chord", Some(hk(HotkeyMsg::FindChord)), t)
    };
    let mut r = row![
        text_input("filter commands", &st.filter)
            .size(14)
            .padding([8, 10])
            .width(Length::Fixed(260.0))
            .on_input(|v| hk(HotkeyMsg::Filter(v))),
        find,
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    if let Some(c) = &st.chord_filter {
        r = r.push(text(c.to_string()).size(14));
    }
    r.push(icon_button(
        Icon::X,
        (st.chord_filter.is_some() || !st.filter.is_empty() || st.searching)
            .then(|| hk(HotkeyMsg::ClearFilter)),
        t,
    ))
    .into()
}

/// The page.
pub fn view(cx: PageCx<'_>) -> Element<'_, Message> {
    let t = cx.t;
    let st = &cx.s.hotkeys;
    let ed = cx.s.file(EDITOR_PATH);
    let ac = cx.s.file(ACTIONS_PATH);
    let bm = cx.s.file(crate::bookmarks::BOOKMARKS_PATH);
    let editable = ed.is_some_and(|f| f.doc().is_some() && !f.saving());
    // The capture note lives in [`notice_bar`], outside the scrollable (plan editor-ui-round-3
    // WP-7): setting or clearing it never changes the list's height or scroll offset.
    let mut col = column![text(INTRO).size(13).color(t.text_faint.to_color())].spacing(8);
    col = col.push(search_bar(st, t));
    let shown = filtered(st, rows(st, ac.and_then(|f| f.doc())));
    if let Some(u) = unbound_line(st, &shown) {
        col = col.push(text(u).size(14).color(t.text_faint.to_color()));
    }
    for r in shown {
        let rec = st.recording.as_ref() == Some(&r.target);
        let ok = match &r.target {
            Target::Command(_) => editable,
            Target::Action(_) => ac.is_some_and(|f| f.doc().is_some() && !f.saving()),
            Target::Bookmark(_) => {
                bm.is_some_and(|f| f.doc().is_some() && !f.saving() && !f.dirty())
            }
        };
        col = col.push(row_view(r, rec, ok, t));
    }
    for f in [ed, ac].into_iter().flatten() {
        col = col.push(file_header(f, t));
        col = col.push(save_bar(f, t));
    }
    col.into()
}
