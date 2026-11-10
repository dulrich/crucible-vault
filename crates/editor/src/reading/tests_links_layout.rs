use super::*;

/// Plan round-6 WP-2 (a): a create/delete/rename elsewhere invalidates the resolver but keeps
/// every image's header-read size; only the changed file's own size is dropped.
#[test]
fn dims_survive_resolver_invalidation_for_an_unrelated_create() {
    use cv_protocol::{ChangeKind, Notification, Origin};

    use crate::message::StatusMsg;

    let pic = png(300, 150);
    let (_tmp, d) = setup(&[("A.md", b"![](pic.png)\n"), ("pic.png", &pic)]);
    let mut app = app_on(&d, "A.md");
    reading(&mut app, "A.md", ViewMode::Reading);
    let _ = app.send(Message::Reading(ReadingMsg::Tick));
    let dims = |app: &App| {
        app.reading
            .images
            .dims("pic.png")
            .map(|d| (d.width, d.height))
    };
    assert_eq!(dims(&app), Some((300, 150)));
    let changed = |path: &str, kind| {
        Message::Status(StatusMsg::Notification(Notification::FileChanged {
            id: None,
            path: path.to_owned(),
            kind,
            hash: None,
            origin: Origin::Daemon,
            from: None,
            replaced_by: None,
        }))
    };
    let epoch = app.resolver.epoch();
    let _ = app.send(changed("other.md", ChangeKind::Created));
    assert!(app.resolver.epoch() > epoch, "resolver invalidated");
    assert_eq!(dims(&app), Some((300, 150)), "unrelated churn keeps dims");
    let _ = app.send(changed("pic.png", ChangeKind::Modified));
    assert_eq!(dims(&app), None, "the changed file's own size is dropped");
}

/// A measuring renderer for layout tests: an RGBA handle measures as its pixel size.
struct Measure;

impl iced::advanced::Renderer for Measure {
    fn start_layer(&mut self, _: iced::Rectangle) {}
    fn end_layer(&mut self) {}
    fn start_transformation(&mut self, _: iced::Transformation) {}
    fn end_transformation(&mut self) {}
    fn reset(&mut self, _: iced::Rectangle) {}
    fn fill_quad(&mut self, _: iced::advanced::renderer::Quad, _: impl Into<iced::Background>) {}
    fn allocate_image(
        &mut self,
        _: &Handle,
        callback: impl FnOnce(Result<iced::advanced::image::Allocation, iced::advanced::image::Error>)
        + Send
        + 'static,
    ) {
        callback(Err(iced::advanced::image::Error::Unsupported));
    }
}

impl iced::advanced::image::Renderer for Measure {
    type Handle = Handle;
    fn load_image(
        &self,
        _: &Handle,
    ) -> Result<iced::advanced::image::Allocation, iced::advanced::image::Error> {
        Err(iced::advanced::image::Error::Unsupported)
    }
    fn measure_image(&self, handle: &Handle) -> Option<iced::Size<u32>> {
        match handle {
            Handle::Rgba { width, height, .. } => Some(iced::Size::new(*width, *height)),
            _ => None,
        }
    }
    fn draw_image(
        &mut self,
        _: iced::advanced::image::Image,
        _: iced::Rectangle,
        _: iced::Rectangle,
    ) {
    }
}

/// Plan round-6 WP-2 (b): the placeholder and the decoded image lay out at the same height —
/// with a `|width` beyond the natural width (the decode never upscales) and for a 1000×667
/// image whose aspect the placeholder rounds.
#[test]
fn placeholder_height_equals_decoded_layout_height() {
    use iced::advanced::layout::Limits;
    use iced::advanced::widget::Tree;

    let height = |el: iced::widget::Stack<'static, (), iced::Theme, Measure>, avail: f32| {
        let mut el: iced::Element<'static, (), iced::Theme, Measure> = el.into();
        let mut tree = Tree::new(el.as_widget());
        let limits = Limits::new(iced::Size::ZERO, iced::Size::new(avail, f32::INFINITY));
        el.as_widget_mut()
            .layout(&mut tree, &Measure, &limits)
            .size()
            .height
    };
    // (natural w, natural h, `|width`, available column width)
    for (w, h, width, avail) in [
        (200, 100, Some(500), 800.0),
        (1000, 667, None, 800.0),
        (1000, 667, None, 1200.0),
        (1000, 667, Some(400), 800.0),
    ] {
        let d = crate::image_cache::Dims::new(w, h).expect("dims");
        let shown = width.map_or(w as f32, |x| x as f32).min(avail);
        let decoded = decode(&png(w, h), decode_width(shown)).expect("decode");
        let placeholder = height(super::view::sized_image(&d, width, None), avail);
        let laid = height(
            super::view::sized_image(&d, width, Some(decoded.handle)),
            avail,
        );
        assert!(
            (placeholder - laid).abs() <= 1.0,
            "{w}x{h} |{width:?} in {avail}: placeholder {placeholder} vs decoded {laid}"
        );
        assert!(laid > 0.0);
    }
}

