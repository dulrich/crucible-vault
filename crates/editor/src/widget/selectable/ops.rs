//! Widget operations over the runs: collecting their boxes once per event, reaching one run's
//! state, and picking the run under a content point.

use std::any::Any;
use std::marker::PhantomData;

use iced::advanced::Layout;
use iced::advanced::text;
use iced::advanced::widget::{Operation, Tree, operation};
use iced::{Element, Point, Rectangle, Vector};

use super::SelPos;
use super::run::RunState;

/// The renderer's paragraph type.
pub(super) type Para = <iced::Renderer as text::Renderer>::Paragraph;

/// A run's laid-out bounds (content coordinates) and text length.
#[derive(Debug, Clone, Copy)]
pub struct RunBox {
    pub run: usize,
    pub bounds: Rectangle,
    pub len: usize,
}

/// Every run's box plus the target scrollable's translation and viewport — collected once per
/// event (WP-R17 F5).
#[derive(Debug, Clone, Default)]
pub(super) struct Collected {
    pub translation: Vector,
    pub viewport: Option<Rectangle>,
    pub runs: Vec<RunBox>,
}

/// The collecting operation.
struct Collect<L> {
    scroll: Option<iced::widget::Id>,
    out: Collected,
    _p: PhantomData<fn() -> L>,
}

impl<L: Clone + 'static> Operation for Collect<L> {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self);
    }

    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: Rectangle,
        _content: Rectangle,
        translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        if id.is_some() && id == self.scroll.as_ref() {
            self.out.translation = translation;
            self.out.viewport = Some(bounds);
        }
    }

    fn custom(&mut self, _id: Option<&iced::widget::Id>, bounds: Rectangle, state: &mut dyn Any) {
        if let Some(s) = state.downcast_ref::<RunState<Para, L>>() {
            self.out.runs.push(RunBox {
                run: s.run,
                bounds,
                len: s.text.len(),
            });
        }
    }
}

/// Runs `f` on the state of run `run` (with its bounds).
struct OnRun<L, F> {
    run: usize,
    f: Option<F>,
    _p: PhantomData<fn() -> L>,
}

impl<L, F> Operation for OnRun<L, F>
where
    L: Clone + 'static,
    F: FnOnce(&RunState<Para, L>, Rectangle) + Send,
{
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        if self.f.is_some() {
            operate(self);
        }
    }

    fn custom(&mut self, _id: Option<&iced::widget::Id>, bounds: Rectangle, state: &mut dyn Any) {
        if let Some(s) = state.downcast_ref::<RunState<Para, L>>()
            && s.run == self.run
            && let Some(f) = self.f.take()
        {
            f(s, bounds);
        }
    }
}

/// Every run's box under `el`, plus the scrollable `scroll`'s translation and viewport.
pub(super) fn collect<M, L: Clone + 'static>(
    el: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    r: &iced::Renderer,
    scroll: Option<iced::widget::Id>,
) -> Collected {
    let mut op = Collect::<L> {
        scroll,
        out: Collected::default(),
        _p: PhantomData,
    };
    el.as_widget_mut().operate(tree, layout, r, &mut op);
    op.out
}

/// `f` applied to run `run`'s state and bounds under `el` (`T::default()` when absent).
pub(super) fn on_run<M, L: Clone + 'static, T: Send + Default>(
    el: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    r: &iced::Renderer,
    run: usize,
    f: impl FnOnce(&RunState<Para, L>, Rectangle) -> T + Send,
) -> T {
    let out = std::sync::Mutex::new(T::default());
    {
        let mut op = OnRun::<L, _> {
            run,
            f: Some(|s: &RunState<Para, L>, b: Rectangle| {
                if let Ok(mut o) = out.lock() {
                    *o = f(s, b);
                }
            }),
            _p: PhantomData,
        };
        el.as_widget_mut().operate(tree, layout, r, &mut op);
    }
    out.into_inner().unwrap_or_default()
}

/// Every run's box under `el` (content coordinates; tests and probes).
pub fn probe_runs<M, L: Clone + 'static>(
    el: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    r: &iced::Renderer,
) -> Vec<RunBox> {
    collect::<M, L>(el, tree, layout, r, None).runs
}

/// The caret point (content coordinates) of `pos` under `el` (tests and probes).
pub fn probe_caret<M, L: Clone + 'static>(
    el: &mut Element<'_, M>,
    tree: &mut Tree,
    layout: Layout<'_>,
    r: &iced::Renderer,
    pos: SelPos,
) -> Option<Point> {
    on_run::<M, L, _>(el, tree, layout, r, pos.run, |s, b| {
        Some(s.caret(pos.byte) + (b.position() - Point::ORIGIN))
    })
}

/// Which run a content point belongs to when it is not over one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Bias {
    /// Dragging: a gap snaps to the end of the run above.
    Drag,
    /// Line down: a gap goes to the run below.
    Down,
    /// Line up: a gap goes to the run above.
    Up,
}

/// The run for content point `p`: `(index into runs, point clamped into it)`.
pub(super) fn pick(runs: &[RunBox], p: Point, bias: Bias) -> Option<(usize, Point)> {
    let clamp = |r: &Rectangle, p: Point| {
        Point::new(
            p.x.clamp(r.x - 1.0, r.x + r.width + 1.0),
            p.y.clamp(r.y, r.y + (r.height - 1.0).max(0.0)),
        )
    };
    if let Some(i) = runs.iter().position(|r| r.bounds.contains(p)) {
        return Some((i, p));
    }
    // Same band (e.g. table cells side by side): the horizontally nearest.
    let band = runs
        .iter()
        .enumerate()
        .filter(|(_, r)| p.y >= r.bounds.y && p.y < r.bounds.y + r.bounds.height)
        .min_by(|(_, a), (_, b)| {
            let d = |r: &Rectangle| (p.x - r.center_x()).abs();
            d(&a.bounds).total_cmp(&d(&b.bounds))
        });
    if let Some((i, r)) = band {
        return Some((i, clamp(&r.bounds, p)));
    }
    let above = runs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.bounds.y + r.bounds.height <= p.y)
        .max_by_key(|(_, r)| r.run);
    let below = runs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.bounds.y > p.y)
        .min_by_key(|(_, r)| r.run);
    let end = |(i, r): (usize, &RunBox)| {
        (
            i,
            Point::new(f32::MAX / 4.0, r.bounds.y + r.bounds.height - 1.0),
        )
    };
    match bias {
        Bias::Drag => above
            .map(end)
            .or_else(|| below.map(|(i, r)| (i, Point::new(-1.0, r.bounds.y)))),
        Bias::Up => above
            .map(|(i, r)| (i, clamp(&r.bounds, p)))
            .or_else(|| below.map(|(i, r)| (i, Point::new(-1.0, r.bounds.y)))),
        Bias::Down => below
            .map(|(i, r)| (i, clamp(&r.bounds, p)))
            .or_else(|| above.map(end)),
    }
}
