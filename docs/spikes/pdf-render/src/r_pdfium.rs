use pdfium_render::prelude::*;
use std::time::Instant;

// usage: r_pdfium <pdf> <scale> ; libpdfium path from PDFIUM_LIB_DIR
fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() == 2 && a[1] == "--help" {
        println!("usage: r_pdfium <pdf> <scale>  (env PDFIUM_LIB_DIR = dir holding libpdfium.so)");
        return;
    }
    let scale: f32 = a[2].parse().unwrap();
    let r = std::panic::catch_unwind(|| {
        let dir = std::env::var("PDFIUM_LIB_DIR").unwrap();
        let lib = Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir)).map_err(|e| format!("{e:?}"))?;
        let pdfium = Pdfium::new(lib);
        let doc = pdfium.load_pdf_from_file(&a[1], None).map_err(|e| format!("{e:?}"))?;
        let n = doc.pages().len() as usize;
        if n == 0 { return Err("zero pages".to_string()); }
        let mut out = format!("pages={n}");
        for (tag, idx) in [("p1", 0usize), ("mid", n / 2)] {
            let t = Instant::now();
            let page = doc.pages().get(idx as _).map_err(|e| format!("{e:?}"))?;
            let bmp = page.render_with_config(&PdfRenderConfig::new().scale_page_by_factor(scale).set_format(PdfBitmapFormat::BGRA)).map_err(|e| format!("{e:?}"))?;
            let bytes = bmp.as_raw_bytes();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let nonwhite = bytes.chunks_exact(4).filter(|p| p[0] != 255 || p[1] != 255 || p[2] != 255).count();
            out += &format!(" {tag}_ms={ms:.1} {tag}_px={}x{} {tag}_nonwhite={nonwhite}", bmp.width(), bmp.height());
        }
        Ok::<String, String>(out)
    });
    match r {
        Ok(Ok(s)) => println!("OK {s}"),
        Ok(Err(e)) => println!("ERR {}", e.chars().take(60).collect::<String>()),
        Err(_) => println!("PANIC"),
    }
}
