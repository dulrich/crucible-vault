//! WP-5 panel tests against `FakeDaemon` on a hand-written synthetic corpus (I1).

use std::path::Path;
use std::sync::Arc;

use ofm::frontmatter::JsValue;

use super::props;
use super::*;
use crate::app::App;
use crate::daemon::{DaemonClient, FakeDaemon};
use crate::message::{Message, PanelsMsg, SourceMsg, TabsMsg};
use editor_model::props::PropType;

const A: &str = "---\ntags: [proj, x]\ncreated: 2026-01-02\nread: true\nword-count: lots\ntopics: one\nmood: [calm, warm]\nrelated: \"[[b]]\"\n---\n# A title\n\nSee [[b]] and [[nope]] and [[scan.pdf]].\nWeb [site](https://example.com) and [local](b.md).\n\n```\n# not a heading\n```\n\n## Section #alpha\n";
const B: &str = "# B\nsee [[a]] here zebraword\n";
const C: &str = "#alpha #beta\n\n[[a]]\n";

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(p, body).expect("write");
}

fn env() -> (tempfile::TempDir, App) {
    let (tmp, _, app) = env_fake();
    (tmp, app)
}

fn env_fake() -> (tempfile::TempDir, Arc<FakeDaemon>, App) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    write(&vault, "a.md", A);
    write(&vault, "b.md", B);
    write(&vault, "sub/c.md", C);
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    let dc: Arc<dyn DaemonClient> = d.clone();
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Tabs(TabsMsg::Open("a.md".into())));
    let _ = app.send(Message::Panels(PanelsMsg::Show(PanelKind::Outline)));
    (tmp, d, app)
}

fn targets(v: &[OutLink]) -> Vec<&str> {
    v.iter().map(|l| l.target.as_str()).collect()
}

#[test]
fn outline_links_and_props_from_the_buffer() {
    let (_tmp, mut app) = env();
    let p = &app.panels;
    let o: Vec<(u8, &str, usize)> = p
        .outline
        .iter()
        .map(|h| (h.level, h.text.as_str(), h.line))
        .collect();
    assert_eq!(o, vec![(1, "A title", 9), (2, "Section #alpha", 18)]);
    assert_eq!(targets(&p.links.resolved), vec!["b", "b.md", "b"]);
    assert!(
        p.links
            .resolved
            .iter()
            .all(|l| l.resolved.as_deref() == Some("b.md"))
    );
    assert_eq!(targets(&p.links.missing), vec!["nope"]);
    assert_eq!(
        targets(&p.links.external),
        vec!["scan.pdf", "https://example.com"]
    );

    let get = |k: &str| {
        app.panels
            .props
            .iter()
            .find(|p| p.key == k)
            .map(|p| (p.ty, props::display(p)))
            .expect(k)
    };
    assert_eq!(get("tags"), (Some(PropType::Tags), "[proj] [x]".to_owned()));
    assert_eq!(
        get("created"),
        (Some(PropType::Date), "2026-01-02".to_owned())
    );
    assert_eq!(get("read"), (Some(PropType::Checkbox), "☑".to_owned()));
    assert_eq!(
        get("word-count"),
        (Some(PropType::Number), "⚠ lots".to_owned())
    );
    assert_eq!(
        get("topics"),
        (Some(PropType::Multitext), "[one]".to_owned())
    );
    assert_eq!(get("mood"), (None, "[calm] [warm]".to_owned()));
    // Plan round-6 WP-5: a wikilink value renders as a link (render model's parse).
    let related = app.panels.props.iter().find(|p| p.key == "related");
    assert_eq!(
        related.and_then(props::links),
        Some(
            &[editor_model::props::PropValue::Link {
                target: "b".into(),
                fragment: None,
                alias: None,
            }][..]
        )
    );
    let unchecked = editor_model::props::Property::new("read", &JsValue::Bool(false));
    assert_eq!(props::display(&unchecked), "☐");

    // Unsaved edits show up on the next tick; the outline click moves the cursor only.
    let _ = app.send(Message::Source(SourceMsg::Action(
        iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Paste(Arc::new(
            "# New top\n".into(),
        ))),
    )));
    let _ = app.send(Message::Panels(PanelsMsg::Tick));
    assert_eq!(app.panels.outline[0].text, "New top");
    // Plan round-19-yt-cleanup-intake WP-1: the jump is mode-aware; Source moves the cursor.
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
        path: "a.md".into(),
        mode: crate::reading::ViewMode::Source,
    }));
    let _ = app.send(Message::Panels(PanelsMsg::OutlineJump(10)));
    assert_eq!(app.active_doc().expect("doc").cursor().position.line, 10);
    let tab = app.active_tab().expect("tab");
    assert_eq!(tab.path(), Some("a.md"));
}

