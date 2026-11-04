//! Reading link handling (split out of `reading/mod.rs`, plan tn-review-2026-10-26 WP-R4): a
//! link target resolved through the shared resolver, and the click that opens it or flags it
//! missing.

use std::sync::Arc;

use editor_model::render;
use iced::Task;

use super::{Effect, ReadingStore, percent_decode};
use crate::daemon::DaemonClient;
use crate::message::Message;

impl ReadingStore {
    /// Plan round-5 WP-8: the vault path a link `target` in `src` resolves to (the
    /// [`ReadingMsg::OpenLink`] resolver; `None` for remote / unresolved / no daemon).
    pub fn resolve_link(
        &mut self,
        src: &str,
        target: &str,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) -> Option<String> {
        if render::is_remote(target) {
            return None;
        }
        let d = daemon?;
        self.resolver.refresh(d.as_ref());
        // A source-mode md-link click still carries its `#fragment` here.
        let target = percent_decode(editor_model::link_at::split_href(target).0);
        // `[[#h]]` resolves to the note itself; keep it (focuses the same tab).
        self.resolve(src, &target)
    }

    /// Resolves `target` written in `src` to a vault path.
    pub(super) fn resolve(&self, src: &str, target: &str) -> Option<String> {
        self.resolver.resolve(src, target)
    }

    /// The vault path a link click opens when it resolves, else `None` (unresolved links and
    /// URLs with a scheme do not open). `App` places the open (WP-R3, review E7).
    pub fn click(
        &mut self,
        src: &str,
        target: &str,
        daemon: Option<&Arc<dyn DaemonClient>>,
    ) -> Option<String> {
        self.resolve_link(src, target, daemon)
    }
}

/// A link click: returns the resolved note's open path as an effect, else flags it missing.
pub(super) fn open_link(
    store: &mut ReadingStore,
    src: String,
    target: String,
    fragment: Option<String>,
    daemon: Option<&Arc<dyn DaemonClient>>,
    fx: &mut Vec<Effect>,
) -> Task<Message> {
    if let Some(p) = store.click(&src, &target, daemon) {
        fx.push(Effect::OpenPath(p, fragment));
        return Task::none();
    }
    // plan round-5 WP-3: an unresolved vault link says so instead of doing nothing;
    // plan round-6 WP-4: `App` warns or creates it per `missing_link`.
    if daemon.is_some() && !render::is_remote(&target) {
        fx.push(Effect::MissingLink(target));
    }
    Task::none()
}
