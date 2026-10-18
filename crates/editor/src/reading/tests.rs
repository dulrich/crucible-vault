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

/// Plan round-5 WP-1: the source editor of `path` measured `lines` visible lines.
fn viewport(app: &mut App, path: &str, lines: usize) {
    let _ = app.update(Message::Source(crate::message::SourceMsg::Viewport {
        path: path.to_owned(),
        lines,
    }));
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
    // Plan round-5 WP-2: eviction never drops a shown (pinned) image, and every cached entry
    // is pinned (unpinned entries are released at once), so over budget the shown set stays.
    let mut c = ImageCache::with_budget(1000);
    for k in ["a", "b", "c"] {
        assert!(c.show("t", k));
        assert!(!c.show("t", k), "in flight: no second load");
        assert!(c.loaded(k, Some(fake_decoded(400))));
    }
    assert_eq!(c.len(), 3, "all shown: none evicted");
    assert_eq!(c.used_bytes(), 1200);
    // Hiding releases at once, bringing the cache back within budget.
    c.hide("t", "a");
    assert!(!c.contains("a"));
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

/// plan editor-ui-round-3 WP-6: the measured probe picks the block at the viewport top and the
/// offset that puts a block there (layout coordinates, never proportional).
#[test]
fn probe_maps_scroll_offset_to_blocks() {
    let p = Probe {
        offset_y: 100.0,
        content_y: 40.0,
        block_ys: vec![Some(48.0), Some(90.0), Some(139.5), Some(240.0), None],
    };
    assert_eq!(
        p.top_block(),
        Some(2),
        "139.5 - 40 = 99.5 <= 100 (+1 slack)"
    );
    assert_eq!(p.block_offset(3), Some(200.0));
    assert_eq!(p.block_offset(0), Some(8.0));
    assert_eq!(p.block_offset(4), None, "not laid out");
    let at_top = Probe {
        offset_y: 0.0,
        ..p.clone()
    };
    assert_eq!(at_top.top_block(), Some(0));
}

const PLACE_NOTE: &[u8] =
    b"---\na: 1\n---\n# H\n\npara\n\n- x\n- y\n\n```\ncode\nmore\n```\n\nlast\n";

/// plan editor-ui-round-3 WP-6: Source -> Reading carries the Source top line to its block.
#[test]
fn source_to_reading_carries_top_line_to_its_block() {
    let p = "wp6/s.md";
    let (_t, d) = setup(&[(p, PLACE_NOTE)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Source);
    // Plan round-5 WP-1: a 3-line viewport, so line 11 is a reachable top.
    viewport(&mut app, p, 3);
    // Wheel down 11 lines: line 11 ("code") is at the top.
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Scroll {
        lines: 11,
    })));
    assert_eq!(app.active_doc().expect("doc").top_line(), 11);
    reading(&mut app, p, ViewMode::Reading);
    let model = &app.reading.doc(p).expect("doc").model;
    assert_eq!(model.block_for_line(11), Some(3), "the code block");
    assert_eq!(
        app.reading.top_block(p, app.view_of(p)),
        Some(3),
        "the scroll task targets it"
    );
}

/// plan editor-ui-round-3 WP-6: Reading -> Source scrolls Source to the top block's first line.
#[test]
fn reading_to_source_scrolls_to_the_top_blocks_first_line() {
    let p = "wp6/r.md";
    let (_t, d) = setup(&[(p, PLACE_NOTE)]);
    let mut app = app_on(&d, p);
    assert_eq!(app.reading.mode(p), ViewMode::Reading);
    let blocks = app.reading.doc(p).expect("doc").model.blocks.len();
    // A probe lands with the list (block 2, source lines 7..9) at the viewport top.
    let mut block_ys: Vec<Option<f32>> = (0..blocks).map(|i| Some(i as f32 * 50.0)).collect();
    block_ys[0] = Some(30.0);
    let view = app.view_of(p);
    let _ = app.update(Message::Reading(ReadingMsg::Probed {
        path: p.to_owned(),
        view,
        probe: Some(Probe {
            offset_y: 100.0,
            content_y: 0.0,
            block_ys,
        }),
        scroll_to: None,
    }));
    assert_eq!(app.reading.top_block(p, app.view_of(p)), Some(2));
    viewport(&mut app, p, 3);
    reading(&mut app, p, ViewMode::Source);
    assert_eq!(app.active_doc().expect("doc").top_line(), 7);
    // And back: Source line 7 maps to the list again.
    reading(&mut app, p, ViewMode::Split);
    assert_eq!(app.reading.top_block(p, app.view_of(p)), Some(2));
}

