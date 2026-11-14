//! Plan sources-publication-model WP-6 tests: the Sources overview's pure model (grouping,
//! badges, In-Works resolution, toggles, apply prompt) over synthetic TOML.

use cv_schema::{ConfigDoc, Path};

use super::sources::*;
use crate::daemon::ContentHash;
use crate::settings::{DocEdit, ScalarValue, Section, Settings};

fn doc(t: &str) -> ConfigDoc {
    ConfigDoc::parse(t).expect("toml")
}

fn model(sources: &str, kinds: &str) -> Settings {
    let mut st = Settings::default();
    for (p, t) in [(SOURCES_PATH, sources), (KINDS_PATH, kinds)] {
        st.file_mut(p)
            .expect("file")
            .apply_read(Ok((t.as_bytes().to_vec(), ContentHash::from_hex("00"))));
    }
    st
}

fn text(st: &Settings, p: &str) -> String {
    st.file(p).and_then(|f| f.doc()).expect("doc").to_string()
}

const KINDS: &str =
    "[kinds.book]\nworks = true\n\n[kinds.talk]\nworks = false\n\n[kinds.plain]\nfolder = \"x\"\n";

#[test]
fn groups_order_by_kind_then_name_and_no_kind_last() {
    let s = doc(
        "[[publication]]\nname = \"zed\"\nkind = \"book\"\n\n[[publication]]\nname = \"orphan\"\n\n[[publication]]\nname = \"Alpha\"\nkind = \"book\"\n\n[[publication]]\nname = \"m\"\nkind = \"Article\"\n",
    );
    let g = overview(&s, None);
    let titles: Vec<&str> = g.iter().map(Group::title).collect();
    assert_eq!(titles, ["Article", "book", NO_KIND]);
    let names: Vec<&str> = g[1].members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Alpha", "zed"]);
    assert_eq!(g[2].default, None);
}

#[test]
fn badges_name_what_feeds_each_record() {
    let s = doc(
        "[[publication]]\nname = \"a\"\nfeeds = [\"u\"]\nfrom = [\"@x.example\"]\nhosts = [\"h\"]\n\n[[publication]]\nname = \"b\"\nlist_id = [\"l\"]\nprefix = [\"p\"]\nchannel_id = \"UC1\"\n\n[[publication]]\nname = \"c\"\nauthor = \"Someone\"\n\n[[publication]]\nname = \"d\"\nfeeds = []\n",
    );
    assert_eq!(badges(&s, 0), ["feed", "mail", "host"]);
    assert_eq!(badges(&s, 1), ["mail", "prefix", "channel"]);
    assert_eq!(badges(&s, 2), ["author"]);
    assert!(badges(&s, 3).is_empty());
}

/// Pins the truth table of `cv_actions::config::resolve_in_works`.
#[test]
fn in_works_resolves_record_then_kind_then_off() {
    let k = doc(KINDS);
    let s = doc(
        "[[publication]]\nname = \"1\"\nkind = \"book\"\nworks = false\n\n[[publication]]\nname = \"2\"\nkind = \"talk\"\nworks = true\n\n[[publication]]\nname = \"3\"\nkind = \"book\"\n\n[[publication]]\nname = \"4\"\nkind = \"talk\"\n\n[[publication]]\nname = \"5\"\nkind = \"plain\"\n\n[[publication]]\nname = \"6\"\nkind = \"ghost\"\n\n[[publication]]\nname = \"7\"\n\n[[publication]]\nname = \"8\"\nworks = true\n\n[[publication]]\nname = \"9\"\nworks = \"yes\"\n",
    );
    let got: Vec<Flag> = (0..9).map(|i| resolve_in_works(&s, Some(&k), i)).collect();
    use Flag::{Invalid, Off, On};
    assert_eq!(got, [Off, On, On, Off, Off, Off, Off, On, Invalid]);
    assert_eq!(resolve_in_works(&s, None, 2), Off);
}

#[test]
fn record_toggle_removes_override_equal_to_kind_default_else_sets() {
    let k = doc(KINDS);
    let s = doc(
        "[[publication]]\nname = \"a\"\nkind = \"book\"\nworks = false\n\n[[publication]]\nname = \"b\"\nkind = \"book\"\n\n[[publication]]\nname = \"c\"\n\n[[publication]]\nname = \"d\"\nworks = \"x\"\n",
    );
    let w = |i: usize| Path::root().key("publication").index(i).key("works");
    // Off with an override, kind default on: turning on equals the default -> Remove.
    assert!(matches!(record_toggle_edit(&s, Some(&k), 0), Some(DocEdit::Remove(p)) if p == w(0)));
    // On by inheritance: turning off differs from the default -> explicit Set false.
    assert!(matches!(
        record_toggle_edit(&s, Some(&k), 1),
        Some(DocEdit::Set(p, ScalarValue::Bool(false))) if p == w(1)
    ));
    // No kind: default off, so turning on is an explicit Set true.
    assert!(matches!(
        record_toggle_edit(&s, Some(&k), 2),
        Some(DocEdit::Set(p, ScalarValue::Bool(true))) if p == w(2)
    ));
    // A non-boolean has no toggle.
    assert!(record_toggle_edit(&s, Some(&k), 3).is_none());
}

