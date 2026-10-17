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

struct PageOp {
    target: iced::widget::Id,
    down: bool,
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
            let y = page_delta(bounds.height, self.down);
            state.scroll_by(AbsoluteOffset { x: 0.0, y }, bounds, content_bounds);
        }
    }

    fn finish(&self) -> Outcome<()> {
        Outcome::None
    }
}

/// Pages the scrollable `target` up or down.
pub fn page<T: Send + 'static>(target: iced::widget::Id, down: bool) -> Task<T> {
    iced::advanced::widget::operate(PageOp { target, down }).discard()
}
