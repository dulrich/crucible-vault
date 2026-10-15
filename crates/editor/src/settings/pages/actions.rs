//! The Actions page: `_custom/crucible-vault/actions.toml` as one schema-driven form.

use iced::Element;

use super::PageCx;
use crate::message::Message;
use crate::settings::ConfigFile;

/// The page intro line.
pub const INTRO: &str = "Actions, each with params, starters, resources and its step list. Lint, localize and search settings are the lint / localize / search_* step forms.";

/// The page.
pub fn view<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Element<'a, Message> {
    super::generic(cx, f, INTRO)
}
