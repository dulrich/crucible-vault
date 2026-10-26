// usage: fidelity <pdf> <scale> — renders page 1 and the mid page with hayro and pdfium and prints
// aggregate pixel agreement only (no image is written): mean |gray diff| (0-255) and the share of
// pixels differing by more than 64 gray levels. Env PDFIUM_LIB_DIR = dir holding libpdfium.so.
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};
use pdfium_render::prelude::*;

fn gray(r: u8, g: u8, b: u8) -> i32 {
    (299 * r as i32 + 587 * g as i32 + 114 * b as i32) / 1000
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() == 2 && a[1] == "--help" {
        println!("usage: fidelity <pdf> <scale>  (hayro vs pdfium aggregate pixel agreement; env PDFIUM_LIB_DIR)");
        return;
    }
    let scale: f32 = a[2].parse().unwrap();
    let r = std::panic::catch_unwind(|| {
        let data = std::fs::read(&a[1]).map_err(|e| e.to_string())?;
        let pdf = Pdf::new(data).map_err(|e| format!("{e:?}"))?;
        let pages = pdf.pages();
        let n = pages.len();
        if n == 0 {
            return Err("zero pages".to_string());
        }
        let dir = std::env::var("PDFIUM_LIB_DIR").unwrap();
        let lib = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir))
            .map_err(|e| format!("{e:?}"))?;
        let pdfium = Pdfium::new(lib);
        let doc = pdfium.load_pdf_from_file(&a[1], None).map_err(|e| format!("{e:?}"))?;
        let cache = RenderCache::new();
        let settings = InterpreterSettings::default();
        let mut out = String::new();
        for (tag, idx) in [("p1", 0usize), ("mid", n / 2)] {
            let pm = render(
                &pages[idx],
                &cache,
                &settings,
                &RenderSettings::default(),
                &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE },
            );
            let (hw, hh) = (pm.width() as usize, pm.height() as usize);
            let h = pm.data_as_u8_slice();
            let page = doc.pages().get(idx as _).map_err(|e| format!("{e:?}"))?;
            let bmp = page
                .render_with_config(
                    &PdfRenderConfig::new()
                        .set_target_width(hw as i32)
                        .set_target_height(hh as i32)
                        .set_format(PdfBitmapFormat::BGRA),
                )
                .map_err(|e| format!("{e:?}"))?;
            let (pw, ph) = (bmp.width() as usize, bmp.height() as usize);
            let p = bmp.as_raw_bytes();
            let (w, hgt) = (hw.min(pw), hh.min(ph));
            let (mut sum, mut big, mut cnt) = (0i64, 0i64, 0i64);
            for y in 0..hgt {
                for x in 0..w {
                    let hi = (y * hw + x) * 4;
                    let pi = (y * pw + x) * 4;
                    // hayro: premultiplied RGBA on white bg (opaque); pdfium: BGRA.
                    let g1 = gray(h[hi], h[hi + 1], h[hi + 2]);
                    let g2 = gray(p[pi + 2], p[pi + 1], p[pi]);
                    let d = (g1 - g2).abs() as i64;
                    sum += d;
                    if d > 64 {
                        big += 1;
                    }
                    cnt += 1;
                }
            }
            // Control: poppler (pdftoppm -gray) at the same pixel size, compared with pdfium and with hayro.
            let (pp_mean, hp_mean, pp_big, hp_big) = poppler_cmp(&a[1], idx + 1, hw, hh, h, &p, pw);
            let mean = sum as f64 / cnt.max(1) as f64;
            let share = 100.0 * big as f64 / cnt.max(1) as f64;
            out += &format!(
                " {tag}_dims={hw}x{hh}/{pw}x{ph} {tag}_mean={mean:.2} {tag}_big_pct={share:.2} {tag}_pdfium_poppler_mean={pp_mean:.2} {tag}_hayro_poppler_mean={hp_mean:.2} {tag}_pdfium_poppler_big={pp_big:.2} {tag}_hayro_poppler_big={hp_big:.2}"
            );
        }
        Ok::<String, String>(out)
    });
    match r {
        Ok(Ok(s)) => println!("OK{s}"),
        Ok(Err(e)) => println!("ERR {}", e.chars().take(60).collect::<String>()),
        Err(_) => println!("PANIC"),
    }
}

/// Renders page `pg` (1-based) with `pdftoppm -gray` into a temp dir (deleted at once) at exactly
/// `w`x`h` and returns (pdfium-vs-poppler mean, hayro-vs-poppler mean, and the >64 shares).
fn poppler_cmp(pdf: &str, pg: usize, w: usize, h: usize, hay: &[u8], pdfm: &[u8], pw: usize) -> (f64, f64, f64, f64) {
    let dir = std::env::temp_dir().join(format!("fid-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let root = dir.join("pg");
    let ok = std::process::Command::new("pdftoppm")
        .args(["-gray", "-singlefile", "-f", &pg.to_string(), "-l", &pg.to_string(), "-scale-to-x", &w.to_string(), "-scale-to-y", &h.to_string(), pdf])
        .arg(&root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    let file = dir.join("pg.pgm");
    let bytes = if ok { std::fs::read(&file).unwrap_or_default() } else { Vec::new() };
    let _ = std::fs::remove_dir_all(&dir);
    // P5 header: "P5\n<w> <h>\n255\n" (whitespace-separated tokens).
    let mut toks = 0;
    let mut i = 0;
    let mut in_tok = false;
    while i < bytes.len() && toks < 4 {
        let ws = bytes[i].is_ascii_whitespace();
        if !ws && !in_tok { in_tok = true; }
        if ws && in_tok { in_tok = false; toks += 1; }
        i += 1;
    }
    let px = &bytes[i.min(bytes.len())..];
    if px.len() < w * h { return (-1.0, -1.0, -1.0, -1.0); }
    let (mut s1, mut s2, mut b1, mut b2) = (0i64, 0i64, 0i64, 0i64);
    for y in 0..h {
        for x in 0..w {
            let g = px[y * w + x] as i32;
            let hi = (y * w + x) * 4;
            let pi = (y * pw + x) * 4;
            let d1 = (gray(pdfm[pi + 2], pdfm[pi + 1], pdfm[pi]) - g).abs() as i64;
            let d2 = (gray(hay[hi], hay[hi + 1], hay[hi + 2]) - g).abs() as i64;
            s1 += d1; s2 += d2;
            if d1 > 64 { b1 += 1; }
            if d2 > 64 { b2 += 1; }
        }
    }
    let n = (w * h) as f64;
    (s1 as f64 / n, s2 as f64 / n, 100.0 * b1 as f64 / n, 100.0 * b2 as f64 / n)
}
