//! Headless harness: drives SelPara blocks inside a scrollable with synthetic
//! mouse/keyboard events, asserts selected text, renders PNGs via tiny-skia.
mod selectable;

use iced_core::text::{Paragraph as _, Span};
use iced_core::{Color, Event, Font, Pixels, Point, Rectangle, Size, Theme, font, keyboard, mouse, renderer};
use iced_core::renderer::Headless as _;
use iced_runtime::user_interface::{Cache, UserInterface};
use iced_widget::{column, scrollable};
use selectable::{Link, SelEvent, SelPara, line_ranges, plain};
use std::time::Instant;

type R = iced_tiny_skia::Renderer;
type P = <R as iced_core::text::Renderer>::Paragraph;
const W: u32 = 600;
const H: u32 = 400;


#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
struct Pos { block: usize, byte: usize }

#[derive(Debug, Clone)]
enum Msg { Sel(SelEvent) }

struct App {
    blocks: Vec<Vec<Span<'static, Link>>>,
    anchor: Option<Pos>,
    focus: Option<Pos>,
    press_at: Option<Point>,
    dragging: bool,
    pending_link: Option<Link>,
    clicked: Vec<Link>,
    copied: Option<String>,
}

fn sp(t: &str) -> Span<'static, Link> { Span::new(t.to_string()) }

impl App {
    fn new() -> Self {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let b0 = vec![
            sp("Hello "), sp("world").font(bold), sp(" see "),
            sp("the docs").link("https://example.org/docs".to_string()).color(Color::from_rgb8(0x1a, 0x5f, 0xd0)).underline(true),
            sp(" and "), sp("fn main()").font(Font::MONOSPACE).color(Color::from_rgb8(0xa0, 0x30, 0x30)), sp("."),
        ];
        let b1 = vec![
            sp("This paragraph is deliberately long so that it wraps across several visual lines at the fixed spike width; "),
            sp("bold words in the middle").font(bold),
            sp(" keep the span boundaries interesting while the selection runs from the first line down to the third line and beyond, checking rectangles per line."),
        ];
        let b2 = vec![sp("café — naïve 日本語 text, "), sp("ünïcödé").font(bold), sp(" end.")];
        Self { blocks: vec![b0, b1, b2], anchor: None, focus: None, press_at: None, dragging: false, pending_link: None, clicked: vec![], copied: None }
    }

    fn ordered(&self) -> Option<(Pos, Pos)> {
        let (a, f) = (self.anchor?, self.focus?);
        if a == f { return None; }
        Some(if a <= f { (a, f) } else { (f, a) })
    }

    fn block_range(&self, i: usize) -> Option<(usize, usize)> {
        let (lo, hi) = self.ordered()?;
        if i < lo.block || i > hi.block { return None; }
        let len = plain(&self.blocks[i]).len();
        Some((if i == lo.block { lo.byte } else { 0 }, if i == hi.block { hi.byte } else { len }))
    }

    fn selected_text(&self) -> String {
        let Some((lo, hi)) = self.ordered() else { return String::new() };
        (lo.block..=hi.block)
            .map(|i| { let (a, b) = self.block_range(i).unwrap(); plain(&self.blocks[i])[a..b].to_string() })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn update(&mut self, m: Msg) {
        let Msg::Sel(e) = m;
        match e {
            SelEvent::Press { block, byte, at, link } => {
                self.anchor = Some(Pos { block, byte });
                self.focus = self.anchor;
                self.press_at = Some(at);
                self.dragging = false;
                self.pending_link = link;
            }
            SelEvent::Drag { block, byte, at } => {
                let Some(p0) = self.press_at else { return };
                if !self.dragging && p0.distance(at) > 4.0 {
                    self.dragging = true;
                    self.pending_link = None;
                }
                if self.dragging { self.focus = Some(Pos { block, byte }); }
            }
            SelEvent::Release => {
                if !self.dragging {
                    if let Some(l) = self.pending_link.take() { self.clicked.push(l); }
                    self.anchor = None;
                    self.focus = None;
                }
                self.press_at = None;
                self.dragging = false;
            }
            SelEvent::Copy => self.copied = Some(self.selected_text()),
        }
    }

    fn view(&self) -> iced_core::Element<'_, Msg, Theme, R> {
        let pressed = self.press_at.is_some();
        let col = column(self.blocks.iter().enumerate().map(|(i, b)| {
            SelPara::new(i, b, self.block_range(i), pressed, Msg::Sel).into()
        }))
        .spacing(14)
        .padding(16);
        scrollable(col).into()
    }
}