#[test]
fn backlinks_tags_and_search_from_the_index() {
    let (_tmp, mut app) = env();
    let b: Vec<(&str, Option<i64>, &str)> = app
        .panels
        .backlinks
        .iter()
        .map(|b| (b.src.as_str(), b.line, b.snippet.as_str()))
        .collect();
    assert_eq!(
        b,
        vec![
            ("b.md", Some(2), "see [[a]] here zebraword"),
            ("sub/c.md", Some(3), "[[a]]"),
        ]
    );

    let tags: Vec<(&str, i64)> = app
        .panels
        .tags
        .iter()
        .map(|t| (t.tag.as_str(), t.occurrences))
        .collect();
    assert_eq!(tags[0], ("alpha", 2), "{tags:?}");
    assert!(tags.contains(&("beta", 1)) && tags.contains(&("proj", 1)));
    let _ = app.send(Message::Panels(PanelsMsg::TagPick("alpha".into())));
    assert_eq!(
        app.panels.tag_files,
        Some(("alpha".into(), vec!["a.md".into(), "sub/c.md".into()]))
    );
    let _ = app.send(Message::Panels(PanelsMsg::Open("sub/c.md".into())));
    assert_eq!(app.active_tab().and_then(|t| t.path()), Some("sub/c.md"));
}

fn hit(path: &str, heading: &str, snippet: &str) -> cv_protocol::SearchHit {
    cv_protocol::SearchHit {
        path: path.into(),
        title: String::new(),
        heading: heading.into(),
        snippet: snippet.into(),
        score: 1.0,
    }
}

#[test]
fn search_goes_through_the_daemon_with_headings_and_a_seq_gate() {
    let (_tmp, d, mut app) = env_fake();
    d.set_search_result(cv_protocol::SearchResult {
        hits: vec![hit("b.md", "B", "see  [[a]]\n here zebraword")],
        semantic: true,
    })
    .unwrap();
    let _ = app.send(Message::Panels(PanelsMsg::SearchInput("zebra".into())));
    let seq = app.panels.search.seq;
    let _ = app.send(Message::Panels(PanelsMsg::SearchInput("zebraw".into())));
    let _ = app.send(Message::Panels(PanelsMsg::SearchDue(seq)));
    assert!(
        app.panels.search.results.is_empty(),
        "stale debounce ignored"
    );
    assert!(
        d.search_calls().unwrap().is_empty(),
        "no call for a stale seq"
    );
    let _ = app.send(Message::Panels(PanelsMsg::SearchDue(seq + 1)));
    assert_eq!(
        app.panels.search.results,
        vec![SearchHit {
            path: "b.md".into(),
            heading: "B".into(),
            snippet: "see [[a]] here zebraword".into(),
        }]
    );
    let limit = u32::try_from(app.prefs.search_limit).unwrap();
    assert_eq!(
        d.search_calls().unwrap(),
        vec![("zebraw".to_owned(), limit)]
    );
    assert!(app.panels.search.error.is_none());

    // A stale result (older seq) arriving late is dropped.
    let _ = app.send(Message::Panels(PanelsMsg::SearchDone {
        seq,
        result: Ok(Vec::new()),
    }));
    assert_eq!(app.panels.search.results.len(), 1);
    // The row renders (path + heading + snippet) without panicking.
    let _ = app.send(Message::Panels(PanelsMsg::Show(PanelKind::Search)));
    let _ = app.view();
}

