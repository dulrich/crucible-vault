//! Plan round-14 WP-3: image tabs, the system opener for external files, file-card embeds.

use super::*;
use crate::daemon::FakeDaemon;
use crate::message::{ImageTabMsg, MediaKey, PdfTabMsg};
use crate::pdf::store::{Consumer, Doc, PageKey};
use crate::ui_state::TabKind;

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)
        .expect("png");
    out.into_inner()
}

/// A vault of `files` under a temp dir; the opener is the no-op `true` (never a real viewer).
fn setup(files: &[(&str, &[u8])], state: bool) -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    for (p, body) in files {
        let abs = vault.join(p);
        std::fs::create_dir_all(abs.parent().expect("parent")).expect("mkdir");
        std::fs::write(abs, body).expect("write");
    }
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let state = state.then(|| tmp.path().join("state/editor.json"));
    let mut app = app_over(&d, state);
    app.prefs.open_url_cmd = vec!["true".into()];
    (tmp, d, app)
}

fn open(app: &mut App, path: &str) {
    let _ = app.send(Message::Tabs(TabsMsg::Open(path.to_owned())));
}

fn kinds(app: &App) -> Vec<TabKind> {
    app.panes
        .iter()
        .flat_map(|(_, ps)| ps.tabs.iter().filter_map(|t| t.persisted(&app.docs)))
        .collect()
}

/// An image opens an image tab (no document), decodes through the shared cache when shown,
/// shows a decode failure in the tab, and is focused (not duplicated) when opened again.
#[test]
fn image_opens_an_image_tab_without_a_document() {
    let (_tmp, _d, mut app) = setup(
        &[("att/p.png", &png(8, 4)), ("att/bad.png", b"nope")],
        false,
    );
    open(&mut app, "att/p.png");
    assert_eq!(kinds(&app), [TabKind::image("att/p.png")]);
    assert!(app.docs.is_empty(), "no SourceBuffer for an image");
    let ps = app.panes.get(app.focus).expect("pane");
    let tab = ps.active_tab().expect("tab").clone();
    assert_eq!(app.tab_header_title(ps, &tab), "p.png");
    drop(app.view());
    let _ = app.send(Message::ImageTab(ImageTabMsg::Shown {
        path: "att/p.png".into(),
        view: tab.id,
        width: 300.0,
    }));
    assert_eq!(app.reading.images.pending_len(), 1, "a decode started");
    let decoded = crate::image_cache::decode(&png(8, 4), 320).map_err(|e| e.to_string());
    let _ = app.send(Message::MediaLoaded {
        key: MediaKey::Image("att/p.png".into()),
        result: decoded,
    });
    assert_eq!(app.reading.images.pins("att/p.png"), 1);
    drop(app.view());
    open(&mut app, "att/p.png");
    assert_eq!(kinds(&app).len(), 1, "an open image is focused");
    // A decode failure is a message in the tab.
    open(&mut app, "att/bad.png");
    let _ = app.send(Message::MediaLoaded {
        key: MediaKey::Image("att/bad.png".into()),
        result: Err("decode: bad".into()),
    });
    assert!(app.reading.images.failure("att/bad.png").is_some());
    assert!(
        app.status.notice.is_none(),
        "an image tab shows it in place"
    );
    drop(app.view());
    // Closing the image tab releases its pin.
    let _ = app.send(Message::Tabs(TabsMsg::Close(app.focus, 0)));
    assert_eq!(app.reading.images.pins("att/p.png"), 0);
}

