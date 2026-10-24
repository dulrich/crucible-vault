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
///
/// The drop targets nest (a slot inside its strip, the strip possibly inside the pane body), and
/// iced reports an outer area's enter after the inner one's, so each layer is tracked on its
/// own and the most specific wins: entering a strip over its first tab lands on that slot, not
/// at the end.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct TabDrag {
    phase: Phase,
    slot: Option<DropTarget>,
    strip: Option<DropTarget>,
    body: Option<DropTarget>,
    /// The tab header under the cursor (by tab id), for the rollover fill.
    header: Option<u64>,
}

impl TabDrag {
    /// A header was pressed.
    pub fn press(&mut self, from: (Pane, usize)) {
        self.phase = Phase::Pressed { from, origin: None };
        self.clear_targets();
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
            *self.layer(t) = Some(t);
        }
    }

    fn layer(&mut self, t: DropTarget) -> &mut Option<DropTarget> {
        match t {
            DropTarget::Slot(..) => &mut self.slot,
            DropTarget::End(_) => &mut self.strip,
            DropTarget::Body(_) => &mut self.body,
        }
    }

    fn clear_targets(&mut self) {
        (self.slot, self.strip, self.body) = (None, None, None);
    }

    /// The cursor left `t` (a stale exit after entering the next target is ignored).
    pub fn leave(&mut self, t: DropTarget) {
        let layer = self.layer(t);
        if *layer == Some(t) {
            *layer = None;
        }
    }

    /// The cursor entered tab header `id` (rollover fill).
    pub fn header_enter(&mut self, id: u64) {
        self.header = Some(id);
    }

    /// The cursor left tab header `id` (a stale exit after entering the next one is ignored).
    pub fn header_leave(&mut self, id: u64) {
        if self.header == Some(id) {
            self.header = None;
        }
    }

    /// The tab header under the cursor, outside a drag.
    pub fn header_hover(&self) -> Option<u64> {
        if self.active() { None } else { self.header }
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
        self.slot.or(self.strip).or(self.body)
    }

    /// Esc / abort: back to idle.
    pub fn cancel(&mut self) {
        self.phase = Phase::Idle;
        self.clear_targets();
    }

    /// The button was released.
    pub fn release(&mut self) -> Outcome {
        let (phase, hover) = (self.phase, self.hover());
        self.cancel();
        match phase {
            Phase::Idle => Outcome::None,
            Phase::Pressed { .. } => Outcome::Click,
            Phase::Dragging { from } => match Self::drop_for(from, hover) {
                Some((to, at)) => Outcome::Drop { from, to, at },
                None => Outcome::Cancel,
            },
        }
    }

    /// Where a release over `hover` would land the tab at `from`: the destination pane and the
    /// insert index in that strip once the tab is removed (`None` = append); `None` = no drop.
    fn drop_for(from: (Pane, usize), hover: Option<DropTarget>) -> Option<(Pane, Option<usize>)> {
        match hover? {
            DropTarget::Slot(p, i) => Some((p, Some(i))),
            DropTarget::End(p) => Some((p, None)),
            DropTarget::Body(p) if p == from.0 => None,
            DropTarget::Body(p) => Some((p, None)),
        }
    }

    /// Plan round-11 WP-6: the strip of `pane` (holding `len` tabs) as it is drawn mid-drag. The
    /// dragged tab sits at its would-land slot in the hovered strip; its origin strip closes the
    /// gap, or keeps it when no drop is pending. Outside a drag it is the plain order. The drop
    /// commits exactly this layout (the same index feeds [`Outcome::Drop`]).
    pub fn layout(&self, pane: Pane, len: usize) -> Vec<Cell> {
        let Phase::Dragging { from } = self.phase else {
            return (0..len).map(Cell::Tab).collect();
        };
        let mut cells: Vec<Cell> = (0..len)
            .filter(|i| !(pane == from.0 && *i == from.1))
            .map(Cell::Tab)
            .collect();
        let at = match Self::drop_for(from, self.hover()) {
            Some((to, at)) if to == pane => Some(at.map_or(cells.len(), |a| a.min(cells.len()))),
            Some(_) => None,
            None if pane == from.0 => Some(from.1.min(cells.len())),
            None => None,
        };
        if let Some(at) = at {
            cells.insert(at, Cell::Dragged);
        }
        cells
    }

    /// The tab being dragged (past the threshold), as `(pane, index)` in the original order.
    pub fn dragged(&self) -> Option<(Pane, usize)> {
        match self.phase {
            Phase::Dragging { from } => Some(from),
            _ => None,
        }
    }
}

/// One drawn slot of a strip mid-drag: an original tab by index, or the dragged tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Tab(usize),
    Dragged,
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

    /// Entering another pane's strip over its first tab: iced reports the inner slot, then the
    /// strip and body around it. The slot wins (the tab lands first, not last); leaving the slot
    /// for the strip's empty area falls back to the end.
    #[test]
    fn nested_targets_prefer_the_slot_over_strip_and_body() {
        let (mut state, a) = iced::widget::pane_grid::State::new(());
        let (b, _) = state
            .split(iced::widget::pane_grid::Axis::Vertical, a, ())
            .expect("split");
        let mut d = TabDrag::default();
        d.press((a, 0));
        d.moved(pt(0.0, 0.0));
        d.moved(pt(30.0, 0.0));
        d.enter(DropTarget::End(a));
        d.enter(DropTarget::Slot(b, 0));
        d.enter(DropTarget::End(b));
        d.enter(DropTarget::Body(b));
        d.leave(DropTarget::End(a));
        assert_eq!(d.hover(), Some(DropTarget::Slot(b, 0)));
        d.leave(DropTarget::Slot(b, 0));
        assert_eq!(d.hover(), Some(DropTarget::End(b)));
        d.enter(DropTarget::Slot(b, 0));
        assert_eq!(
            d.release(),
            Outcome::Drop {
                from: (a, 0),
                to: b,
                at: Some(0)
            }
        );
        assert_eq!(d.hover(), None);
    }

    #[test]
    fn header_rollover_ignores_stale_exit_and_hides_mid_drag() {
        let p = pane();
        let mut d = TabDrag::default();
        d.header_enter(1);
        d.header_enter(2);
        d.header_leave(1);
        assert_eq!(d.header_hover(), Some(2));
        d.press((p, 0));
        d.moved(pt(0.0, 0.0));
        d.moved(pt(30.0, 0.0));
        assert_eq!(d.header_hover(), None, "no rollover while dragging");
        d.header_leave(2);
        d.release();
        assert_eq!(d.header_hover(), None);
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
