//! Key chords: the grammar `ActionInfo.keys` uses and the editor's reserved defaults.
//!
//! Grammar: `Mod+…+Key`, `+`-separated, case-insensitive. Modifiers: `Ctrl`/`Control`/`Mod`/
//! `Cmd`/`Command`/`Meta`/`Super` → the platform command key (`Modifiers::command()`), `Shift`,
//! `Alt`/`Option`. Key: one character, or a name (`Enter`, `Escape`/`Esc`, `Tab`, `Space`,
//! `Backspace`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`, `Up`/`Down`/`Left`/`Right`,
//! `F1`…`F12`). Exactly one key; modifiers may repeat.

use iced::keyboard::{Key, Modifiers, key::Named};

/// A normalized chord.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chord {
    pub command: bool,
    pub shift: bool,
    pub alt: bool,
    /// Lowercased character or canonical lowercase name.
    pub key: String,
}

impl std::fmt::Display for Chord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.command {
            f.write_str("Ctrl+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        let mut c = self.key.chars();
        match c.next() {
            Some(first) => {
                f.write_str(&first.to_uppercase().collect::<String>())?;
                f.write_str(c.as_str())
            }
            None => Ok(()),
        }
    }
}

const NAMES: [&str; 12] = [
    "enter",
    "escape",
    "tab",
    "space",
    "backspace",
    "delete",
    "home",
    "end",
    "pageup",
    "pagedown",
    "up",
    "down",
];

fn canonical_name(s: &str) -> Option<String> {
    let l = s.to_ascii_lowercase();
    let l = match l.as_str() {
        "esc" => "escape".to_owned(),
        "return" => "enter".to_owned(),
        "arrowup" => "up".to_owned(),
        "arrowdown" => "down".to_owned(),
        "arrowleft" | "left" => "left".to_owned(),
        "arrowright" | "right" => "right".to_owned(),
        "del" => "delete".to_owned(),
        other => other.to_owned(),
    };
    if NAMES.contains(&l.as_str()) || l == "left" || l == "right" {
        return Some(l);
    }
    if let Some(n) = l.strip_prefix('f')
        && let Ok(n) = n.parse::<u8>()
        && (1..=12).contains(&n)
    {
        return Some(l);
    }
    None
}

/// Parses a chord string; `None` when malformed.
pub fn parse(s: &str) -> Option<Chord> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last()?;
    let mut c = Chord {
        command: false,
        shift: false,
        alt: false,
        key: String::new(),
    };
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "mod" | "cmd" | "command" | "meta" | "super" => c.command = true,
            "shift" => c.shift = true,
            "alt" | "option" => c.alt = true,
            _ => return None,
        }
    }
    let mut chars = key.chars();
    c.key = match (chars.next(), chars.next()) {
        (Some(ch), None) => ch.to_lowercase().collect(),
        (Some(_), Some(_)) => canonical_name(key)?,
        (None, _) => return None,
    };
    Some(c)
}

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

/// Chords the editor itself owns (other listeners or `text_editor` handle them). Editor defaults
/// win: an action binding one of these is reported as a conflict and not bound.
pub const RESERVED: &[&str] = &[
    "Ctrl+P",
    "Ctrl+Shift+P",
    "Ctrl+O",
    "Ctrl+R",
    "Ctrl+F",
    "Ctrl+Shift+F",
    "Ctrl+W",
    "Ctrl+Q",
    "Alt+1",
    "Alt+2",
    "Alt+3",
    "Alt+4",
    "Alt+5",
    "Alt+6",
    "Alt+7",
    "Alt+8",
    // Reserved, inert (plan `editor-settings-tab`: Ctrl+, is now the Open settings client
    // command, resolved by `palette::client_chord`).
    "Ctrl+Shift+,",
    "Ctrl+E",
    "Ctrl+S",
    "Ctrl+Z",
    "Ctrl+Shift+Z",
    "Ctrl+Y",
    "Ctrl+A",
    "Ctrl+C",
    "Ctrl+V",
    "Ctrl+X",
    "Ctrl+Enter",
];

/// Whether `c` is an editor default.
pub fn reserved(c: &Chord) -> bool {
    RESERVED.iter().filter_map(|r| parse(r)).any(|r| &r == c)
}