/// An image tab persists as a plain `note` tab (no new wire tag) and restores as an image tab.
#[test]
fn image_tab_round_trips_as_a_note_tab() {
    let (tmp, d, mut app) = setup(&[("att/p.png", &png(4, 4)), ("a.md", b"# A\n")], true);
    open(&mut app, "a.md");
    open(&mut app, "att/p.png");
    let _ = app.send(Message::PersistDue);
    let state = tmp.path().join("state/editor.json");
    let json: String = std::fs::read_to_string(&state)
        .expect("written")
        .split_whitespace()
        .collect();
    assert!(
        json.contains(r#"{"tab":"note","path":"att/p.png"}"#),
        "{json}"
    );
    assert!(!json.contains("image"), "no new wire tag: {json}");
    let back = app_over(&d, Some(state));
    assert_eq!(
        kinds(&back),
        [TabKind::Note("a.md".into()), TabKind::image("att/p.png")]
    );
    assert!(!back.docs.contains_key("att/p.png"));
}

/// An office document (round-15 WP-3: no longer a PDF, which opens a PDF tab) goes to the system opener by its absolute vault path and opens no tab; a `.txt` and an
/// unknown extension without NUL bytes open as text; one with a NUL byte goes to the opener.
#[test]
fn external_files_go_to_the_opener_and_text_stays_text() {
    let (_tmp, d, mut app) = setup(
        &[
            ("doc.docx", b"PK\0binary"),
            ("n.txt", b"plain\n"),
            ("t.qqq", b"also plain\n"),
            ("b.qqq", b"bin\0ary"),
        ],
        false,
    );
    open(&mut app, "doc.docx");
    assert!(kinds(&app).is_empty(), "no tab for an external file");
    assert!(app.docs.is_empty());
    let root = d.vault_root().to_path_buf();
    assert_eq!(
        app.opened_files(),
        [root.join("doc.docx").to_string_lossy().into_owned()]
    );
    assert!(app.status.error_text().is_none());
    open(&mut app, "n.txt");
    open(&mut app, "t.qqq");
    assert_eq!(
        kinds(&app),
        [TabKind::Note("n.txt".into()), TabKind::Note("t.qqq".into())]
    );
    open(&mut app, "b.qqq");
    assert_eq!(kinds(&app).len(), 2, "NUL-sniffed binary opens no tab");
    assert!(!app.docs.contains_key("b.qqq"));
    assert_eq!(app.opened_files().len(), 2);
    // A missing opener is a status-bar error.
    app.prefs.open_url_cmd = vec!["/nonexistent/cv-opener".into()];
    open(&mut app, "doc.docx");
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.starts_with("open file:"))
    );
}

/// `![[doc.pdf]]` lowers to a PDF embed block (round-15 WP-4; it was a file card in round-14 and
/// WP-3 -- the expectation changed with the new block); the fallback card's click still opens
/// the PDF tab; `![[B]]` stays a transclusion.
#[test]
fn pdf_embed_is_a_file_card_and_a_note_embed_stays_a_transclusion() {
    let (_tmp, d, mut app) = setup(
        &[
            ("A.md", b"![[doc.pdf]]\n\n![[B]]\n"),
            ("B.md", b"b\n"),
            ("doc.pdf", b"%PDF"),
        ],
        false,
    );
    open(&mut app, "A.md");
    let doc = app.reading.doc(&app.docs, "A.md").expect("rendered");
    assert_eq!(
        doc.model.blocks[0],
        editor_model::render::Block::PdfEmbed {
            target: "doc.pdf".into(),
            page: 1,
            width: None,
        }
    );
    assert!(matches!(
        doc.model.blocks[1],
        editor_model::render::Block::Transclusion { .. }
    ));
    drop(app.view());
    // The fallback card's click is the link route: it resolves to an open, which opens the PDF tab.
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let click = app.reading.click("A.md", "doc.pdf", Some(&dc));
    assert_eq!(click.as_deref(), Some("doc.pdf"));
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::OpenLink {
        src: "A.md".into(),
        target: "doc.pdf".into(),
        fragment: None,
    }));
    assert!(app.opened_files().is_empty(), "no opener for a PDF");
    assert_eq!(
        kinds(&app),
        [TabKind::Note("A.md".into()), TabKind::pdf("doc.pdf")]
    );
}