/// The live daemon indexes incrementally without bumping the generation, so a generation-keyed
/// resolver never saw files created after the first render (localized attachments rendered as
/// broken images until restart). Created/Deleted/Renamed drop the cache; Modified keeps it.
#[test]
fn file_membership_change_drops_cached_resolver() {
    use cv_protocol::{ChangeKind, Notification, Origin};

    use crate::message::StatusMsg;

    let (_tmp, d) = setup(&[("A.md", b"![](att/late.png)\n")]);
    let mut app = app_on(&d, "A.md");
    reading(&mut app, "A.md", ViewMode::Reading);
    let changed = |app: &mut App, path: &str, kind: ChangeKind| {
        let _ = app.update(Message::Status(StatusMsg::Notification(
            Notification::FileChanged {
                path: path.to_owned(),
                kind,
                hash: None,
                origin: Origin::Daemon,
            },
        )));
    };
    for kind in [
        ChangeKind::Created,
        ChangeKind::Deleted,
        ChangeKind::Renamed,
    ] {
        app.reading.refresh_resolver(d.as_ref());
        assert!(app.reading.resolver.is_some());
        changed(&mut app, "att/late.png", kind);
        assert!(
            app.reading.resolver.is_none(),
            "{kind:?} drops the resolver"
        );
    }
    app.reading.refresh_resolver(d.as_ref());
    changed(&mut app, "B.md", ChangeKind::Modified);
    assert!(
        app.reading.resolver.is_some(),
        "Modified keeps the resolver"
    );
}

/// plan editor-round-4 WP-2: 100 one-line paragraphs (blocks), 200 source lines.
fn long_note() -> Vec<u8> {
    (0..100)
        .map(|i| format!("para {i}\n\n"))
        .collect::<String>()
        .into_bytes()
}

/// plan editor-round-4 WP-2: Ctrl+PgDn in Source tracks the last viewport. Plan round-6 WP-1:
/// the Source -> Reading half (it carried the window's first block, para 81) is now an edge
/// snap — see `source_go_to_end_then_reading_snaps_to_the_end`.
#[test]
fn go_to_bottom_in_source_tracks_the_last_viewport() {
    let p = "r4/long.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Source);
    // Plan round-5 WP-1: the measured viewport (40 lines), not the 30-line estimate.
    viewport(&mut app, p, 40);
    let _ = app.update(Message::Reading(ReadingMsg::GoToEdge { bottom: true }));
    let top = app.active_doc().expect("doc").top_line();
    assert_eq!(top, 200 - 39, "the last 40 lines are the visible window");
}

/// plan editor-round-4 WP-2: Reading at its last block, then Reading -> Source lands on the
/// last lines.
#[test]
fn reading_at_bottom_then_source_lands_on_the_last_lines() {
    let p = "r4/long2.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    let blocks = app.reading.doc(p).expect("doc").model.blocks.len();
    let block_ys: Vec<Option<f32>> = (0..blocks).map(|i| Some(i as f32 * 20.0)).collect();
    let view = app.view_of(p);
    let _ = app.update(Message::Reading(ReadingMsg::Probed {
        path: p.to_owned(),
        view,
        probe: Some(Probe {
            offset_y: (blocks - 1) as f32 * 20.0,
            content_y: 0.0,
            block_ys,
        }),
        scroll_to: None,
    }));
    assert_eq!(app.reading.top_block(p, app.view_of(p)), Some(blocks - 1));
    // Plan round-5 WP-1: the last block starts at line 198, but the editor cannot scroll past
    // its end — the carry clamps to the last full viewport (cause B: it used to set 198 and
    // show two lines and blank space).
    viewport(&mut app, p, 40);
    reading(&mut app, p, ViewMode::Source);
    let doc = app.active_doc().expect("doc");
    let lines = doc.text().matches('\n').count() + 1;
    assert_eq!(doc.top_line(), lines - 40);
    assert_eq!(
        doc.top_line() + doc.visible_lines(),
        lines,
        "the window ends at the last line"
    );
}

