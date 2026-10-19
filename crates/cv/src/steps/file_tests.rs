//! `file` step tests: forward task / `mark_forwarded`, guard, move, copy, props, tags (was `wp10_tests.rs`). Synthetic notes in a temp
//! vault, the example config from `config/examples/`, steps driven
//! through `steps::validate`/`steps::execute` via [`super::testkit`].

use cv_actions::Step;
use cv_protocol::ActionContext;
use serde_json::{Map, json};

use super::StepOutcome;
use super::testkit::{active, example_env, fails, today, tomorrow};

// ---- forward task / mark_forwarded -----------------------------------------------------------

#[test]
fn forward_task_captures_into_next_daily_and_marks_forwarded() {
    let src = "# Project\n## Doing\n- [ ] carry me\nplain\n";
    let env = example_env(&[("proj.md", src)]);
    let ctx = ActionContext {
        active_path: Some("proj.md".into()),
        line: Some(2),
        ..ActionContext::default()
    };
    env.run_action("capture.forward-task", json!({}), ctx.clone())
        .unwrap();
    let next = format!("daily/day/{}.md", tomorrow());
    // `## Doing` is not in the target, so the fixed `# Todos` section is used.
    assert_eq!(
        env.read(&next).unwrap(),
        format!("# {}\n\n# Todos\n- [ ] carry me\n\n# Daylogs\n", tomorrow())
    );
    assert_eq!(
        env.read("proj.md").unwrap(),
        "# Project\n## Doing\n- [>] carry me\nplain\n"
    );
    // E2: both steps re-run on their own output converge.
    let cap = Step::from_table(
        "type = \"capture\"\ncapture = \"forward-task\""
            .parse()
            .unwrap(),
    )
    .unwrap();
    let o = env.exec(&cap, &json!({}), &ctx, &Map::new()).unwrap();
    assert!(o.output_paths.is_empty());
    let o = env
        .run("type = \"mark_forwarded\"", json!({}), ctx.clone())
        .unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(
        env.read("proj.md").unwrap(),
        "# Project\n## Doing\n- [>] carry me\nplain\n"
    );
    assert_eq!(env.read(&next).unwrap().matches("carry me").count(), 1);
    // A non-task line fails closed in both steps.
    let bad = ActionContext {
        line: Some(3),
        ..ctx
    };
    assert!(
        fails(env.run("type = \"mark_forwarded\"", json!({}), bad.clone()))
            .contains("not an open task")
    );
    assert!(fails(env.exec(&cap, &json!({}), &bad, &Map::new())).contains("not a task"));
}

#[test]
fn mark_forwarded_open_task_variants() {
    let env = example_env(&[("t.md", "- [>] a\r\n  * [ ] b\r\n")]);
    let ctx = |l| ActionContext {
        active_path: Some("t.md".into()),
        line: Some(l),
        ..ActionContext::default()
    };
    env.run("type = \"mark_forwarded\"", json!({}), ctx(1))
        .unwrap();
    assert_eq!(env.read("t.md").unwrap(), "- [>] a\r\n  * [>] b\r\n");
    assert!(
        env.run("type = \"mark_forwarded\"", json!({}), ctx(0))
            .unwrap()
            .output_paths
            .is_empty()
    );
    assert!(
        fails(env.run("type = \"mark_forwarded\"", json!({}), ctx(9))).contains("out of range")
    );
}

// ---- file steps ------------------------------------------------------------------------------

