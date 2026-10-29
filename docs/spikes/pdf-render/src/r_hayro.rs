use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{RenderCache, RenderSettings, PixmapSettings, render};
use std::time::Instant;

// usage: r_hayro <pdf> <scale>   (scale = dpi/72)
fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() == 2 && a[1] == "--help" {
        println!("usage: r_hayro <pdf> <scale>  (prints one aggregate line: pages, ms and non-white px for page 1 and mid page)");
        return;
    }
    if a.iter().skip(1).any(|x| x == "--build" || x == "--version") {
        println!("{}", env!("FLEET_BUILD_ID"));
        return;
    }
    let scale: f32 = a[2].parse().unwrap();
    let r = std::panic::catch_unwind(|| {
        let data = std::fs::read(&a[1]).map_err(|e| e.to_string())?;
        let pdf = Pdf::new(data).map_err(|e| format!("{e:?}"))?;
        let pages = pdf.pages();
        let n = pages.len();
        if n == 0 { return Err("zero pages".to_string()); }
        let cache = RenderCache::new();
        let settings = InterpreterSettings::default();
        let mut out = format!("pages={n}");
        for (tag, idx) in [("p1", 0usize), ("mid", n / 2)] {
            let t = Instant::now();
            let pm = render(&pages[idx], &cache, &settings, &RenderSettings::default(),
                &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE });
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let nonwhite = pm.data_as_u8_slice().chunks_exact(4).filter(|p| p[0] != 255 || p[1] != 255 || p[2] != 255).count();
            out += &format!(" {tag}_ms={ms:.1} {tag}_px={}x{} {tag}_nonwhite={nonwhite}", pm.width(), pm.height());
        }
        Ok::<String, String>(out)
    });
    match r {
        Ok(Ok(s)) => println!("OK {s}"),
        Ok(Err(e)) => println!("ERR {}", e.chars().take(60).collect::<String>()),
        Err(_) => println!("PANIC"),
    }
}