/// Plan round-6 WP-2 (c): a restore guard the re-created scrollable never consumed (content
/// not taller than the viewport → no scroll report) expires, so a later wheel event after the
/// content grows does not snap back to the stored place.
#[test]
fn stale_restore_guard_does_not_snap_after_content_grows() {
    let (a, b) = ("r6/a.md", "r6/b.md");
    let body = long_note();
    let (_t, d) = setup(&[(a, &body), (b, b"other\n")]);
    let mut app = app_on(&d, a);
    let view = app.view_of(a);
    let blocks = app
        .reading
        .doc(&app.docs, a)
        .expect("doc")
        .model
        .blocks
        .len();
    let block_ys: Vec<Option<f32>> = (0..blocks).map(|i| Some(i as f32 * 20.0)).collect();
    let _ = app.send(Message::Reading(ReadingMsg::Probed {
        path: a.to_owned(),
        view,
        probe: Some(Probe {
            offset_y: 40.0 * 20.0,
            content_y: 0.0,
            block_ys,
            ..Probe::default()
        }),
        scroll_to: None,
    }));
    let _ = app.send(Message::Tabs(TabsMsg::Open(b.to_owned())));
    let pane = app.focus;
    let _ = app.send(Message::Tabs(TabsMsg::Select(pane, 0)));
    assert!(
        app.reading.is_restoring(&app.docs, view),
        "armed on activation"
    );
    // No scroll report arrives (content fits); later the content grows and the user wheels.
    app.reading.age_restore_guard(
        &mut app.docs,
        view,
        RESTORE_GUARD + std::time::Duration::from_millis(1),
    );
    assert!(
        !app.reading.is_restoring(&app.docs, view),
        "the guard expired"
    );
    let _ = app.send(Message::Reading(ReadingMsg::Scrolled {
        path: a.to_owned(),
        view,
        y: 0.3,
    }));
    assert!(!app.reading.is_restoring(&app.docs, view));
}

/// Plan round-6 WP-4: the Doc marks unresolved vault links (the view dims them and the link
/// menu offers "Create note"); resolved, self and remote links are not marked.
#[test]
fn doc_marks_unresolved_vault_links() {
    let (_tmp, d) = setup(&[
        (
            "A.md",
            b"[[B]] [[Nope]] [[#h]] [m](Gone.md) [u](https://x.y)\n",
        ),
        ("B.md", b"b\n"),
    ]);
    let mut app = app_on(&d, "A.md");
    reading(&mut app, "A.md", ViewMode::Reading);
    let doc = app.reading.doc(&app.docs, "A.md").expect("doc");
    let want: HashSet<String> = ["Nope", "Gone.md"].map(String::from).into();
    assert_eq!(doc.unresolved, want);
}

/// Plan round-6 WP-4: `missing_link = "create"` creates the note through `CreateNote` (the
/// explorer's New-note folder for a bare target, the link's path otherwise) and opens it.
#[test]
fn unresolved_link_click_creates_under_create_mode() {
    let (_tmp, d) = setup(&[("A.md", b"see [[Nope]] [[sub/Deep]]\n")]);
    let mut app = app_on(&d, "A.md");
    app.queue_rpcs();
    app.prefs.missing_link = crate::settings::editor_prefs::MissingLink::Create;
    let _ = app.send(Message::Reading(ReadingMsg::OpenLink {
        src: "A.md".into(),
        target: "Nope".into(),
        fragment: None,
    }));
    assert!(d.read_file("Nope.md").is_ok(), "created at the root");
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("Nope.md"));
    let _ = app.send(Message::Reading(ReadingMsg::OpenLink {
        src: "A.md".into(),
        target: "sub/Deep".into(),
        fragment: None,
    }));
    assert!(
        d.read_file("sub/Deep.md").is_ok(),
        "created at the link's path"
    );
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("sub/Deep.md"));
    assert!(
        app.status
            .notice
            .as_ref()
            .is_none_or(|n| !n.text.starts_with("unresolved")),
        "no warning in create mode"
    );
}

/// Plan round-11 WP-7: a nav link to a missing note creates it at exactly the configured vault
/// path (no New-note-folder rewrite) and opens it in the clicked pane.
#[test]
fn nav_link_to_missing_note_creates_and_opens_it() {
    use editor_model::period::NavTarget;
    let (_tmp, d) = setup(&[("A.md", b"a\n")]);
    let mut app = app_on(&d, "A.md");
    app.queue_rpcs();
    let pane = app.focus;
    let _ = app.send(Message::Nav(crate::message::NavMsg::Go {
        pane,
        target: NavTarget::Path("core/Inbox.md".into()),
    }));
    assert!(d.read_file("core/Inbox.md").is_ok(), "created at the path");
    assert_eq!(
        app.active_tab().and_then(|t| t.path()),
        Some("core/Inbox.md")
    );
}

