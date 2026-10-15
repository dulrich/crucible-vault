//! The N1 theme (plan `v0-editor.md` WP-1; Crucible inventory #24).
//!
//! The three surrounds are vendored verbatim (only the `--n1-*` colour tokens the UI uses) from
//! `https://github.com/dulrich/obsidian-crucible/blob/master/theme/theme.css`: Med `:35-112` (the default, `body, body[data-surround="med"]`),
//! Dark `:114-180`, Light `:182-249`. `accent-ui` (links, selection, focus) is kept separate from
//! the brand `--n1-accent` per `theme/AGENTS.md` "Token vendoring" and the `theme.css:30-33`
//! warning never to fold the adapter back onto `--n1-accent`.
//!
//! The surround switch is a client command (`https://github.com/dulrich/obsidian-crucible/blob/master/src/surround.ts:28` the
//! `SURROUNDS` order, `:75` `nextSurround` dark → med → light → dark). Dropped as Obsidian-induced:
//! `alignBaseTheme` (`surround.ts:51-57`, flipping Obsidian's base light/dark theme for JS-side
//! surfaces) and the whole Obsidian semantic adapter (`theme.css` block 2), plus the overlay /
//! shadow / blend tokens that only feed Obsidian's CSS.
//!
//! Font: Inter at base size 22 (Crucible's `.obsidian/appearance.json`). Inter is requested by
//! name; no font file is vendored, so iced falls back when it is not installed.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Base text size (px).
pub const BASE_TEXT_SIZE: f32 = 22.0;
/// The UI font family, requested by name.
pub const FONT_FAMILY: &str = "Inter";

/// One of the three N1 surrounds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Surround {
    Dark,
    #[default]
    Med,
    Light,
}

impl Surround {
    /// `surround.ts:28` order.
    pub const ALL: [Self; 3] = [Self::Dark, Self::Med, Self::Light];

    /// The next surround, dark → med → light → dark (`surround.ts:75`).
    pub fn next(self) -> Self {
        match self {
            Self::Dark => Self::Med,
            Self::Med => Self::Light,
            Self::Light => Self::Dark,
        }
    }

    /// Display label (`surround.ts` `SURROUND_LABELS`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Med => "Med",
            Self::Light => "Light",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Dark => DARK,
            Self::Med => MED,
            Self::Light => LIGHT,
        }
    }

    /// The parsed token set.
    pub fn tokens(self) -> Result<Tokens, ThemeError> {
        Tokens::parse(self.source())
    }
}

/// An sRGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// To iced.
    pub fn to_color(self) -> iced::Color {
        iced::Color::from_rgb8(self.0, self.1, self.2)
    }
}

/// Theme parse failures (a vendoring mistake, caught by tests).
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ThemeError {
    #[error("missing token --n1-{0}")]
    Missing(&'static str),
    #[error("bad colour for --n1-{0}")]
    BadColour(String),
}

macro_rules! tokens {
    ($($field:ident => $name:literal),* $(,)?) => {
        /// The `--n1-*` tokens the editor UI uses.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Tokens { $(pub $field: Rgb,)* }

        impl Tokens {
            /// Every token name, in declaration order.
            pub const NAMES: &'static [&'static str] = &[$($name),*];

            /// Parses a vendored `--n1-name: #hex;` block.
            pub fn parse(src: &str) -> Result<Self, ThemeError> {
                let map = parse_block(src)?;
                Ok(Self { $($field: *map.get($name).ok_or(ThemeError::Missing($name))?,)* })
            }
        }
    };
}

tokens! {
    bg => "bg",
    panel => "panel",
    panel_2 => "panel-2",
    inset => "inset",
    line => "line",
    text => "text",
    text_dim => "text-dim",
    muted => "muted",
    text_faint => "text-faint",
    heading => "heading",
    accent => "accent",
    accent_hover => "accent-hover",
    accent_text => "accent-text",
    on_accent => "on-accent",
    sel_tint => "sel-tint",
    ok_text => "ok-text",
    warn_text => "warn-text",
    error_text => "error-text",
    info_text => "info-text",
    red => "red",
    orange => "orange",
    yellow => "yellow",
    green => "green",
    cyan => "cyan",
    blue => "blue",
    purple => "purple",
    pink => "pink",
    accent_ui => "accent-ui",
    accent_ui_hover => "accent-ui-hover",
    on_accent_ui => "on-accent-ui",
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let h = s.strip_prefix('#')?;
    let digit = |i: usize, n: usize| u8::from_str_radix(h.get(i..i + n)?, 16).ok();
    match h.len() {
        3 => Some(Rgb(digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17)),
        6 => Some(Rgb(digit(0, 2)?, digit(2, 2)?, digit(4, 2)?)),
        _ => None,
    }
}

fn parse_block(src: &str) -> Result<HashMap<String, Rgb>, ThemeError> {
    let mut out = HashMap::new();
    for line in src.lines() {
        let Some(rest) = line.trim().strip_prefix("--n1-") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_end_matches(';').trim();
        let rgb = parse_hex(value).ok_or_else(|| ThemeError::BadColour(name.to_owned()))?;
        out.insert(name.trim().to_owned(), rgb);
    }
    Ok(out)
}

