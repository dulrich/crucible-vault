//! Plan round-15-relative-images-pdf-viewer WP-3: the PDF render seam — the only module that
//! imports `hayro`, so swapping the renderer touches this file alone.
//!
//! **Confinement (audit E3):** hayro's `RenderCache` holds `Rc<RefCell<…>>` and its parsed
//! `Pdf` borrows, so no hayro value leaves a function here. Each call parses the shared file
//! bytes (`Arc<Vec<u8>>`, `Send`) and drops every hayro value before returning; callers run
//! [`open`] / [`render_page`] inside `spawn_blocking`, and the `App` holds only bytes, page
//! sizes and errors. Re-parsing per page is cheap next to rasterising it (hayro reads the
//! xref lazily) and keeps the render jobs independent (no per-document thread to manage).
//!
//! **Bitmap budget (audit E1):** [`admit`] is the one admission rule for page bitmaps.

use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
use iced::widget::image::Handle;

use crate::image_cache::{Decoded, decode_width};

/// The page sizes (render dimensions in PDF points, rotation applied) of a document.
pub type PageSizes = Vec<(f32, f32)>;

/// Parses `bytes`: the size of every page, or why it cannot be shown.
pub fn open(bytes: &Arc<Vec<u8>>) -> Result<PageSizes, String> {
    guarded(|| {
        let pdf = Pdf::new(Arc::clone(bytes)).map_err(|e| format!("not a readable PDF ({e:?})"))?;
        let sizes: PageSizes = pdf.pages().iter().map(|p| p.render_dimensions()).collect();
        if sizes.is_empty() {
            return Err("the PDF has no pages".to_owned());
        }
        Ok(sizes)
    })
}

/// Renders page `index` (0-based) of `bytes` `width` pixels wide (aspect kept) on white, into
/// an RGBA handle built exactly as `image_cache::decode` builds one.
pub fn render_page(bytes: &Arc<Vec<u8>>, index: usize, width: u32) -> Result<Decoded, String> {
    guarded(|| {
        let pdf = Pdf::new(Arc::clone(bytes)).map_err(|e| format!("not a readable PDF ({e:?})"))?;
        let pages = pdf.pages();
        let page = pages
            .get(index)
            .ok_or_else(|| format!("no page {}", index + 1))?;
        let (pw, _) = page.render_dimensions();
        if !(pw.is_finite() && pw > 0.0) {
            return Err(format!("page {} has no size", index + 1));
        }
        let scale = width as f32 / pw;
        let cache = RenderCache::new();
        let pm = render(
            page,
            &cache,
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        let (w, h) = (u32::from(pm.width()), u32::from(pm.height()));
        if w == 0 || h == 0 {
            return Err(format!("page {} rendered empty", index + 1));
        }
        // The background is opaque white, so premultiplied and straight RGBA coincide.
        let raw = pm.data_as_u8_slice().to_vec();
        let bytes = raw.len();
        Ok(Decoded {
            handle: Handle::from_rgba(w, h, raw),
            width: w,
            height: h,
            bytes,
        })
    })
}

/// Runs `f`, turning a panic inside the renderer (malformed input) into an error.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err("the PDF renderer failed on this file".to_owned()))
}

/// Narrowest page bitmap worth rendering; below it a page stays a placeholder.
pub const MIN_RENDER_WIDTH: u32 = 64;

/// Projected RGBA bytes of a page `(w_pt, h_pt)` rendered `width` pixels wide.
pub fn projected_bytes(page: (f32, f32), width: u32) -> usize {
    let h = page_height(page, width);
    (width as usize) * (h as usize) * 4
}

fn page_height(page: (f32, f32), width: u32) -> u32 {
    let (pw, ph) = page;
    if !(pw.is_finite() && ph.is_finite() && pw > 0.0 && ph > 0.0) {
        return 1;
    }
    // Saturating float→int.
    ((width as f32 * ph / pw).floor() as u32).max(1)
}

/// Audit E1, the one admission rule: the width to render page `page` (points) at for a pane
/// `display` px wide, given `reserved` bytes already pinned or in flight for PDF pages and the
/// image-cache `budget`. The width is [`decode_width`]-clamped; a page whose projected RGBA
/// would push PDF usage past the budget is rendered at the largest width that fits, and
/// refused (`None`, shown as a placeholder) when even [`MIN_RENDER_WIDTH`] does not fit.
pub fn admit(page: (f32, f32), display: f32, reserved: usize, budget: usize) -> Option<u32> {
    let room = budget.checked_sub(reserved)?;
    let mut width = decode_width(display);
    if projected_bytes(page, width) <= room {
        return Some(width);
    }
    // bytes ≈ 4 · w² · (h/w): solve for w, then step down until it truly fits (rounding).
    let (pw, ph) = page;
    let ratio = if pw > 0.0 && ph > 0.0 { ph / pw } else { 1.0 };
    let est = ((room as f64) / (4.0 * f64::from(ratio))).sqrt();
    width = width.min(est.floor() as u32);
    while width >= MIN_RENDER_WIDTH && projected_bytes(page, width) > room {
        width -= 1;
    }
    (width >= MIN_RENDER_WIDTH).then_some(width)
}

