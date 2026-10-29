use super::*;
use iced::advanced::widget::Tree;
use iced::{Point, Size};

type TiState = text_input::State<()>;

fn cursor(start: usize, end: usize) -> text_input::Cursor {
    let mut s = TiState::new();
    if start == end {
        s.move_cursor_to(start);
    } else {
        s.select_range(start, end);
    }
    s.cursor()
}

#[derive(Default)]
struct Clip {
    primary: Option<String>,
    written: Vec<String>,
}

impl Clipboard for Clip {
    fn read(&self, kind: Kind) -> Option<String> {
        (kind == Kind::Primary)
            .then(|| self.primary.clone())
            .flatten()
    }
    fn write(&mut self, kind: Kind, contents: String) {
        if kind == Kind::Primary {
            self.written.push(contents);
        }
    }
}

type W = ClipInput<'static, String, iced::Theme, ()>;

/// Drives one event through the wrapper over a 200x30 layout; returns the published messages.
fn drive(w: &mut W, tree: &mut Tree, ev: Event, clip: &mut Clip) -> Vec<String> {
    let node = Widget::layout(
        w,
        tree,
        &(),
        &layout::Limits::new(Size::ZERO, Size::new(200.0, 30.0)),
    );
    let mut msgs = Vec::new();
    let mut shell = Shell::new(&mut msgs);
    Widget::update(
        w,
        tree,
        &ev,
        Layout::new(&node),
        mouse::Cursor::Available(Point::new(5.0, 5.0)),
        &(),
        clip,
        &mut shell,
        &Rectangle::new(Point::ORIGIN, Size::new(200.0, 30.0)),
    );
    msgs
}

fn middle() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Middle))
}

/// Assumption gate: the wrapped input's state is iced's public `text_input::State`, reachable as
/// tree child 0.
#[test]
fn child_state_is_the_text_input_state() {
    let w: W = clip_input("p", "abc");
    let mut tree = Tree::new(&w as &dyn Widget<String, iced::Theme, ()>);
    assert!(
        !tree.children[0]
            .state
            .downcast_ref::<TiState>()
            .is_focused()
    );
    child_state::<()>(&mut tree).select_all();
    assert_eq!(
        selected_text("abc", child_state::<()>(&mut tree).cursor()).as_deref(),
        Some("abc")
    );
}

#[test]
fn selected_text_maps_grapheme_indices() {
    assert_eq!(
        selected_text("héllo wörld", cursor(1, 4)).as_deref(),
        Some("éll")
    );
    assert_eq!(selected_text("a😀b", cursor(1, 2)).as_deref(), Some("😀"));
    assert_eq!(selected_text("abc", cursor(2, 2)), None);
}

#[test]
fn insert_at_cursor_places_or_replaces() {
    assert_eq!(
        insert_at_cursor("añb", cursor(2, 2), "XY"),
        ("añXYb".into(), 4)
    );
    assert_eq!(
        insert_at_cursor("abcd", cursor(3, 1), "é"),
        ("aéd".into(), 2)
    );
    assert_eq!(insert_at_cursor("", cursor(0, 0), "x"), ("x".into(), 1));
}

#[test]
fn middle_press_pastes_primary_through_on_input() {
    let mut w: W = clip_input("p", "").on_input(|s| s);
    let mut tree = Tree::new(&w as &dyn Widget<String, iced::Theme, ()>);
    let mut clip = Clip {
        primary: Some("pasted".into()),
        ..Clip::default()
    };
    let msgs = drive(&mut w, &mut tree, middle(), &mut clip);
    assert_eq!(msgs, vec!["pasted".to_owned()]);
    let st = child_state::<()>(&mut tree);
    assert!(st.is_focused());
    assert_eq!(
        st.cursor().state(&Value::new("pasted")),
        text_input::cursor::State::Index(6)
    );
}

#[test]
fn read_only_never_pastes_but_copies_selection_once() {
    let mut w: W = clip_input("p", "hello");
    let mut tree = Tree::new(&w as &dyn Widget<String, iced::Theme, ()>);
    let mut clip = Clip {
        primary: Some("pasted".into()),
        ..Clip::default()
    };
    assert!(drive(&mut w, &mut tree, middle(), &mut clip).is_empty());
    child_state::<()>(&mut tree).select_range(1, 3);
    let redraw = Event::Window(iced::window::Event::RedrawRequested(
        std::time::Instant::now(),
    ));
    drive(&mut w, &mut tree, redraw.clone(), &mut clip);
    drive(&mut w, &mut tree, redraw, &mut clip);
    assert_eq!(clip.written, vec!["el".to_owned()]);
}