struct Harness { app: App, r: R, cache: Cache, cursor: Point }

impl Harness {
    fn new() -> Self {
        let r = R::new(Font::DEFAULT, Pixels(16.0));
        Self { app: App::new(), r, cache: Cache::new(), cursor: Point::ORIGIN }
    }
    fn events(&mut self, evs: &[Event]) {
        let cache = std::mem::take(&mut self.cache);
        let mut ui = UserInterface::build(self.app.view(), Size::new(W as f32, H as f32), cache, &mut self.r);
        let mut msgs = vec![];
        let _ = ui.update(evs, mouse::Cursor::Available(self.cursor), &mut self.r, &mut iced_core::clipboard::Null, &mut msgs);
        // record block bounds through a draw pass of the same layout
        ui.draw(&mut self.r, &Theme::Light, &renderer::Style { text_color: Color::BLACK }, mouse::Cursor::Available(self.cursor));
        self.cache = ui.into_cache();
        for m in msgs { self.app.update(m); }
    }
    fn mv(&mut self, p: Point) { self.cursor = p; self.events(&[Event::Mouse(mouse::Event::CursorMoved { position: p })]); }
    fn down(&mut self) { self.events(&[Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))]); }
    fn up(&mut self) { self.events(&[Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))]); }
    fn drag(&mut self, a: Point, b: Point) {
        self.mv(a); self.down();
        for k in 1..=8 { let t = k as f32 / 8.0; self.mv(Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)); }
        self.up();
    }
    fn ctrl_c(&mut self) -> String {
        self.events(&[Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character("c".into()), modified_key: keyboard::Key::Character("c".into()),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyC), location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::CTRL, text: None, repeat: false,
        })]);
        self.app.copied.take().unwrap_or_default()
    }
    fn bounds(&self, block: usize) -> Rectangle { selectable::DEBUG_BOUNDS.with(|b| b.borrow()[block]) }
    /// Screen point at the left edge of grapheme boundary `byte` in `block`,
    /// computed via a mirror paragraph (same spans, same width).
    fn pt(&self, block: usize, byte: usize) -> Point {
        let b = self.bounds(block);
        let spans = &self.app.blocks[block];
        let p = P::with_spans(iced_core::text::Text {
            content: spans.as_slice(), bounds: Size::new(b.width, f32::INFINITY), size: Pixels(16.0),
            line_height: Default::default(), font: Font::DEFAULT, align_x: Default::default(),
            align_y: iced_core::alignment::Vertical::Top, shaping: iced_core::text::Shaping::Advanced,
            wrapping: iced_core::text::Wrapping::Word,
        });
        let lh = p.line_height().to_absolute(Pixels(16.0)).0;
        let text = plain(spans);
        let lines = line_ranges(&p, lh);
        let (k, (ls, _)) = lines.iter().copied().enumerate().rfind(|(_, (ls, _))| *ls <= byte).unwrap();
        let g = unicode_segmentation::UnicodeSegmentation::graphemes(&text[ls..byte], false).count();
        let x = p.grapheme_position(k, g).unwrap().x;
        Point::new(b.x + x + 0.5, b.y + (k as f32 + 0.5) * lh)
    }
    fn png(&mut self, name: &str) {
        self.events(&[]);
        let px = self.r.screenshot(Size::new(W, H), 1.0, Color::WHITE);
        image::save_buffer(format!("out/{name}"), &px, W, H, image::ExtendedColorType::Rgba8).unwrap();
    }
    fn clear(&mut self) { self.app.anchor = None; self.app.focus = None; }
}

fn check(name: &str, got: &str, want: &str) {
    let ok = got == want;
    println!("{} {name}: got {got:?}{}", if ok { "PASS" } else { "FAIL" }, if ok { String::new() } else { format!(" want {want:?}") });
    assert!(ok);
}

