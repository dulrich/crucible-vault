//! `capture` step tests (was `wp10_tests.rs`). Synthetic notes in a temp
//! vault, the example config from `config/examples/`, steps driven
//! through `steps::validate`/`steps::execute` via [`super::testkit`].

use cv_protocol::ActionContext;
use serde_json::{Map, json};

use super::StepError;
use super::testkit::{active, example_env, note_gone, today};

// ---- capture ---------------------------------------------------------------------------------

#[test]
fn capture_daylog_materializes_daily_and_rerun_does_not_duplicate() {
    let env = example_env(&[]);
    let daily = format!("daily/day/{}.md", today());
    let step = "type = \"capture\"\ncapture = \"daylog\"\nformat = \"- {{value}}\"";
    let o = env
        .run(step, json!({"value": "hello"}), ActionContext::default())
        .unwrap();
    assert_eq!(o.output_paths, vec![daily.clone()]);
    let want = format!("# {}\n\n# Todos\n\n# Daylogs\n- hello\n", today());
    assert_eq!(env.read(&daily).unwrap(), want);
    let o = env
        .run(step, json!({"value": "hello"}), ActionContext::default())
        .unwrap();
    assert!(o.output_paths.is_empty(), "E2 re-run converges");
    assert_eq!(env.read(&daily).unwrap(), want);
    env.run(step, json!({"value": "second"}), ActionContext::default())
        .unwrap();
    assert!(env.read(&daily).unwrap().ends_with("- hello\n- second\n"));
    // The preset's own `{{time}}` format renders too.
    env.run(
        "type = \"capture\"\ncapture = \"daylog\"",
        json!({"value": "t"}),
        ActionContext::default(),
    )
    .unwrap();
    let text = env.read(&daily).unwrap();
    let last = text.lines().last().unwrap();
    assert!(last.starts_with("- [") && last.ends_with("] t"), "{last}");
}

#[test]
fn capture_whole_note_modes_and_targets() {
    let env = example_env(&[
        ("a.md", "---\ntitle: a\n---\n\nbody\n"),
        ("b.md", "first\n"),
    ]);
    // prepend into the active note keeps frontmatter on top.
    let pre = "type = \"capture\"\ntarget = \"active\"\nmode = \"prepend\"";
    env.run(pre, json!({"value": "top"}), active("a.md"))
        .unwrap();
    assert_eq!(
        env.read("a.md").unwrap(),
        "---\ntitle: a\n---\n\ntop\n\nbody\n"
    );
    let o = env
        .run(pre, json!({"value": "top"}), active("a.md"))
        .unwrap();
    assert!(o.output_paths.is_empty(), "prepend re-run converges");
    // append into a path target (value from the context).
    let app = "type = \"capture\"\ntarget = \"path\"\npath = \"{param.p}\"\nmode = \"append\"";
    let ctx = ActionContext {
        value: Some("tail".into()),
        ..ActionContext::default()
    };
    env.run(app, json!({"p": "b.md"}), ctx.clone()).unwrap();
    assert_eq!(env.read("b.md").unwrap(), "first\n\ntail");
    assert!(
        env.run(app, json!({"p": "b.md"}), ctx)
            .unwrap()
            .output_paths
            .is_empty()
    );
    // section mode into the active note.
    let sec = "type = \"capture\"\ntarget = \"active\"\nmode = \"section\"\nsection = \"## Log\"\nformat = \"- {{value}}\"";
    env.run(sec, json!({"value": "x"}), active("b.md")).unwrap();
    assert_eq!(env.read("b.md").unwrap(), "first\n\ntail\n\n## Log\n\n- x");
    env.run(sec, json!({"value": "y"}), active("b.md")).unwrap();
    assert_eq!(
        env.read("b.md").unwrap(),
        "first\n\ntail\n\n## Log\n\n- x\n- y"
    );
    assert!(
        env.run(sec, json!({"value": "y"}), active("b.md"))
            .unwrap()
            .output_paths
            .is_empty()
    );
    // A missing active/path target fails closed.
    assert_eq!(
        note_gone(env.run(app, json!({"p": "nope.md", "value": "v"}), active("b.md"))),
        "nope.md"
    );
}

