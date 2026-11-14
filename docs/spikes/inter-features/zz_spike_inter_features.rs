// THROWAWAY spike (docs/spikes/inter-features): which Inter OpenType features substitute ASCII sequences.
use iced::advanced::graphics::text::cosmic_text::{self as ct, Attrs, Buffer, Family, FeatureTag, FontFeatures, Metrics, Shaping};

fn ids(fs: &mut ct::FontSystem, s: &str, feats: &[&[u8; 4]]) -> Vec<u16> {
    let mut f = FontFeatures::new();
    for t in feats { f.enable(FeatureTag::new(t)); }
    let attrs = Attrs::new().family(Family::Name("Inter")).font_features(f);
    let mut b = Buffer::new(fs, Metrics::new(22.0, 28.0));
    b.set_text(fs, s, &attrs, Shaping::Advanced, None);
    b.shape_until_scroll(fs, false);
    b.layout_runs().flat_map(|r| r.glyphs.iter().map(|g| g.glyph_id).collect::<Vec<_>>()).collect()
}

#[test]
fn spike() {
    let mut fs = ct::FontSystem::new();
    let seqs = ["->", "<-", "<->", "=>", "<=", "<=>", "-->", "<--", "--", "---", "!=", "==", ">=", "=<", "...", "||", "::", "~>", "|>", "(c)", "1/2", "a -> b"];
    let feats: [&[u8;4]; 14] = [b"calt", b"dlig", b"liga", b"ss01", b"ss02", b"ss03", b"ss04", b"ss05", b"ss06", b"ss07", b"ss08", b"frac", b"cv01", b"case"];
    for s in seqs {
        let base = ids(&mut fs, s, &[]);
        let mut hits = vec![];
        for f in feats {
            let g = ids(&mut fs, s, &[f]);
            if g != base { hits.push(format!("{}:{}g", std::str::from_utf8(f).unwrap(), g.len())); }
        }
        let plain: Vec<u16> = s.chars().flat_map(|c| ids(&mut fs, &c.to_string(), &[])).collect();
        println!("{s:8} default_substituted={} base={}g extra=[{}]", base != plain, base.len(), hits.join(" "));
    }
}
