//! WP-5 panel tests against `FakeDaemon` on a hand-written synthetic corpus (I1).

use std::path::Path;
use std::sync::Arc;

use ofm::frontmatter::JsValue;

use super::props::{self, PropType, Rendered};
use super::*;
use crate::app::App;
use crate::daemon::{DaemonClient, FakeDaemon};
use crate::message::{Message, PanelsMsg, SourceMsg, TabsMsg};

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
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    write(&vault, "a.md", A);
    write(&vault, "b.md", B);
    write(&vault, "sub/c.md", C);
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    let dc: Arc<dyn DaemonClient> = Arc::new(d);
    let mut app = App::new(Some(dc), None);
    app.rpc_inline = true;
    let _ = app.update(Message::Tabs(TabsMsg::Open("a.md".into())));
    let _ = app.update(Message::Panels(PanelsMsg::Show(PanelKind::Outline)));
    (tmp, app)
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
            .map(|p| (p.ty, p.value.clone()))
            .expect(k)
    };
    assert_eq!(
        get("tags"),
        (
            Some(PropType::Tags),
            Rendered::Chips(vec!["proj".into(), "x".into()])
        )
    );
    assert_eq!(
        get("created"),
        (Some(PropType::Date), Rendered::Text("2026-01-02".into()))
    );
    assert_eq!(
        get("read"),
        (Some(PropType::Checkbox), Rendered::Check(true))
    );
    assert_eq!(
        get("word-count"),
        (Some(PropType::Number), Rendered::Flagged("lots".into()))
    );
    assert_eq!(
        get("topics"),
        (
            Some(PropType::Multitext),
            Rendered::Chips(vec!["one".into()])
        )
    );
    assert_eq!(
        get("mood"),
        (None, Rendered::Chips(vec!["calm".into(), "warm".into()]))
    );
    assert_eq!(get("related").1, Rendered::Text("[[b]]".into()));
    assert_eq!(Rendered::Check(false).display(), "☐");

    // Unsaved edits show up on the next tick; the outline click moves the cursor only.
    let _ = app.update(Message::Source(SourceMsg::Action(
        iced::widget::text_editor::Action::Edit(iced::widget::text_editor::Edit::Paste(Arc::new(
            "# New top\n".into(),
        ))),
    )));
    let _ = app.update(Message::Panels(PanelsMsg::Tick));
    assert_eq!(app.panels.outline[0].text, "New top");
    let _ = app.update(Message::Panels(PanelsMsg::OutlineJump(10)));
    assert_eq!(app.active_doc().expect("doc").cursor().position.line, 10);
    let tab = app.active_tab().expect("tab");
    assert_eq!(tab.path, "a.md");
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
    let _ = app.update(Message::Panels(PanelsMsg::TagPick("alpha".into())));
    assert_eq!(
        app.panels.tag_files,
        Some(("alpha".into(), vec!["a.md".into(), "sub/c.md".into()]))
    );
    let _ = app.update(Message::Panels(PanelsMsg::Open("sub/c.md".into())));
    assert_eq!(app.active_tab().map(|t| t.path.as_str()), Some("sub/c.md"));

    let _ = app.update(Message::Panels(PanelsMsg::SearchInput("zebra".into())));
    let seq = app.panels.search.seq;
    let _ = app.update(Message::Panels(PanelsMsg::SearchInput("zebraw".into())));
    let _ = app.update(Message::Panels(PanelsMsg::SearchDue(seq)));
    assert!(
        app.panels.search.results.is_empty(),
        "stale debounce ignored"
    );
    let _ = app.update(Message::Panels(PanelsMsg::SearchDue(seq + 1)));
    assert_eq!(
        app.panels.search.results,
        vec![SearchHit {
            path: "b.md".into(),
            snippet: "see [[a]] here zebraword".into(),
        }]
    );
    let _ = app.update(Message::Panels(PanelsMsg::SearchInput(
        "\"unbalanced (".into(),
    )));
    let s = app.panels.search.seq;
    let _ = app.update(Message::Panels(PanelsMsg::SearchDue(s)));
    assert!(
        app.panels.search.error.is_none(),
        "{:?}",
        app.panels.search.error
    );
}

#[test]
fn pure_helpers() {
    assert!(is_externalish("https://x.y/z"));
    assert!(is_externalish("assets/scan.PDF"));
    assert!(!is_externalish("notes/missing"));
    assert!(!is_externalish("v1.2 release"));
    assert!(!is_externalish("Note.md"));
    assert_eq!(fts_query("  "), None);
    assert_eq!(fts_query("a \"b"), Some("\"a\" \"\"\"b\"*".into()));
    let long = "x".repeat(200);
    assert_eq!(snippet(&long).chars().count(), SNIPPET_CHARS + 1);
    assert_eq!(props::TYPES.len(), 34);
    assert_eq!(
        props::render("TQ_show_tree", &JsValue::Str("yes".into())).value,
        Rendered::Flagged("yes".into())
    );
    assert_eq!(
        props::render("created", &JsValue::Str("2026-01-02T10:00".into())).value,
        Rendered::Text("2026-01-02T10:00".into())
    );
    assert_eq!(
        props::render("review-after-lines", &JsValue::Num(12.0)).value,
        Rendered::Text("12".into())
    );
}

/// editor-usability-round WP-5 measurement harness (not a gate): Toggle Panels latency against a
/// real `cv-index` index of the synthetic spike vault in `CV_SPIKE_VAULT`
/// (`runs/spike/panels-lag/gen.py`). Run:
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
    app.rpc_inline = true;
    let _ = app.update(Message::Tabs(TabsMsg::Open(rel)));
    for round in 0..3 {
        // Each round: a new index generation (as after any save), then Toggle Panels open.
        app.status.generation += 1;
        let t = std::time::Instant::now();
        let _ = app.update(Message::Panels(PanelsMsg::Toggle));
        let open = t.elapsed();
        let t = std::time::Instant::now();
        let _ = app.update(Message::Panels(PanelsMsg::Tick));
        let tick = t.elapsed();
        println!(
            "round {round}: toggle-open {:.1} ms, next tick {:.1} ms",
            open.as_secs_f64() * 1e3,
            tick.as_secs_f64() * 1e3
        );
        let _ = app.update(Message::Panels(PanelsMsg::Toggle));
    }
}
