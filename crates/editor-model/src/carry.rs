//! Deferred source-editor placement: the
//! state machine behind a Reading → Source line carry and an edge scroll, as plain data.
//!
//! A scroll performed before the source editor is laid out at its size is lost (cosmic clamps
//! an unsized buffer's scroll to the top), so placement waits for the editor's layout report and
//! runs in passes (plan round-7 WP-4, round-8 WP-4): pass 1 puts the caret at the document end,
//! pass 2 moves it up to the target line (cosmic scrolls the line to the top), pass 3 scrolls up
//! to the `scroll_anchor` height. [`layout_pass`] is one transition.

/// A placement waiting for the source editor's next layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Carry {
    /// Scroll to the document's start (`bottom = false`) or end; wins over any line carry.
    Edge { bottom: bool },
    /// A line carry to `line` at `anchor` (fraction of the viewport height, 0 = top). With
    /// anchor 0 `line` is the already clamped top; otherwise it is the unclamped target and the
    /// anchor shift + viewport clamp happen once in pass 2 (audit M2). `at_end`: pass 1 ran.
    Line {
        line: usize,
        anchor: f32,
        at_end: bool,
    },
    /// Pass 3 of an anchored carry: after pass 2's layout put the line at the top, scroll up
    /// `rows` visual rows (the caret-follow of that layout would undo a scroll performed with
    /// the caret move).
    Shift { rows: usize },
}

/// What the editor performs on one layout pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Scroll fully to an edge (an overshooting scroll).
    Edge { bottom: bool },
    /// Move the caret to the document end (pass 1).
    CaretToEnd,
    /// Move the caret to `line`'s start; the tracked top line becomes `top` (pass 2).
    CaretToLine { line: usize, top: usize },
    /// Scroll up `rows` visual rows (pass 3).
    ScrollUp { rows: usize },
}

/// One transition: the step to perform now and the carry left for the next layout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pass {
    pub step: Option<Step>,
    pub next: Option<Carry>,
}

impl Pass {
    /// Another layout pass is needed (the caller re-sends the layout report).
    pub fn relayout(&self) -> bool {
        self.next.is_some()
    }
}

/// Plan round-8-intake-views-anchor WP-4: the visual rows from the viewport top to the anchor
/// height `anchor` (fraction) of a `visible`-row viewport — rounded, and at most one less than
/// the viewport so a caret on that row stays visible.
pub fn rows_at(anchor: f32, visible: usize) -> usize {
    let a = clamp_anchor(anchor);
    #[allow(clippy::cast_precision_loss)]
    let rows = (a * visible as f32).round();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rows = rows.max(0.0) as usize;
    rows.min(visible.saturating_sub(1))
}

/// An anchor fraction clamped to 0–1 (non-finite → 0).
pub fn clamp_anchor(anchor: f32) -> f32 {
    if anchor.is_finite() {
        anchor.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The carry a line placement leaves for the next layout, given the clamped `top` the
/// immediate scroll aimed at (see [`Carry::Line`]).
pub fn line_carry(line: usize, top: usize, anchor: f32) -> Carry {
    let anchor = clamp_anchor(anchor);
    Carry::Line {
        line: if anchor > 0.0 { line } else { top },
        anchor,
        at_end: false,
    }
}

/// One layout pass of `carry` for an editor whose last reachable top line is `max_top` and
/// whose viewport shows `visible` rows.
pub fn layout_pass(carry: Option<Carry>, max_top: usize, visible: usize) -> Pass {
    match carry {
        None => Pass {
            step: None,
            next: None,
        },
        Some(Carry::Edge { bottom }) => Pass {
            step: Some(Step::Edge { bottom }),
            next: None,
        },
        Some(Carry::Line {
            line,
            anchor,
            at_end: false,
        }) => Pass {
            step: Some(Step::CaretToEnd),
            next: Some(Carry::Line {
                line,
                anchor,
                at_end: true,
            }),
        },
        Some(Carry::Line {
            line,
            anchor,
            at_end: true,
        }) => {
            // The line sits at the top (or the view at the end when it is past the last top);
            // move it down to the anchor height by scrolling up from the unclamped target.
            let at = line.min(max_top);
            let top = line.saturating_sub(rows_at(anchor, visible)).min(max_top);
            Pass {
                step: Some(Step::CaretToLine { line, top }),
                next: (top != at).then_some(Carry::Shift { rows: at - top }),
            }
        }
        Some(Carry::Shift { rows }) => Pass {
            step: Some(Step::ScrollUp { rows }),
            next: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mut carry: Option<Carry>, max_top: usize, visible: usize) -> Vec<Step> {
        let mut steps = Vec::new();
        loop {
            let p = layout_pass(carry, max_top, visible);
            steps.extend(p.step);
            if !p.relayout() {
                return steps;
            }
            carry = p.next;
        }
    }

    #[test]
    fn top_carry_is_two_passes() {
        let c = line_carry(50, 50, 0.0);
        assert_eq!(
            run(Some(c), 170, 30),
            [Step::CaretToEnd, Step::CaretToLine { line: 50, top: 50 }]
        );
    }

    #[test]
    fn anchored_carry_shifts_in_pass_three() {
        let c = line_carry(50, 35, 0.5);
        assert_eq!(
            c,
            Carry::Line {
                line: 50,
                anchor: 0.5,
                at_end: false
            }
        );
        assert_eq!(
            run(Some(c), 170, 30),
            [
                Step::CaretToEnd,
                Step::CaretToLine { line: 50, top: 35 },
                Step::ScrollUp { rows: 15 },
            ]
        );
    }

    #[test]
    fn anchored_carry_near_the_end_clamps_once() {
        // Past the last top: at = 170, target = 195 - 15 = 180 → clamped to 170, no shift.
        let c = line_carry(195, 170, 0.5);
        assert_eq!(
            run(Some(c), 170, 30),
            [
                Step::CaretToEnd,
                Step::CaretToLine {
                    line: 195,
                    top: 170
                }
            ]
        );
        // Between: at = 170, target = 160 → shift 10.
        let c = line_carry(175, 160, 0.5);
        assert_eq!(
            run(Some(c), 170, 30)[2],
            Step::ScrollUp { rows: 10 },
            "shift from the clamped line"
        );
    }

    #[test]
    fn edge_is_one_pass_and_none_is_a_no_op() {
        assert_eq!(
            run(Some(Carry::Edge { bottom: true }), 170, 30),
            [Step::Edge { bottom: true }]
        );
        let p = layout_pass(None, 170, 30);
        assert!(p.step.is_none() && !p.relayout());
    }

    #[test]
    fn rows_at_rounds_and_keeps_the_caret_visible() {
        assert_eq!(rows_at(0.0, 30), 0);
        assert_eq!(rows_at(0.5, 30), 15);
        assert_eq!(rows_at(1.0, 30), 29);
        assert_eq!(rows_at(f32::NAN, 30), 0);
        assert_eq!(rows_at(0.5, 0), 0);
        assert_eq!(line_carry(9, 4, f32::INFINITY), line_carry(9, 4, 0.0));
    }
}
