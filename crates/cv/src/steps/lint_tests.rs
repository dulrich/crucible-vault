//! `lint` step tests (was `wp10_tests.rs`). Synthetic notes in a temp
//! vault, the example config from `config/examples/`, steps driven
//! through `steps::validate`/`steps::execute` via [`super::testkit`].

use serde_json::json;

use super::testkit::{active, example_env, note_gone, today};

#[test]
fn lint_writes_only_on_change() {
    let env = example_env(&[("n.md", "---\ntitle: n\n---\nsome words here\n")]);
    let o = env
        .run("type = \"lint\"", json!({}), active("n.md"))
        .unwrap();
    assert_eq!(o.output_paths, vec!["n.md".to_owned()]);
    let once = env.read("n.md").unwrap();
    assert!(once.contains(&format!("updated: {}", today())), "{once}");
    let o = env
        .run("type = \"lint\"", json!({}), active("n.md"))
        .unwrap();
    assert!(o.output_paths.is_empty());
    assert_eq!(env.read("n.md").unwrap(), once);
}

/// D11: a missing lint target is `NoteGone`, not a hard failure.
#[test]
fn lint_missing_note_is_note_gone() {
    let env = example_env(&[("n.md", "x\n")]);
    assert_eq!(
        note_gone(env.run("type = \"lint\"", json!({}), active("missing.md"))),
        "missing.md"
    );
}