/// SR7: capture writes through `modify` — a concurrent edit between its read and CAS write is
/// kept and the capture re-applied; a race on every attempt defers (`Wait`), never fails.
#[test]
fn capture_concurrent_edit_reapplies_and_persistent_race_defers() {
    let env = example_env(&[("b.md", "first\n"), ("c.md", "first\n")]);
    let app = "type = \"capture\"\ntarget = \"active\"\nmode = \"append\"";
    super::race::inject(env.vault(), "b.md", 1, |t| format!("{t}racer\n"));
    let o = env
        .run(app, json!({"value": "tail"}), active("b.md"))
        .unwrap();
    assert_eq!(o.output_paths, vec!["b.md".to_owned()]);
    assert_eq!(env.read("b.md").unwrap(), "first\nracer\n\ntail");
    super::race::inject(env.vault(), "c.md", super::MODIFY_ATTEMPTS, |t| {
        format!("{t}x\n")
    });
    assert_eq!(
        env.run(app, json!({"value": "tail"}), active("c.md")),
        Err(StepError::conflict())
    );
    assert_eq!(env.read("c.md").unwrap(), "first\nx\nx\nx\n");
}

#[test]
fn capture_monthly_observation_uses_selection_and_source_link() {
    let env = example_env(&[("src/Idea.md", "text\n")]);
    let ctx = ActionContext {
        active_path: Some("src/Idea.md".into()),
        selection: Some("a  multi\nline".into()),
        ..ActionContext::default()
    };
    let (_, vars) = env
        .run_action("capture.monthly-observation", json!({}), ctx)
        .unwrap();
    let month = chrono::Local::now().format("%Y-%m").to_string();
    let path = format!("daily/month/{month}.md");
    assert_eq!(vars["captured_path"], json!(path));
    assert_eq!(
        env.read(&path).unwrap(),
        format!("# Month {month}\n\n# Observations\n- [[Idea]]\n\t- a multi line\n")
    );
}

#[test]
fn capture_interpolates_vars_for_agent_output() {
    let env = example_env(&[("_raw_transcript/t.md", "raw\n")]);
    let mut vars = Map::new();
    vars.insert("agent_output".into(), json!("refined text"));
    vars.insert("copied_path".into(), json!("_raw_transcript/t.md"));
    let step = "type = \"capture\"\ntarget = \"path\"\npath = \"{vars.copied_path}\"\nmode = \"append\"\nformat = \"{{vars.agent_output}}\"";
    let o = env
        .run_vars(step, json!({}), ActionContext::default(), &vars)
        .unwrap();
    assert_eq!(o.output_paths, vec!["_raw_transcript/t.md".to_owned()]);
    assert_eq!(
        env.read("_raw_transcript/t.md").unwrap(),
        "raw\n\nrefined text"
    );
    // Non-string vars render as JSON text.
    vars.insert("agent_output".into(), json!({"k": 1}));
    env.run_vars(step, json!({}), ActionContext::default(), &vars)
        .unwrap();
    assert!(
        env.read("_raw_transcript/t.md")
            .unwrap()
            .ends_with("{\"k\":1}")
    );
}

// Vectors from `https://github.com/dulrich/obsidian-crucible/blob/master/tests/captureEmptyBodyGuard.test.mjs`.
#[test]
fn capture_empty_body_guard_vectors() {
    let existing = "---\ntitle: note\n---\n\nOriginal body, must survive untouched.";
    let env = example_env(&[("note.md", existing)]);
    let replace = "type = \"capture\"\ntarget = \"active\"\nmode = \"replace\"";
    // 1. YAML-only value + replace: success, no write, byte-identical.
    let o = env
        .run(
            replace,
            json!({"value": "---\ncaptured: true\n---\n"}),
            active("note.md"),
        )
        .unwrap();
    assert!(o.output_paths.is_empty() && !o.stop);
    assert_eq!(env.read("note.md").unwrap(), existing);
    // 2. A normal replace still writes, keeping the frontmatter.
    let o = env
        .run(
            replace,
            json!({"value": "Fresh replacement body."}),
            active("note.md"),
        )
        .unwrap();
    assert_eq!(o.output_paths.len(), 1);
    assert_eq!(
        env.read("note.md").unwrap(),
        "---\ntitle: note\n---\n\nFresh replacement body."
    );
    // 3. YAML-only + append is not gated by the replace guard: it succeeds (and, a deviation,
    //    writes nothing because the stripped body is empty).
    let append = "type = \"capture\"\ntarget = \"active\"\nmode = \"append\"";
    let o = env
        .run(
            append,
            json!({"value": "---\ncaptured: true\n---\n"}),
            active("note.md"),
        )
        .unwrap();
    assert!(!o.stop);
    // An empty value is a no-op success (Crucible's first guard).
    let o = env
        .run(append, json!({"value": "  "}), active("note.md"))
        .unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(
        env.read("note.md").unwrap(),
        "---\ntitle: note\n---\n\nFresh replacement body."
    );
}