/// Plan round-5 WP-1 (item 1, cause b): two panes showing one note page / probe their own
/// reading scrollable — the id was per path, so a PageDown scrolled both (and a probe read the
/// last match).
#[test]
fn reading_scroll_ids_are_per_tab_not_per_path() {
    assert_ne!(
        crate::find::reading_scroll_id("a.md", 1),
        crate::find::reading_scroll_id("a.md", 2)
    );
    assert_ne!(
        crate::reading::block_id("a.md", 1, 0),
        crate::reading::block_id("a.md", 2, 0)
    );
}

/// Plan round-5 WP-1 (item 4): a Reading tab blurred and re-selected keeps its place — the
/// re-created scrollable's first `on_scroll` (offset 0) re-applies it instead of overwriting.
#[test]
fn tab_resume_keeps_the_reading_place() {
    let (a, b) = ("r5/a.md", "r5/b.md");
    let body = long_note();
    let (_t, d) = setup(&[(a, &body), (b, b"other\n")]);
    let mut app = app_on(&d, a);
    let view = app.view_of(a);
    let blocks = app.reading.doc(a).expect("doc").model.blocks.len();
    let block_ys: Vec<Option<f32>> = (0..blocks).map(|i| Some(i as f32 * 20.0)).collect();
    let probe = |offset_y: f32| Probe {
        offset_y,
        content_y: 0.0,
        block_ys: block_ys.clone(),
    };
    let _ = app.update(Message::Reading(ReadingMsg::Probed {
        path: a.to_owned(),
        view,
        probe: Some(probe(40.0 * 20.0)),
        scroll_to: None,
    }));
    assert_eq!(app.reading.top_block(a, view), Some(40));
    let _ = app.update(Message::Tabs(TabsMsg::Open(b.to_owned())));
    assert!(!app.reading.is_restoring(view));
    let pane = app.focus;
    let task = app.update(Message::Tabs(TabsMsg::Select(pane, 0)));
    assert_eq!(app.view_of(a), view);
    assert!(task.units() > 0, "activation issues the restore probe");
    assert!(app.reading.is_restoring(view));
    // The re-created scrollable reports offset 0 first: the place is re-applied, not lost.
    let _ = app.update(Message::Reading(ReadingMsg::Scrolled {
        path: a.to_owned(),
        view,
        y: 0.0,
    }));
    assert!(!app.reading.is_restoring(view));
    assert_eq!(app.reading.top_block(a, view), Some(40));
    // Afterwards scroll reports update the place normally.
    let _ = app.update(Message::Reading(ReadingMsg::Probed {
        path: a.to_owned(),
        view,
        probe: Some(probe(10.0 * 20.0)),
        scroll_to: None,
    }));
    assert_eq!(app.reading.top_block(a, view), Some(10));
}

/// Plan round-5 WP-1 (item 2): Up/Down with no palette open scroll the focused Reading pane
/// one line; nothing in Source-only, and the open palette keeps its own arrows.
#[test]
fn arrows_step_the_reading_pane_one_line() {
    use crate::message::PaletteMsg;
    use iced::keyboard::{Event, Key, Location, Modifiers, key::Named};
    let press = Event::KeyPressed {
        key: Key::Named(Named::ArrowDown),
        modified_key: Key::Named(Named::ArrowDown),
        physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::ArrowDown),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    };
    assert!(matches!(
        crate::palette::key_event(press),
        Some(Message::Palette(PaletteMsg::Down))
    ));
    assert!((crate::page::line_step(16.0, true) - 20.8).abs() < 1e-3);
    assert!(crate::page::line_step(16.0, false) < 0.0);
    let p = "r5/arrows.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    assert!(app.update(Message::Palette(PaletteMsg::Down)).units() > 0);
    reading(&mut app, p, ViewMode::Source);
    assert_eq!(app.update(Message::Palette(PaletteMsg::Down)).units(), 0);
}

