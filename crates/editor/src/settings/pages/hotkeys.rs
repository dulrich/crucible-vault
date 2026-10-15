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
use iced::widget::{column, container, row, text};
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
pub const ACTIONS_PATH: &str = "_custom/crucible-vault/actions.toml";

/// The page intro line.
pub const INTRO: &str = "Every bindable command. Record: the next Ctrl/Alt chord becomes its chord (Esc cancels). Editor commands are stored in editor.toml [keys], config actions in their actions.toml keys; nothing is written until Save.";

/// Which row a Hotkeys edit targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A client command or preset (`[keys]` id).
    Command(String),
    /// A config action (its id).
    Action(String),
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
}

/// The Hotkeys page state: the capture, its last note, and snapshots of the effective keymap
/// and the loaded action list (refreshed by the app whenever the palette rebuilds them).
#[derive(Debug, Clone, Default)]
pub struct HotkeyState {
    pub recording: Option<Target>,
    pub note: Option<String>,
    pub keymap: Arc<Keymap>,
    pub actions: Vec<ActionInfo>,
}

impl HotkeyState {
    /// Starts a capture.
    pub fn record(&mut self, t: Target) {
        self.recording = Some(t);
        self.note = None;
    }

    /// Cancels a capture.
    pub fn cancel(&mut self) {
        self.recording = None;
    }
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
        }
    }));
    v
}

/// The document edit a capture of `c` for `t` makes: `(file, edit)`. `Err` = a note to show
/// (reserved chord, unknown action).
pub fn record_edit(
    t: &Target,
    c: &Chord,
    actions_doc: Option<&ConfigDoc>,
) -> Result<(String, DocEdit), String> {
    if chord::text_editing(c) {
        return Err(format!("{c} is reserved (not bound)"));
    }
    if !c.command && !c.alt {
        return Err(format!("{c} needs Ctrl or Alt"));
    }
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
            text(r.title).size(14).width(Length::Fixed(280.0)),
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

/// The page.
pub fn view(cx: PageCx<'_>) -> Element<'_, Message> {
    let t = cx.t;
    let st = &cx.s.hotkeys;
    let ed = cx.s.file(EDITOR_PATH);
    let ac = cx.s.file(ACTIONS_PATH);
    let editable = ed.is_some_and(|f| f.doc.is_some() && !f.saving);
    let mut col = column![text(INTRO).size(13).color(t.text_faint.to_color())].spacing(8);
    if let Some(n) = &st.note {
        col = col.push(text(n.as_str()).size(13).color(t.warn_text.to_color()));
    }
    for r in rows(st, ac.and_then(|f| f.doc.as_ref())) {
        let rec = st.recording.as_ref() == Some(&r.target);
        let ok = match &r.target {
            Target::Command(_) => editable,
            Target::Action(_) => ac.is_some_and(|f| f.doc.is_some() && !f.saving),
        };
        col = col.push(row_view(r, rec, ok, t));
    }
    for f in [ed, ac].into_iter().flatten() {
        col = col.push(file_header(f, t));
        col = col.push(save_bar(f, t, cx.mode));
    }
    col.into()
}
