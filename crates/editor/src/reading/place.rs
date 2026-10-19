//! Reading place-keeping (plan editor-ui-round-3 WP-6, round-7 WP-4): the layout probe that
//! measures each top-level block, and [`ScrollIntent`], every programmatic reading scroll.

use iced::Task;

use super::block_id;
use crate::message::{Message, ReadingMsg};

pub use editor_model::place::Probe;

/// The widget operation behind [`Probe`]: reads the reading scrollable and the id'd block
/// containers of one path.
pub(crate) struct ProbeOp {
    scroll: iced::widget::Id,
    blocks: Vec<iced::widget::Id>,
    out: Probe,
    found: bool,
}

impl iced::advanced::widget::Operation<Probe> for ProbeOp {
    fn traverse(
        &mut self,
        operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<Probe>),
    ) {
        operate(self);
    }

    fn container(&mut self, id: Option<&iced::widget::Id>, bounds: iced::Rectangle) {
        if let Some(id) = id
            && let Some(i) = self.blocks.iter().position(|b| b == id)
        {
            self.out.block_ys[i] = Some(bounds.y);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: iced::Rectangle,
        content_bounds: iced::Rectangle,
        translation: iced::Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id == Some(&self.scroll) {
            self.out.offset_y = translation.y;
            self.out.content_y = content_bounds.y;
            self.out.viewport_h = bounds.height;
            self.out.content_h = content_bounds.height;
            self.found = true;
        }
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<Probe> {
        if self.found {
            iced::advanced::widget::operation::Outcome::Some(self.out.clone())
        } else {
            iced::advanced::widget::operation::Outcome::None
        }
    }
}

/// Plan round-7 WP-4: builds the [`ProbeOp`] for `path`'s reading layout in tab `view`
/// (`blocks` top-level blocks) — shared by [`probe`] and the headless test harness.
pub(crate) fn probe_op(path: &str, view: u64, blocks: usize) -> ProbeOp {
    ProbeOp {
        scroll: crate::find::reading_scroll_id(path, view),
        blocks: (0..blocks).map(|i| block_id(path, view, i)).collect(),
        out: Probe {
            block_ys: vec![None; blocks],
            ..Probe::default()
        },
        found: false,
    }
}

/// plan editor-ui-round-3 WP-6: probes `path`'s reading layout (`blocks` top-level blocks),
/// answering with [`ReadingMsg::Probed`] (`scroll_to` passed through).
pub fn probe(path: &str, view: u64, blocks: usize, scroll_to: Option<usize>) -> Task<Message> {
    ScrollIntent::Probe {
        path: path.to_owned(),
        view,
        blocks,
        scroll_to,
    }
    .task()
}

/// Plan round-7 WP-4 (audit E1): one programmatic scroll or layout probe of tab `view`'s
/// reading scrollable on `path`, as a plain value. Every editor-issued reading scroll goes
/// through [`ScrollIntent::task`], so the headless tests can apply the same intents to a
/// laid-out tree (the returned `Task` is opaque outside the runtime).
#[derive(Debug, Clone, PartialEq)]
pub enum ScrollIntent {
    /// Snap to a relative vertical offset (0 = top, 1 = bottom).
    SnapTo { path: String, view: u64, y: f32 },
    /// Scroll to an absolute vertical offset.
    ScrollTo { path: String, view: u64, y: f32 },
    /// PageUp / PageDown ([`crate::page::page`]).
    Page { path: String, view: u64, down: bool },
    /// One line step of `dy` pixels ([`crate::page::step`]).
    Step { path: String, view: u64, dy: f32 },
    /// A layout probe answered by [`ReadingMsg::Probed`].
    Probe {
        path: String,
        view: u64,
        blocks: usize,
        scroll_to: Option<usize>,
    },
    /// Plan round-7 WP-4 (attempt 2): re-sends the source editor's viewport report after the
    /// next layout (the next pass of a Reading → Source line carry, [`SourceBuffer::on_layout`]
    /// returned [`Relayout::Again`]).
    ///
    /// [`SourceBuffer::on_layout`]: crate::source::SourceBuffer::on_layout
    /// [`Relayout::Again`]: crate::source::Relayout::Again
    SourceRelayout { path: String, lines: usize },
}

#[cfg(test)]
thread_local! {
    /// Test hook: every intent turned into a task on this thread, in order.
    pub(crate) static INTENTS: std::cell::RefCell<Vec<ScrollIntent>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

impl ScrollIntent {
    /// The runtime task carrying out this intent.
    pub fn task(self) -> Task<Message> {
        #[cfg(test)]
        INTENTS.with(|i| i.borrow_mut().push(self.clone()));
        let id = |path: &str, view| crate::find::reading_scroll_id(path, view);
        match self {
            Self::SnapTo { path, view, y } => iced::widget::operation::snap_to(
                id(&path, view),
                iced::widget::scrollable::RelativeOffset { x: 0.0, y },
            ),
            Self::ScrollTo { path, view, y } => iced::widget::operation::scroll_to(
                id(&path, view),
                iced::widget::scrollable::AbsoluteOffset {
                    x: None,
                    y: Some(y),
                },
            ),
            Self::Page { path, view, down } => crate::page::page(id(&path, view), down),
            Self::Step { path, view, dy } => crate::page::step(id(&path, view), dy),
            Self::SourceRelayout { path, lines } => {
                Task::done(Message::Source(crate::message::SourceMsg::Viewport {
                    path,
                    lines,
                }))
            }
            Self::Probe {
                path,
                view,
                blocks,
                scroll_to,
            } => {
                let op = probe_op(&path, view, blocks);
                iced::advanced::widget::operate(op).map(move |p: Probe| {
                    Message::Reading(ReadingMsg::Probed {
                        path: path.clone(),
                        view,
                        probe: Some(p),
                        scroll_to,
                    })
                })
            }
        }
    }
}
