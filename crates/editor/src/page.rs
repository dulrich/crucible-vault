//! plan editor-round-4 WP-2: PageUp/PageDown for scrollables (Reading, Settings) — scrolls by
//! one viewport height minus one line, read from the scrollable's own bounds at operate time.

use iced::advanced::widget::operation::{Operation, Outcome, Scrollable};
use iced::widget::scrollable::AbsoluteOffset;
use iced::{Rectangle, Task, Vector};

/// The overlap kept between pages (about one text line), in pixels.
pub const PAGE_OVERLAP: f32 = 24.0;

/// The page distance for a viewport `height` (never negative).
pub fn page_delta(height: f32, down: bool) -> f32 {
    let d = (height - PAGE_OVERLAP).max(PAGE_OVERLAP.min(height));
    if down { d } else { -d }
}

/// How far [`PageOp`] scrolls.
enum Step {
    /// One viewport minus [`PAGE_OVERLAP`], down (`true`) or up.
    Page(bool),
    /// A fixed distance in pixels (plan round-5 WP-1: arrow keys, one line).
    Pixels(f32),
}

struct PageOp {
    target: iced::widget::Id,
    step: Step,
}

impl Operation<()> for PageOp {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<()>)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        content_bounds: Rectangle,
        _translation: Vector,
        state: &mut dyn Scrollable,
    ) {
        if id == Some(&self.target) {
            let y = match self.step {
                Step::Page(down) => page_delta(bounds.height, down),
                Step::Pixels(y) => y,
            };
            state.scroll_by(AbsoluteOffset { x: 0.0, y }, bounds, content_bounds);
        }
    }

    fn finish(&self) -> Outcome<()> {
        Outcome::None
    }
}

/// Pages the scrollable `target` up or down.
pub fn page<T: Send + 'static>(target: iced::widget::Id, down: bool) -> Task<T> {
    iced::advanced::widget::operate(PageOp {
        target,
        step: Step::Page(down),
    })
    .discard()
}

/// Plan round-5 WP-1: scrolls `target` by `dy` pixels — unless some widget (a text input, a
/// source editor) holds keyboard focus, which keeps its own arrows.
pub fn step<T: Send + 'static>(target: iced::widget::Id, dy: f32) -> Task<T> {
    iced::advanced::widget::operate(AnyFocused(false)).then(move |focused| {
        if focused {
            Task::none()
        } else {
            iced::advanced::widget::operate(PageOp {
                target: target.clone(),
                step: Step::Pixels(dy),
            })
            .discard()
        }
    })
}

/// The arrow-key line step for Reading text at `font_size` (iced's 1.3 relative line height).
pub fn line_step(font_size: f32, down: bool) -> f32 {
    let d = (font_size * 1.3).max(1.0);
    if down { d } else { -d }
}

/// Answers whether any focusable widget holds focus.
struct AnyFocused(bool);

impl Operation<bool> for AnyFocused {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<bool>)) {
        operate(self);
    }

    fn focusable(
        &mut self,
        _id: Option<&iced::widget::Id>,
        _bounds: Rectangle,
        state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        self.0 |= state.is_focused();
    }

    fn finish(&self) -> Outcome<bool> {
        Outcome::Some(self.0)
    }
}
