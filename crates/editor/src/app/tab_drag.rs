//! Plan round-10 WP-8: the tab-drag state machine. A press on a tab header arms it; cursor
//! movement past [`THRESHOLD`] starts the drag; the release drops (over a target), cancels
//! (elsewhere) or — before the threshold — is a plain click. Pure: `App` owns one [`TabDrag`]
//! and feeds it messages; the view only reads [`TabDrag::hover`] / [`TabDrag::active`].

use iced::Point;
use iced::widget::pane_grid::Pane;

/// Logical px the cursor must travel from its first observed position before a press is a drag.
pub const THRESHOLD: f32 = 5.0;

/// Where a release would drop the tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropTarget {
    /// Over a tab header: the tab lands at this index of that pane's strip.
    Slot(Pane, usize),
    /// Over a strip's empty area: appended to that pane.
    End(Pane),
    /// Over a pane body: appended to that pane (a no-op for the tab's own pane).
    Body(Pane),
}

/// What a release means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing was armed.
    None,
    /// Released before the threshold: a plain click (the tab was already activated on press).
    Click,
    /// Released over a target: move `from` to `to` at `at` (`None` = append).
    Drop {
        from: (Pane, usize),
        to: Pane,
        at: Option<usize>,
    },
    /// Released over nothing, or over its own pane's body.
    Cancel,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
enum Phase {
    #[default]
    Idle,
    /// Pressed; `origin` = the first cursor position seen since (the press carries none).
    Pressed {
        from: (Pane, usize),
        origin: Option<Point>,
    },
    Dragging {
        from: (Pane, usize),
    },
}

/// The tab-drag tracker.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct TabDrag {
    phase: Phase,
    hover: Option<DropTarget>,
}

impl TabDrag {
    /// A header was pressed.
    pub fn press(&mut self, from: (Pane, usize)) {
        self.phase = Phase::Pressed { from, origin: None };
        self.hover = None;
    }

    /// The cursor moved to `p`; true when this move started the drag.
    pub fn moved(&mut self, p: Point) -> bool {
        let Phase::Pressed { from, origin } = self.phase else {
            return false;
        };
        let Some(o) = origin else {
            self.phase = Phase::Pressed {
                from,
                origin: Some(p),
            };
            return false;
        };
        if (p.x - o.x).hypot(p.y - o.y) > THRESHOLD {
            self.phase = Phase::Dragging { from };
            return true;
        }
        false
    }

    /// The cursor entered `t`.
    pub fn enter(&mut self, t: DropTarget) {
        if self.active() {
            self.hover = Some(t);
        }
    }

    /// The cursor left `t` (a stale exit after entering the next target is ignored).
    pub fn leave(&mut self, t: DropTarget) {
        if self.hover == Some(t) {
            self.hover = None;
        }
    }

    /// Something is pressed or dragging (the cursor/release subscriptions are live).
    pub fn armed(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// A drag is under way (past the threshold).
    pub fn active(&self) -> bool {
        matches!(self.phase, Phase::Dragging { .. })
    }

    /// The target a release would drop on.
    pub fn hover(&self) -> Option<DropTarget> {
        self.hover
    }

    /// Esc / abort: back to idle.
    pub fn cancel(&mut self) {
        *self = Self::default();
    }

    /// The button was released.
    pub fn release(&mut self) -> Outcome {
        let (phase, hover) = (self.phase, self.hover);
        *self = Self::default();
        match phase {
            Phase::Idle => Outcome::None,
            Phase::Pressed { .. } => Outcome::Click,
            Phase::Dragging { from } => match hover {
                None => Outcome::Cancel,
                Some(DropTarget::Slot(p, i)) => Outcome::Drop {
                    from,
                    to: p,
                    at: Some(i),
                },
                Some(DropTarget::End(p)) => Outcome::Drop {
                    from,
                    to: p,
                    at: None,
                },
                Some(DropTarget::Body(p)) if p == from.0 => Outcome::Cancel,
                Some(DropTarget::Body(p)) => Outcome::Drop {
                    from,
                    to: p,
                    at: None,
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> Pane {
        let (state, p) = iced::widget::pane_grid::State::new(());
        let _ = state;
        p
    }

    fn pt(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn release_before_threshold_is_a_click() {
        let p = pane();
        let mut d = TabDrag::default();
        d.press((p, 0));
        assert!(!d.moved(pt(10.0, 10.0)));
        assert!(!d.moved(pt(12.0, 11.0)));
        assert_eq!(d.release(), Outcome::Click);
        assert_eq!(d.release(), Outcome::None);
    }

    #[test]
    fn past_threshold_drags_and_drops_on_a_slot() {
        let p = pane();
        let mut d = TabDrag::default();
        d.press((p, 1));
        d.enter(DropTarget::Slot(p, 0));
        assert_eq!(d.hover(), None, "no hover before the drag starts");
        assert!(!d.moved(pt(0.0, 0.0)));
        assert!(d.moved(pt(20.0, 0.0)));
        d.enter(DropTarget::Slot(p, 0));
        assert_eq!(
            d.release(),
            Outcome::Drop {
                from: (p, 1),
                to: p,
                at: Some(0)
            }
        );
        assert!(!d.armed());
    }

    #[test]
    fn stale_leave_keeps_the_new_target_and_unhovered_release_cancels() {
        let p = pane();
        let mut d = TabDrag::default();
        d.press((p, 0));
        d.moved(pt(0.0, 0.0));
        d.moved(pt(30.0, 0.0));
        d.enter(DropTarget::Slot(p, 1));
        d.enter(DropTarget::Slot(p, 0));
        d.leave(DropTarget::Slot(p, 1));
        assert_eq!(d.hover(), Some(DropTarget::Slot(p, 0)));
        d.leave(DropTarget::Slot(p, 0));
        assert_eq!(d.release(), Outcome::Cancel);
    }

    #[test]
    fn own_body_cancels_and_cancel_resets() {
        let p = pane();
        let mut d = TabDrag::default();
        d.press((p, 0));
        d.moved(pt(0.0, 0.0));
        d.moved(pt(30.0, 0.0));
        d.enter(DropTarget::Body(p));
        assert_eq!(d.release(), Outcome::Cancel);
        d.press((p, 0));
        d.cancel();
        assert_eq!(d.release(), Outcome::None);
    }
}
