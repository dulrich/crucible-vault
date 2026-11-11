use super::*;

fn ok(_: &Step, _: &ConfigSet) -> Result<(), String> {
    Ok(())
}

fn files(actions: &str) -> BTreeMap<String, Option<String>> {
    BTreeMap::from([("actions.toml".to_owned(), Some(actions.to_owned()))])
}

const GOOD: &str = r#"
[[action]]
id = "a"
label = "A"
durable = false
[[action.params]]
name = "title"
label = "Title"
kind = "text"
required = true
[[action.steps]]
type = "set_props"

[[action]]
id = "b"
[[action.steps]]
type = "guard"
"#;

/// Plan round-20-transcript-heading-auto-holds WP-2: `hold` parses; a non-bool is an error.
#[test]
fn hold_key_parses_and_rejects_non_bool() {
    let mut c = ConfigSet::new();
    c.reload(
        &files("[[action]]\nid = \"h\"\nhold = true\n[[action.steps]]\ntype = \"guard\"\n"),
        &ok,
    );
    assert!(c.armed("h").unwrap().hold);
    c.reload(
        &files("[[action]]\nid = \"h\"\nhold = \"yes\"\n[[action.steps]]\ntype = \"guard\"\n"),
        &ok,
    );
    assert_eq!(c.errors().len(), 1, "{:?}", c.errors());
}

/// Plan tn-review-2026-11-04-remediation Decision 6: `hold = true` with `resources.note =
/// false` is a load error (the opt-in would silently do nothing).
#[test]
fn hold_without_resources_note_is_rejected() {
    let mut c = ConfigSet::new();
    c.reload(
        &files(
            "[[action]]\nid = \"h\"\nhold = true\nresources = { note = false, keys = [\"k::x\"] }\n[[action.steps]]\ntype = \"guard\"\n",
        ),
        &ok,
    );
    assert!(c.armed("h").is_none());
    assert_eq!(c.errors().len(), 1, "{:?}", c.errors());
    assert!(
        c.errors()[0].contains("hold = true needs resources.note"),
        "{:?}",
        c.errors()
    );
}

/// Plan job-slots-fast-lane WP-1: `fast` parses (default false), a non-bool is an error.
#[test]
fn fast_key_parses_and_rejects_non_bool() {
    let mut c = ConfigSet::new();
    c.reload(
        &files("[[action]]\nid = \"f\"\nfast = true\n[[action.steps]]\ntype = \"guard\"\n"),
        &ok,
    );
    assert!(c.armed("f").unwrap().fast);
    c.reload(
        &files("[[action]]\nid = \"f\"\n[[action.steps]]\ntype = \"guard\"\n"),
        &ok,
    );
    assert!(!c.armed("f").unwrap().fast);
    c.reload(
        &files("[[action]]\nid = \"f\"\nfast = \"yes\"\n[[action.steps]]\ntype = \"guard\"\n"),
        &ok,
    );
    assert_eq!(c.errors().len(), 1, "{:?}", c.errors());
}

/// Plan job-slots-fast-lane WP-1 (audit E2): `fast` rejects inline model-calling steps and
/// accepts a `run_action` child enqueue.
#[test]
fn fast_rejects_model_calling_steps_but_accepts_run_action() {
    for kind in [
        "agent",
        "intake_enrich",
        "transcript_refine",
        "image_describe",
    ] {
        let mut c = ConfigSet::new();
        let src = format!(
            "[[action]]\nid = \"f\"\nfast = true\n[[action.steps]]\ntype = \"guard\"\n[[action.steps]]\ntype = \"{kind}\"\n"
        );
        c.reload(&files(&src), &ok);
        assert!(c.armed("f").is_none(), "{kind} armed");
        assert_eq!(c.errors().len(), 1, "{kind}: {:?}", c.errors());
        assert!(c.errors()[0].contains(kind), "{:?}", c.errors());
    }
    let mut c = ConfigSet::new();
    c.reload(
        &files("[[action]]\nid = \"f\"\nfast = true\n[[action.steps]]\ntype = \"run_action\"\naction = \"x\"\n"),
        &ok,
    );
    assert!(c.armed("f").unwrap().fast, "{:?}", c.errors());
}

