//! The keyboard keys the source editor routes as messages ([`super::SourceMsg`]): split out of
//! `message.rs` (plan tn-review-2026-11-10-remediation WP-R5), re-exported there unchanged.

/// Plan round-5 WP-9: keys the open `[[` note list takes from the source editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKey {
    Up,
    Down,
    Enter,
    Tab,
    Escape,
}

/// WP-1 source keys routed through `SourceMsg::Key` (each one transaction or motion).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKey {
    /// Enter with list continuation.
    Enter,
    /// Ctrl+Backspace: delete to the previous word boundary (or the selection).
    DeleteWordBack,
    /// Ctrl+Delete: delete to the next word boundary (or the selection).
    DeleteWordForward,
    /// Smart Home (`select` = Shift).
    Home { select: bool },
    /// Smart End (`select` = Shift).
    End { select: bool },
    /// Tab: indent the selected lines, else insert `\t` (plan editor-feedback-round-2 WP-4).
    Indent,
    /// Shift+Tab: outdent the selected lines (or the cursor's line).
    Outdent,
}
