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
    // plan editor-settings-tab WP-2: the settings rail (codepoints read from the vendored
    // font's `cmap` + `post` glyph names: settings, type, keyboard, zap, timer, calendar-days,
    // tags, bot, server, table, activity, triangle-alert).
    Settings,
    Type,
    Keyboard,
    Zap,
    Timer,
    CalendarDays,
    Tags,
    Bot,
    Server,
    Table,
    Activity,
    TriangleAlert,
    // plan editor-feedback-round-2 WP-5: reading-view task checkboxes (codepoints read from the
    // vendored font's `cmap` + `post` glyph names: check, arrow-right, calendar, circle-help, square-slash, quote, minus, star, map-pin, info, dollar-sign, lightbulb, flame, key, trending-up, trending-down, trophy, thumbs-up, thumbs-down).
    Check,
    ArrowRight,
    Calendar,
    CircleHelp,
    SquareSlash,
    Quote,
    Minus,
    Star,
    MapPin,
    Info,
    DollarSign,
    Lightbulb,
    Flame,
    Key,
    TrendingUp,
    TrendingDown,
    Trophy,
    ThumbsUp,
    ThumbsDown,
    // plan round-17 WP-5: the status-bar failsafes. Codepoints read from the vendored font's
    // `cmap` + `post` glyph names (fontTools `getBestCmap()`: command = U+E09E, file = U+E0C4).
    Command,
    File,
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
            Self::Settings => '\u{e157}',
            Self::Type => '\u{e197}',
            Self::Keyboard => '\u{e283}',
            Self::Zap => '\u{e1b3}',
            Self::Timer => '\u{e1df}',
            Self::CalendarDays => '\u{e2b8}',
            Self::Tags => '\u{e35f}',
            Self::Bot => '\u{e1ba}',
            Self::Server => '\u{e156}',
            Self::Table => '\u{e180}',
            Self::Activity => '\u{e038}',
            Self::TriangleAlert => '\u{e192}',
            Self::Check => '\u{e070}',
            Self::ArrowRight => '\u{e04d}',
            Self::Calendar => '\u{e067}',
            Self::CircleHelp => '\u{e082}',
            Self::SquareSlash => '\u{e177}',
            Self::Quote => '\u{e238}',
            Self::Minus => '\u{e11f}',
            Self::Star => '\u{e179}',
            Self::MapPin => '\u{e114}',
            Self::Info => '\u{e0fe}',
            Self::DollarSign => '\u{e0b5}',
            Self::Lightbulb => '\u{e1c1}',
            Self::Flame => '\u{e0d7}',
            Self::Key => '\u{e102}',
            Self::TrendingUp => '\u{e190}',
            Self::TrendingDown => '\u{e18f}',
            Self::Trophy => '\u{e376}',
            Self::ThumbsUp => '\u{e189}',
            Self::ThumbsDown => '\u{e188}',
            Self::Command => '\u{e09e}',
            Self::File => '\u{e0c4}',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_are_private_use_and_font_is_vendored() {
        for i in [
            Icon::ChevronLeft,
            Icon::Search,
            Icon::FolderTree,
            Icon::X,
            Icon::Check,
            Icon::ArrowRight,
            Icon::Calendar,
            Icon::CircleHelp,
            Icon::SquareSlash,
            Icon::Quote,
            Icon::Minus,
            Icon::Star,
            Icon::MapPin,
            Icon::Info,
            Icon::DollarSign,
            Icon::Lightbulb,
            Icon::Flame,
            Icon::Key,
            Icon::TrendingUp,
            Icon::TrendingDown,
            Icon::Trophy,
            Icon::ThumbsUp,
            Icon::ThumbsDown,
            Icon::Command,
            Icon::File,
        ] {
            assert!(('\u{e000}'..='\u{f8ff}').contains(&i.glyph()));
        }
        // TrueType magic.
        assert_eq!(&FONT_BYTES[..4], &[0, 1, 0, 0]);
    }
}
