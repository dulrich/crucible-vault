//! The Providers page: `_custom/crucible-vault/providers.toml`. `api_key_env` / `api_key_file`
//! are reference names only — the page shows the names as written in the file and never reads
//! an environment variable or a key file (no code path here touches `std::env` or the secrets
//! dir), so a secret value can never be displayed.

use iced::Element;

use super::PageCx;
use crate::message::Message;
use crate::settings::ConfigFile;

/// The page intro line.
pub const INTRO: &str = "Model providers. API keys are referenced by env var name or key file name only; the values are never read or shown here.";

/// The page.
pub fn view<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Element<'a, Message> {
    super::generic(cx, f, INTRO)
}