#[test]
fn kind_toggle_edits_kinds_toml_only() {
    let mut st = model("[[publication]]\nname = \"a\"\nkind = \"talk\"\n", KINDS);
    st.sources_msg(SourcesMsg::KindDefault {
        kind: "talk".into(),
        on: true,
    });
    assert!(text(&st, KINDS_PATH).contains("works = true"));
    assert!(!st.file(SOURCES_PATH).expect("f").dirty());
    assert!(st.file(KINDS_PATH).expect("f").dirty());
}

#[test]
fn undefined_kind_has_no_default_toggle_and_is_never_created() {
    let k = doc(KINDS);
    let s = doc("[[publication]]\nname = \"a\"\nkind = \"ghost\"\n");
    assert_eq!(
        overview(&s, Some(&k))[0].default,
        Some(KindDefault::Undefined)
    );
    assert_eq!(overview(&s, None)[0].default, Some(KindDefault::Undefined));
    let mut st = model("[[publication]]\nname = \"a\"\nkind = \"ghost\"\n", KINDS);
    st.sources_msg(SourcesMsg::KindDefault {
        kind: "ghost".into(),
        on: true,
    });
    assert!(!text(&st, KINDS_PATH).contains("ghost"));
    assert!(st.sources.prompt.is_none());
}

const OVERRIDES: &str = "[[publication]]\nname = \"a\"\nkind = \"talk\"\nworks = true\n\n[[publication]]\nname = \"b\"\nkind = \"talk\"\nworks = false\n\n[[publication]]\nname = \"c\"\nkind = \"talk\"\n\n[[publication]]\nname = \"d\"\nkind = \"book\"\nworks = false\n";

#[test]
fn apply_prompt_counts_only_overrides_that_differ_from_the_new_default() {
    let mut st = model(OVERRIDES, KINDS);
    // talk default false -> true: only a's explicit true agrees; b's explicit false differs.
    st.sources_msg(SourcesMsg::KindDefault {
        kind: "talk".into(),
        on: true,
    });
    assert_eq!(st.sources.prompt.as_deref(), Some("talk"));
    let (s, k) = (
        st.file(SOURCES_PATH).and_then(|f| f.doc()).expect("s"),
        st.file(KINDS_PATH).and_then(|f| f.doc()),
    );
    assert_eq!(pending_overrides(s, k, "talk"), [1]);
    // Another kind default replaces the prompt; no differing override -> none.
    st.sources_msg(SourcesMsg::KindDefault {
        kind: "book".into(),
        on: false,
    });
    assert!(st.sources.prompt.is_none());
}

#[test]
fn apply_clears_exactly_the_differing_overrides_and_keep_clears_none() {
    let mut st = model(OVERRIDES, KINDS);
    st.sources_msg(SourcesMsg::KindDefault {
        kind: "talk".into(),
        on: true,
    });
    let before = text(&st, SOURCES_PATH);
    st.sources_msg(SourcesMsg::Keep);
    assert!(st.sources.prompt.is_none());
    assert_eq!(text(&st, SOURCES_PATH), before);
    st.sources_msg(SourcesMsg::Apply("talk".into()));
    let after = text(&st, SOURCES_PATH);
    assert_eq!(
        after.matches("works").count(),
        before.matches("works").count() - 1
    );
    assert!(after.contains("works = true"), "agreeing override stays");
    assert!(
        after.contains("kind = \"book\"\nworks = false"),
        "other kinds untouched"
    );
    assert!(!after.contains("name = \"b\"\nkind = \"talk\"\nworks"));
}

#[test]
fn legacy_file_gets_banner_shape_and_absent_file_stays_generic() {
    let st = model("[[blog]]\nname = \"x\"\nlink = \"l\"\n", "");
    assert_eq!(
        page_shape(st.file(SOURCES_PATH).expect("f")),
        PageShape::Legacy
    );
    assert!(LEGACY_BANNER.contains("cv sources migrate") && LEGACY_BANNER.contains("--dry-run"));
    let st = model("[[publication]]\nname = \"x\"\n", "");
    assert_eq!(
        page_shape(st.file(SOURCES_PATH).expect("f")),
        PageShape::Overview
    );
    let st = model("not = [valid", "");
    assert_eq!(
        page_shape(st.file(SOURCES_PATH).expect("f")),
        PageShape::Generic
    );
    assert_eq!(
        page_shape(Settings::default().file(SOURCES_PATH).expect("f")),
        PageShape::Generic
    );
}

#[test]
fn open_publication_expands_existing_and_appends_missing() {
    let mut st = model("[[publication]]\nname = \"a\"\n", KINDS);
    let ip = |i: usize| Path::root().key("publication").index(i);
    assert_eq!(st.open_publication("a", Vec::new()), Ok(0));
    assert_eq!(st.section, Section::Sources);
    assert!(st.form.is_expanded(SOURCES_PATH, &ip(0)));
    let new = vec![("author".to_owned(), ScalarValue::Str("Me".into()))];
    assert_eq!(st.open_publication("fresh", new), Ok(1));
    assert!(st.form.is_expanded(SOURCES_PATH, &ip(1)));
    assert!(text(&st, SOURCES_PATH).contains("name = \"fresh\""));
    // An edit of the same record keeps it expanded.
    let _ = st.edit(
        SOURCES_PATH,
        &DocEdit::Set(ip(1).key("short"), ScalarValue::Str("f".into())),
    );
    assert!(st.form.is_expanded(SOURCES_PATH, &ip(1)));
}
