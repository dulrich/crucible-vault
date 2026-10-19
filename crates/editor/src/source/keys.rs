//! Source editor key bindings and click mapping.

use iced::keyboard::{self, key};
use iced::widget::text_editor::{self, Action, Binding, KeyPress};

use crate::message::{Message, SourceKey, SourceMsg};

/// Ctrl+S / Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, then iced's defaults.
pub(super) fn key_binding(
    kp: KeyPress,
    km: &crate::palette::Keymap,
    link_open: bool,
) -> Option<Binding<Message>> {
    if !matches!(kp.status, text_editor::Status::Focused { .. }) {
        return None;
    }
    // Plan round-5 WP-9: the open `[[` list takes Up/Down/Enter/Tab/Esc (never the buffer).
    if link_open
        && kp.modifiers.is_empty()
        && let keyboard::Key::Named(named) = kp.key.as_ref()
    {
        use crate::message::LinkKey;
        let k = match named {
            key::Named::ArrowUp => Some(LinkKey::Up),
            key::Named::ArrowDown => Some(LinkKey::Down),
            key::Named::Enter => Some(LinkKey::Enter),
            key::Named::Tab => Some(LinkKey::Tab),
            key::Named::Escape => Some(LinkKey::Escape),
            _ => None,
        };
        if let Some(k) = k {
            return Some(Binding::Custom(Message::Source(SourceMsg::LinkKey(k))));
        }
    }
    if let keyboard::Key::Named(named) = kp.key.as_ref() {
        let m = kp.modifiers;
        let plain = !m.alt() && !m.logo();
        // editor-usability-round WP-3: Ctrl+Alt+Left/Right is pane back/forward, not word motion.
        if m.command() && m.alt() {
            let nav = match named {
                key::Named::ArrowLeft => Some(crate::message::PanesMsg::BackFocused),
                key::Named::ArrowRight => Some(crate::message::PanesMsg::ForwardFocused),
                _ => None,
            };
            if let Some(n) = nav {
                return Some(Binding::Custom(Message::Panes(n)));
            }
        }
        let k = match named {
            key::Named::Enter if plain && !m.command() && !m.shift() => Some(SourceKey::Enter),
            key::Named::Backspace if plain && m.command() => Some(SourceKey::DeleteWordBack),
            key::Named::Delete if plain && m.command() => Some(SourceKey::DeleteWordForward),
            key::Named::Home if plain && !m.command() => {
                Some(SourceKey::Home { select: m.shift() })
            }
            key::Named::End if plain && !m.command() => Some(SourceKey::End { select: m.shift() }),
            // plan editor-feedback-round-2 WP-4: Tab stays in the editor (indent / outdent).
            key::Named::Tab if plain && !m.command() => Some(if m.shift() {
                SourceKey::Outdent
            } else {
                SourceKey::Indent
            }),
            _ => None,
        };
        if let Some(k) = k {
            return Some(Binding::Custom(Message::Source(SourceMsg::Key(k))));
        }
    }
    if kp.modifiers.command()
        && let keyboard::Key::Character(c) = kp.key.as_ref()
    {
        let msg = match c.to_ascii_lowercase().as_str() {
            "z" if kp.modifiers.shift() => Some(SourceMsg::Redo),
            "z" => Some(SourceMsg::Undo),
            "y" => Some(SourceMsg::Redo),
            "s" => Some(SourceMsg::Save),
            // plan editor-ui-round-3 WP-5: line cut without a selection.
            "x" if !kp.modifiers.shift() && !kp.modifiers.alt() => Some(SourceMsg::Cut),
            // Plan editor-feedback-round-2 WP-9: image-aware paste (was iced's text Paste).
            "v" if !kp.modifiers.shift() && !kp.modifiers.alt() => Some(SourceMsg::Paste),
            _ => None,
        };
        if let Some(m) = msg {
            return Some(Binding::Custom(Message::Source(m)));
        }
    }
    // editor-usability-round WP-4: client chords (Ctrl+W/Q/F/P, Alt+1..8, …) reach the app even
    // while the editor is focused — `text_editor` would swallow some (Alt+digit inserts).
    // plan editor-settings-tab WP-4 (audit E1): decided from the effective keymap, so `[keys]`
    // overrides route here too (and a chord moved away no longer does).
    if (kp.modifiers.command() || kp.modifiers.alt())
        && let Some(c) = crate::palette::chord::from_event(&kp.key, kp.modifiers)
        && km.routed(&c)
    {
        return Some(Binding::Custom(Message::Palette(
            crate::message::PaletteMsg::Key(c),
        )));
    }
    Binding::from_key_press(kp)
}

/// `key_binding` for tests.
#[cfg(test)]
pub(crate) fn test_key_binding(kp: KeyPress) -> Option<Binding<Message>> {
    key_binding(kp, &crate::palette::Keymap::default(), false)
}

/// `key_binding` with the `[[` list open, for tests.
#[cfg(test)]
pub(crate) fn test_key_binding_link(kp: KeyPress) -> Option<Binding<Message>> {
    key_binding(kp, &crate::palette::Keymap::default(), true)
}

/// `key_binding` under a given keymap, for tests.
#[cfg(test)]
pub(crate) fn test_key_binding_with(
    kp: KeyPress,
    km: &crate::palette::Keymap,
) -> Option<Binding<Message>> {
    key_binding(kp, km, false)
}

/// plan editor-round-4 WP-2: iced maps a single click to `Action::Click` whatever the
/// modifiers; with Shift held it becomes a drag to the click point, which extends the
/// selection from its anchor (or the cursor when there is none).
pub fn shift_click(msg: SourceMsg, shift: bool) -> SourceMsg {
    match msg {
        SourceMsg::Action(Action::Click(p)) if shift => SourceMsg::Action(Action::Drag(p)),
        m => m,
    }
}

/// Plan usability-sprint-captures-links WP-3: what a Ctrl/Cmd+click at byte `col` of `line` in
/// note `path` does — a remote URL (bare or md-link) opens in the browser, a wikilink or vault
/// md-link opens the note exactly as Reading's `OpenLink` does; elsewhere nothing.
pub fn command_click_msg(path: &str, line: &str, col: usize) -> Option<Message> {
    use editor_model::link_at::{LinkAt, link_at};
    Some(match link_at(line, col)? {
        LinkAt::Url(u) => Message::OpenUrl(u),
        LinkAt::Md(u) if editor_model::render::is_remote(&u) => Message::OpenUrl(u),
        LinkAt::Md(target) | LinkAt::Wiki(target) => {
            Message::Reading(crate::message::ReadingMsg::OpenLink {
                src: path.to_owned(),
                target,
            })
        }
    })
}
