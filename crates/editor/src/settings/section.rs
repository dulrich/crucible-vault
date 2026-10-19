//! Section routing: which section a daemon error names, and each section's body scroll.

use super::{Section, Settings};

/// The section a daemon config error names, from the error formats of
/// `cv-actions/src/config.rs` (`"<file>: …"`, `"action \"id\" disarmed: …"`) as surfaced by
/// `Runner::reload_config` (`crates/cv/src/runner/mod.rs:319-332`). `None` → Status.
pub fn attribute(error: &str) -> Option<Section> {
    if error.starts_with("action ") {
        return Some(Section::Actions);
    }
    let (head, _) = error.split_once(':')?;
    let head = head.trim();
    let name = head.rsplit('/').next().unwrap_or(head);
    Section::for_file_name(name)
}

/// The page body scrollable's stable id, per section (plan editor-ui-round-3 WP-7): its scroll
/// offset survives view rebuilds (capture enter/leave, notices) instead of following tree shape.
pub fn body_scroll_id(sec: Section) -> iced::widget::Id {
    iced::widget::Id::from(format!("settings-body:{}", sec.label()))
}

impl Settings {
    /// Plan round-5 WP-1: re-applies the current section's stored body offset (0 when none).
    pub fn restore_scroll<T: Send + 'static>(&self) -> iced::Task<T> {
        let y = self.body_offsets.get(&self.section).copied().unwrap_or(0.0);
        iced::widget::operation::scroll_to(
            body_scroll_id(self.section),
            iced::widget::scrollable::AbsoluteOffset {
                x: None,
                y: Some(y),
            },
        )
    }
}
