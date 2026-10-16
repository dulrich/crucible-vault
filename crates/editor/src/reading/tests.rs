//! WP-3 reading-pane tests against `FakeDaemon` (synthetic vault in a temp dir, I1; images are
//! generated in code, never binary fixtures).

use std::collections::HashSet;
use std::sync::Arc;

use editor_model::render::{Block, Inline};
use iced::widget::image::Handle;
use iced::widget::text_editor::{Action, Edit};

use super::*;
use crate::app::App;
use crate::daemon::FakeDaemon;
use crate::image_cache::{Decoded, decode, decode_width};
use crate::message::{Message, ReadingMsg, SourceMsg, TabsMsg};

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .expect("png");
    out.into_inner()
}

fn setup(files: &[(&str, &[u8])]) -> (tempfile::TempDir, Arc<dyn DaemonClient>) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    for (p, body) in files {
        let abs = vault.join(p);
        std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
        std::fs::write(abs, body).expect("write");
    }
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    (tmp, Arc::new(d))
}

fn app_on(d: &Arc<dyn DaemonClient>, path: &str) -> App {
    let mut app = App::new(Some(Arc::clone(d)), None);
    let _ = app.update(Message::Tabs(TabsMsg::Open(path.to_owned())));
    assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some(path));
    app
}

fn reading(app: &mut App, path: &str, mode: ViewMode) {
    let _ = app.update(Message::Reading(ReadingMsg::SetMode {
        path: path.to_owned(),
        mode,
    }));
}

fn fake_decoded(bytes: usize) -> Decoded {
    Decoded {
        handle: Handle::from_rgba(1, 1, vec![0u8; 4]),
        width: 1,
        height: 1,
        bytes,
    }
}

#[test]
fn wikilink_click_resolves_through_index() {
    let (_tmp, d) = setup(&[
        ("A.md", b"see [[Target|t]] and [[Nope]]\n"),
        ("sub/Target.md", b"target\n"),
    ]);
    let mut store = ReadingStore::new();
    let m = store.click("A.md", "Target", Some(&d));
    assert!(matches!(m, Some(Message::Tabs(TabsMsg::Open(p))) if p == "sub/Target.md"));
    let m = store.click("A.md", "sub/Target#Heading", Some(&d));
    assert!(matches!(m, Some(Message::Tabs(TabsMsg::Open(p))) if p == "sub/Target.md"));
    assert!(store.click("A.md", "Nope", Some(&d)).is_none());
    assert!(
        store
            .click("A.md", "https://example.com/x", Some(&d))
            .is_none()
    );
    assert!(store.click("A.md", "Target", None).is_none());
}

#[test]
fn modes_cycle_and_transclusion_is_depth_one() {
    let (_tmp, d) = setup(&[
        ("A.md", b"# A\n\n![[B]]\n"),
        ("B.md", b"b text\n\n![[C]]\n"),
        ("C.md", b"c text\n"),
    ]);
    let mut app = app_on(&d, "A.md");
    // New tabs open in Reading and render without a Ctrl+E (audit E1, open path).
    assert_eq!(app.reading.mode("A.md"), ViewMode::Reading);
    let doc = app.reading.doc("A.md").expect("rendered");
    let Block::Transclusion {
        content: Some(c), ..
    } = &doc.model.blocks[1]
    else {
        panic!("transclusion filled: {:?}", doc.model.blocks)
    };
    assert_eq!(
        c[1],
        Block::Paragraph(vec![Inline::WikiLink {
            target: "C".into(),
            fragment: None,
            alias: None
        }])
    );
    drop(app.view());
    reading(&mut app, "A.md", ViewMode::Split);
    assert_eq!(app.reading.mode("A.md"), ViewMode::Split);
    drop(app.view());
    let _ = app.update(Message::Reading(ReadingMsg::CycleMode));
    assert_eq!(app.reading.mode("A.md"), ViewMode::Source);
    assert!(app.reading.doc("A.md").is_none());
    let _ = app.update(Message::Reading(ReadingMsg::CycleMode));
    assert_eq!(app.reading.mode("A.md"), ViewMode::Reading);
    assert!(app.reading.doc("A.md").is_some());
}

#[test]
fn recompute_only_when_edit_seq_moves() {
    let (_tmp, d) = setup(&[("A.md", b"hello\n")]);
    let mut app = app_on(&d, "A.md");
    reading(&mut app, "A.md", ViewMode::Split);
    assert_eq!(app.reading.recomputes, 1);
    for _ in 0..3 {
        let _ = app.update(Message::Reading(ReadingMsg::Tick));
    }
    assert_eq!(
        app.reading.recomputes, 1,
        "unchanged edit_seq: cached model"
    );
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('Z'),
    ))));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    assert_eq!(
        app.reading.recomputes, 1,
        "debounce: first tick only notes the seq"
    );
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    assert_eq!(app.reading.recomputes, 2);
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    assert_eq!(app.reading.recomputes, 2);
    let seq = app.active_doc().expect("tab").edit_seq();
    assert_eq!(app.reading.doc("A.md").expect("doc").seq, seq);
}