#[test]
fn good_then_bad_keeps_old_actions_and_reports() {
    let mut c = ConfigSet::new();
    c.reload(&files(GOOD), &ok);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    assert_eq!(c.armed_actions().count(), 2);
    let a = c.armed("a").unwrap();
    assert_eq!(a.params[0].kind, ParamKind::Text);
    assert!(!a.durable);
    assert!(!a.hold);
    assert_eq!(a.starters, vec![Starter::Command]);
    c.reload(&files("[[action]\nid = "), &ok);
    let errs = c.errors();
    assert_eq!(errs.len(), 1);
    assert!(errs[0].starts_with("actions.toml:"), "{errs:?}");
    assert_eq!(c.armed_actions().count(), 2, "last valid version kept");
    // A parseable file with one invalid entry disarms exactly that action.
    c.reload(
        &files(&format!("{GOOD}\n[[action]]\nid = \"c\"\nbogus = 1\n")),
        &ok,
    );
    assert!(c.armed("c").is_none());
    assert!(c.is_known("c"));
    assert!(c.disarm_reason("c").unwrap().contains("bogus"));
    assert!(c.armed("a").is_some());
    // Missing file → empty.
    c.reload(&BTreeMap::new(), &ok);
    assert_eq!(c.armed_actions().count(), 0);
    assert!(c.errors().is_empty());
}

#[test]
fn empty_selector_and_bad_steps_disarm() {
    let src = r#"
[[action]]
id = "storm"
[[action.steps]]
type = "lint"
[[action.starters]]
type = "event"
kinds = ["created"]
[action.starters.selector]

[[action]]
id = "fine"
[[action.steps]]
type = "lint"
[[action.starters]]
type = "event"
kinds = ["modified"]
origin = "any"
selector = { folder = "inbox" }

[[action]]
id = "badstep"
[[action.steps]]
type = "agent"
"#;
    let mut c = ConfigSet::new();
    let v = |s: &Step, _: &ConfigSet| {
        if s.kind() == crate::StepKind::Agent {
            Err("no agents configured".to_owned())
        } else {
            Ok(())
        }
    };
    c.reload(&files(src), &v);
    assert!(c.disarm_reason("storm").unwrap().contains("empty selector"));
    assert!(c.disarm_reason("badstep").unwrap().contains("no agents"));
    assert!(c.armed("fine").is_some());
    assert_eq!(c.errors().len(), 2);
}

#[test]
fn needs_parse_typed_and_unknown_need_disarms() {
    let src = r#"
[[action]]
id = "typed"
needs = ["active_path", "line"]
[[action.steps]]
type = "lint"

[[action]]
id = "bogus"
needs = ["active_path", "cursor"]
[[action.steps]]
type = "lint"
"#;
    let mut c = ConfigSet::new();
    c.reload(&files(src), &ok);
    assert_eq!(
        c.armed("typed").unwrap().needs,
        vec![ContextNeed::ActivePath, ContextNeed::Line]
    );
    assert!(c.armed("bogus").is_none());
    assert_eq!(c.disarm_reason("bogus"), Some("unknown need \"cursor\""));
}

#[test]
fn schedules_attach_and_raw_tables_load() {
    let mut f = files(GOOD);
    f.insert(
        "schedules.toml".into(),
        Some("[[schedule]]\naction = \"b\"\nevery = \"30m\"\n[[schedule]]\naction = \"zz\"\nevery = \"1h\"\n".into()),
    );
    f.insert("kinds.toml".into(), Some("[book]\nfolder = \"x\"\n".into()));
    let mut c = ConfigSet::new();
    c.reload(&f, &ok);
    assert!(
        c.armed("b")
            .unwrap()
            .starters
            .contains(&Starter::Schedule(Schedule::Every {
                period_ms: 1_800_000
            }))
    );
    assert_eq!(c.errors().len(), 1, "unknown action reported");
    assert!(c.table("kinds.toml").contains_key("book"));
    assert!(c.table("agents.toml").is_empty());
}