/// plan editor-round-4 WP-2: plain PageDown (uncaptured) routes to the focused pane's
/// scrollable — Reading's in Reading/Split, none in Source; Ctrl+PageDown stays a chord.
#[test]
fn page_down_routes_to_the_reading_scrollable() {
    use iced::keyboard::{Event, Key, Location, Modifiers, key::Named};
    let press = |m: Modifiers| Event::KeyPressed {
        key: Key::Named(Named::PageDown),
        modified_key: Key::Named(Named::PageDown),
        physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::PageDown),
        location: Location::Standard,
        modifiers: m,
        text: None,
        repeat: false,
    };
    assert!(matches!(
        crate::palette::key_event(press(Modifiers::empty())),
        Some(Message::Panes(crate::message::PanesMsg::Page(true)))
    ));
    assert!(matches!(
        crate::palette::key_event(press(Modifiers::CTRL)),
        Some(Message::Palette(crate::message::PaletteMsg::Key(_)))
    ));
    let p = "r4/page.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Reading);
    let (id, note) = app.page_target().expect("reading target");
    assert_eq!(id, crate::find::reading_scroll_id(p, app.view_of(p)));
    assert_eq!(note.as_deref(), Some(p));
    reading(&mut app, p, ViewMode::Split);
    assert!(app.page_target().is_some());
    reading(&mut app, p, ViewMode::Source);
    assert!(app.page_target().is_none());
}

/// plan editor-round-4 WP-2: PageDown pages by a viewport minus one line.
#[test]
fn page_delta_keeps_one_line_of_overlap() {
    let line = crate::page::PAGE_OVERLAP;
    assert!((crate::page::page_delta(500.0, true) - (500.0 - line)).abs() < f32::EPSILON);
    assert!((crate::page::page_delta(500.0, false) + (500.0 - line)).abs() < f32::EPSILON);
    assert!(crate::page::page_delta(10.0, true) > 0.0);
}

// ---- plan editor-round-4 WP-3: Reading find over visible text ----