#[test]
fn lru_evicts_by_bytes() {
    let mut c = ImageCache::with_budget(1000);
    for k in ["a", "b", "c"] {
        assert!(c.show("t", k));
        assert!(!c.show("t", k), "in flight: no second load");
        assert!(c.loaded(k, Some(fake_decoded(400))));
    }
    // 1200 > 1000: the least recently used ("a") went.
    assert_eq!(c.len(), 2);
    assert!(!c.contains("a"));
    assert_eq!(c.used_bytes(), 800);
    // Touch "b", load "d": "c" is now the LRU.
    assert!(!c.show("t", "b"));
    assert!(c.show("t", "d"));
    c.loaded("d", Some(fake_decoded(400)));
    assert!(c.contains("b") && c.contains("d") && !c.contains("c"));
    assert!(c.used_bytes() <= c.budget());
    // A load nobody waits for any more is discarded.
    assert!(c.show("t", "e"));
    c.hide("t", "e");
    assert!(!c.loaded("e", Some(fake_decoded(10))));
    assert!(!c.contains("e"));
}

#[test]
fn hide_and_owner_release_drop_handles() {
    let mut c = ImageCache::with_budget(1 << 20);
    c.show("t1", "k");
    c.loaded("k", Some(fake_decoded(100)));
    c.show("t2", "k");
    assert_eq!(c.pins("k"), 2);
    c.hide("t1", "k");
    assert_eq!(c.pins("k"), 1, "still shown by t2");
    c.hide("t2", "k");
    assert!(
        !c.contains("k"),
        "on_hide of the last owner releases the handle"
    );
    assert_eq!(c.used_bytes(), 0);

    c.show("t1", "x");
    c.loaded("x", Some(fake_decoded(100)));
    c.show("t1", "y");
    c.loaded("y", Some(fake_decoded(100)));
    c.retain_owner("t1", &HashSet::from(["y".to_owned()]));
    assert!(!c.contains("x") && c.contains("y"));
    c.release_owner("t1");
    assert!(c.is_empty());
    assert_eq!(c.used_bytes(), 0);
}

#[test]
fn decode_downscales_to_display_width() {
    let d = decode(&png(200, 100), 64).expect("decode");
    assert_eq!((d.width, d.height, d.bytes), (64, 32, 64 * 32 * 4));
    let d = decode(&png(10, 10), 64).expect("decode");
    assert_eq!((d.width, d.height), (10, 10));
    assert!(decode(b"not an image", 64).is_err());
    assert_eq!(decode_width(100.0), 128);
    assert_eq!(decode_width(f32::INFINITY), crate::image_cache::MAX_WIDTH);
    assert_eq!(decode_width(1.0e9), crate::image_cache::MAX_WIDTH);
}

/// Shows `key` in `path` through the app and completes the load inline (the off-thread task is
/// replaced by a direct decode, as the test has no executor).
fn show_and_load(app: &mut App, d: &Arc<dyn DaemonClient>, path: &str, key: &str) {
    let _ = app.update(Message::Reading(ReadingMsg::ImageShown {
        tab: path.to_owned(),
        view: 1,
        key: key.to_owned(),
        width: 300.0,
    }));
    assert_eq!(app.reading.images.pending_len(), 1, "a load started");
    let (bytes, _) = d.read_file(key).expect("read");
    let result = decode(&bytes, 320).map_err(|e| e.to_string());
    let _ = app.update(Message::Reading(ReadingMsg::ImageLoaded {
        key: key.to_owned(),
        result,
    }));
    assert_eq!(app.reading.images.len(), 1);
    assert_eq!(app.reading.images.pins(key), 1);
}