#[test]
fn typed_pipeline_files_hot_reload_and_keep_last_valid() {
    let mut c = ConfigSet::new();
    assert!(!c.pipelines().youtube.enabled, "absent = off");
    let mut f = BTreeMap::new();
    f.insert(
        "pipelines.toml".to_owned(),
        Some("[youtube]\nenabled = true\n".to_owned()),
    );
    f.insert(
        "sources.toml".to_owned(),
        Some("[[youtube]]\nchannel = \"C\"\nid = \"UC1\"\n".to_owned()),
    );
    c.reload(&f, &ok);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    assert!(c.pipelines().youtube.enabled);
    assert_eq!(c.sources().youtube.len(), 1);
    // Type error: last valid kept, reported.
    f.insert(
        "pipelines.toml".to_owned(),
        Some("[youtube]\nenabled = \"yes\"\n".to_owned()),
    );
    c.reload(&f, &ok);
    assert!(c.pipelines().youtube.enabled);
    assert!(c.errors().iter().any(|e| e.starts_with("pipelines.toml:")));
    // Removed files: everything off, errors cleared.
    c.reload(&BTreeMap::new(), &ok);
    assert!(!c.pipelines().youtube.enabled);
    assert!(c.sources().youtube.is_empty());
    assert!(c.errors().is_empty());
}

/// Plan tidiness-trash-orphans WP-1: `vault.toml` absent = defaults; invalid keeps the
/// last valid value and reports.
#[test]
fn vault_toml_absent_valid_invalid() {
    use crate::vault::TrashLocation;
    let mut c = ConfigSet::new();
    assert_eq!(c.vault().trash.location, TrashLocation::Vault);
    let mut f = BTreeMap::new();
    f.insert(
        "vault.toml".to_owned(),
        Some("[trash]\nlocation = \"system\"\nretention_days = 7\n".to_owned()),
    );
    c.reload(&f, &ok);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    assert_eq!(c.vault().trash.location, TrashLocation::System);
    f.insert(
        "vault.toml".to_owned(),
        Some("[trash]\nlocation = \"bin\"\n".to_owned()),
    );
    c.reload(&f, &ok);
    assert_eq!(c.vault().trash.location, TrashLocation::System);
    assert_eq!(c.vault().trash.retention_days, 7);
    assert!(c.errors().iter().any(|e| e.starts_with("vault.toml:")));
    c.reload(&BTreeMap::new(), &ok);
    assert_eq!(c.vault().trash.location, TrashLocation::Vault);
    assert!(c.errors().is_empty());
}

/// Plan public-release WP-2 (C#7, C#9, C#1): empty config = the neutral defaults; the shared
/// exclude list derives from the app folder and the configured metadata roots; an explicit
/// list (the owner's five folders) is used as stated.
#[test]
fn structure_defaults_and_shared_exclude_list() {
    let mut c = ConfigSet::new();
    c.reload(&BTreeMap::new(), &ok);
    assert_eq!(c.app_folder(), "_crucible");
    assert_eq!(
        c.vault().structure.raw_transcript_folder(),
        "_raw_transcript"
    );
    assert_eq!(c.day_folder(), "daily/day");
    assert_eq!(
        c.exclude_folders(),
        [
            ".trash",
            "_custom",
            "_crucible",
            "_yt_metadata",
            "_x_metadata"
        ]
    );
    let f = |pairs: &[(&str, &str)]| -> BTreeMap<String, Option<String>> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), Some((*v).to_owned())))
            .collect()
    };
    c.reload(
        &f(&[
            ("vault.toml", "[structure]\napp_folder = \"/_app/\"\n"),
            ("pipelines.toml", "[youtube]\nmetadata_root = \"_yt\"\n"),
            ("captures.toml", "[periods.day]\nfolder = \"journal/\"\n"),
        ]),
        &ok,
    );
    assert_eq!(c.app_folder(), "_app");
    assert_eq!(c.day_folder(), "journal");
    assert_eq!(
        c.exclude_folders(),
        [".trash", "_custom", "_app", "_yt", "_x_metadata"]
    );
    c.reload(
        &f(&[(
            "vault.toml",
            "[structure]\nexclude = [\".trash\", \"initiatives\", \"_yt_metadata\", \"_custom\", \"_crucible\", \"initiatives/\"]\n",
        )]),
        &ok,
    );
    assert_eq!(
        c.exclude_folders(),
        [
            ".trash",
            "initiatives",
            "_yt_metadata",
            "_custom",
            "_crucible"
        ]
    );
    assert!(c.errors().is_empty(), "{:?}", c.errors());
}