fn main() {
    std::fs::create_dir_all("out").unwrap();
    let mut h = Harness::new();
    h.events(&[]);
    let t0 = plain(&h.app.blocks[0]);
    let t1 = plain(&h.app.blocks[1]);
    let t2 = plain(&h.app.blocks[2]);

    // 1. single word in the bold span, char-precise
    let (a, b) = (t0.find("world").unwrap(), t0.find("world").unwrap() + 5);
    h.drag(h.pt(0, a), h.pt(0, b));
    check("c1_single_word", &h.ctrl_c(), "world");
    h.png("01-single-word.png");
    h.clear();

    // 6a. wrapped paragraph: from line 0 into line 2
    let lines = { let b = h.bounds(1); println!("block1 bounds {b:?}"); b.height / 20.8 };
    println!("block1 visual lines ~{lines:.1}");
    assert!(lines >= 3.0, "block1 must wrap to 3+ lines");
    let a = t1.find("deliberately").unwrap();
    let b = t1.find("selection").unwrap() + "selection".len();
    h.drag(h.pt(1, a), h.pt(1, b));
    check("c6_wrapped", &h.ctrl_c(), &t1[a..b]);
    h.png("02-wrapped-multiline.png");
    h.clear();

    // 3 + 5b. cross-block drag that starts ON the link span: selects, no click
    let a = t0.find("docs").unwrap();
    let b = t1.find("long").unwrap() + 4;
    h.drag(h.pt(0, a), h.pt(1, b));
    check("c3_cross_block", &h.ctrl_c(), &format!("{}\n\n{}", &t0[a..], &t1[..b]));
    assert!(h.app.clicked.is_empty(), "drag across link must not click");
    println!("PASS c5_drag_on_link_selects (clicked={:?})", h.app.clicked);
    h.png("03-cross-block.png");
    h.clear();

    // reverse-direction cross-block (focus before anchor), 3 blocks
    let a = t2.find("日本").unwrap() + "日".len();
    let b = t0.find("main").unwrap();
    h.drag(h.pt(2, a), h.pt(0, b));
    check("c3_reverse_three_blocks", &h.ctrl_c(), &format!("{}\n\n{}\n\n{}", &t0[b..], t1, &t2[..a]));
    h.clear();

    // 6b. multibyte
    let a = t2.find("naïve").unwrap();
    let b = t2.find("ünïcödé").unwrap() + "ünïc".len();
    h.drag(h.pt(2, a), h.pt(2, b));
    check("c6_multibyte", &h.ctrl_c(), &t2[a..b]);
    h.png("04-multibyte.png");
    h.clear();

    // 5. click on link (2px jitter, below threshold) yields the link message
    let p = h.pt(0, t0.find("docs").unwrap());
    h.mv(p); h.down(); h.mv(Point::new(p.x + 2.0, p.y)); h.up();
    check("c5_link_click", &format!("{:?}", h.app.clicked), "[\"https://example.org/docs\"]");
    assert!(h.app.ordered().is_none());

    // 2. press on first glyph, char-by-char sweep across every boundary in block 2
    let mut bad = 0;
    let bounds: Vec<usize> = t2.char_indices().map(|(i, _)| i).chain([t2.len()]).collect();
    for w in bounds.windows(2) {
        h.app.anchor = Some(Pos { block: 2, byte: 0 }); h.app.focus = None;
        // drag from start to boundary w[1]
        h.drag(h.pt(2, 0), h.pt(2, w[1]));
        if h.ctrl_c() != t2[..w[1]] { bad += 1; println!("  boundary {} mismatch: {:?}", w[1], h.app.selected_text()); }
        h.clear();
    }
    println!("{} c2_every_char_boundary_block2: {} boundaries, {bad} mismatches", if bad == 0 { "PASS" } else { "FAIL" }, bounds.len() - 1);
    assert_eq!(bad, 0);

    // timing: one drag-move event = build + update + draw (+ selection rects) ; and screenshot raster
    let (a, b) = (h.pt(0, 0), h.pt(2, t2.len()));
    h.mv(a); h.down();
    let n = 200;
    let t = Instant::now();
    for k in 0..n { let s = k as f32 / n as f32; h.mv(Point::new(a.x + (b.x - a.x) * s, a.y + (b.y - a.y) * s)); }
    let per = t.elapsed().as_secs_f64() * 1e3 / n as f64;
    h.up();
    let t = Instant::now();
    for _ in 0..20 { let _ = h.r.screenshot(Size::new(W, H), 1.0, Color::WHITE); }
    println!("TIMING drag-move (build+update+layout+draw-list) {per:.3} ms/event; tiny-skia raster {:.2} ms/frame",
        t.elapsed().as_secs_f64() * 1e3 / 20.0);
    println!("ALL PASS");
}
