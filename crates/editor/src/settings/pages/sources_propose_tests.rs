//! Plan sources-publication-model WP-7 tests: the editor's mapping of a `propose_source` answer
//! onto the Sources page, over synthetic TOML.

use cv_protocol::{ProposeSourceResult, ProposedSource, RpcError, SourceMatchers, error_codes};
use cv_schema::Path;

use super::sources::{KINDS_PATH, SOURCES_PATH};
use super::sources_propose::*;
use crate::daemon::{ContentHash, DaemonError};
use crate::settings::{ScalarValue, Section, Settings};

fn model(sources: &str) -> Settings {
    let mut st = Settings::default();
    for (p, t) in [(SOURCES_PATH, sources), (KINDS_PATH, "[kinds.blog]\n")] {
        st.file_mut(p)
            .expect("file")
            .apply_read(Ok((t.as_bytes().to_vec(), ContentHash::from_hex("00"))));
    }
    st
}

fn text(st: &Settings) -> String {
    st.file(SOURCES_PATH)
        .and_then(|f| f.doc())
        .expect("doc")
        .to_string()
}

#[test]
fn proposal_maps_to_new_record_fields_and_appends_unsaved() {
    let record = ProposedSource {
        name: "Site".into(),
        kind: Some("blog".into()),
        matchers: SourceMatchers {
            hosts: vec!["site.example".into()],
            prefix: vec!["https://site.example/a/".into()],
            ..SourceMatchers::default()
        },
    };
    assert_eq!(
        proposal_fields(&record),
        vec![
            ("name".to_owned(), ScalarValue::Str("Site".into())),
            ("kind".to_owned(), ScalarValue::Str("blog".into())),
            (
                "hosts".to_owned(),
                ScalarValue::StrList(vec!["site.example".into()])
            ),
            (
                "prefix".to_owned(),
                ScalarValue::StrList(vec!["https://site.example/a/".into()])
            ),
        ]
    );
    let mut st = model("[[publication]]\nname = \"other\"\n");
    let r = ProposeSourceResult::Proposed {
        record,
        notes: vec!["n1".into()],
    };
    assert_eq!(st.apply_proposal(r), Ok(1));
    assert_eq!(st.section, Section::Sources);
    assert!(
        st.form
            .is_expanded(SOURCES_PATH, &Path::root().key("publication").index(1))
    );
    assert!(
        text(&st).contains("hosts = [\"site.example\"]"),
        "{}",
        text(&st)
    );
    assert_eq!(
        st.sources.from_note.as_ref().map(|f| f.notes.clone()),
        Some(vec!["n1".into()])
    );
}

#[test]
fn existing_expands_without_appending_and_offer_applies_on_accept() {
    let src = "[[publication]]\nname = \"Weekly\"\nfrom = [\"old@news.example\"]\n";
    let mut st = model(src);
    let r = ProposeSourceResult::Existing {
        name: "Weekly".into(),
        missing: SourceMatchers {
            from: vec!["new@news.example".into()],
            hosts: vec!["news.example".into()],
            ..SourceMatchers::default()
        },
        notes: vec![],
    };
    assert_eq!(st.apply_proposal(r), Ok(0));
    assert_eq!(
        text(&st),
        src,
        "nothing appended or changed before the accept"
    );
    assert!(
        st.form
            .is_expanded(SOURCES_PATH, &Path::root().key("publication").index(0))
    );
    st.apply_offer();
    let t = text(&st);
    assert!(
        t.contains("from = [\"old@news.example\", \"new@news.example\"]"),
        "{t}"
    );
    assert!(t.contains("hosts = [\"news.example\"]"), "{t}");
    assert_eq!(
        st.sources.from_note.as_ref().and_then(|f| f.offer.clone()),
        None
    );
    // A name the file does not hold is refused, never appended.
    let r = ProposeSourceResult::Existing {
        name: "Folded".into(),
        missing: SourceMatchers::default(),
        notes: vec![],
    };
    assert!(st.apply_proposal(r).is_err());
    assert_eq!(text(&st).matches("[[publication]]").count(), 1);
}

#[test]
fn unknown_request_error_reads_as_restart_the_daemon() {
    let e = DaemonError::Rpc(RpcError {
        code: error_codes::METHOD_NOT_FOUND,
        message: "unknown method".into(),
        data: None,
    });
    assert!(
        propose_error(&e)
            .contains("the running daemon does not support this — restart it after installing"),
        "{}",
        propose_error(&e)
    );
    let nf = DaemonError::Rpc(RpcError {
        code: error_codes::NOT_FOUND,
        message: "not found: x.md".into(),
        data: None,
    });
    assert!(!propose_error(&nf).contains("restart"));
}