impl Tokens {
    /// The iced theme for this token set: background/text from the surround, `primary` from
    /// `accent-ui` (never the brand accent), semantic colours from the status tokens.
    pub fn iced_theme(&self, name: &'static str) -> iced::Theme {
        iced::Theme::custom(
            name,
            iced::theme::Palette {
                background: self.bg.to_color(),
                text: self.text.to_color(),
                primary: self.accent_ui.to_color(),
                success: self.green.to_color(),
                warning: self.yellow.to_color(),
                danger: self.red.to_color(),
            },
        )
    }
}

// Vendored from https://github.com/dulrich/obsidian-crucible/blob/master/theme/theme.css:35-112 (body, body[data-surround="med"]).
const MED: &str = "
  --n1-bg: #3a3a3b;
  --n1-panel: #454547;
  --n1-panel-2: #5a5a5c;
  --n1-inset: #333335;
  --n1-line: #666668;
  --n1-text: #f3f3f4;
  --n1-text-dim: #dedee0;
  --n1-muted: #d2d2d4;
  --n1-text-faint: #949496;
  --n1-heading: #fff;
  --n1-accent: #ff8a3d;
  --n1-accent-hover: #ffa05c;
  --n1-accent-text: #ffbe91;
  --n1-on-accent: #1a1108;
  --n1-sel-tint: #4e4847;
  --n1-ok-text: #79eba8;
  --n1-warn-text: #f4cd85;
  --n1-error-text: #ffc9c2;
  --n1-info-text: #b7d7fd;
  --n1-red: #fb464c;
  --n1-orange: #e9973f;
  --n1-yellow: #e0de71;
  --n1-green: #44cf6e;
  --n1-cyan: #53dfdd;
  --n1-blue: #027aff;
  --n1-purple: #a882ff;
  --n1-pink: #fa99cd;
  --n1-accent-ui: #ba6b36;
  --n1-accent-ui-hover: #cb804d;
  --n1-on-accent-ui: #fff;
";

// Vendored from https://github.com/dulrich/obsidian-crucible/blob/master/theme/theme.css:114-180 (body[data-surround="dark"]).
const DARK: &str = "
  --n1-bg: #0e0e10;
  --n1-panel: #17181a;
  --n1-panel-2: #26262a;
  --n1-inset: #0d0d0f;
  --n1-line: #37383b;
  --n1-text: #e9e9ea;
  --n1-text-dim: #cacbcd;
  --n1-muted: #9b9c9f;
  --n1-text-faint: #7c7d80;
  --n1-heading: #fff;
  --n1-accent: #ff8a3d;
  --n1-accent-hover: #ffa05c;
  --n1-accent-text: #ff8a3d;
  --n1-on-accent: #1a1108;
  --n1-sel-tint: #231e1c;
  --n1-ok-text: #4ade80;
  --n1-warn-text: #f1c675;
  --n1-error-text: #ffc0b9;
  --n1-info-text: #60a5fa;
  --n1-red: #fb464c;
  --n1-orange: #e9973f;
  --n1-yellow: #e0de71;
  --n1-green: #44cf6e;
  --n1-cyan: #53dfdd;
  --n1-blue: #027aff;
  --n1-purple: #a882ff;
  --n1-pink: #fa99cd;
  --n1-accent-ui: #cc763e;
  --n1-accent-ui-hover: #d48d5e;
  --n1-on-accent-ui: #fff;
";

// Vendored from https://github.com/dulrich/obsidian-crucible/blob/master/theme/theme.css:182-249 (body[data-surround="light"]).
const LIGHT: &str = "
  --n1-bg: #f4f4f5;
  --n1-panel: #dfdfe0;
  --n1-panel-2: #d2d2d4;
  --n1-inset: #fbfbfc;
  --n1-line: #a5a5a8;
  --n1-text: #191a1b;
  --n1-text-dim: #333335;
  --n1-muted: #4f5052;
  --n1-text-faint: #6d6d6f;
  --n1-heading: #0c0c0d;
  --n1-accent: #f07714;
  --n1-accent-hover: #d96a0e;
  --n1-accent-text: #7a3800;
  --n1-on-accent: #fff;
  --n1-sel-tint: #e0dad6;
  --n1-ok-text: #1d5c31;
  --n1-warn-text: #684400;
  --n1-error-text: #8f231a;
  --n1-info-text: #144a8f;
  --n1-red: #e93147;
  --n1-orange: #ec7500;
  --n1-yellow: #e0ac00;
  --n1-green: #08b94e;
  --n1-cyan: #00bfbc;
  --n1-blue: #086ddd;
  --n1-purple: #7852ee;
  --n1-pink: #d53984;
  --n1-accent-ui: #af571d;
  --n1-accent-ui-hover: #954a18;
  --n1-on-accent-ui: #fff;
";

