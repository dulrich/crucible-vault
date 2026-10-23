//! Plan round-10 WP-7: the bare-Alt tap tracker. Alt pressed alone arms it; any other key press
//! or mouse button press while armed disarms it; Alt released while armed is a tap (toggles the
//! menu bar). Pure: the subscription maps iced events to [`AltInput`], `App` owns one [`AltTap`].

/// One input the tracker cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltInput {
    /// The Alt key went down; `others_held` = Ctrl/Shift/Logo already down (never a bare Alt).
    AltDown { others_held: bool },
    /// The Alt key went up.
    AltUp,
    /// Any other key press or any mouse button press.
    Other,
}

/// The tap state machine.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AltTap {
    armed: bool,
    /// Alt is currently down (so an auto-repeat press never re-arms after a disarm).
    held: bool,
}

impl AltTap {
    /// Feeds one input; `true` when it completes a bare-Alt tap.
    pub fn feed(&mut self, input: AltInput) -> bool {
        match input {
            // Auto-repeat Alt presses keep it armed; a held modifier makes it a chord instead.
            AltInput::AltDown { others_held } => {
                if !self.held {
                    self.held = true;
                    self.armed = !others_held;
                }
                false
            }
            AltInput::AltUp => {
                self.held = false;
                std::mem::take(&mut self.armed)
            }
            AltInput::Other => {
                self.armed = false;
                false
            }
        }
    }
}

/// Maps a raw iced event (any status) to a tracker input.
pub fn input_of(
    e: iced::Event,
    _: iced::event::Status,
    _: iced::window::Id,
) -> Option<crate::message::Message> {
    use iced::keyboard::{Event as K, Key, key::Named};
    let input = match e {
        iced::Event::Keyboard(K::KeyPressed { key, modifiers, .. }) => {
            if key.as_ref() == Key::Named(Named::Alt) {
                AltInput::AltDown {
                    others_held: modifiers.command() || modifiers.shift() || modifiers.logo(),
                }
            } else {
                AltInput::Other
            }
        }
        iced::Event::Keyboard(K::KeyReleased { key, .. }) => {
            if key.as_ref() == Key::Named(Named::Alt) {
                AltInput::AltUp
            } else {
                return None;
            }
        }
        iced::Event::Mouse(iced::mouse::Event::ButtonPressed(_)) => AltInput::Other,
        _ => return None,
    };
    Some(crate::message::Message::AltKey(input))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOWN: AltInput = AltInput::AltDown { others_held: false };

    #[test]
    fn bare_alt_press_release_is_a_tap() {
        let mut t = AltTap::default();
        assert!(!t.feed(DOWN));
        assert!(t.feed(AltInput::AltUp));
        assert!(!t.feed(AltInput::AltUp), "a second release is not a tap");
    }

    #[test]
    fn alt_plus_key_or_mouse_is_not_a_tap() {
        let mut t = AltTap::default();
        t.feed(DOWN);
        t.feed(AltInput::Other);
        assert!(!t.feed(AltInput::AltUp));
    }

    #[test]
    fn ctrl_held_then_alt_is_not_a_tap() {
        let mut t = AltTap::default();
        t.feed(AltInput::AltDown { others_held: true });
        assert!(!t.feed(AltInput::AltUp));
    }

    #[test]
    fn auto_repeat_alt_presses_still_tap() {
        let mut t = AltTap::default();
        t.feed(DOWN);
        t.feed(DOWN);
        t.feed(DOWN);
        assert!(t.feed(AltInput::AltUp));
    }

    #[test]
    fn auto_repeat_after_a_chord_key_does_not_rearm() {
        let mut t = AltTap::default();
        t.feed(DOWN);
        t.feed(AltInput::Other);
        t.feed(DOWN);
        assert!(!t.feed(AltInput::AltUp));
    }

    #[test]
    fn a_key_before_alt_does_not_poison_the_next_tap() {
        let mut t = AltTap::default();
        t.feed(AltInput::Other);
        t.feed(DOWN);
        assert!(t.feed(AltInput::AltUp));
    }
}
