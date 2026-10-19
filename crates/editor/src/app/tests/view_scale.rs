//! View-fence text and rows scale with `font_size`.

use crate::reading::view::{block_size, reading_scale, scaled};
use crate::views::{MAX_HEIGHT, ROW_HEIGHT, VIRTUAL_THRESHOLD, px, row_height};
use crate::widget::virtual_list::{ListWindow, OVERSCAN, VirtualList};

/// View text keeps its design ratio to the Reading paragraph at every `font_size`.
#[test]
fn view_text_tracks_font_size() {
    for fs in [8.0_f32, 16.0, 22.0, 40.0, 64.0] {
        let s = reading_scale(fs);
        let para = scaled(block_size(None), s);
        assert!((para - fs).abs() < 1e-3, "paragraph = font_size at {fs}");
        for design in [12.0_f32, 13.0, 14.0] {
            assert!((px(design, s) - scaled(design, s)).abs() < 1e-3);
            if fs >= 16.0 {
                assert!((px(design, s) / para - design / 16.0).abs() < 1e-3, "{fs}");
            }
        }
        assert!((row_height(s) - ROW_HEIGHT * fs / 16.0).abs() < 1e-3);
    }
}

/// Audit E4: a virtualized view at `font_size` 40 lays rows out at the scaled height and its
/// visible range covers exactly the window at that height (no clipped / overlapping rows).
#[test]
fn virtualized_view_rows_scale_at_large_font() {
    let h = row_height(reading_scale(40.0));
    assert!((h - 65.0).abs() < 1e-3, "{h}");
    let lines: Vec<u32> = (0..200).collect();
    assert!(lines.len() > VIRTUAL_THRESHOLD);
    let window = ListWindow {
        offset: 20.0 * h,
        height: MAX_HEIGHT,
    };
    let r = VirtualList::new(&lines, h, window, |w| w).range();
    assert_eq!(r.start, 20 - OVERSCAN);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let shown = (MAX_HEIGHT / h).ceil() as usize;
    assert_eq!(r.end, 20 + shown + OVERSCAN);
    // At the unscaled height the same window would build far more rows than fit.
    let flat = VirtualList::new(&lines, ROW_HEIGHT, window, |w| w).range();
    assert!(flat.len() > r.len());
}