/// Test support: a minimal valid PDF of `pages` pages, each `w` × `h` points, with one filled
/// rectangle per page (hand-written objects and xref; no dependency).
pub fn synthetic_pdf(pages: usize, w: u32, h: u32) -> Vec<u8> {
    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets: Vec<usize> = Vec::new();
    let n_obj = 2 + pages * 2 + 1;
    let mut obj = |out: &mut Vec<u8>, body: String| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", offsets.len()).as_bytes());
    };
    obj(&mut out, "<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
    obj(
        &mut out,
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ),
    );
    for i in 0..pages {
        obj(
            &mut out,
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R >>",
                4 + i * 2
            ),
        );
        let s = format!(
            "0.2 0.4 0.8 rg 10 10 {} {} re f",
            w / 2,
            h / 3 + (i as u32 % 7)
        );
        obj(
            &mut out,
            format!("<< /Length {} >>\nstream\n{s}\nendstream", s.len()),
        );
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {n_obj}\n0000000000 65535 f \n").as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            n_obj
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const A4: (f32, f32) = (595.0, 842.0);

    #[test]
    fn renders_page_one_of_a_generated_pdf_at_the_requested_width() {
        let bytes = Arc::new(synthetic_pdf(3, 200, 100));
        assert_eq!(open(&bytes).expect("parses"), vec![(200.0, 100.0); 3]);
        let d = render_page(&bytes, 0, 400).expect("renders");
        assert_eq!((d.width, d.height, d.bytes), (400, 200, 400 * 200 * 4));
        assert!(render_page(&bytes, 3, 400).is_err(), "no page 4");
    }

    #[test]
    fn a_corrupt_pdf_is_an_error_not_a_panic() {
        assert!(open(&Arc::new(b"%PDF-1.4\0garbage".to_vec())).is_err());
        assert!(render_page(&Arc::new(b"nope".to_vec()), 0, 128).is_err());
    }

    #[test]
    fn admission_clamps_width_and_refuses_past_the_budget() {
        // Fits: the decode-width bucket of the pane width.
        assert_eq!(admit(A4, 700.0, 0, 64 << 20), Some(704));
        // Over the max decode width: clamped.
        assert_eq!(admit(A4, 9000.0, 0, 64 << 20), Some(2048));
        // The projected bytes would pass the budget given what is reserved: refused.
        let one = projected_bytes(A4, 704);
        assert_eq!(admit(A4, 700.0, 4 * one, 4 * one), None);
        assert_eq!(admit(A4, 700.0, 5 * one, 4 * one), None);
    }

    #[test]
    fn an_oversized_page_renders_at_a_reduced_scale_that_fits() {
        let budget = 1 << 20;
        let w = admit(A4, 2048.0, 0, budget).expect("downscaled");
        assert!((MIN_RENDER_WIDTH..2048).contains(&w), "{w}");
        assert!(projected_bytes(A4, w) <= budget);
        assert!(
            projected_bytes(A4, w + 1) > budget,
            "the largest width that fits"
        );
        // Half the room left: a smaller width again, still within the remainder.
        let w2 = admit(A4, 2048.0, budget / 2, budget).expect("fits the rest");
        assert!(w2 < w && projected_bytes(A4, w2) <= budget / 2);
    }

    /// Audit item 5: RSS for 20 rendered pages through the cache path (run with
    /// `cargo test -p editor --release pdf::tests::measure -- --ignored --nocapture`).
    #[test]
    #[ignore = "measurement"]
    fn measure_twenty_pages_rss() {
        fn rss_kib() -> usize {
            std::fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|s| {
                    s.lines()
                        .find(|l| l.starts_with("VmRSS:"))
                        .and_then(|l| l.split_whitespace().nth(1).map(str::to_owned))
                })
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        }
        let bytes = Arc::new(synthetic_pdf(500, 612, 792));
        let mut cache = crate::image_cache::ImageCache::from_env();
        let before = rss_kib();
        let mut reserved = 0;
        for i in 0..20 {
            let key = format!("big.pdf#page={i}@w");
            let Some(w) = admit((612.0, 792.0), 1000.0, reserved, cache.budget()) else {
                println!("page {i} refused (reserved {reserved})");
                continue;
            };
            assert!(cache.show("big.pdf\u{1f}1", &key));
            reserved += projected_bytes((612.0, 792.0), w);
            let d = render_page(&bytes, i, w).expect("renders");
            assert!(cache.loaded(&key, Some(d)));
        }
        let after = rss_kib();
        println!(
            "file={} B pages=500 rendered=20 rss_before={before} KiB rss_after={after} KiB delta={} KiB cache_used={} budget={} entries={}",
            bytes.len(),
            after.saturating_sub(before),
            cache.used_bytes(),
            cache.budget(),
            cache.len()
        );
        assert!(cache.used_bytes() <= cache.budget());
    }
}