#[test]
fn e2_release_points_tab_close_mode_switch_model_replacement() {
    let pic = png(40, 20);
    let (_tmp, d) = setup(&[
        ("A.md", b"![[pic.png]]\n\n![remote](https://host/r.png)\n"),
        ("att/pic.png", &pic),
    ]);
    let mut app = app_on(&d, "A.md");
    reading(&mut app, "A.md", ViewMode::Reading);
    let doc = app.reading.doc("A.md").expect("doc");
    assert_eq!(
        doc.images.get("pic.png"),
        Some(&Some("att/pic.png".to_owned()))
    );
    assert!(!doc.images.contains_key("https://host/r.png"));

    // Remote URLs never trigger a load.
    let _ = app.update(Message::Reading(ReadingMsg::ImageShown {
        tab: "A.md".into(),
        view: 1,
        key: "https://host/r.png".into(),
        width: 100.0,
    }));
    assert_eq!(app.reading.images.pending_len(), 0);

    // Mode switch away from reading releases.
    show_and_load(&mut app, &d, "A.md", "att/pic.png");
    reading(&mut app, "A.md", ViewMode::Source);
    assert!(app.reading.images.is_empty());

    // Render-model replacement that drops the image releases.
    reading(&mut app, "A.md", ViewMode::Split);
    show_and_load(&mut app, &d, "A.md", "att/pic.png");
    let _ = app.update(Message::Source(SourceMsg::Action(Action::SelectAll)));
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('z'),
    ))));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    assert!(app.reading.images.is_empty(), "image no longer referenced");

    // A replacement that keeps the image keeps its pin.
    let _ = app.update(Message::Source(SourceMsg::Action(Action::SelectAll)));
    for c in "![[pic.png]]".chars() {
        let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
            Edit::Insert(c),
        ))));
    }
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    show_and_load(&mut app, &d, "A.md", "att/pic.png");
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Edit(
        Edit::Insert('\n'),
    ))));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    assert_eq!(app.reading.images.pins("att/pic.png"), 1);

    // Tab close releases (the dirty buffer is saved first — WP-4 — inline here).
    app.rpc_inline = true;
    let _ = app.update(Message::Tabs(TabsMsg::Close(app.focus, 0)));
    assert!(app.reading.images.is_empty());
    assert_eq!(
        app.reading.mode("A.md"),
        ViewMode::default(),
        "closed tab: state forgotten"
    );
}

#[test]
fn e1_modes_persist_and_restored_reading_tabs_render_on_tick() {
    let (tmp, d) = setup(&[("A.md", b"# A\n"), ("B.md", b"# B\n")]);
    let state = tmp.path().join("state/editor.json");
    let mut app = App::new(Some(Arc::clone(&d)), Some(state.clone()));
    let _ = app.update(Message::Tabs(TabsMsg::Open("A.md".into())));
    let _ = app.update(Message::Tabs(TabsMsg::Open("B.md".into())));
    assert!(
        app.reading.doc("B.md").is_some(),
        "open path renders at once"
    );
    let shaped = |app: &App, p: &str| {
        app.doc(p)
            .is_some_and(crate::source::SourceBuffer::is_shaped)
    };
    assert!(
        !shaped(&app, "A.md") && !shaped(&app, "B.md"),
        "Reading: plain"
    );
    reading(&mut app, "A.md", ViewMode::Source);
    assert!(!shaped(&app, "A.md"), "hidden Source tab stays plain");
    let _ = app.update(Message::Tabs(TabsMsg::Select(app.focus, 0)));
    assert!(shaped(&app, "A.md") && !shaped(&app, "B.md"));

    let mut back = App::new(Some(Arc::clone(&d)), Some(state));
    assert_eq!(back.open_paths(), ["A.md", "B.md"]);
    assert_eq!(back.reading.mode("A.md"), ViewMode::Source);
    assert_eq!(back.reading.mode("B.md"), ViewMode::Reading);
    assert!(
        shaped(&back, "A.md"),
        "active Source tab shaped after restore"
    );
    assert!(!shaped(&back, "B.md"));
    assert!(
        back.reading.doc("B.md").is_none(),
        "restore defers the render"
    );
    let _ = back.update(Message::Reading(ReadingMsg::Tick));
    assert!(back.reading.doc("B.md").is_some(), "first tick renders it");
    assert!(back.reading.doc("A.md").is_none());
}

/// plan editor-ui-round-3 WP-5 (audit E1): the active find match maps to (block, nth occurrence).
#[test]
fn active_find_match_maps_to_its_block_and_occurrence() {
    use crate::message::FindMsg;
    let p = "wp5/f.md";
    let (_t, d) = setup(&[(p, b"alpha foo\n\nbeta foo foo\n")]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Reading);
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    assert!(app.reading.doc(p).is_some(), "rendered");
    let _ = app.update(Message::Find(FindMsg::Open));
    let _ = app.update(Message::Find(FindMsg::Query("foo".into())));
    assert_eq!(app.reading.find_active(p), Some((0, 0)));
    let _ = app.update(Message::Find(FindMsg::Next));
    assert_eq!(
        app.reading.find_active(p),
        Some((1, 0)),
        "second block, first occurrence"
    );
    let _ = app.update(Message::Find(FindMsg::Next));
    assert_eq!(app.reading.find_active(p), Some((1, 1)));
    // Painting: exactly one occurrence of the second block is the active one.
    drop(app.view());
    let _ = app.update(Message::Find(FindMsg::Close));
    assert_eq!(app.reading.find_active(p), None);
}
