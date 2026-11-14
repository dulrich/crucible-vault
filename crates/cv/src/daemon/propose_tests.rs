//! `propose_source` (plan sources-publication-model WP-7): the pure answer over synthetic notes,
//! a synthetic config and a temp Maildir.

use std::collections::BTreeMap;

use cv_actions::ConfigSet;
use cv_protocol::{ProposeSourceResult, SourceMatchers};

use super::propose;

fn config(sources: &str) -> ConfigSet {
    let mut files = BTreeMap::new();
    files.insert("sources.toml".to_owned(), Some(sources.to_owned()));
    files.insert(
        "kinds.toml".to_owned(),
        Some("[kinds.blog]\nlabel = 'Blog'\n[kinds.essay]\nlabel = 'Essay'\n".to_owned()),
    );
    let mut c = ConfigSet::new();
    c.reload(&files, &crate::steps::validate);
    c
}

fn none(_: &str) -> Option<cv_ingest::mail::MailPost> {
    None
}

fn eml(mid: &str, from: &str, list: &str) -> Vec<u8> {
    format!(
        "From: Sender <{from}>\nList-Id: {list}\nSubject: Weekly\nDate: Tue, 14 Oct 2026 10:00:00 +0000\nMessage-ID: <{mid}>\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n\nbody\n"
    )
    .replace('\n', "\r\n")
    .into_bytes()
}

/// A temp Maildir holding one processed message, and the daemon's lookup over it.
fn maildir_lookup(
    dir: &std::path::Path,
) -> impl Fn(&str) -> Option<cv_ingest::mail::MailPost> + '_ {
    move |id: &str| {
        let map = crate::steps::message_map(dir, "cur");
        let f = map.get(id)?;
        cv_ingest::mail::parse_eml(&std::fs::read(f).ok()?)
    }
}

const MAIL_NOTE: &str =
    "---\nvia: email\nmessage-id: m1@news.example\ntags:\n  - blog\n---\nbody\n";

#[test]
fn web_note_proposes_name_kind_host_and_prefix() {
    let c = config("");
    let note = "---\nsource: https://www.site.example/essays/one?x=1\npublication: Site Essays\ntags:\n  - essay\n---\nbody\n";
    let ProposeSourceResult::Proposed { record, notes } = propose(note.as_bytes(), &c, &none)
    else {
        panic!("expected a proposal");
    };
    assert_eq!(record.name, "Site Essays");
    assert_eq!(record.kind.as_deref(), Some("essay"));
    assert_eq!(record.matchers.hosts, ["site.example"]);
    assert_eq!(record.matchers.prefix, ["https://www.site.example/essays/"]);
    assert!(record.matchers.from.is_empty() && record.matchers.list_id.is_empty());
    assert!(notes.iter().any(|n| n.contains("prefix")), "{notes:?}");
    // No `publication`: the host names it, and the note says so.
    let ProposeSourceResult::Proposed { record, notes } =
        propose(b"---\nsource: https://site.example/\n---\n", &c, &none)
    else {
        panic!("expected a proposal");
    };
    assert_eq!(record.name, "site.example");
    assert!(record.matchers.prefix.is_empty());
    assert!(notes.iter().any(|n| n.contains("source host")), "{notes:?}");
}

#[test]
fn email_note_with_its_message_fills_sender_and_list_id() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cur")).unwrap();
    std::fs::write(
        tmp.path().join("cur/1.eml"),
        eml(
            "m1@news.example",
            "Writer@News.Example",
            "<weekly.news.example>",
        ),
    )
    .unwrap();
    let c = config("");
    let lookup = maildir_lookup(tmp.path());
    let ProposeSourceResult::Proposed { record, .. } = propose(MAIL_NOTE.as_bytes(), &c, &lookup)
    else {
        panic!("expected a proposal");
    };
    assert_eq!(record.matchers.from, ["writer@news.example"]);
    assert_eq!(record.matchers.list_id, ["weekly.news.example"]);
    assert_eq!(record.kind.as_deref(), Some("blog"));
}

#[test]
fn email_note_whose_message_is_gone_leaves_sender_empty_and_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let c = config("");
    let lookup = maildir_lookup(tmp.path());
    let ProposeSourceResult::Proposed { record, notes } =
        propose(MAIL_NOTE.as_bytes(), &c, &lookup)
    else {
        panic!("expected a proposal");
    };
    assert_eq!(record.matchers, SourceMatchers::default());
    assert!(
        notes
            .iter()
            .any(|n| n.contains("no longer in the mail directory")),
        "{notes:?}"
    );
}

#[test]
fn existing_match_by_source_host_or_author() {
    let c = config(
        "[[publication]]\nname = 'Site'\nhosts = ['site.example']\n\n[[publication]]\nname = 'Saga'\nauthor = 'Pen Name'\n",
    );
    let r = propose(b"---\nsource: https://site.example/a/b\n---\n", &c, &none);
    assert_eq!(
        r,
        ProposeSourceResult::Existing {
            name: "Site".into(),
            missing: SourceMatchers::default(),
            notes: vec![],
        }
    );
    let r = propose(b"---\nauthor: '[[Pen Name]]'\n---\n", &c, &none);
    assert!(
        matches!(&r, ProposeSourceResult::Existing { name, .. } if name == "Saga"),
        "{r:?}"
    );
}

#[test]
fn existing_match_reports_missing_sender_and_host() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("cur")).unwrap();
    std::fs::write(
        tmp.path().join("cur/1.eml"),
        eml(
            "m1@news.example",
            "new@news.example",
            "<weekly.news.example>",
        ),
    )
    .unwrap();
    // Matched by its List-Id; the sender and the clip host are not on the record yet.
    let c = config("[[publication]]\nname = 'Weekly'\nlist_id = ['weekly.news.example']\n");
    let note =
        "---\nvia: email\nmessage-id: m1@news.example\nsource: https://news.example/p/1\n---\n";
    let lookup = maildir_lookup(tmp.path());
    let ProposeSourceResult::Existing { name, missing, .. } = propose(note.as_bytes(), &c, &lookup)
    else {
        panic!("expected an existing match");
    };
    assert_eq!(name, "Weekly");
    assert_eq!(missing.from, ["new@news.example"]);
    assert_eq!(missing.hosts, ["news.example"]);
    assert!(missing.list_id.is_empty());
}