/// Opens `p` (body `body`) in `mode`, rendered, with the find bar searching `q`.
fn find_on(p: &str, body: &[u8], mode: ViewMode, q: &str) -> (tempfile::TempDir, App) {
    use crate::message::FindMsg;
    let (t, d) = setup(&[(p, body)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, mode);
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    assert!(app.reading.doc(p).is_some(), "rendered");
    let _ = app.update(Message::Find(FindMsg::Open));
    let _ = app.update(Message::Find(FindMsg::Query(q.into())));
    (t, app)
}

fn bar(app: &App) -> crate::find::FindBar {
    app.panes
        .get(app.focus)
        .and_then(|ps| ps.find.clone())
        .expect("find bar")
}

#[test]
fn code_span_and_link_labels_are_visible_text() {
    let m =
        render::lower("a `foo` b [[t|foo]] [foo](https://foo.example) #foo\n\n```foo\nfoo\n```\n");
    let segs = visible_segments(&m.blocks[0]);
    assert!(segs.iter().any(|s| s == "foo"), "code span: {segs:?}");
    assert!(segs.iter().any(|s| s == "#foo"), "tag: {segs:?}");
    assert!(
        !segs.iter().any(|s| s.contains("example")),
        "no URL: {segs:?}"
    );
    // code span, alias, link label, tag; code block: text only (not the `foo` info string).
    assert_eq!(
        visible_occurrences(&m, "foo"),
        vec![(0, 0), (0, 1), (0, 2), (0, 3), (1, 0)]
    );
}

#[test]
fn reading_code_span_match_is_counted_and_painted() {
    let p = "r4/code.md";
    let (_t, app) = find_on(p, b"run `cargo foo` now\n", ViewMode::Reading, "foo");
    assert_eq!(bar(&app).count(), "1/1");
    assert_eq!(app.reading.find_active(p), Some((0, 0)));
    drop(app.view());
}

#[test]
fn reading_url_only_query_counts_zero() {
    let p = "r4/url.md";
    let (_t, app) = find_on(
        p,
        b"see [label](https://zzz.example/x) and [[target|alias]]\n",
        ViewMode::Reading,
        "zzz",
    );
    assert_eq!(bar(&app).count(), "0/0");
    assert_eq!(app.reading.find_active(p), None);
    let (_t2, app2) = find_on(p, b"see [[target|alias]]\n", ViewMode::Reading, "target");
    assert_eq!(
        bar(&app2).count(),
        "0/0",
        "an aliased wiki-link target is hidden"
    );
}

#[test]
fn reading_next_steps_over_visible_occurrences() {
    use crate::message::FindMsg;
    let p = "r4/next.md";
    let (_t, mut app) = find_on(
        p,
        b"[x](https://foo.example) foo\n\nmid\n\nfoo `foo`\n",
        ViewMode::Reading,
        "foo",
    );
    assert_eq!(bar(&app).count(), "1/3");
    assert_eq!(app.reading.find_active(p), Some((0, 0)));
    let _ = app.update(Message::Find(FindMsg::Next));
    assert_eq!(app.reading.find_active(p), Some((2, 0)));
    assert_eq!(
        app.reading.top_block(p, app.view_of(p)),
        Some(2),
        "scrolled to its block"
    );
    let _ = app.update(Message::Find(FindMsg::Next));
    assert_eq!(app.reading.find_active(p), Some((2, 1)));
    let _ = app.update(Message::Find(FindMsg::Next));
    assert_eq!(app.reading.find_active(p), Some((0, 0)), "wraps");
    drop(app.view());
}

#[test]
fn split_active_match_inside_url_has_no_strong_occurrence() {
    use crate::message::FindMsg;
    let p = "r4/split.md";
    let (_t, mut app) = find_on(p, b"[x](https://foo.example) foo\n", ViewMode::Split, "foo");
    // Source drives the bar: the cursor sits at 0, so the URL match is current.
    assert_eq!(bar(&app).count(), "1/2");
    assert_eq!(
        app.reading.find_active(p),
        None,
        "invisible: nothing strong"
    );
    let _ = app.update(Message::Find(FindMsg::Next));
    assert_eq!(
        app.reading.find_active(p),
        Some((0, 0)),
        "the visible match after an invisible one is the block's first occurrence"
    );
    drop(app.view());
}

#[test]
fn source_counterpart_skips_invisible_matches() {
    let text = "intro\n\n[[foo|x]] ![[foo.png]] [a](foo) foo `foo`\n";
    let m = render::lower(text);
    let ms = editor_model::find::matches(text, "foo");
    assert_eq!(ms.len(), 5);
    let got: Vec<_> = ms
        .iter()
        .map(|r| source_counterpart(&m, text, "foo", r))
        .collect();
    assert_eq!(got, vec![None, None, None, Some((1, 0)), Some((1, 1))]);
}

/// Plan round-5 WP-2: the placeholder (header-read size) is exactly as tall as the decoded image
/// displayed at the same width.
#[test]
fn placeholder_height_matches_the_decoded_scaled_height() {
    let tmp = tempfile::tempdir().expect("tmp");
    let path = tmp.path().join("pic.png");
    let bytes = png(300, 150);
    std::fs::write(&path, &bytes).expect("write");
    let (w, h) = crate::image_cache::header_dims(&path).expect("header");
    assert_eq!((w, h), (300, 150));
    let dims = crate::image_cache::Dims::new(w, h).expect("dims");
    for display in [64u32, 200, 300] {
        let d = decode(&bytes, display).expect("decode");
        assert_eq!(dims.scaled_height(d.width as f32), d.height as f32);
    }
    // The cache records it once the key is known.
    let mut c = ImageCache::with_budget(1 << 20);
    c.note_dims("pic.png", tmp.path());
    assert_eq!(
        c.dims("pic.png").map(|d| (d.width, d.height)),
        Some((300, 150))
    );
    c.note_dims("missing.png", tmp.path());
    assert!(
        c.dims("missing.png").is_none(),
        "unknown dims: 120 px fallback"
    );
}

/// Plan round-5 WP-2: iced's sensor visibility test (`sensor.rs:207-213`: nearest-corner distance
/// against `anticipate`) — a tall image covering the viewport is hidden with margin 0 and stays
/// shown with ours; and eviction never drops a pinned image.
#[test]
fn tall_image_stays_pinned_while_it_overlaps_the_viewport() {
    use iced::{Point, Rectangle, Size, Vector};
    let visible = |viewport: Rectangle, bounds: Rectangle, anticipate: f32| {
        let tl = viewport.distance(bounds.position());
        let br = viewport.distance(bounds.position() + Vector::from(bounds.size()));
        tl.min(br) <= anticipate
    };
    let viewport = Rectangle::new(Point::ORIGIN, Size::new(800.0, 600.0));
    let (w, h) = (700.0, 3000.0);
    let margin = crate::image_cache::sensor_margin(w, h);
    for y in [-2399.0, -1200.0, -10.0, 590.0] {
        let bounds = Rectangle::new(Point::new(20.0, y), Size::new(w, h));
        assert!(visible(viewport, bounds, margin), "y={y}: overlaps");
    }
    let covering = Rectangle::new(Point::new(20.0, -1200.0), Size::new(w, h));
    assert!(
        !visible(viewport, covering, 0.0),
        "the iced default reports a covering image hidden (the jump's cause)"
    );

    let mut c = ImageCache::with_budget(100);
    for k in ["a", "b"] {
        assert!(c.show("t", k));
        assert!(c.loaded(k, Some(fake_decoded(80))));
    }
    assert!(
        c.contains("a") && c.contains("b"),
        "shown images are never evicted"
    );
    c.hide("t", "a");
    assert!(!c.contains("a"));
}

/// Plan round-5 WP-2: a Doc rendered before its attachment is resolvable re-renders with the
/// image once the attachment appears (index generation unchanged on the editor side).
#[test]
fn late_attachment_re_renders_the_doc() {
    use cv_protocol::{ChangeKind, Notification, Origin};

    use crate::message::StatusMsg;

    let pic = png(8, 4);
    let h = crate::daemon::ContentHash::of(&pic);
    let key = format!("_attachments/A/{}_FAKE.png", &h.as_str()[..16]);
    let body = format!("![]({})\n", key.replace('_', "%5F"));
    let (_tmp, d) = setup(&[("A.md", body.as_bytes())]);
    let mut app = app_on(&d, "A.md");
    reading(&mut app, "A.md", ViewMode::Reading);
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    let resolved = |app: &App| {
        app.reading
            .doc("A.md")
            .and_then(|doc| doc.images.values().next().cloned().flatten())
    };
    assert_eq!(resolved(&app), None, "not resolvable yet");
    let written = d.write_attachment("A.md", "png", &pic).expect("attach");
    assert_eq!(written, key);
    // The live daemon indexes incrementally (same generation); the fake rebuilds into a new
    // one. Point the editor's reader at it and re-stamp the Doc with that generation, so the
    // only change left is the vault membership one (the race: before the fix nothing marked
    // the Doc stale and it kept the broken image).
    d.index().on_generation(u64::MAX).expect("reader");
    let doc = app
        .reading
        .tabs
        .get_mut("A.md")
        .and_then(|t| t.doc.as_mut())
        .expect("doc");
    doc.generation = d.index().generation();
    let _ = app.update(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            path: key.clone(),
            kind: ChangeKind::Created,
            hash: None,
            origin: Origin::Daemon,
        },
    )));
    let _ = app.update(Message::Reading(ReadingMsg::Tick));
    assert_eq!(
        resolved(&app),
        Some(key.clone()),
        "re-rendered with the image"
    );
    assert_eq!(
        app.reading.images.dims(&key).map(|d| (d.width, d.height)),
        Some((8, 4))
    );
}