#[test]
fn search_with_the_daemon_disconnected_shows_unavailable_and_reads_no_index() {
    let (tmp, d, mut app) = env_fake();
    d.set_search_disconnected(true).unwrap();
    // Remove the local index files: any fallback read of index.db would now fail loudly.
    let _ = std::fs::remove_dir_all(tmp.path().join("data"));
    let _ = app.send(Message::Panels(PanelsMsg::SearchInput("zebra".into())));
    let seq = app.panels.search.seq;
    let _ = app.send(Message::Panels(PanelsMsg::SearchDue(seq)));
    assert_eq!(app.panels.search.error.as_deref(), Some(SEARCH_UNAVAILABLE));
    assert!(app.panels.search.results.is_empty());
    assert_eq!(d.search_calls().unwrap().len(), 1);

    // No daemon at all: the same error line, no RPC.
    let mut app = App::new(None, None);
    app.queue_rpcs();
    let _ = app.send(Message::Panels(PanelsMsg::SearchInput("zebra".into())));
    let seq = app.panels.search.seq;
    let _ = app.send(Message::Panels(PanelsMsg::SearchDue(seq)));
    assert_eq!(app.panels.search.error.as_deref(), Some(SEARCH_UNAVAILABLE));
}

#[test]
fn pure_helpers() {
    assert!(is_externalish("https://x.y/z"));
    assert!(is_externalish("assets/scan.PDF"));
    assert!(!is_externalish("notes/missing"));
    assert!(!is_externalish("v1.2 release"));
    assert!(!is_externalish("Note.md"));
    let long = "x".repeat(200);
    assert_eq!(snippet(&long).chars().count(), SNIPPET_CHARS + 1);
    assert_eq!(editor_model::props::TYPES.len(), 35);
    let show = |k: &str, v: JsValue| props::display(&editor_model::props::Property::new(k, &v));
    assert_eq!(show("TQ_show_tree", JsValue::Str("yes".into())), "⚠ yes");
    assert_eq!(
        show("created", JsValue::Str("2026-01-02T10:00".into())),
        "2026-01-02T10:00"
    );
    assert_eq!(show("review-after-lines", JsValue::Num(12.0)), "12");
}

/// editor-usability-round WP-5 measurement harness (not a gate): Toggle Panels latency against a
/// real `cv-index` index of the synthetic spike vault in `CV_SPIKE_VAULT`
/// (`docs/spikes/panels-lag/gen.py`). Run:
/// `CV_SPIKE_VAULT=$PWD/runs/spike/panels-lag/vault cargo test -p editor --release --lib panels_open_latency -- --ignored --nocapture`
#[test]
#[ignore = "measurement harness; needs CV_SPIKE_VAULT"]
fn panels_open_latency() {
    let Ok(vault) = std::env::var("CV_SPIKE_VAULT") else {
        return;
    };
    let vault = std::path::PathBuf::from(vault);
    let tmp = tempfile::tempdir().expect("tmp");
    let t = std::time::Instant::now();
    let d = Arc::new(FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake"));
    println!("index build: {:?}", t.elapsed());
    let first = {
        let mut stack = vec![vault.clone()];
        let mut found = None;
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).expect("dir").flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if found.is_none() && p.extension().is_some_and(|x| x == "md") {
                    found = Some(p);
                }
            }
        }
        found.expect("a note")
    };
    let rel = first
        .strip_prefix(&vault)
        .expect("rel")
        .to_string_lossy()
        .into_owned();
    let dc: Arc<dyn DaemonClient> = d;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Tabs(TabsMsg::Open(rel)));
    for round in 0..3 {
        // Each round: a new index generation (as after any save), then Toggle Panels open.
        app.status.generation += 1;
        let t = std::time::Instant::now();
        let _ = app.send(Message::Panels(PanelsMsg::Show(PanelKind::Outline)));
        let open = t.elapsed();
        let t = std::time::Instant::now();
        let _ = app.send(Message::Panels(PanelsMsg::Tick));
        let tick = t.elapsed();
        println!(
            "round {round}: toggle-open {:.1} ms, next tick {:.1} ms",
            open.as_secs_f64() * 1e3,
            tick.as_secs_f64() * 1e3
        );
        let _ = app.send(Message::Panels(PanelsMsg::Toggle));
    }
}

