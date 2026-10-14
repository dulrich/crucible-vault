//! Virtualized row list (WP-4).
//!
//! iced has no virtual list (plan "Dependency posture"), so this renders only the rows that
//! intersect the `scrollable` viewport (plus [`OVERSCAN`] rows each side), with fixed-height
//! spacers before and after so the scrollbar reflects the full length. Rows have a fixed height.
//! The viewport (offset + height) comes back through `on_scroll` into a [`ListWindow`] the owner
//! keeps. Reused by the explorer and later by the tags pane, backlinks and view tables.

use std::ops::Range;

use iced::widget::{Space, column, container, scrollable};
use iced::{Element, Length};

/// Extra rows built above and below the viewport.
pub const OVERSCAN: usize = 4;

/// Assumed viewport height before the first `on_scroll` report.
pub const INITIAL_HEIGHT: f32 = 1200.0;

/// The scroll window the owner stores (updated from `on_scroll`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListWindow {
    pub offset: f32,
    pub height: f32,
}

impl Default for ListWindow {
    fn default() -> Self {
        Self {
            offset: 0.0,
            height: INITIAL_HEIGHT,
        }
    }
}

impl ListWindow {
    /// From a scrollable viewport.
    pub fn from_viewport(v: &scrollable::Viewport) -> Self {
        Self {
            offset: v.absolute_offset().y,
            height: v.bounds().height,
        }
    }
}

/// The row indices to build for `len` rows of `row_height` in `window`.
pub fn visible_range(window: ListWindow, row_height: f32, len: usize) -> Range<usize> {
    if len == 0 || row_height <= 0.0 || !row_height.is_finite() {
        return 0..0;
    }
    let offset = if window.offset.is_finite() {
        window.offset.max(0.0)
    } else {
        0.0
    };
    let height = if window.height.is_finite() {
        window.height.max(0.0)
    } else {
        INITIAL_HEIGHT
    };
    // Float→index: values are non-negative and finite; `as` saturates.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let first = (offset / row_height).floor() as usize;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let last = ((offset + height) / row_height).ceil() as usize;
    let start = first.saturating_sub(OVERSCAN).min(len);
    let end = last.saturating_add(OVERSCAN).min(len);
    start..end.max(start)
}

/// A virtual list over `items`.
pub struct VirtualList<'a, T, M> {
    items: &'a [T],
    row_height: f32,
    window: ListWindow,
    on_scroll: Box<dyn Fn(ListWindow) -> M + 'a>,
}

impl<'a, T, M: 'a> VirtualList<'a, T, M> {
    /// `on_scroll` maps the new window to the owner's message.
    pub fn new(
        items: &'a [T],
        row_height: f32,
        window: ListWindow,
        on_scroll: impl Fn(ListWindow) -> M + 'a,
    ) -> Self {
        Self {
            items,
            row_height,
            window,
            on_scroll: Box::new(on_scroll),
        }
    }

    /// The rows that will be built.
    pub fn range(&self) -> Range<usize> {
        visible_range(self.window, self.row_height, self.items.len())
    }

    /// Builds the scrollable, calling `row` only for [`Self::range`].
    pub fn view(self, row: impl Fn(usize, &'a T) -> Element<'a, M>) -> Element<'a, M> {
        let range = self.range();
        let h = self.row_height;
        #[allow(clippy::cast_precision_loss)]
        let before = range.start as f32 * h;
        #[allow(clippy::cast_precision_loss)]
        let after = (self.items.len() - range.end) as f32 * h;
        let rows = self.items[range.clone()]
            .iter()
            .zip(range)
            .map(|(item, i)| {
                container(row(i, item))
                    .height(Length::Fixed(h))
                    .width(Length::Fill)
                    .into()
            });
        let body = column![Space::new().height(Length::Fixed(before))]
            .extend(rows)
            .push(Space::new().height(Length::Fixed(after)))
            .width(Length::Fill);
        let on_scroll = self.on_scroll;
        scrollable(body)
            .on_scroll(move |v| on_scroll(ListWindow::from_viewport(&v)))
            .height(Length::Fill)
            .width(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use iced::widget::text;

    use super::*;

    #[test]
    fn slice_math() {
        let w = |offset, height| ListWindow { offset, height };
        assert_eq!(
            visible_range(w(0.0, 100.0), 20.0, 10_000),
            0..(5 + OVERSCAN)
        );
        assert_eq!(
            visible_range(w(2000.0, 100.0), 20.0, 10_000),
            (100 - OVERSCAN)..(105 + OVERSCAN)
        );
        assert_eq!(
            visible_range(w(199_950.0, 100.0), 20.0, 10_000),
            (9997 - OVERSCAN)..10_000
        );
        assert_eq!(visible_range(w(1e9, 100.0), 20.0, 10), 10..10);
        assert_eq!(visible_range(w(0.0, 100.0), 20.0, 0), 0..0);
        assert_eq!(
            visible_range(w(f32::NAN, 100.0), 20.0, 50),
            0..(5 + OVERSCAN)
        );
    }

    #[test]
    fn builds_only_visible_rows_of_10k() {
        let items: Vec<u32> = (0..10_000).collect();
        for offset in [0.0, 5_000.0, 123_456.0, 239_000.0] {
            let built = Cell::new(0usize);
            let window = ListWindow {
                offset,
                height: 600.0,
            };
            let list = VirtualList::new(&items, 24.0, window, |_| ());
            let expect = list.range().len();
            let _el: Element<'_, ()> = list.view(|_, n| {
                built.set(built.get() + 1);
                text(n.to_string()).into()
            });
            assert_eq!(built.get(), expect);
            assert!(
                built.get() <= 600 / 24 + 1 + 2 * OVERSCAN,
                "offset {offset}"
            );
            assert!(built.get() > 0);
        }
    }
}