#[test]
fn percent_decode_handles_every_escape() {
    assert_eq!(percent_decode("a%20b%5Fc"), "a b_c");
    assert_eq!(percent_decode("caf%C3%A9.png"), "café.png");
    assert_eq!(percent_decode("100%zz%2"), "100%zz%2");
    assert_eq!(percent_decode("bad%FF"), "bad%FF");
}

/// Plan round-5 WP-3: an image that fails to load raises a Warning notice (basename + reason).
#[test]
fn image_load_failure_raises_a_warning_notice() {
    let (_tmp, d) = setup(&[("n.md", b"![[att/pic.png]]\n")]);
    let mut app = app_on(&d, "n.md");
    let _ = app.update(Message::Reading(ReadingMsg::ImageLoaded {
        key: "att/pic.png".into(),
        result: Err("unsupported format\nmore detail".into()),
    }));
    let n = app.status.notice.clone().expect("notice");
    assert_eq!(n.severity, crate::app::Severity::Warning);
    assert_eq!(n.text, "image failed to load: pic.png (unsupported format)");
}

/// Plan round-5 WP-3: clicking an unresolved link raises a notice naming the target; a URL does not.
#[test]
fn unresolved_link_click_raises_a_notice() {
    let (_tmp, d) = setup(&[("A.md", b"see [[Nope]]\n")]);
    let mut app = app_on(&d, "A.md");
    let _ = app.update(Message::Reading(ReadingMsg::OpenLink {
        src: "A.md".into(),
        target: "https://example.com/x".into(),
    }));
    assert!(app.status.notice.is_none());
    let _ = app.update(Message::Reading(ReadingMsg::OpenLink {
        src: "A.md".into(),
        target: "Nope".into(),
    }));
    let n = app.status.notice.clone().expect("notice");
    assert_eq!(n.severity, crate::app::Severity::Warning);
    assert_eq!(n.text, "unresolved link: Nope");
}