// ---- editor-usability-round WP-3: Signalworks icon buttons and seg-groups ----
//
// Signalworks (`signalworks-design/readme.md`, CC-11 icon button, CC-16 seg-group) mapped onto
// N1 tokens: `panel_2` raised fill, `line` hairline, `text_faint` achromatic hover promote,
// `text_dim` glyph, `accent_text` active glyph. One control height: 34px.

/// The single control height (Signalworks).
pub const CONTROL: f32 = 34.0;
/// The seg-group inner segment height.
pub const SEGMENT: f32 = 32.0;
/// Control corner radius.
pub const RADIUS: f32 = 8.0;
/// Icon glyph size.
pub const ICON_SIZE: f32 = 16.0;

/// CC-11 icon-button style: `panel_2` fill, 1px `line` border; hover/press promotes the border
/// to `text_faint` (never an accent tint); `active` draws the glyph in `accent_text`; disabled
/// dims the glyph to `text_faint`.
pub fn icon_button_style(
    t: Tokens,
    active: bool,
) -> impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    move |_, status| {
        let border = match status {
            Status::Hovered | Status::Pressed => t.text_faint,
            Status::Active | Status::Disabled => t.line,
        };
        let glyph = match status {
            Status::Disabled => t.text_faint,
            Status::Pressed => t.accent_text,
            _ if active => t.accent_text,
            _ => t.text_dim,
        };
        iced::widget::button::Style {
            background: Some(t.panel_2.to_color().into()),
            text_color: glyph.to_color(),
            border: iced::Border {
                color: border.to_color(),
                width: 1.0,
                radius: RADIUS.into(),
            },
            ..iced::widget::button::Style::default()
        }
    }
}

/// The glyph text for `icon`.
pub fn icon_text<'a>(icon: crate::icons::Icon) -> iced::widget::Text<'a> {
    iced::widget::text(icon.glyph().to_string())
        .font(crate::icons::FONT)
        .size(ICON_SIZE)
        .line_height(1.0)
        .align_x(iced::alignment::Horizontal::Center)
        .align_y(iced::alignment::Vertical::Center)
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
}

/// A 34×34 CC-11 icon button; `msg = None` renders it disabled.
pub fn icon_button<'a, M: Clone + 'a>(
    icon: crate::icons::Icon,
    msg: Option<M>,
    t: Tokens,
) -> iced::widget::Button<'a, M> {
    iced::widget::button(icon_text(icon))
        .width(CONTROL)
        .height(CONTROL)
        .padding(0)
        .style(icon_button_style(t, false))
        .on_press_maybe(msg)
}

/// CC-16 seg-group container: `panel` fill, 1px `line` border, radius 8.
pub fn seg_group_style(t: Tokens) -> impl Fn(&iced::Theme) -> iced::widget::container::Style {
    move |_| iced::widget::container::Style {
        background: Some(t.panel.to_color().into()),
        border: iced::Border {
            color: t.line.to_color(),
            width: 1.0,
            radius: RADIUS.into(),
        },
        ..iced::widget::container::Style::default()
    }
}

/// CC-16 segment: flush, transparent; the active one is a `panel_2` block with an `accent_text`
/// glyph; hover promotes the glyph achromatically (`text`) and never clears the active block.
pub fn segment_style(
    t: Tokens,
    active: bool,
) -> impl Fn(&iced::Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    move |_, status| {
        let hovered = matches!(status, Status::Hovered | Status::Pressed);
        let glyph = if active {
            t.accent_text
        } else if hovered {
            t.text
        } else {
            t.text_dim
        };
        iced::widget::button::Style {
            background: active.then(|| t.panel_2.to_color().into()),
            text_color: glyph.to_color(),
            border: iced::Border {
                radius: (RADIUS - 1.0).into(),
                ..iced::Border::default()
            },
            ..iced::widget::button::Style::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_surrounds_parse_with_every_token() {
        for s in Surround::ALL {
            let src = s.source();
            for name in Tokens::NAMES {
                assert!(
                    src.contains(&format!("--n1-{name}:")),
                    "{s:?} lacks --n1-{name}"
                );
            }
            assert!(s.tokens().is_ok(), "{s:?} failed to parse");
        }
    }

    #[test]
    fn default_is_med_and_accent_ui_is_separate() {
        assert_eq!(Surround::default(), Surround::Med);
        let t = Surround::default().tokens().expect("med");
        assert_eq!(t.accent_ui, Rgb(0xba, 0x6b, 0x36));
        assert_eq!(t.accent, Rgb(0xff, 0x8a, 0x3d));
        assert_eq!(t.heading, Rgb(255, 255, 255));
    }

    #[test]
    fn cycle_order_matches_surround_ts() {
        assert_eq!(Surround::Dark.next(), Surround::Med);
        assert_eq!(Surround::Med.next(), Surround::Light);
        assert_eq!(Surround::Light.next(), Surround::Dark);
    }

    #[test]
    fn missing_and_bad_tokens_are_errors() {
        assert_eq!(
            Tokens::parse("--n1-bg: #000;"),
            Err(ThemeError::Missing("panel"))
        );
        assert!(matches!(
            Tokens::parse("--n1-bg: red;"),
            Err(ThemeError::BadColour(_))
        ));
    }
}
