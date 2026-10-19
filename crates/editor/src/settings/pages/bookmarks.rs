//! The Bookmarks page: `bookmarks.toml` as a form. The bookmarks panel works on the same
//! [`ConfigFile`] (one loaded doc + hash — see `crate::bookmarks`): a panel edit is refused while
//! this page has unsaved edits, and a save from either surface moves the shared hash, so neither
//! conflicts with the other.

use iced::Element;

use super::PageCx;
use crate::message::Message;
use crate::settings::ConfigFile;

/// The page intro line.
pub const INTRO: &str = "Bookmarks shown in the side panel. The panel edits the same file; both save automatically, and while this page has an edit not yet saved the panel waits for it.";

/// The page.
pub fn view<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Element<'a, Message> {
    super::generic(cx, f, INTRO)
}
