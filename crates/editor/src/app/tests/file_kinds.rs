//! Plan round-14 WP-3: image tabs, the system opener for external files, file-card embeds.

use super::*;
use crate::app::TabKind;
use crate::daemon::FakeDaemon;
use crate::message::ImageTabMsg;

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
        .flat_map(|(_, ps)| ps.tabs.iter().map(|t| t.kind.clone()))
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
    assert_eq!(kinds(&app), [TabKind::Image("att/p.png".into())]);
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
    let _ = app.send(Message::ImageTab(ImageTabMsg::Loaded {
        key: "att/p.png".into(),
        result: decoded,
    }));
    assert_eq!(app.reading.images.pins("att/p.png"), 1);
    drop(app.view());
    open(&mut app, "att/p.png");
    assert_eq!(kinds(&app).len(), 1, "an open image is focused");
    // A decode failure is a message in the tab.
    open(&mut app, "att/bad.png");
    let _ = app.send(Message::ImageTab(ImageTabMsg::Loaded {
        key: "att/bad.png".into(),
        result: Err("decode: bad".into()),
    }));
    assert!(app.image_errors.contains_key("att/bad.png"));
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
        [
            TabKind::Note("a.md".into()),
            TabKind::Image("att/p.png".into())
        ]
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

/// `![[doc.pdf]]` lowers to a file card (round-15 WP-3: its click opens the PDF tab); `![[B]]` stays a
/// transclusion.
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
        editor_model::render::Block::FileEmbed {
            target: "doc.pdf".into()
        }
    );
    assert!(matches!(
        doc.model.blocks[1],
        editor_model::render::Block::Transclusion { .. }
    ));
    drop(app.view());
    // The card's click is the link route: it resolves to an open, which opens the PDF tab.
    let dc: Arc<dyn DaemonClient> = Arc::clone(&d) as Arc<dyn DaemonClient>;
    let click = app.reading.click("A.md", "doc.pdf", false, Some(&dc));
    assert!(matches!(&click, Some(Message::Tabs(TabsMsg::Open(p))) if p == "doc.pdf"));
    let _ = app.send(click.expect("resolved"));
    assert!(app.opened_files().is_empty(), "no opener for a PDF");
    assert_eq!(
        kinds(&app),
        [TabKind::Note("A.md".into()), TabKind::Pdf("doc.pdf".into())]
    );
}

/// Plan round-15 WP-2: a daemon-side rename of an image under an open image tab keeps it an
/// image tab (same id) at the new path, moves its history entry, its pin and its cached failure.
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
    let _ = app.send(Message::ImageTab(ImageTabMsg::Loaded {
        key: "att/p.png".into(),
        result: decoded,
    }));
    assert_eq!(app.reading.images.pins("att/p.png"), 1);
    app.image_errors.insert("att/p.png".into(), "stale".into());
    let moved = d.daemon_move("att/p.png", "att/q.png").expect("move");
    for n in moved {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
    assert_eq!(kinds(&app), [TabKind::Image("att/q.png".into())]);
    assert_eq!(app.active_tab().expect("tab").id, id, "same tab");
    assert!(app.docs.is_empty(), "still no document");
    let ps = app.panes.get(app.focus).expect("pane");
    assert_eq!(ps.history.current(), Some("att/q.png"));
    assert_eq!(app.reading.images.pins("att/p.png"), 0, "old pin released");
    assert!(!app.image_errors.contains_key("att/p.png"));
    assert!(app.image_errors.contains_key("att/q.png"));
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
    use crate::message::PdfTabMsg;
    drop(app.view());
    let _ = app.send(Message::PdfTab(PdfTabMsg::Open { path: path.into() }));
    let bytes = Arc::new(bytes.to_vec());
    let result = crate::pdf::open(&bytes).map(|s| (bytes, s));
    let _ = app.send(Message::PdfTab(PdfTabMsg::Opened {
        path: path.into(),
        result,
    }));
}

