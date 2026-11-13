use super::*;

type W = ChromeText<'static, &'static str>;

fn drive(w: &mut W, tree: &mut Tree, ev: Event) -> Vec<&'static str> {
    let _ = Widget::<_, iced::Theme, ()>::layout(
        w,
        tree,
        &(),
        &layout::Limits::new(Size::ZERO, Size::new(200.0, 30.0)),
    );
    // The unit renderer shapes nothing, so give the widget a real box to hit.
    let node = layout::Node::new(Size::new(50.0, 20.0));
    let mut msgs = Vec::new();
    let mut shell = Shell::new(&mut msgs);
    struct Clip;
    impl Clipboard for Clip {
        fn read(&self, _: Kind) -> Option<String> {
            None
        }
        fn write(&mut self, _: Kind, _: String) {}
    }
    Widget::<_, iced::Theme, ()>::update(
        w,
        tree,
        &ev,
        Layout::new(&node),
        mouse::Cursor::Available(Point::new(1.0, 1.0)),
        &(),
        &mut Clip,
        &mut shell,
        &Rectangle::new(Point::ORIGIN, Size::new(200.0, 30.0)),
    );
    msgs
}

fn press() -> Event {
    Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
}

fn release() -> Event {
    Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
}

#[test]
fn a_plain_click_publishes_on_click_so_a_header_still_sorts() {
    let mut w: W = chrome_text("Name", 13.0, Color::BLACK, Color::BLACK).on_click("sort");
    let mut tree = Tree::new(&w as &dyn Widget<&str, iced::Theme, ()>);
    assert!(drive(&mut w, &mut tree, press()).is_empty());
    assert_eq!(drive(&mut w, &mut tree, release()), vec!["sort"]);
}

#[test]
fn a_drag_past_the_threshold_selects_and_does_not_click() {
    let mut w: W = chrome_text("Name", 13.0, Color::BLACK, Color::BLACK).on_click("sort");
    let mut tree = Tree::new(&w as &dyn Widget<&str, iced::Theme, ()>);
    drive(&mut w, &mut tree, press());
    let mv = Event::Mouse(mouse::Event::CursorMoved {
        position: Point::new(30.0, 1.0),
    });
    drive(&mut w, &mut tree, mv);
    assert!(drive(&mut w, &mut tree, release()).is_empty());
    assert!(is_drag(Point::ORIGIN, Point::new(5.0, 0.0)));
    assert!(!is_drag(Point::ORIGIN, Point::new(3.0, 0.0)));
}

#[test]
fn selected_is_the_ordered_char_safe_slice() {
    assert_eq!(selected("héllo", 4, 1), "él");
    assert_eq!(selected("héllo", 2, 2), "");
    assert_eq!(selected("a😀b", 1, 2), "");
}

/// plan round-24 WP-2: the click message of a sortable header is still `ViewsMsg::SortBy`.
#[test]
fn header_click_message_is_sort_by() {
    use crate::message::{Message, ViewsMsg};
    match crate::views::sort_msg("v#V", "Host.md", "file.name") {
        Message::Views(ViewsMsg::SortBy { id, host, column }) => assert_eq!(
            (id.as_str(), host.as_deref(), column.as_str()),
            ("v#V", Some("Host.md"), "file.name")
        ),
        _ => panic!("not SortBy"),
    }
}