/// Plan round-15 WP-2: a daemon-side rename of an image under an open image tab keeps it an
/// image tab (same id) at the new path, moves its history entry and its pin. WP-R2: a cached
/// failure is per key and forgotten when the file changes (the move's events), so `q` re-decodes.
#[test]
fn renaming_an_image_keeps_its_tab_an_image_tab() {
    let (_tmp, d, mut app) = setup(&[("att/p.png", &png(8, 4))], false);
    open(&mut app, "att/p.png");
    let id = app.active_tab().expect("tab").id;
    drop(app.view());
    let _ = app.send(Message::ImageTab(ImageTabMsg::Shown {
        path: "att/p.png".into(),
        view: id,
        width: 320.0,
    }));
    let decoded = crate::image_cache::decode(&png(8, 4), 320).map_err(|e| e.to_string());
    let _ = app.send(Message::MediaLoaded {
        key: MediaKey::Image("att/p.png".into()),
        result: decoded,
    });
    assert_eq!(app.reading.images.pins("att/p.png"), 1);
    app.reading.images.fail("att/p.png", "stale".into());
    let moved = d.daemon_move("att/p.png", "att/q.png").expect("move");
    for n in moved {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
    assert_eq!(kinds(&app), [TabKind::image("att/q.png")]);
    assert_eq!(app.active_tab().expect("tab").id, id, "same tab");
    assert!(app.docs.is_empty(), "still no document");
    let ps = app.panes.get(app.focus).expect("pane");
    assert_eq!(ps.history.current(), Some("att/q.png"));
    assert_eq!(app.reading.images.pins("att/p.png"), 0, "old pin released");
    assert!(app.reading.images.failure("att/p.png").is_none());
    assert!(
        app.reading.images.failure("att/q.png").is_none(),
        "re-learned by the next decode"
    );
}

/// Plan round-15 WP-2: an opener that exits non-zero reaches the status bar once its exit is
/// observed (the channel stands in for the subscription).
#[test]
fn a_failing_opener_exit_becomes_a_status_error() {
    let (_tmp, _d, mut app) = setup(&[("doc.docx", b"%PDF")], false);
    app.prefs.open_url_cmd = vec!["false".into()];
    open(&mut app, "doc.docx");
    assert!(app.status.error_text().is_none(), "spawn succeeded");
    let msg = app
        .opener_notices
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the reaper reports the exit");
    assert!(msg.starts_with("open file: false exited with"), "{msg}");
    let _ = app.send(Message::Status(StatusMsg::OpenerFailed(msg)));
    assert!(
        app.status
            .error_text()
            .is_some_and(|e| e.contains("exited with"))
    );
}

/// Plan round-15 WP-3: feeds the open the tab body asks for (tasks do not run here).
fn pdf_opened(app: &mut App, path: &str, bytes: &[u8]) {
    drop(app.view());
    let _ = app.send(Message::PdfTab(PdfTabMsg::Open { path: path.into() }));
    let bytes = Arc::new(bytes.to_vec());
    let result = crate::pdf::open(&bytes).map(|s| (bytes, s));
    let _ = app.send(Message::PdfTab(PdfTabMsg::Opened {
        path: path.into(),
        result,
    }));
}

fn page_shown(app: &mut App, view: u64, page: usize, width: f32) {
    let _ = app.send(Message::PdfTab(PdfTabMsg::PageShown {
        view,
        page: PageKey::new("doc.pdf", page),
        width,
    }));
}

/// WP-R2 (review E2): the one completion message, for page `page` of `doc.pdf`.
fn rendered(
    app: &mut App,
    page: usize,
    key: &str,
    result: Result<crate::image_cache::Decoded, String>,
) {
    let _ = app.send(Message::MediaLoaded {
        key: MediaKey::Page(PageKey::new("doc.pdf", page), key.to_owned()),
        result,
    });
}

/// Plan round-15 WP-3: a PDF opens a PDF tab (no document, no opener), renders a shown page
/// through the shared cache under a page key, releases it on close, and persists as a plain
/// `note` tab that restores as a PDF tab.
#[test]
fn pdf_opens_a_pdf_tab_and_round_trips_as_a_note_tab() {
    let pdf = crate::pdf::synthetic_pdf(3, 200, 100);
    let (tmp, d, mut app) = setup(&[("doc.pdf", &pdf), ("a.md", b"# A\n")], true);
    open(&mut app, "a.md");
    open(&mut app, "doc.pdf");
    assert_eq!(
        kinds(&app),
        [TabKind::Note("a.md".into()), TabKind::pdf("doc.pdf")]
    );
    assert!(
        !app.docs.contains_key("doc.pdf"),
        "no SourceBuffer for a PDF"
    );
    assert!(app.opened_files().is_empty(), "no opener call");
    let id = app.active_tab().expect("tab").id;
    pdf_opened(&mut app, "doc.pdf", &pdf);
    drop(app.view());
    page_shown(&mut app, id, 0, 300.0);
    let key = PageKey::new("doc.pdf", 0).cache_key(320);
    assert_eq!(key, "doc.pdf#page=1@320");
    assert_eq!(app.reading.images.pending_len(), 1, "a render started");
    assert!(
        app.reading.images.pdf_bytes() > 0,
        "projected bytes reserved"
    );
    let bytes = Arc::new(pdf.clone());
    rendered(&mut app, 0, &key, crate::pdf::render_page(&bytes, 0, 320));
    assert_eq!(app.reading.images.pins(&key), 1);
    assert!(
        !app.reading.images.contains("doc.pdf"),
        "no image-key collision"
    );
    drop(app.view());
    // Session round-trip: the wire stays `note`.
    let _ = app.send(Message::PersistDue);
    let state = tmp.path().join("state/editor.json");
    let json: String = std::fs::read_to_string(&state)
        .expect("written")
        .split_whitespace()
        .collect();
    assert!(
        json.contains(r#"{"tab":"note","path":"doc.pdf"}"#),
        "{json}"
    );
    assert!(!json.contains(r#""tab":"pdf""#), "no new wire tag: {json}");
    let back = app_over(&d, Some(state));
    assert_eq!(
        kinds(&back),
        [TabKind::Note("a.md".into()), TabKind::pdf("doc.pdf")]
    );
    // Closing the PDF tab releases its page pin and reservation.
    let _ = app.send(Message::Tabs(TabsMsg::Close(app.focus, 1)));
    assert_eq!(app.reading.images.pins(&key), 0);
    assert_eq!(app.reading.images.pdf_bytes(), 0);
    assert!(!app.pdf.has_docs());
}

/// Plan round-15 WP-3: a corrupt PDF shows the error state in its tab (with "Open externally").
#[test]
fn a_corrupt_pdf_shows_the_error_state() {
    let (_tmp, _d, mut app) = setup(&[("bad.pdf", b"%PDF-1.4\0garbage")], false);
    open(&mut app, "bad.pdf");
    assert_eq!(kinds(&app), [TabKind::pdf("bad.pdf")]);
    pdf_opened(&mut app, "bad.pdf", b"%PDF-1.4\0garbage");
    assert!(matches!(app.pdf.doc("bad.pdf"), Some(Doc::Failed(_))));
    drop(app.view());
    let _ = app.send(Message::PdfTab(PdfTabMsg::OpenExternal("bad.pdf".into())));
    assert_eq!(app.opened_files().len(), 1, "Open externally hands it over");
}

/// Plan round-15 WP-3: a renamed PDF keeps its PDF tab (same id); its page pins are released
/// and the parsed document moves with the file.
#[test]
fn renaming_a_pdf_keeps_its_tab_a_pdf_tab() {
    let pdf = crate::pdf::synthetic_pdf(1, 200, 100);
    let (_tmp, d, mut app) = setup(&[("doc.pdf", &pdf)], false);
    open(&mut app, "doc.pdf");
    let id = app.active_tab().expect("tab").id;
    pdf_opened(&mut app, "doc.pdf", &pdf);
    page_shown(&mut app, id, 0, 300.0);
    assert!(app.reading.images.pdf_bytes() > 0);
    let moved = d.daemon_move("doc.pdf", "d2.pdf").expect("move");
    for n in moved {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
    assert_eq!(kinds(&app), [TabKind::pdf("d2.pdf")]);
    assert_eq!(app.active_tab().expect("tab").id, id, "same tab");
    assert_eq!(app.reading.images.pdf_bytes(), 0, "old page pins released");
    assert!(app.pdf.doc("d2.pdf").is_some() && app.pdf.doc("doc.pdf").is_none());
}

/// Plan round-15 WP-4: A.md embeds page 2 of a three-page PDF; returns the app with the note
/// open and the document parsed (tasks do not run here, so their results are fed by hand).
fn embed_setup(note: &[u8]) -> (tempfile::TempDir, Arc<FakeDaemon>, App, Vec<u8>, u64) {
    let pdf = crate::pdf::synthetic_pdf(3, 200, 100);
    let (tmp, d, mut app) = setup(&[("A.md", note), ("doc.pdf", &pdf)], false);
    open(&mut app, "A.md");
    let view = app.active_tab().expect("tab").id;
    drop(app.view());
    (tmp, d, app, pdf, view)
}

fn embed_shown(app: &mut App, view: u64, page: usize, width: f32) {
    let _ = app.send(Message::PdfTab(PdfTabMsg::EmbedShown {
        note: "A.md".into(),
        view,
        page: PageKey::new("doc.pdf", page),
        width,
    }));
}

fn embed_doc_opened(app: &mut App, pdf: &[u8]) {
    let bytes = Arc::new(pdf.to_vec());
    let result = crate::pdf::open(&bytes).map(|s| (bytes, s));
    let _ = app.send(Message::PdfTab(PdfTabMsg::Opened {
        path: "doc.pdf".into(),
        result,
    }));
}

fn embed_of(view: u64) -> Consumer {
    Consumer::Embed {
        note: "A.md".into(),
        view,
    }
}

/// Plan round-15 WP-4: an embed sensor opens the shared document, pins its page under the
/// page's cache key through the E1 reservation, survives a recompute (audit E4), and is
/// released when the embed leaves the model.
#[test]
fn a_pdf_embed_renders_pins_survives_recompute_and_opens_the_tab_at_its_page() {
    let (_tmp, _d, mut app, pdf, view) = embed_setup(b"![[doc.pdf#page=2]]\n");
    embed_shown(&mut app, view, 1, 300.0);
    assert!(
        matches!(app.pdf.doc("doc.pdf"), Some(Doc::Loading)),
        "the embed asks for the document"
    );
    embed_doc_opened(&mut app, &pdf);
    let width = crate::image_cache::decode_width(300.0);
    let key = PageKey::new("doc.pdf", 1).cache_key(width);
    assert_eq!(key, format!("doc.pdf#page=2@{width}"));
    assert!(
        app.reading.images.is_reserved(&key),
        "pinned, load in flight"
    );
    let bytes = Arc::new(pdf.clone());
    rendered(&mut app, 1, &key, crate::pdf::render_page(&bytes, 1, width));
    assert!(
        app.reading.images.contains(&key),
        "rendered into the shared cache"
    );

    // Audit E4: a recompute keeps the embed's page pin.
    let dc = app.daemon.clone();
    let mut od = app.docs.remove("A.md").expect("open");
    app.reading.recompute(&mut od, dc.as_ref());
    app.docs.insert(od);
    app.prune_pdf_embeds();
    assert_eq!(app.reading.images.pins(&key), 1, "survives recompute");
    // The embed leaves the model: the recompute releases it.
    let mut od = app.docs.remove("A.md").expect("open");
    od.buffer
        .reload(b"gone\n", crate::daemon::ContentHash::of(b"gone\n"));
    app.reading.recompute(&mut od, dc.as_ref());
    app.docs.insert(od);
    app.prune_pdf_embeds();
    assert_eq!(
        app.reading.images.pins(&key),
        0,
        "released when the embed is gone"
    );
    assert!(!app.reading.images.contains(&key));
}

/// Plan round-15 WP-4: a click on the rendered page opens the PDF tab (existing rules) and the
/// tab scrolls to that page once it has a width.
#[test]
fn clicking_a_pdf_embed_opens_the_pdf_tab_at_the_page() {
    let (_tmp, _d, mut app, pdf, view) = embed_setup(b"![[doc.pdf#page=3]]\n");
    embed_shown(&mut app, view, 2, 300.0);
    embed_doc_opened(&mut app, &pdf);
    let _ = app.send(Message::PdfTab(PdfTabMsg::EmbedOpen {
        path: "doc.pdf".into(),
        page: 2,
    }));
    assert_eq!(
        kinds(&app),
        [TabKind::Note("A.md".into()), TabKind::pdf("doc.pdf")]
    );
    assert_eq!(app.pdf.goto.get("doc.pdf"), Some(&2), "waits for a width");
    let tab = app.active_tab().expect("tab").id;
    drop(app.view());
    page_shown(&mut app, tab, 0, 400.0);
    assert!(app.pdf.goto.is_empty(), "scroll request consumed");
    let pages = vec![(200.0, 100.0); 3];
    assert_eq!(crate::pdf::view::page_offset(&pages, 400.0, 0), 8.0);
    assert_eq!(
        crate::pdf::view::page_offset(&pages, 400.0, 2),
        8.0 + 2.0 * 208.0
    );
}

/// Plan round-15 WP-4: an out-of-range page holds nothing, and a failed render is recorded on
/// the page (the embed then draws the file card) and releases its pin.
#[test]
fn a_pdf_embed_falls_back_to_the_card() {
    let (_tmp, _d, mut app, pdf, view) = embed_setup(b"![[doc.pdf#page=9]]\n");
    embed_shown(&mut app, view, 8, 300.0);
    embed_doc_opened(&mut app, &pdf);
    assert!(
        app.pdf
            .held(&embed_of(view), &PageKey::new("doc.pdf", 8))
            .is_none(),
        "page 9 of 3"
    );
    assert!(app.reading.images.pdf_bytes() == 0, "nothing reserved");
    drop(app.view());

    // A render failure marks the page failed and releases its pin.
    embed_shown(&mut app, view, 0, 300.0);
    let width = crate::image_cache::decode_width(300.0);
    let key = PageKey::new("doc.pdf", 0).cache_key(width);
    assert!(app.reading.images.is_reserved(&key));
    rendered(&mut app, 0, &key, Err("boom".into()));
    assert_eq!(
        app.pdf.page_failed(&PageKey::new("doc.pdf", 0)),
        Some("boom")
    );
    assert!(!app.reading.images.contains(&key));
    assert!(app.reading.images.pdf_bytes() == 0);
    drop(app.view());
}

/// Plan round-15 WP-4: an unparseable PDF shows the card.
#[test]
fn an_unparseable_pdf_embed_shows_the_card() {
    let (_tmp, _d, mut app, _pdf, view) = embed_setup(b"![[doc.pdf]]\n");
    embed_shown(&mut app, view, 0, 300.0);
    embed_doc_opened(&mut app, b"%PDF-1.4\0garbage");
    assert!(matches!(app.pdf.doc("doc.pdf"), Some(Doc::Failed(_))));
    assert!(app.reading.images.pdf_bytes() == 0);
    drop(app.view());
}

/// WP-R1: completes every render tab `view` has in flight with a fake bitmap of the projected
/// size (Letter pages), until nothing is left in flight.
fn settle_renders(app: &mut App, view: u64) {
    for _ in 0..64 {
        let Some(v) = app.pdf.pages_of(&Consumer::Tab(view), "doc.pdf") else {
            return;
        };
        let pending: Vec<(usize, crate::pdf::store::Held)> = v
            .held
            .iter()
            .chain(v.upgrading.iter())
            .map(|(p, h)| (*p, h.clone()))
            .filter(|(_, h)| !app.reading.images.contains(&h.key))
            .collect();
        if pending.is_empty() {
            return;
        }
        for (page, h) in pending {
            let bytes = crate::pdf::projected_bytes(LETTER, h.width);
            let d = crate::image_cache::Decoded {
                handle: iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
                width: h.width,
                height: (bytes / 4 / h.width as usize) as u32,
                bytes,
            };
            rendered(app, page, &h.key, Ok(d));
        }
    }
    panic!("renders never settled");
}

const LETTER: (f32, f32) = (612.0, 792.0);

fn tab_pages(app: &App, id: u64) -> &crate::pdf::store::Pages {
    app.pdf
        .pages_of(&Consumer::Tab(id), "doc.pdf")
        .expect("view")
}

/// WP-R1 (P1): at a 1900 px pane over the 64 MiB budget the shown set spans six Letter pages
/// (only three fit at full width); the visible pages — the middle of the shown set — still
/// render at the full decode width, whatever order the sensors report in.
#[test]
fn visible_pdf_pages_render_at_full_width_under_budget_pressure() {
    let pdf = crate::pdf::synthetic_pdf(6, 612, 792);
    let (_tmp, _d, mut app) = setup(&[("doc.pdf", &pdf)], false);
    app.reading.images = crate::image_cache::ImageCache::with_budget(64 << 20);
    open(&mut app, "doc.pdf");
    let id = app.active_tab().expect("tab").id;
    pdf_opened(&mut app, "doc.pdf", &pdf);
    for page in 0..6 {
        page_shown(&mut app, id, page, 1900.0);
    }
    settle_renders(&mut app, id);
    let v = tab_pages(&app, id);
    for page in [2, 3] {
        let h = v.held.get(&page).expect("visible page admitted");
        assert_eq!(h.width, 1920, "page {page}: {}", h.key);
    }
}

/// WP-R1 (review P1): a page admitted below its planned width re-renders at full width once
/// farther pages leave view, and its smaller bitmap is dropped.
#[test]
fn a_downscaled_pdf_page_upgrades_when_room_frees() {
    let pdf = crate::pdf::synthetic_pdf(6, 612, 792);
    let (_tmp, _d, mut app) = setup(&[("doc.pdf", &pdf)], false);
    app.reading.images = crate::image_cache::ImageCache::with_budget(64 << 20);
    open(&mut app, "doc.pdf");
    let id = app.active_tab().expect("tab").id;
    pdf_opened(&mut app, "doc.pdf", &pdf);
    for page in 0..6 {
        page_shown(&mut app, id, page, 1900.0);
    }
    settle_renders(&mut app, id);
    let small = tab_pages(&app, id).held[&4].clone();
    assert!(small.width < 1920, "{}", small.key);
    for page in [0, 1, 5] {
        let _ = app.send(Message::PdfTab(PdfTabMsg::PageHidden {
            view: id,
            page: PageKey::new("doc.pdf", page),
        }));
    }
    assert_eq!(
        tab_pages(&app, id).held[&4],
        small,
        "shown until the upgrade lands"
    );
    settle_renders(&mut app, id);
    let v = tab_pages(&app, id);
    for page in [2, 3, 4] {
        assert_eq!(v.held[&page].width, 1920, "page {page}");
    }
    assert!(v.upgrading.is_empty());
    assert!(
        !app.reading.images.contains(&small.key),
        "the smaller key is dropped"
    );
    assert!(app.reading.images.pdf_bytes() <= app.reading.images.budget());
}
/// WP-R1 (review E3, audit B1): PDF accounting lives in the cache — a reservation becomes the
/// resident entry's bytes on load (several completed batches all count), a shared key counts
/// once, and the last owner's release frees an outstanding reservation.
#[test]
fn pdf_page_accounting_spans_completed_batches_and_releases() {
    use crate::image_cache::{Decoded, ImageCache, view_owner};
    let decoded = |bytes: usize| Decoded {
        handle: iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]),
        width: 1,
        height: 1,
        bytes,
    };
    let mut cache = ImageCache::with_budget(1 << 20);
    let tab = view_owner("doc.pdf", 1);
    for (i, batch) in [["p1", "p2"], ["p3", "p4"]].iter().enumerate() {
        for k in batch {
            assert!(cache.show(&tab, k));
            cache.reserve(k, 1000);
        }
        // Shown by a second view too: still one reservation.
        assert!(!cache.show(&view_owner("doc.pdf", 2), batch[0]));
        assert_eq!(cache.pdf_bytes(), 2000 * (i + 1));
        for k in batch {
            assert!(cache.loaded(k, Some(decoded(1000))));
        }
        assert_eq!(cache.pdf_bytes(), 2000 * (i + 1), "batch {i} resident");
    }
    assert_eq!(
        crate::pdf::admit((10.0, 10.0), 64.0, cache.pdf_bytes(), 4500),
        None,
        "admission reads resident pages, not only pending ones"
    );
    // An image (not a PDF page) never counts.
    assert!(cache.show("a.md", "img.png"));
    assert!(cache.loaded("img.png", Some(decoded(500))));
    assert_eq!(cache.pdf_bytes(), 4000);
    // Outstanding reservation released with its owner; the late load is dropped.
    assert!(cache.show(&tab, "p5"));
    cache.reserve("p5", 1000);
    assert_eq!(cache.pdf_bytes(), 5000);
    cache.release_owner("doc.pdf");
    assert_eq!(cache.pdf_bytes(), 0);
    assert!(!cache.loaded("p5", Some(decoded(1000))));
    assert_eq!(cache.pdf_bytes(), 0);
}

/// WP-R1 (review E5): renaming a note releases its inline PDF embed state and pins.
#[test]
fn renaming_a_note_releases_its_pdf_embeds() {
    let (_tmp, d, mut app, pdf, view) = embed_setup(b"![[doc.pdf#page=2]]\n");
    embed_shown(&mut app, view, 1, 300.0);
    embed_doc_opened(&mut app, &pdf);
    let key = PageKey::new("doc.pdf", 1).cache_key(crate::image_cache::decode_width(300.0));
    assert!(app.reading.images.is_reserved(&key));
    let moved = d.daemon_move("A.md", "B.md").expect("move");
    for n in moved {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
    assert!(app.pdf.embed_notes().all(|n| n != "A.md"));
    assert!(!app.reading.images.is_reserved(&key));
    assert_eq!(app.reading.images.pdf_bytes(), 0);
}

/// WP-R2 (review E2): one page completion lands for a PDF tab and a note embed showing the
/// same page at the same width — one render, both consumers hold it.
#[test]
fn one_completion_serves_a_tab_and_an_embed_of_the_same_page() {
    let (_tmp, _d, mut app, pdf, view) = embed_setup(b"![[doc.pdf#page=2]]\n");
    embed_shown(&mut app, view, 1, 300.0);
    embed_doc_opened(&mut app, &pdf);
    open(&mut app, "doc.pdf");
    let tab = app.active_tab().expect("tab").id;
    page_shown(&mut app, tab, 1, 300.0);
    let pk = PageKey::new("doc.pdf", 1);
    let key = pk.cache_key(crate::image_cache::decode_width(300.0));
    assert_eq!(app.reading.images.pending_len(), 1, "one render for both");
    let bytes = Arc::new(pdf.clone());
    rendered(&mut app, 1, &key, crate::pdf::render_page(&bytes, 1, 320));
    assert_eq!(app.pdf.held(&embed_of(view), &pk), Some(key.as_str()));
    assert_eq!(app.pdf.held(&Consumer::Tab(tab), &pk), Some(key.as_str()));
    assert_eq!(app.reading.images.pins(&key), 2);
}
