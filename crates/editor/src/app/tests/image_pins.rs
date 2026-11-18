//! Image-cache pins per view and per tab (split out of `shared_docs.rs`).

use std::collections::HashSet;

use super::shared_docs::{decoded_px, views};
use super::support::setup;
use super::*;

#[test]
fn image_pins_are_per_view() {
    use crate::image_cache::{Decoded, ImageCache, view_owner};
    let decoded = || Decoded {
        handle: iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
        width: 1,
        height: 1,
        bytes: 4,
    };
    let mut c = ImageCache::with_budget(1 << 20);
    let (v1, v2) = (view_owner("a.md", 1), view_owner("a.md", 2));
    assert!(c.show(&v1, "k"));
    assert!(!c.show(&v2, "k"), "the second view waits on the same load");
    assert!(c.loaded("k", Some(decoded())));
    // show / show / hide: the other view keeps the pin.
    c.hide(&v1, "k");
    assert!(c.contains("k"), "the other view still shows it");
    assert_eq!(c.pins("k"), 1);
    c.hide(&v2, "k");
    assert!(!c.contains("k"), "no view shows it: released");
    // A bare-path release (last tab closed) covers every view.
    assert!(c.show(&v1, "k"));
    assert!(c.loaded("k", Some(decoded())));
    c.show(&v2, "k");
    c.release_owner("a.md");
    assert!(c.is_empty());
    // ... and never another note whose path merely starts with it.
    assert!(c.show(&view_owner("a.md.md", 3), "j"));
    assert!(c.loaded("j", Some(decoded())));
    c.retain_owner("a.md", &HashSet::new());
    assert!(c.contains("j"));
}

/// Memory follow-ups WP-2: a tab no pane shows holds no pins and no unpinned entries; shown
/// again it re-pins through the sensor's `show` and the cache is what a fresh open gives.
#[test]
fn hidden_note_tab_releases_its_pins_and_a_reshow_repins() {
    use crate::image_cache::view_owner;
    let (_tmp, _d, mut app) = setup(&["w2/a.md", "w2/b.md"]);
    let pane = app.focus;
    let (a, b) = (views(&app, "w2/a.md")[0].1, views(&app, "w2/b.md")[0].1);
    let tab_of = |app: &mut App, id: u64| {
        let ps = app.panes.get_mut(pane).expect("pane");
        let i = ps.tabs.iter().position(|t| t.id == id).expect("tab");
        ps.activate(i);
        app.release_hidden_pins();
    };
    let pin = |app: &mut App, id: u64| {
        let o = view_owner("w2/a.md", id);
        assert!(app.reading.images.show(&o, "k"), "on_show starts the load");
        assert!(app.reading.images.loaded("k", Some(decoded_px())));
    };
    tab_of(&mut app, a);
    pin(&mut app, a);
    let fresh = app.reading.images.used_bytes();
    assert_eq!(app.reading.images.pins("k"), 1);
    tab_of(&mut app, b);
    assert!(!app.reading.images.contains("k"), "hidden: entry gone");
    assert_eq!(app.reading.images.used_bytes(), 0);
    // A pass over a tab already released does nothing more (a pin taken since stays).
    assert!(app.reading.images.show(&view_owner("w2/a.md", a), "k"));
    app.release_hidden_pins();
    assert_eq!(app.reading.images.pending_len(), 1, "not released twice");
    tab_of(&mut app, a);
    let _ = app.reading.images.loaded("k", Some(decoded_px()));
    assert_eq!(app.reading.images.pins("k"), 1);
    assert_eq!(app.reading.images.used_bytes(), fresh, "as a fresh open");
}
