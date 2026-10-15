//! Pane grid layout (WP-4): an `iced::widget::pane_grid` whose panes each hold their own tabs,
//! plus conversion to/from the persisted [`PaneLayout`].

use iced::widget::pane_grid::{self, Axis, Configuration, Node, Pane};

use crate::app::Tab;
use crate::ui_state::{PaneLayout, PaneNode, SplitDir};

/// One pane: its tabs and the active index.
#[derive(Debug, Default)]
pub struct PaneState {
    pub tabs: Vec<Tab>,
    pub active: Option<usize>,
    /// editor-usability-round WP-3: back/forward (not persisted).
    pub history: crate::history::History,
    /// editor-usability-round WP-4: the find bar (`None` = closed).
    pub find: Option<crate::find::FindBar>,
}

impl PaneState {
    /// The active tab.
    pub fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    /// The active tab when it is a note (never the Settings tab).
    pub fn active_note(&self) -> Option<&Tab> {
        self.active_tab().filter(|t| !t.is_settings())
    }

    /// Removes tab `i`, fixing the active index; returns the removed tab.
    pub fn remove(&mut self, i: usize) -> Option<Tab> {
        if i >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(i);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(a) if a > i || a >= self.tabs.len() => Some(a.saturating_sub(1)),
            other => other,
        };
        Some(tab)
    }
}

/// The grid's panes in depth-first layout order.
pub fn order(state: &pane_grid::State<PaneState>) -> Vec<Pane> {
    let mut out = Vec::new();
    let mut stack = vec![state.layout()];
    while let Some(n) = stack.pop() {
        match n {
            Node::Pane(p) => out.push(*p),
            Node::Split { a, b, .. } => {
                stack.push(b);
                stack.push(a);
            }
        }
    }
    out
}

fn dir_of(axis: Axis) -> SplitDir {
    match axis {
        Axis::Vertical => SplitDir::Right,
        Axis::Horizontal => SplitDir::Down,
    }
}

/// The pane_grid axis for a split direction (`Vertical` = a vertical divider, side by side).
pub fn axis_of(dir: SplitDir) -> Axis {
    match dir {
        SplitDir::Right => Axis::Vertical,
        SplitDir::Down => Axis::Horizontal,
    }
}

/// Snapshot of the grid for `editor.json`.
pub fn snapshot(state: &pane_grid::State<PaneState>, focus: Pane) -> PaneLayout {
    fn walk(n: &Node, state: &pane_grid::State<PaneState>) -> PaneNode {
        match n {
            Node::Pane(p) => {
                let (tabs, active) = state
                    .get(*p)
                    .map(|s| (s.tabs.iter().map(|t| t.path.clone()).collect(), s.active))
                    .unwrap_or_default();
                PaneNode::Leaf { tabs, active }
            }
            Node::Split {
                axis, ratio, a, b, ..
            } => PaneNode::Split {
                dir: dir_of(*axis),
                ratio_milli: ratio_to_milli(*ratio),
                a: Box::new(walk(a, state)),
                b: Box::new(walk(b, state)),
            },
        }
    }
    let focused = order(state).iter().position(|p| *p == focus).unwrap_or(0);
    PaneLayout {
        root: walk(state.layout(), state),
        focused,
    }
}

fn ratio_to_milli(r: f32) -> u16 {
    let r = if r.is_finite() {
        r.clamp(0.05, 0.95)
    } else {
        0.5
    };
    // In [50, 950]: the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let m = (r * 1000.0).round() as u16;
    m
}

fn milli_to_ratio(m: u16) -> f32 {
    (f32::from(m) / 1000.0).clamp(0.05, 0.95)
}

/// An empty grid shaped like `layout`, the tab paths to open into each pane (`(pane, paths,
/// active)` in layout order) and the pane to focus.
pub type Restore = (
    pane_grid::State<PaneState>,
    Vec<(Pane, Vec<String>, Option<usize>)>,
    Pane,
);

/// Rebuilds the grid skeleton from a persisted layout.
pub fn restore(layout: &PaneLayout) -> Restore {
    fn config(n: &PaneNode) -> Configuration<PaneState> {
        match n {
            PaneNode::Leaf { .. } => Configuration::Pane(PaneState::default()),
            PaneNode::Split {
                dir,
                ratio_milli,
                a,
                b,
            } => Configuration::Split {
                axis: axis_of(*dir),
                ratio: milli_to_ratio(*ratio_milli),
                a: Box::new(config(a)),
                b: Box::new(config(b)),
            },
        }
    }
    let state = pane_grid::State::with_configuration(config(&layout.root));
    let panes = order(&state);
    let leaves = layout.root.leaves();
    let plan: Vec<_> = panes
        .iter()
        .zip(leaves)
        .map(|(p, (tabs, active))| (*p, tabs.to_vec(), active))
        .collect();
    let focus = panes.get(layout.focused).or_else(|| panes.first()).copied();
    match focus {
        Some(f) => (state, plan, f),
        None => {
            let (state, p) = pane_grid::State::new(PaneState::default());
            (state, Vec::new(), p)
        }
    }
}

/// The single-pane layout a WP-1 `editor.json` describes.
pub fn from_flat(tabs: &[String], active: Option<usize>) -> PaneLayout {
    PaneLayout {
        root: PaneNode::Leaf {
            tabs: tabs.to_vec(),
            active,
        },
        focused: 0,
    }
}

/// The pane and index holding `path`, if open anywhere.
pub fn find(state: &pane_grid::State<PaneState>, path: &str) -> Option<(Pane, usize)> {
    state
        .iter()
        .find_map(|(p, s)| s.tabs.iter().position(|t| t.path == path).map(|i| (*p, i)))
}

/// The pane and index of the tab with id `id` (plan editor-feedback-round-2 WP-7).
pub fn find_id(state: &pane_grid::State<PaneState>, id: u64) -> Option<(Pane, usize)> {
    state
        .iter()
        .find_map(|(p, s)| s.tabs.iter().position(|t| t.id == id).map(|i| (*p, i)))
}