/// Plan round-6 WP-4: the link menu offers "Create note" on an unresolved link under `warn`,
/// and it creates the note.
#[test]
fn link_menu_create_note_under_warn_mode() {
    use crate::context_menu::{Item, MenuMsg, Target, items};
    let (_tmp, d) = setup(&[("A.md", b"see [[Nope]]\n")]);
    let mut app = app_on(&d, "A.md");
    app.queue_rpcs();
    let t = Target::Link {
        src: "A.md".into(),
        target: "Nope".into(),
        external: false,
        view: 1,
        selection: false,
        missing: true,
    };
    assert!(items(&t).iter().any(|(i, _)| *i == Item::CreateNote));
    let at = iced::Point::new(1.0, 1.0);
    let _ = app.send(Message::Menu(MenuMsg::Open(t, at)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CreateNote)));
    assert!(d.read_file("Nope.md").is_ok());
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("Nope.md"));
}

/// Plan round-6 WP-5: a Reading table-cell link (drawn through the same `run_el` spans as the
/// body) yields the link menu from a right press — Create note when unresolved — and a
/// frontmatter wikilink is marked unresolved too (dimmed like the body).
#[test]
fn table_cell_link_right_click_yields_the_link_menu() {
    use crate::context_menu::{Target, items};
    use crate::reading::view::LinkTarget;
    let (_tmp, d) = setup(&[
        (
            "A.md",
            b"---\nup: \"[[Gone]]\"\n---\n| a | b |\n| - | - |\n| [[B]] | [[Nope]] |\n",
        ),
        ("B.md", b"b\n"),
    ]);
    let mut app = app_on(&d, "A.md");
    reading(&mut app, "A.md", ViewMode::Reading);
    let doc = app.reading.doc(&app.docs, "A.md").expect("doc");
    let Some(Block::Table { rows, .. }) = doc
        .model
        .blocks
        .iter()
        .find(|b| matches!(b, Block::Table { .. }))
    else {
        panic!("no table");
    };
    assert!(matches!(rows[0][1].as_slice(), [Inline::WikiLink { target, .. }] if target == "Nope"));
    let want: HashSet<String> = ["Nope", "Gone"].map(String::from).into();
    assert_eq!(doc.unresolved, want);
    let names = |t: &Target| -> Vec<String> { items(t).into_iter().map(|(_, l)| l).collect() };
    let hit = |t: &str| {
        crate::reading::view::press_target(
            "A.md",
            7,
            &doc.unresolved,
            Some(LinkTarget::Vault {
                target: t.into(),
                fragment: None,
            }),
            false,
        )
    };
    let b = hit("B");
    assert!(
        matches!(&b, Target::Link { src, target, missing: false, .. } if src == "A.md" && target == "B")
    );
    assert_eq!(names(&b), ["Open", "Open in new pane", "Copy path"]);
    assert_eq!(
        names(&hit("Nope")),
        ["Open", "Open in new pane", "Copy path", "Create note"]
    );
}

/// A find hit inside a footnote is counted, so Next/Prev reach it (it was painted
/// by the view but missing from the bar's occurrence list).
#[test]
fn reading_find_counts_and_reaches_footnote_hits() {
    use crate::message::FindMsg;
    let p = "r17/fn.md";
    let (_t, mut app) = find_on(
        p,
        b"body foo[^1]\n\n[^1]: the foo note\n",
        ViewMode::Reading,
        "foo",
    );
    assert_eq!(bar(&app).count(), "1/2", "the footnote hit is counted");
    let _ = app.send(Message::Find(FindMsg::Next));
    assert_eq!(bar(&app).count(), "2/2", "Next reaches the footnote hit");
    drop(app.view());
    let _ = app.send(Message::Find(FindMsg::Prev));
    assert_eq!(bar(&app).count(), "1/2");
}

/// Find paint is derived at view time from the focused pane's bar — two panes on
/// one note no longer share it: the pane without the bar paints nothing.
#[test]
fn find_paint_follows_the_focused_pane_only() {
    use crate::context_menu::MenuMsg;
    use crate::message::{FindMsg, PanesMsg};
    let p = "r17/two.md";
    let (_t, mut app) = find_on(p, b"alpha foo\n", ViewMode::Reading, "foo");
    let first = app.focus;
    let tab_a = app.active_tab().map(|t| t.id).expect("tab a");
    // Explorer row → Open in new pane: a second tab on the note, in a new focused pane.
    let row = crate::context_menu::Target::Row {
        path: p.to_owned(),
        folder: false,
    };
    let _ = app.send(Message::Menu(MenuMsg::Open(row, iced::Point::ORIGIN)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(
        crate::context_menu::Item::OpenInNewPane,
    )));
    assert_ne!(app.focus, first);
    let tab_b = app.active_tab().map(|t| t.id).expect("tab b");
    assert_ne!(tab_a, tab_b, "two tabs on one note");
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some(p));
    let _ = app.send(Message::Panes(PanesMsg::Focus(first)));
    let _ = app.send(Message::Find(FindMsg::Next));
    let paint = app
        .find_bar_for(tab_a)
        .and_then(crate::find::FindBar::reading_paint)
        .expect("the focused pane paints");
    assert_eq!((paint.query, paint.active), ("foo", Some(0)));
    assert!(
        app.find_bar_for(tab_b).is_none(),
        "the other pane's view input carries no find paint"
    );
    drop(app.view());
}
