//! The Kinds page: `_custom/crucible-vault/kinds.toml` as one schema-driven form.

use iced::Element;

use super::PageCx;
use crate::message::Message;
use crate::settings::ConfigFile;

/// The page intro line.
pub const INTRO: &str = "Note kinds: folder, template and defaults per kind.";

/// The page.
pub fn view<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Element<'a, Message> {
    super::generic(cx, f, INTRO)
}