/// Plan round-15 WP-3: a PDF opens a PDF tab (no document, no opener), renders a shown page
/// through the shared cache under a page key, releases it on close, and persists as a plain
/// `note` tab that restores as a PDF tab.
#[test]
fn pdf_opens_a_pdf_tab_and_round_trips_as_a_note_tab() {
    use crate::message::PdfTabMsg;
    let pdf = crate::pdf::synthetic_pdf(3, 200, 100);
    let (tmp, d, mut app) = setup(&[("doc.pdf", &pdf), ("a.md", b"# A\n")], true);
    open(&mut app, "a.md");
    open(&mut app, "doc.pdf");
    assert_eq!(
        kinds(&app),
        [TabKind::Note("a.md".into()), TabKind::Pdf("doc.pdf".into())]
    );
    assert!(
        !app.docs.contains_key("doc.pdf"),
        "no SourceBuffer for a PDF"
    );
    assert!(app.opened_files().is_empty(), "no opener call");
    let id = app.active_tab().expect("tab").id;
    pdf_opened(&mut app, "doc.pdf", &pdf);
    drop(app.view());
    let _ = app.send(Message::PdfTab(PdfTabMsg::PageShown {
        path: "doc.pdf".into(),
        view: id,
        page: 0,
        width: 300.0,
    }));
    let key = crate::app::pdf_tab::page_key("doc.pdf", 0, 320);
    assert_eq!(key, "doc.pdf#page=1@320");
    assert_eq!(app.reading.images.pending_len(), 1, "a render started");
    assert!(app.pdf.reserved_bytes() > 0, "projected bytes reserved");
    let bytes = Arc::new(pdf.clone());
    let _ = app.send(Message::PdfTab(PdfTabMsg::Rendered {
        path: "doc.pdf".into(),
        view: id,
        page: 0,
        key: key.clone(),
        result: crate::pdf::render_page(&bytes, 0, 320),
    }));
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
        [TabKind::Note("a.md".into()), TabKind::Pdf("doc.pdf".into())]
    );
    // Closing the PDF tab releases its page pin and reservation.
    let _ = app.send(Message::Tabs(TabsMsg::Close(app.focus, 1)));
    assert_eq!(app.reading.images.pins(&key), 0);
    assert_eq!(app.pdf.reserved_bytes(), 0);
    assert!(app.pdf.docs.is_empty());
}

/// Plan round-15 WP-3: a corrupt PDF shows the error state in its tab (with "Open externally").
#[test]
fn a_corrupt_pdf_shows_the_error_state() {
    let (_tmp, _d, mut app) = setup(&[("bad.pdf", b"%PDF-1.4\0garbage")], false);
    open(&mut app, "bad.pdf");
    assert_eq!(kinds(&app), [TabKind::Pdf("bad.pdf".into())]);
    pdf_opened(&mut app, "bad.pdf", b"%PDF-1.4\0garbage");
    assert!(matches!(
        app.pdf.docs.get("bad.pdf"),
        Some(crate::app::pdf_tab::PdfDoc::Failed(_))
    ));
    drop(app.view());
    let _ = app.send(Message::PdfTab(crate::message::PdfTabMsg::OpenExternal(
        "bad.pdf".into(),
    )));
    assert_eq!(app.opened_files().len(), 1, "Open externally hands it over");
}

/// Plan round-15 WP-3: a renamed PDF keeps its PDF tab (same id); its page pins are released
/// and the parsed document moves with the file.
#[test]
fn renaming_a_pdf_keeps_its_tab_a_pdf_tab() {
    use crate::message::PdfTabMsg;
    let pdf = crate::pdf::synthetic_pdf(1, 200, 100);
    let (_tmp, d, mut app) = setup(&[("doc.pdf", &pdf)], false);
    open(&mut app, "doc.pdf");
    let id = app.active_tab().expect("tab").id;
    pdf_opened(&mut app, "doc.pdf", &pdf);
    let _ = app.send(Message::PdfTab(PdfTabMsg::PageShown {
        path: "doc.pdf".into(),
        view: id,
        page: 0,
        width: 300.0,
    }));
    assert!(app.pdf.reserved_bytes() > 0);
    let moved = d.daemon_move("doc.pdf", "d2.pdf").expect("move");
    for n in moved {
        let _ = app.send(Message::Status(StatusMsg::Notification(n)));
    }
    assert_eq!(kinds(&app), [TabKind::Pdf("d2.pdf".into())]);
    assert_eq!(app.active_tab().expect("tab").id, id, "same tab");
    assert_eq!(app.pdf.reserved_bytes(), 0, "old page pins released");
    assert!(app.pdf.docs.contains_key("d2.pdf") && !app.pdf.docs.contains_key("doc.pdf"));
}