#[test]
fn guard_stops_without_error() {
    let env = example_env(&[
        ("done.md", "---\ntags: [refined]\n---\nx\n"),
        ("todo.md", "---\ntags: transcript\n---\nx\n"),
    ]);
    let g = "type = \"guard\"\nnot_tag = \"refined\"";
    assert!(env.run(g, json!({}), active("done.md")).unwrap().stop);
    assert!(!env.run(g, json!({}), active("todo.md")).unwrap().stop);
    assert!(env.run(g, json!({}), active("missing.md")).unwrap().stop);
    assert!(
        !env.run(
            "type = \"guard\"\ntag = \"#transcript\"\nfolder = \"\"",
            json!({}),
            active("todo.md")
        )
        .unwrap()
        .stop
    );
    assert!(
        env.run(
            "type = \"guard\"\nfolder = \"inbox\"",
            json!({}),
            active("todo.md")
        )
        .unwrap()
        .stop
    );
    assert!(
        env.run(
            "type = \"guard\"\nprop = \"source\"",
            json!({}),
            active("todo.md")
        )
        .unwrap()
        .stop
    );
    assert!(
        !env.run(
            "type = \"guard\"\nnot_prop = \"source\"",
            json!({}),
            active("todo.md")
        )
        .unwrap()
        .stop
    );
    // A refine run on an already-refined note stops at the guard and writes nothing.
    let before = env.read("done.md");
    let (_, vars) = env
        .run_action(
            "refine-transcript",
            json!({"provider": "local"}),
            active("done.md"),
        )
        .unwrap();
    assert!(vars.is_empty());
    assert_eq!(env.read("done.md"), before);
    assert!(env.read("_raw_transcript/done.md").is_none());
}

#[test]
fn move_rewrites_links_and_rerun_converges() {
    let env = example_env(&[
        ("inbox/x.md", "# X\n"),
        ("ref.md", "see [[inbox/x|the x]]\n"),
    ]);
    let step = "type = \"move\"\nto = \"archive\"";
    let o = env.run(step, json!({}), active("inbox/x.md")).unwrap();
    assert_eq!(o.vars["moved_path"], json!("archive/x.md"));
    assert_eq!(o.open_path.as_deref(), Some("archive/x.md"));
    assert!(o.output_paths.contains(&"ref.md".to_owned()));
    assert_eq!(env.read("ref.md").unwrap(), "see [[archive/x|the x]]\n");
    assert!(env.read("inbox/x.md").is_none());
    // Re-run: source gone, destination present → success.
    let o = env.run(step, json!({}), active("inbox/x.md")).unwrap();
    assert_eq!(o.vars["moved_path"], json!("archive/x.md"));
    assert!(o.output_paths.is_empty());
}

#[test]
fn file_as_kind_tags_moves_and_lints() {
    let env = example_env(&[
        ("inbox/clip.md", "---\ntags: [clip]\n---\nbody\n"),
        ("ref.md", "[[inbox/clip]]\n"),
    ]);
    let (open, vars) = env
        .run_action(
            "file-as-kind",
            json!({"kind": "repo"}),
            active("inbox/clip.md"),
        )
        .unwrap();
    assert_eq!(open.as_deref(), Some("stack/clip.md"));
    assert_eq!(vars["moved_path"], json!("stack/clip.md"));
    let text = env.read("stack/clip.md").unwrap();
    assert!(text.contains("- repo"), "{text}");
    assert!(text.contains(&format!("created: {}", today())), "{text}");
    assert_eq!(env.read("ref.md").unwrap(), "[[stack/clip]]\n");
    // A daily-folder kind lands in today's daily asset folder.
    let env = example_env(&[("inbox/n.md", "news\n")]);
    let (open, _) = env
        .run_action(
            "file-as-kind",
            json!({"kind": "news"}),
            active("inbox/n.md"),
        )
        .unwrap();
    assert_eq!(open, Some(format!("daily/day/{}/n.md", today())));
}

/// plan round-5 WP-3: the example `file-as-kind` moves first, so a destination collision fails
/// with "already exists" and leaves the note (and its tags) untouched.
#[test]
fn file_as_kind_collision_fails_before_tagging() {
    let src = "---\ntags: [clip]\n---\nbody\n";
    let env = example_env(&[("inbox/clip.md", src), ("stack/clip.md", "other\n")]);
    let err = env
        .run_action(
            "file-as-kind",
            json!({"kind": "repo"}),
            active("inbox/clip.md"),
        )
        .expect_err("collision fails");
    assert!(err.to_string().contains("already exists"), "{err}");
    assert_eq!(env.read("inbox/clip.md").as_deref(), Some(src));
    assert_eq!(env.read("stack/clip.md").as_deref(), Some("other\n"));
}

