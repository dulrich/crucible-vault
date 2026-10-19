//! Key chords: the grammar `ActionInfo.keys` uses and the editor's reserved defaults.
//!
//! Grammar: `Mod+…+Key`, `+`-separated, case-insensitive. Modifiers: `Ctrl`/`Control`/`Mod`/
//! `Cmd`/`Command`/`Meta`/`Super` → the platform command key (`Modifiers::command()`), `Shift`,
//! `Alt`/`Option`. Key: one character, or a name (`Enter`, `Escape`/`Esc`, `Tab`, `Space`,
//! `Backspace`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`, `Up`/`Down`/`Left`/`Right`,
//! `F1`…`F12`). Exactly one key; modifiers may repeat.
//!
//! WP-R19: the pure grammar lives in [`editor_model::chord`]; this module adds the iced key-event
//! conversion.

use iced::keyboard::{Key, Modifiers, key::Named};

pub use editor_model::chord::{Chord, RESERVED, TEXT_EDITING, parse, reserved, text_editing};

/// The chord of a key press, or `None` for keys a chord cannot name.
pub fn from_event(key: &Key, modifiers: Modifiers) -> Option<Chord> {
    let k = match key.as_ref() {
        Key::Character(c) => c.to_lowercase(),
        Key::Named(n) => match n {
            Named::Enter => "enter".into(),
            Named::Escape => "escape".into(),
            Named::Tab => "tab".into(),
            Named::Space => "space".into(),
            Named::Backspace => "backspace".into(),
            Named::Delete => "delete".into(),
            Named::Home => "home".into(),
            Named::End => "end".into(),
            Named::PageUp => "pageup".into(),
            Named::PageDown => "pagedown".into(),
            Named::ArrowUp => "up".into(),
            Named::ArrowDown => "down".into(),
            Named::ArrowLeft => "left".into(),
            Named::ArrowRight => "right".into(),
            Named::F1 => "f1".into(),
            Named::F2 => "f2".into(),
            Named::F3 => "f3".into(),
            Named::F4 => "f4".into(),
            Named::F5 => "f5".into(),
            Named::F6 => "f6".into(),
            Named::F7 => "f7".into(),
            Named::F8 => "f8".into(),
            Named::F9 => "f9".into(),
            Named::F10 => "f10".into(),
            Named::F11 => "f11".into(),
            Named::F12 => "f12".into(),
            _ => return None,
        },
        Key::Unidentified => return None,
    };
    Some(Chord {
        command: modifiers.command(),
        shift: modifiers.shift(),
        alt: modifiers.alt(),
        key: k,
    })
}
