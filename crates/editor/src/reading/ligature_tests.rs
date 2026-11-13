//! Plan round-24 WP-1: Reading text pieces shape with Inter (its contextual alternates for ASCII
//! arrows), not the generic sans-serif fallback. Needs Inter installed (as the editor does).

use editor_model::render::{Block, Inline, lower};
use iced::advanced::graphics::text::Paragraph as GParagraph;
use iced::advanced::text::{self, Paragraph as _, Span as TSpan};
use iced::{Pixels, Size};

use super::view::text_font;

/// `(glyph id, family)` per glyph of `s` shaped as one span with `font`.
fn shape(s: &str, font: iced::Font) -> Vec<(u16, String)> {
    let spans = [TSpan::<(), iced::Font>::new(s).font(font)];
    let p = GParagraph::with_spans(text::Text {
        content: &spans[..],
        bounds: Size::new(800.0, f32::INFINITY),
        size: Pixels(22.0),
        line_height: text::LineHeight::default(),
        font: iced::Font::with_name(crate::theme::FONT_FAMILY),
        align_x: text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: text::Shaping::Advanced,
        wrapping: text::Wrapping::Word,
    });
    let mut fs = iced::advanced::graphics::text::font_system()
        .write()
        .unwrap();
    let mut out = Vec::new();
    for run in p.buffer().layout_runs() {
        for g in run.glyphs {
            let family = fs
                .raw()
                .db()
                .face(g.font_id)
                .map(|f| f.families[0].0.clone())
                .unwrap_or_default();
            out.push((g.glyph_id, family));
        }
    }
    out
}

/// The model hands Reading one text inline per arrow line (`merge_text`): the span split is not
/// the cause (measured: the font was).
#[test]
fn arrow_lines_lower_to_one_text_inline() {
    for s in ["a -> b", "a <- b", "a -- b", "a => b"] {
        let m = lower(s);
        let [Block::Paragraph(inl)] = m.blocks.as_slice() else {
            panic!("{s}: {:#?}", m.blocks);
        };
        assert!(
            matches!(inl.as_slice(), [Inline::Text { text, .. }] if text == s),
            "{s}"
        );
    }
}

#[test]
fn reading_text_font_is_the_app_family_not_generic_sans() {
    let f = text_font(false, false);
    assert_eq!(f, iced::Font::with_name(crate::theme::FONT_FAMILY));
    assert_ne!(f, iced::Font::DEFAULT);
    assert_eq!(text_font(true, true).weight, iced::font::Weight::Bold);
    assert_eq!(text_font(true, true).style, iced::font::Style::Italic);
}

/// Inter substitutes one ligature glyph each for `<-` and `=>`; the old `Font::DEFAULT` face was DejaVu Sans, which never does.
#[test]
fn inter_ligates_arrows_and_old_default_did_not() {
    let inter = text_font(false, false);
    // The font is a host install, not vendored: skip where Inter is absent.
    if shape("a", inter).iter().any(|(_, f)| f != "Inter") {
        eprintln!("skipped: Inter is not installed");
        return;
    }
    for (s, glyphs) in [("a <- b", 5), ("a => b", 5)] {
        let g = shape(s, inter);
        assert!(g.iter().all(|(_, f)| f == "Inter"), "{s}: {g:?}");
        assert_eq!(g.len(), glyphs, "{s}: {g:?}");
        let old = shape(s, iced::Font::DEFAULT);
        assert_eq!(old.len(), glyphs + 1, "{s} old: {old:?}");
        assert!(old.iter().all(|(_, f)| f != "Inter"), "{s} old: {old:?}");
    }
    // `->` and `--` are not substituted by Inter: same glyph ids as the plain characters.
    let arrow = shape("a -> b", inter);
    assert_eq!(arrow.len(), 6, "{arrow:?}");
    assert_eq!(arrow[3].0, shape("a > b", inter)[2].0, "{arrow:?}");
    assert_eq!(arrow[2].0, shape("a - b", inter)[2].0, "{arrow:?}");
    assert_eq!(shape("a -- b", inter)[3].0, arrow[2].0);
}
