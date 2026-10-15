//! Lucide icons (plan editor-usability-round, Decisions locked: "Fleet icons: lucide"), rendered as
//! glyph text from the vendored `lucide-static@0.468.0` icon font
//! (`crates/editor/assets/lucide/lucide.ttf`, ISC licence beside it). Codepoints are from the
//! package's `font/info.json`.

use iced::Font;

/// The vendored font file, loaded at startup by `main.rs`.
pub const FONT_BYTES: &[u8] = include_bytes!("../assets/lucide/lucide.ttf");
/// The font's family name (fc-query).
pub const FAMILY: &str = "lucide";
/// The icon font.
pub const FONT: Font = Font::with_name(FAMILY);

/// The named icons the editor uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    PanelLeftClose,
    PanelLeftOpen,
    Folder,
    FolderTree,
    Bookmark,
    BookOpen,
    Code,
    Columns2,
    Eye,
    X,
    Search,
}

impl Icon {
    /// The glyph's codepoint in the lucide font.
    pub fn glyph(self) -> char {
        match self {
            Self::ChevronLeft => '\u{e072}',
            Self::ChevronRight => '\u{e073}',
            Self::ChevronUp => '\u{e074}',
            Self::ChevronDown => '\u{e071}',
            Self::PanelLeftClose => '\u{e21b}',
            Self::PanelLeftOpen => '\u{e21c}',
            Self::Folder => '\u{e0dc}',
            Self::FolderTree => '\u{e33f}',
            Self::Bookmark => '\u{e064}',
            Self::BookOpen => '\u{e063}',
            Self::Code => '\u{e097}',
            Self::Columns2 => '\u{e09c}',
            Self::Eye => '\u{e0be}',
            Self::X => '\u{e1b1}',
            Self::Search => '\u{e154}',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_private_use_and_font_is_vendored() {
        for i in [Icon::ChevronLeft, Icon::Search, Icon::FolderTree, Icon::X] {
            assert!(('\u{e000}'..='\u{f8ff}').contains(&i.glyph()));
        }
        // TrueType magic.
        assert_eq!(&FONT_BYTES[..4], &[0, 1, 0, 0]);
    }
}
