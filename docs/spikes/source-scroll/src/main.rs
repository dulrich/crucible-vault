use cosmic_text::{Attrs, Buffer, Edit, Editor, FontSystem, Metrics, Shaping, Action};
fn run(label: &str, steps: &[i32]) {
    let mut fs = FontSystem::new();
    let mut buf = Buffer::new(&mut fs, Metrics::new(14.0, 18.2));
    buf.set_size(&mut fs, Some(600.0), Some(18.2 * 30.0));
    let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
    buf.set_text(&mut fs, &text, &Attrs::new(), Shaping::Advanced, None);
    let mut ed = Editor::new(buf);
    for &px in steps { ed.action(&mut fs, Action::Scroll { pixels: px as f32 }); }
    ed.with_buffer_mut(|b| b.shape_until_scroll(&mut fs, false));
    ed.with_buffer(|b| println!("{label}: scroll = {:?}", b.scroll()));
}
fn main() {
    // Help is answered before anything is created or read.
    if std::env::args().skip(1).any(|a| a == "--help" || a == "-h") {
        println!("usage: source-scroll-spike\n\nSource-view scroll_content measurement. Takes no arguments.");
        return;
    }
    if std::env::args().skip(1).any(|a| a == "--build" || a == "--version") {
        println!("{}", env!("FLEET_BUILD_ID"));
        return;
    }
    let lh = 18.2f32; let n = 200f32;
    run("up 4n then down 50 (current scroll_content)", &[(-4.0*n*lh) as i32, (50.0*lh) as i32]);
    run("single down 50", &[(50.0*lh) as i32]);
    run("up 4n then down 190 (go-to-end)", &[(-4.0*n*lh) as i32, (190.0*lh) as i32]);
}
