use super::*;

// ---- plan editor-round-4 WP-3: Reading find over visible text ----

#[test]
fn code_span_and_link_labels_are_visible_text() {
    let m =
        render::lower("a `foo` b [[t|foo]] [foo](https://foo.example) #foo\n\n```foo\nfoo\n```\n");
    let runs = editor_model::visible::visible_runs(&m);
    // Block 0's find segments (the one walk's segments).
    let segs: Vec<&str> = runs
        .iter()
        .filter(|r| r.part.block() == Some(0))
        .flat_map(|r| r.segments.iter().map(|s| &r.text[s.clone()]))
        .collect();
    assert!(segs.contains(&"foo"), "code span: {segs:?}");
    assert!(segs.contains(&"#foo"), "tag: {segs:?}");
    assert!(
        !segs.iter().any(|s| s.contains("example")),
        "no URL: {segs:?}"
    );
    // code span, alias, link label, tag; code block: text only (not the `foo` info string).
    let occ = editor_model::visible::occurrences(&runs, "foo");
    let keyed: Vec<(usize, usize)> = editor_model::visible::keyed(&occ)
        .into_iter()
        .filter_map(|(p, n)| Some((p.block()?, n)))
        .collect();
    assert_eq!(keyed, vec![(0, 0), (0, 1), (0, 2), (0, 3), (1, 0)]);
}

#[test]
fn reading_code_span_match_is_counted_and_painted() {
    let p = "r4/code.md";
    let (_t, app) = find_on(p, b"run `cargo foo` now\n", ViewMode::Reading, "foo");
    assert_eq!(bar(&app).count(), "1/1");
    assert_eq!(strong_hit(&app, p), Some((0, 0)));
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
    assert_eq!(strong_hit(&app, p), None);
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
    assert_eq!(strong_hit(&app, p), Some((0, 0)));
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(strong_hit(&app, p), Some((2, 0)));
    assert_eq!(
        app.reading.top_block(&app.docs, p, app.view_of(p)),
        Some(2),
        "scrolled to its block"
    );
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(strong_hit(&app, p), Some((2, 1)));
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(strong_hit(&app, p), Some((0, 0)), "wraps");
    drop(app.view());
}

#[test]
fn split_active_match_inside_url_has_no_strong_occurrence() {
    use crate::message::FindMsg;
    let p = "r4/split.md";
    let (_t, mut app) = find_on(p, b"[x](https://foo.example) foo\n", ViewMode::Split, "foo");
    // Source drives the bar: the cursor sits at 0, so the URL match is current.
    assert_eq!(bar(&app).count(), "1/2");
    assert_eq!(strong_hit(&app, p), None, "invisible: nothing strong");
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(
        strong_hit(&app, p),
        Some((0, 0)),
        "the visible match after an invisible one is the block's first occurrence"
    );
    drop(app.view());
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
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
    let resolved = |app: &App| {
        app.reading
            .doc(&app.docs, "A.md")
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
    let doc = app.reading.doc_mut(&mut app.docs, "A.md").expect("doc");
    doc.generation = d.index().generation();
    let _ = app.send(Message::Status(StatusMsg::Notification(
        Notification::FileChanged {
            id: None,
            path: key.clone(),
            kind: ChangeKind::Created,
            hash: None,
            origin: Origin::Daemon,
            from: None,
            replaced_by: None,
        },
    )));
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
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

/// Plan round-5 WP-3: an image that fails to load raises a Warning notice (basename + reason).
#[test]
fn image_load_failure_raises_a_warning_notice() {
    let (_tmp, d) = setup(&[("n.md", b"![[att/pic.png]]\n")]);
    let mut app = app_on(&d, "n.md");
    let _ = app.send(Message::MediaLoaded {
        key: crate::message::MediaKey::Image("att/pic.png".into()),
        result: Err("unsupported format\nmore detail".into()),
    });
    let n = app.status.notice.clone().expect("notice");
    assert_eq!(n.severity, crate::app::Severity::Warning);
    assert_eq!(n.text, "image failed to load: pic.png (unsupported format)");
}

/// Plan round-5 WP-3: clicking an unresolved link raises a notice naming the target; a URL does not.
#[test]
fn unresolved_link_click_raises_a_notice() {
    let (_tmp, d) = setup(&[("A.md", b"see [[Nope]]\n")]);
    let mut app = app_on(&d, "A.md");
    let _ = app.send(Message::Reading(ReadingMsg::OpenLink {
        src: "A.md".into(),
        target: "https://example.com/x".into(),
        fragment: None,
    }));
    assert!(app.status.notice.is_none());
    let _ = app.send(Message::Reading(ReadingMsg::OpenLink {
        src: "A.md".into(),
        target: "Nope".into(),
        fragment: None,
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
    let _ = app.send(Message::Reading(ReadingMsg::GoToEdge { bottom: true }));
    assert_eq!(app.reading.edge(&app.docs, p, view), Some(true));
    // The snap's own scroll report at the end keeps the edge.
    let _ = app.send(Message::Reading(ReadingMsg::Scrolled {
        path: p.to_owned(),
        view,
        y: 1.0,
    }));
    assert_eq!(app.reading.edge(&app.docs, p, view), Some(true));
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
    let _ = app.send(Message::Reading(ReadingMsg::GoToEdge { bottom: true }));
    assert_eq!(app.reading.edge(&app.docs, p, view), Some(true));
    app.reading.clear_top_block(&mut app.docs, p, view);
    let task = app.send(Message::Reading(ReadingMsg::SetMode {
        path: p.to_owned(),
        mode: ViewMode::Reading,
    }));
    assert!(task.units() > 0, "the snap_to(1.0) task");
    assert_eq!(
        app.reading.top_block(&app.docs, p, view),
        None,
        "no carried line-estimate block"
    );
    assert_eq!(app.reading.edge(&app.docs, p, view), Some(true));
    let _ = app.send(Message::Reading(ReadingMsg::Scrolled {
        path: p.to_owned(),
        view,
        y: 0.5,
    }));
    assert_eq!(
        app.reading.edge(&app.docs, p, view),
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
    let _ = app.send(Message::Reading(ReadingMsg::GoToEdge { bottom: false }));
    assert_eq!(app.reading.edge(&app.docs, p, view), Some(false));
    let _ = app.send(Message::Source(SourceMsg::Action(Action::Scroll {
        lines: 3,
    })));
    assert_eq!(app.reading.edge(&app.docs, p, view), None);
}
