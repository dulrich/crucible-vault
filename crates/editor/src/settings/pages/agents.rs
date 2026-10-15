//! The Agents page: `_custom/crucible-vault/agents.toml` as one schema-driven form.

use iced::Element;

use super::PageCx;
use crate::message::Message;
use crate::settings::ConfigFile;

/// The page intro line.
pub const INTRO: &str = "Agent definitions (one table per agent id).";

/// The page.
pub fn view<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Element<'a, Message> {
    super::generic(cx, f, INTRO)
}
