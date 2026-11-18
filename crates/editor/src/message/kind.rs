//! `kind()` for every message type: whether a message is a user gesture ([`MsgKind::Input`]) or a
//! completion / passive tracking message ([`MsgKind::Completion`]).

use super::{
    ExplorerMsg, FindMsg, Message, MsgKind, NavMsg, PaletteMsg, PanelsMsg, PanesMsg, ReadingMsg,
    RemovalsMsg, SourceMsg, SwitcherMsg, TabsMsg, ViewsMsg,
};

impl RemovalsMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Trim(_) | Self::Next => MsgKind::Input,
            Self::Resolved(_) | Self::NextFound(_) => MsgKind::Completion,
        }
    }
}

impl SourceMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::AutosaveDue { .. }
            | Self::Saved { .. }
            | Self::PastePrimary { .. }
            | Self::PasteResolved { .. }
            | Self::Viewport { .. } => MsgKind::Completion,
            Self::Action(_)
            | Self::Undo
            | Self::Redo
            | Self::Save
            | Self::KeepMine(_)
            | Self::TakeTheirs(_)
            | Self::Key(_)
            | Self::MiddleClick { .. }
            | Self::CommandClick { .. }
            | Self::Cut
            | Self::Paste
            | Self::LinkKey(_)
            | Self::LinkPick(_) => MsgKind::Input,
        }
    }
}

impl SwitcherMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Created(_) => MsgKind::Completion,
            Self::Open
            | Self::OpenMove
            | Self::Close
            | Self::Escape
            | Self::Query(_)
            | Self::Up
            | Self::Down
            | Self::Submit
            | Self::Create
            | Self::Pick(_) => MsgKind::Input,
        }
    }
}

impl TabsMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Open(_)
            | Self::Select(..)
            | Self::Close(..)
            | Self::CloseActive
            | Self::GoTo(_)
            | Self::Quit
            | Self::FlushAll
            | Self::DragPress(..)
            | Self::DragMove(_)
            | Self::DragEnter(_)
            | Self::DragLeave(_)
            | Self::DragRelease
            | Self::DragCancel
            | Self::HeaderEnter(_)
            | Self::HeaderLeave(_)
            | Self::StripWheel(..)
            | Self::StripKey(_) => MsgKind::Input,
        }
    }
}

impl Message {
    /// Whether `self` is a user gesture ([`MsgKind::Input`]) or a completion / passive tracking
    /// message (an async result, a timer tick, modifier state, the global pointer listeners).
    /// Plan WP-R8 (E5): a tab strip with keyboard focus loses it on any `Input` that is not the
    /// strip's own ([`TabsMsg::is_strip_own`]).
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Source(m) => m.kind(),
            Self::Switcher(m) => m.kind(),
            Self::Find(m) => m.kind(),
            Self::Tabs(m) => m.kind(),
            Self::Reading(m) => m.kind(),
            Self::Removals(m) => m.kind(),
            Self::Explorer(m) => m.kind(),
            Self::Palette(m) => m.kind(),
            Self::Panels(m) => m.kind(),
            Self::Views(m) => m.kind(),
            Self::Panes(m) => m.kind(),
            Self::Nav(NavMsg::Go { .. }) | Self::Theme(_) | Self::Dialog(_) | Self::Menu(_) => {
                MsgKind::Input
            }
            Self::Nav(NavMsg::Opened { .. })
            | Self::PersistDue
            | Self::MemoryDue
            | Self::TrimTick
            | Self::HoldTick
            | Self::Status(_)
            | Self::Settings(_)
            | Self::Log(_)
            | Self::OpenUrl(_)
            | Self::ImageTab(_)
            | Self::PdfTab(_)
            | Self::MediaLoaded { .. }
            | Self::TitleRenamed { .. }
            | Self::Settled { .. }
            | Self::NoteOpened { .. }
            | Self::Modifiers(_) => MsgKind::Completion,
        }
    }
}