/// Plan round-6 WP-1: Reading Ctrl+PgDn, then Reading -> Source shows the last lines (an edge
/// snap, not a carried line estimate).
#[test]
fn reading_go_to_end_then_source_shows_the_end() {
    let p = "r6/end.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    let view = app.view_of(p);
    let _ = app.update(Message::Reading(ReadingMsg::GoToEdge { bottom: true }));
    assert_eq!(app.reading.edge(p, view), Some(true));
    // The snap's own scroll report at the end keeps the edge.
    let _ = app.update(Message::Reading(ReadingMsg::Scrolled {
        path: p.to_owned(),
        view,
        y: 1.0,
    }));
    assert_eq!(app.reading.edge(p, view), Some(true));
    viewport(&mut app, p, 40);
    reading(&mut app, p, ViewMode::Source);
    let doc = app.active_doc().expect("doc");
    let lines = doc.text().matches('\n').count() + 1;
    assert_eq!(doc.top_line(), lines - 40, "the last viewport");
}

/// Plan round-6 WP-1: Source Ctrl+PgDn, then Source -> Reading snaps Reading to its end
/// (the edge state drives the switch; no line-estimate block is carried); a user scroll away
/// from the edge clears it.
#[test]
fn source_go_to_end_then_reading_snaps_to_the_end() {
    let p = "r6/send.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Source);
    viewport(&mut app, p, 40);
    let view = app.view_of(p);
    let _ = app.update(Message::Reading(ReadingMsg::GoToEdge { bottom: true }));
    assert_eq!(app.reading.edge(p, view), Some(true));
    app.reading.clear_top_block(p, view);
    let task = app.update(Message::Reading(ReadingMsg::SetMode {
        path: p.to_owned(),
        mode: ViewMode::Reading,
    }));
    assert!(task.units() > 0, "the snap_to(1.0) task");
    assert_eq!(
        app.reading.top_block(p, view),
        None,
        "no carried line-estimate block"
    );
    assert_eq!(app.reading.edge(p, view), Some(true));
    let _ = app.update(Message::Reading(ReadingMsg::Scrolled {
        path: p.to_owned(),
        view,
        y: 0.5,
    }));
    assert_eq!(
        app.reading.edge(p, view),
        None,
        "a user scroll leaves the edge"
    );
}

/// Plan round-6 WP-1: a source wheel scroll clears the edge, so the next switch carries the
/// line again.
#[test]
fn source_wheel_clears_the_edge() {
    let p = "r6/wheel.md";
    let body = long_note();
    let (_t, d) = setup(&[(p, &body)]);
    let mut app = app_on(&d, p);
    reading(&mut app, p, ViewMode::Source);
    let view = app.view_of(p);
    let _ = app.update(Message::Reading(ReadingMsg::GoToEdge { bottom: false }));
    assert_eq!(app.reading.edge(p, view), Some(false));
    let _ = app.update(Message::Source(SourceMsg::Action(Action::Scroll {
        lines: 3,
    })));
    assert_eq!(app.reading.edge(p, view), None);
}
