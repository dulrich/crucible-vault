//! `materialize` step tests (was `wp10_tests.rs`). Synthetic notes in a temp
//! vault, the example config from `config/examples/`, steps driven
//! through `steps::validate`/`steps::execute` via [`super::testkit`].

use cv_protocol::ActionContext;
use serde_json::json;

use super::testkit::{active, example_env, today};

// ---- materialize -----------------------------------------------------------------------------

const DAY_STEP: &str = "type = \"materialize\"\nperiod = \"day\"\ndate = \"2026-10-13\"";

#[test]
fn materialize_creates_from_template_and_rerun_converges() {
    let env = example_env(&[]);
    let o = env
        .run(DAY_STEP, json!({}), ActionContext::default())
        .unwrap();
    let p = "daily/day/2026-10-13.md";
    assert_eq!(o.open_path.as_deref(), Some(p));
    assert_eq!(o.output_paths, vec![p.to_owned()]);
    assert_eq!(o.vars["asset_folder"], json!("daily/day/2026-10-13"));
    assert_eq!(
        env.read(p).unwrap(),
        "# 2026-10-13\n\n# Todos\n\n# Daylogs\n"
    );
    // Re-run (E2): untouched, still opens it.
    let o2 = env
        .run(DAY_STEP, json!({}), ActionContext::default())
        .unwrap();
    assert_eq!(o2.open_path.as_deref(), Some(p));
    assert!(o2.output_paths.is_empty());
    assert_eq!(
        env.read(p).unwrap(),
        "# 2026-10-13\n\n# Todos\n\n# Daylogs\n"
    );
}

#[test]
fn materialize_fills_empty_and_leaves_non_empty() {
    let env = example_env(&[
        ("daily/day/2026-10-13.md", ""),
        ("daily/day/2026-10-14.md", "mine\n"),
    ]);
    let o = env
        .run(DAY_STEP, json!({}), ActionContext::default())
        .unwrap();
    assert_eq!(o.output_paths.len(), 1);
    assert!(
        env.read("daily/day/2026-10-13.md")
            .unwrap()
            .starts_with("# 2026-10-13")
    );
    let o = env
        .run(
            "type = \"materialize\"\nperiod = \"day\"\ndate = \"2026-10-14\"",
            json!({}),
            ActionContext::default(),
        )
        .unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(o.open_path.as_deref(), Some("daily/day/2026-10-14.md"));
    assert_eq!(env.read("daily/day/2026-10-14.md").unwrap(), "mine\n");
}

#[test]
fn materialize_only_if_empty_guard() {
    let env = example_env(&[
        ("daily/day/2026-10-13.md", "written elsewhere\n"),
        ("daily/day/2026-10-15.md", ""),
        ("daily/day/2026-10-15/clip.md", ""),
    ]);
    let step = "type = \"materialize\"\ntarget = \"active\"\nonly_if_empty = true";
    let o = env
        .run(step, json!({}), active("daily/day/2026-10-13.md"))
        .unwrap();
    assert!(o.stop, "non-empty note stops the action");
    assert_eq!(
        env.read("daily/day/2026-10-13.md").unwrap(),
        "written elsewhere\n"
    );
    let o = env
        .run(step, json!({}), active("daily/day/2026-10-15.md"))
        .unwrap();
    assert!(!o.stop);
    assert_eq!(
        env.read("daily/day/2026-10-15.md").unwrap(),
        "# 2026-10-15\n\n# Todos\n\n# Daylogs\n"
    );
    // Re-run on its own output: stops, unchanged.
    assert!(
        env.run(step, json!({}), active("daily/day/2026-10-15.md"))
            .unwrap()
            .stop
    );
    // A note in an asset subfolder is not a period note.
    let o = env
        .run(step, json!({}), active("daily/day/2026-10-15/clip.md"))
        .unwrap();
    assert!(o.stop);
    assert_eq!(env.read("daily/day/2026-10-15/clip.md").unwrap(), "");
}

#[test]
fn period_open_end_to_end_returns_open_path() {
    let env = example_env(&[]);
    let (open, _) = env
        .run_action(
            "period.open",
            json!({"period": "week", "date": "2026-10-13"}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("daily/week/2026-W42.md"));
    assert_eq!(
        env.read("daily/week/2026-W42.md").unwrap(),
        "# Week 2026-W42\n"
    );
    // Default date = today.
    let (open, _) = env
        .run_action(
            "period.open",
            json!({"period": "day", "date": ""}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open, Some(format!("daily/day/{}.md", today())));
    let (open, _) = env
        .run_action(
            "period.open",
            json!({"period": "month", "date": "2026-10-13"}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("daily/month/2026-10.md"));
    // Resource keys name the period note, not the active path.
    let a = env.config.armed("period.open").unwrap();
    let keys = a
        .resources
        .derive(None, json!({"period": "day"}).as_object().unwrap())
        .unwrap();
    assert_eq!(keys, vec!["note::period::day".to_owned()]);
}

#[test]
fn note_create_with_and_without_template() {
    let env = example_env(&[("_custom/plain.md", "title: {{title}}\n")]);
    let (open, _) = env
        .run_action(
            "note.create",
            json!({"path": "notes/fresh"}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("notes/fresh.md"));
    assert_eq!(env.read("notes/fresh.md").unwrap(), "");
    let (open, _) = env
        .run_action(
            "note.create",
            json!({"path": "notes/tpl.md", "template": "_custom/plain.md"}),
            ActionContext::default(),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("notes/tpl.md"));
    assert_eq!(env.read("notes/tpl.md").unwrap(), "title: tpl\n");
}