/// WP-8 (editor-feedback-round-2): the live `file-as-kind` orders `move` first; the following
/// `upsert_tags` (no `path`) must target the moved note, not the dead pre-move path.
#[test]
fn file_as_kind_move_first_tags_the_moved_note() {
    let env = example_env(&[("inbox/clip.md", "---\ntags: [clip]\n---\nbody\n")]);
    let steps: Vec<Step> = [
        "type = \"move\"\nkind = \"{param.kind}\"",
        "type = \"upsert_tags\"\nkind = \"{param.kind}\"",
        "type = \"lint\"\npath = \"{vars.moved_path}\"\nkind = \"{param.kind}\"",
    ]
    .iter()
    .map(|s| Step::from_table(s.parse::<toml::Table>().unwrap()).unwrap())
    .collect();
    let (open, vars) = env
        .run_steps(&steps, json!({"kind": "repo"}), active("inbox/clip.md"))
        .unwrap();
    assert_eq!(open.as_deref(), Some("stack/clip.md"));
    assert_eq!(vars["moved_path"], json!("stack/clip.md"));
    assert!(env.read("inbox/clip.md").is_none());
    let text = env.read("stack/clip.md").unwrap();
    assert!(text.contains("- repo"), "{text}");
    assert!(
        text.contains(&format!("created: {}", today())),
        "lint ran: {text}"
    );
}

#[test]
fn follow_move_only_follows_the_active_note() {
    let mut ctx = active("a.md");
    let mut o = StepOutcome::default();
    o.vars.insert("moved_path".into(), json!("b/a.md"));
    o.vars.insert(super::MOVED_FROM.into(), json!("other.md"));
    super::follow_move(&mut ctx, &o);
    assert_eq!(ctx.active_path.as_deref(), Some("a.md"));
    o.vars.insert(super::MOVED_FROM.into(), json!("a.md"));
    super::follow_move(&mut ctx, &o);
    assert_eq!(ctx.active_path.as_deref(), Some("b/a.md"));
}

#[test]
fn copy_set_props_upsert_tags_are_idempotent() {
    let env = example_env(&[("t.md", "---\ntags:\n  - transcript\n---\nraw words\n")]);
    let copy = "type = \"copy\"\nto = \"_raw_transcript\"";
    let o = env.run(copy, json!({}), active("t.md")).unwrap();
    assert_eq!(o.vars["copied_path"], json!("_raw_transcript/t.md"));
    assert_eq!(o.vars["copied_link"], json!("[[_raw_transcript/t]]"));
    assert_eq!(env.read("_raw_transcript/t.md"), env.read("t.md"));
    assert!(
        env.run(copy, json!({}), active("t.md")).is_ok(),
        "identical copy converges"
    );

    let mut vars = Map::new();
    vars.insert("copied_link".into(), json!("[[_raw_transcript/t]]"));
    let sp = "type = \"set_props\"\nprops = { raw-transcript = \"{vars.copied_link}\", n = 3 }";
    let o = env.run_vars(sp, json!({}), active("t.md"), &vars).unwrap();
    assert_eq!(o.output_paths.len(), 1);
    let after = env.read("t.md").unwrap();
    assert!(
        after.contains("raw-transcript: \"[[_raw_transcript/t]]\""),
        "{after}"
    );
    let o = env.run_vars(sp, json!({}), active("t.md"), &vars).unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(env.read("t.md").unwrap(), after);

    let up = "type = \"upsert_tags\"\ntags = [\"#refined\", \"{param.extra}\"]";
    env.run(up, json!({"extra": "transcript"}), active("t.md"))
        .unwrap();
    let tagged = env.read("t.md").unwrap();
    assert!(tagged.contains("  - transcript\n  - refined\n"), "{tagged}");
    assert!(
        env.run(up, json!({"extra": "transcript"}), active("t.md"))
            .unwrap()
            .output_paths
            .is_empty()
    );

    let rp = "type = \"remove_props\"\nprops = [\"n\"]";
    assert_eq!(
        env.run(rp, json!({}), active("t.md"))
            .unwrap()
            .output_paths
            .len(),
        1
    );
    assert!(
        env.run(rp, json!({}), active("t.md"))
            .unwrap()
            .output_paths
            .is_empty()
    );
}
