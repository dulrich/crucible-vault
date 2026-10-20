//! Pure place-keeping math: the measured Reading layout
//! [`Probe`], the Source viewport line count, and the Source relative scroll delta. The iced
//! adapters (the probe operation, `Action::Scroll`) stay in the editor.

/// plan editor-ui-round-3 WP-6: measured reading layout — the scrollable's offset and each
/// top-level block's top, all in absolute layout coordinates (audit E1: measured, never a
/// proportional estimate).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Probe {
    /// The scrollable's vertical scroll offset.
    pub offset_y: f32,
    /// The top of the scrollable's content.
    pub content_y: f32,
    /// Plan round-8 WP-4: the scrollable's viewport height.
    pub viewport_h: f32,
    /// Plan round-8 WP-4: the scrollable's content height.
    pub content_h: f32,
    /// The top of each top-level block (`None` = not laid out / not found).
    pub block_ys: Vec<Option<f32>>,
}

impl Probe {
    /// Plan round-8 WP-4: the block under the anchor height `a` (fraction of the viewport;
    /// `a = 0` is the block at the viewport top).
    pub fn anchor_block(&self, a: f32) -> Option<usize> {
        self.block_at(a * self.viewport_h)
    }

    /// The block `dy` px below the viewport top: the last block whose content-relative top is
    /// at or above `offset + dy` (1px slack), else the first laid-out block.
    fn block_at(&self, dy: f32) -> Option<usize> {
        let rel = |y: f32| y - self.content_y;
        let mut first = None;
        let mut top = None;
        for (i, y) in self.block_ys.iter().enumerate() {
            let Some(y) = *y else { continue };
            first.get_or_insert(i);
            if rel(y) <= self.offset_y + dy + 1.0 {
                top = Some(i);
            }
        }
        top.or(first)
    }

    /// Plan round-8 WP-4: the scroll offset that puts content-relative `block_y` at the anchor
    /// height `a`, clamped to `[0, content_h − viewport_h]`.
    pub fn anchored_offset(&self, block_y: f32, a: f32) -> f32 {
        let max = (self.content_h - self.viewport_h).max(0.0);
        (block_y - a * self.viewport_h).clamp(0.0, max)
    }

    /// The scroll offset that puts block `b`'s top at the viewport top.
    pub fn block_offset(&self, b: usize) -> Option<f32> {
        let y = (*self.block_ys.get(b)?)?;
        Some((y - self.content_y).max(0.0))
    }
}

/// Plan round-5 WP-1: the source lines a laid-out editor `height` (px) shows at text size
/// `font_size` — iced's default relative line height (1.3), minus
/// `padding` (top + bottom).
pub fn viewport_lines(height: f32, font_size: f32, padding: f32) -> usize {
    let line = (font_size * 1.3).max(1.0);
    let lines = ((height - 2.0 * padding).max(0.0) / line).floor();
    // Truncation is the intent: a partly visible last line does not count.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = lines as usize;
    n.max(1)
}

/// Plan round-6 WP-1 (spike `docs/spikes/source-scroll/FINDINGS.md`): the single scroll that
/// moves a view whose (pending) top source line is `from` to `to`. cosmic-text =0.15.0 sums
/// every `Scroll` into one pixel offset that is only resolved at the next shaping (where a
/// net-negative offset clamps to line 0), so the old "fully up, then down `line`" pair landed
/// at the top; one relative delta from the tracked top lands on `to` (wrapped lines above
/// count once each). Returns the `Scroll { lines }` delta;
/// `None` when already there.
pub fn scroll_lines(from: usize, to: usize) -> Option<i32> {
    let delta = i64::try_from(to).unwrap_or(i64::MAX) - i64::try_from(from).unwrap_or(i64::MAX);
    let lines = i32::try_from(delta).unwrap_or(if delta < 0 { i32::MIN } else { i32::MAX });
    (lines != 0).then_some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// plan editor-ui-round-3 WP-6: the measured probe picks the block at the viewport top and the
    /// offset that puts a block there (layout coordinates, never proportional).
    #[test]
    fn probe_maps_scroll_offset_to_blocks() {
        let p = Probe {
            offset_y: 100.0,
            content_y: 40.0,
            block_ys: vec![Some(48.0), Some(90.0), Some(139.5), Some(240.0), None],
            ..Probe::default()
        };
        assert_eq!(
            p.anchor_block(0.0),
            Some(2),
            "139.5 - 40 = 99.5 <= 100 (+1 slack)"
        );
        assert_eq!(p.block_offset(3), Some(200.0));
        assert_eq!(p.block_offset(0), Some(8.0));
        assert_eq!(p.block_offset(4), None, "not laid out");
        let at_top = Probe {
            offset_y: 0.0,
            ..p.clone()
        };
        assert_eq!(at_top.anchor_block(0.0), Some(0));
    }

    #[test]
    fn scroll_lines_is_one_relative_delta() {
        assert_eq!(scroll_lines(3, 3), None);
        assert_eq!(scroll_lines(3, 10), Some(7));
        assert_eq!(scroll_lines(10, 0), Some(-10));
    }
}