impl ReadingMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Scrolled { .. }
            | Self::Probed { .. }
            | Self::Tick
            | Self::ImageShown { .. }
            | Self::ImageHidden { .. }
            | Self::ImageDescribed { .. }
            | Self::SelectDone { .. }
            | Self::CodeCopiedDone { .. } => MsgKind::Completion,
            Self::CycleMode
            | Self::SetMode { .. }
            | Self::OpenLink { .. }
            | Self::ToggleTask { .. }
            | Self::ToggleProp { .. }
            | Self::GoToEdge { .. }
            | Self::TitleInput { .. }
            | Self::TitleSubmit { .. }
            | Self::TitleRevert
            | Self::TitleFocus { .. }
            | Self::Select { .. }
            | Self::Removals { .. }
            | Self::Copy { .. } => MsgKind::Input,
        }
    }
}

impl ExplorerMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Refresh
            | Self::Scrolled(_)
            | Self::Done(..)
            | Self::DeletePreview { .. }
            | Self::NoteDeleted(_) => MsgKind::Completion,
            Self::Toggle(_)
            | Self::Select(_)
            | Self::Open(_)
            | Self::BeginNew
            | Self::BeginRename
            | Self::BeginMove
            | Self::Trash
            | Self::Input(_)
            | Self::Submit
            | Self::Cancel
            | Self::DeleteActive => MsgKind::Input,
        }
    }
}

impl PaletteMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Loaded(_) | Self::Ran { .. } | Self::Tick => MsgKind::Completion,
            Self::Open
            | Self::Close
            | Self::Escape
            | Self::Query(_)
            | Self::Up
            | Self::Down
            | Self::Submit
            | Self::Pick(_)
            | Self::Key(_)
            | Self::CtrlEnter
            | Self::Enter
            | Self::Prompt(_)
            | Self::RunPreset(_) => MsgKind::Input,
        }
    }
}

impl PanelsMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Tick
            | Self::Backlinks { .. }
            | Self::SearchDue(_)
            | Self::SearchDone { .. }
            | Self::Scrolled(..) => MsgKind::Completion,
            Self::Toggle
            | Self::Search
            | Self::CloseSearchModal
            | Self::SearchModalEscape
            | Self::Show(_)
            | Self::OutlineJump(_)
            | Self::Open(_)
            | Self::TagPick(_)
            | Self::TagBack
            | Self::SearchInput(_)
            | Self::ResolveDup { .. } => MsgKind::Input,
        }
    }
}

impl ViewsMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Need { .. }
            | Self::Loaded { .. }
            | Self::Scrolled { .. }
            | Self::StatusSet(_)
            | Self::BoolSet(_) => MsgKind::Completion,
            Self::SortBy { .. }
            | Self::Open { .. }
            | Self::SetStatus { .. }
            | Self::SetBool { .. }
            | Self::ToggleOpenOnly { .. }
            | Self::Select { .. } => MsgKind::Input,
        }
    }
}

impl PanesMsg {
    /// `Hover`, `MouseButton` and `Resized` are passive pointer tracking (the global listeners
    /// and the grid's own divider), not a gesture on another widget.
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Hover(_) | Self::MouseButton(_) | Self::Resized(_) => MsgKind::Completion,
            Self::SplitRight
            | Self::SplitDown
            | Self::Close(_)
            | Self::Focus(_)
            | Self::Bookmark(_)
            | Self::Back(_)
            | Self::Forward(_)
            | Self::BackFocused
            | Self::ForwardFocused
            | Self::ToggleSide
            | Self::Side(_)
            | Self::ToggleBorders
            | Self::ModeMenu(_)
            | Self::Page(_) => MsgKind::Input,
        }
    }
}

impl FindMsg {
    pub fn kind(&self) -> MsgKind {
        match self {
            Self::Open
            | Self::Close
            | Self::Query(_)
            | Self::Submit
            | Self::Next
            | Self::Prev
            | Self::Focus(_) => MsgKind::Input,
        }
    }
}