/// Plan round-6 WP-5: a right-click on a panel link entry yields `Target::Link` in the active
/// note (missing entries offer Create note; external ones the URL items); an Outline entry is an
/// anchor with Open / Open in new pane only, and Open jumps the cursor.
#[test]
fn panel_entries_yield_the_link_menu_and_outline_anchors() {
    use crate::context_menu::{Item, MenuMsg, Target, items};
    let (_tmp, mut app) = env();
    let names = |t: &Target| -> Vec<String> { items(t).into_iter().map(|(_, l)| l).collect() };
    let resolved = link_target("a.md", "b.md", false, false);
    assert_eq!(names(&resolved), ["Open", "Open in new pane", "Copy path"]);
    assert_eq!(
        names(&link_target("a.md", "nope", false, true)),
        ["Open", "Open in new pane", "Copy path", "Create note"]
    );
    assert_eq!(
        names(&link_target("a.md", "https://example.com", true, false)),
        ["Open", "Copy URL"]
    );
    let anchor = anchor_target("a.md", 18);
    assert_eq!(names(&anchor), ["Open", "Open in new pane"]);

    let _ = app.send(Message::Menu(MenuMsg::Open(resolved, iced::Point::ORIGIN)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::CopyPath)));
    assert_eq!(app.copied(), ["b.md"]);
    // Plan round-19-yt-cleanup-intake WP-1: the jump is mode-aware; the cursor moves in Source.
    let _ = app.send(Message::Reading(crate::message::ReadingMsg::SetMode {
        path: "a.md".into(),
        mode: crate::reading::ViewMode::Source,
    }));
    let _ = app.send(Message::Menu(MenuMsg::Open(anchor, iced::Point::ORIGIN)));
    let _ = app.send(Message::Menu(MenuMsg::Choose(Item::Open)));
    assert_eq!(app.active_doc().expect("doc").cursor().position.line, 18);
}

/// Plan round-6 WP-5: sidebar link values — wikilinks and URLs, scalar or list — render as
/// links; an unresolved wikilink is marked (dimmed, Create note); plain text stays text.
#[test]
fn sidebar_property_links_and_unresolved_marking() {
    use editor_model::props::PropValue;
    let r = editor_model::props::Property::new(
        "rel",
        &JsValue::Arr(vec![
            JsValue::Str("[[b|Bee]]".into()),
            JsValue::Str("plain".into()),
            JsValue::Str("https://e.com".into()),
        ]),
    );
    assert_eq!(
        props::links(&r),
        Some(
            &[
                PropValue::Link {
                    target: "b".into(),
                    fragment: None,
                    alias: Some("Bee".into()),
                },
                PropValue::Text("plain".into()),
                PropValue::Url {
                    url: "https://e.com".into(),
                    label: "https://e.com".into(),
                },
            ][..]
        )
    );
    assert_eq!(props::display(&r), "Bee, plain, https://e.com");
    let model = editor_model::props::Property::new("model", &JsValue::Str("gpt".into()));
    assert!(props::links(&model).is_none());
    assert_eq!(props::display(&model), "gpt");

    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    write(
        &vault,
        "a.md",
        "---\nup: \"[[b]]\"\nx: \"[[gone]]\"\n---\nbody\n",
    );
    write(&vault, "b.md", B);
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    let dc: Arc<dyn DaemonClient> = Arc::new(d);
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    let _ = app.send(Message::Tabs(TabsMsg::Open("a.md".into())));
    let _ = app.send(Message::Panels(PanelsMsg::Show(PanelKind::Properties)));
    let want: std::collections::HashSet<String> = ["gone".to_owned()].into();
    assert_eq!(app.panels.prop_unresolved, want);
}
