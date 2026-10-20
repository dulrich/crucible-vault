//! Key chords (pure): the grammar `ActionInfo.keys` uses,
//! the canonical spelling, and the editor's reserved defaults.
//!
//! Grammar: `Mod+…+Key`, `+`-separated, case-insensitive. Modifiers: `Ctrl`/`Control`/`Mod`/
//! `Cmd`/`Command`/`Meta`/`Super` → the platform command key (`Modifiers::command()`), `Shift`,
//! `Alt`/`Option`. Key: one character, or a name (`Enter`, `Escape`/`Esc`, `Tab`, `Space`,
//! `Backspace`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`, `Up`/`Down`/`Left`/`Right`,
//! `F1`…`F12`). Exactly one key; modifiers may repeat.

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
    // command, bound in `Keymap::default()`).
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

/// Chords no override or action may bind (plan `editor-settings-tab` WP-4 "Resolver
/// precedence"): the inert `Ctrl+Shift+,`, the `text_editor` editing chords (save, undo/redo,
/// select-all, clipboard, Ctrl+Enter submit) and the pane back/forward chords. The other
/// [`RESERVED`] entries are editor defaults, rebindable through `editor.toml` `[keys]`.
pub const TEXT_EDITING: &[&str] = &[
    "Ctrl+Shift+,",
    "Ctrl+S",
    "Ctrl+Z",
    "Ctrl+Shift+Z",
    "Ctrl+Y",
    "Ctrl+A",
    "Ctrl+C",
    "Ctrl+V",
    "Ctrl+X",
    "Ctrl+Enter",
    "Ctrl+Alt+Left",
    "Ctrl+Alt+Right",
];

/// Named keys the source editor uses for motion and editing (without Alt they never bind).
const EDITING_KEYS: [&str; 13] = [
    "left",
    "right",
    "up",
    "down",
    "home",
    "end",
    "pageup",
    "pagedown",
    "backspace",
    "delete",
    "enter",
    "tab",
    "escape",
];

/// Whether `c` stays unrebindable: a [`TEXT_EDITING`] chord, or an editing/motion key without
/// Alt — except Ctrl+PageUp / Ctrl+PageDown (Ctrl, no Shift, no Alt; plan editor-ui-round-3
/// WP-5, audit E2), which the text editor does not use and which are the Go to top / bottom
/// defaults. Plain and Shift+PageUp/PageDown (scroll / select) and Ctrl+Shift+PageUp/PageDown
/// stay reserved.
pub fn text_editing(c: &Chord) -> bool {
    let page_jump =
        c.command && !c.shift && !c.alt && matches!(c.key.as_str(), "pageup" | "pagedown");
    TEXT_EDITING
        .iter()
        .filter_map(|r| parse(r))
        .any(|r| &r == c)
        || (!c.alt && !page_jump && EDITING_KEYS.contains(&c.key.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_canonical_spelling_and_reserved() {
        let c = parse("cmd + shift + k").expect("parse");
        assert_eq!(
            (c.command, c.shift, c.alt, c.key.as_str()),
            (true, true, false, "k")
        );
        assert_eq!(c.to_string(), "Ctrl+Shift+K");
        assert_eq!(
            parse("Alt+ArrowLeft").map(|c| c.to_string()),
            Some("Alt+Left".into())
        );
        assert_eq!(parse("Ctrl+F13"), None);
        assert!(reserved(&parse("Ctrl+P").expect("p")));
        assert!(text_editing(&parse("Ctrl+C").expect("p")));
        assert!(!text_editing(&parse("Ctrl+PageUp").expect("p")));
        assert!(text_editing(&parse("Shift+PageUp").expect("p")));
    }
}
