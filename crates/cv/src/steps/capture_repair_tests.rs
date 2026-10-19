//! `capture_repair` step tests (plan `usability-sprint-captures-links` WP-2): synthetic vaults in
//! temp dirs, the real writer thread (rename rewrites incoming links through the index).

use cv_actions::Step;
use cv_vault::test_support::LiveFixture;
use serde_json::Value;

use super::testkit::{StepEnv, config_of};
use super::{StepOutcome, capture_repair};

struct Env(StepEnv);

impl Env {
    fn new(files: &[(&str, &str)]) -> Self {
        let config = config_of(&[(
            "actions.toml",
            "[[action]]\nid = \"cr\"\n[[action.steps]]\ntype = \"capture_repair\"\nfolder = \"daily/day\"\n",
        )]);
        let action = config.armed("cr").unwrap().clone();
        Self(StepEnv::new(
            LiveFixture::new(files.iter().copied()),
            config,
            action,
        ))
    }

    fn text(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.0.fx.path(rel)).ok()
    }

    fn run(&self) -> StepOutcome {
        let mut t = toml::Table::new();
        t.insert("type".into(), "capture_repair".into());
        t.insert("folder".into(), "daily/day".into());
        let step = Step::from_table(t).unwrap();
        crate::steps::validate(&step, &self.0.config).unwrap();
        crate::steps::execute(&step, &mut self.0.ctx()).unwrap()
    }
}

fn counts(o: &StepOutcome) -> (u64, u64, u64) {
    let v = |k: &str| o.vars.get(k).and_then(Value::as_u64).unwrap();
    (v("renamed"), v("retagged"), v("skipped"))
}

fn blog(title: &str, post_id: &str, tags: &str) -> String {
    format!(
        "---\ntitle: \"{title}\"\nsource: \"https://ex.test/p/{post_id}\"\npost-id: \"{post_id}\"\ntags:\n{tags}read: false\n---\nbody\n"
    )
}

#[test]
fn renames_slug_note_rewrites_links_and_is_idempotent() {
    let note = blog("Hello: World", "p1", "  - blog\n");
    let env = Env::new(&[
        ("daily/day/hello-world.md", &note),
        ("daily/ref.md", "see [[hello-world]]\n"),
        ("daily/day/plain.md", "---\ntags:\n  - journal\n---\nx\n"),
    ]);
    let o = env.run();
    assert_eq!(counts(&o), (1, 0, 1));
    assert_eq!(env.text("daily/day/hello-world.md"), None);
    assert_eq!(env.text("daily/day/Hello World.md"), Some(note));
    assert_eq!(env.text("daily/ref.md").unwrap(), "see [[Hello World]]\n");
    assert_eq!(
        env.text("daily/day/plain.md").unwrap(),
        "---\ntags:\n  - journal\n---\nx\n",
        "non-blog note untouched"
    );
    assert_eq!(counts(&env.run()), (0, 0, 2));
}

#[test]
fn collision_takes_the_hash_form() {
    let other = "---\ntitle: other\n---\nnot this post\n";
    let env = Env::new(&[
        ("daily/day/Same.md", other),
        ("daily/day/same.md", &blog("Same", "p2", "  - blog\n")),
    ]);
    assert_eq!(counts(&env.run()).0, 1);
    let hash = cv_text::url_canon::short_hash("p2");
    assert!(env.text(&format!("daily/day/Same {hash}.md")).is_some());
    assert_eq!(env.text("daily/day/Same.md").unwrap(), other);
    assert_eq!(counts(&env.run()), (0, 0, 2));
}

#[test]
fn non_slug_basename_is_not_renamed_but_still_retagged() {
    let env = Env::new(&[(
        "daily/day/Some Clipper Name.md",
        &blog("Some: Clipper Name", "p5", "  - blog #x\n"),
    )]);
    assert_eq!(counts(&env.run()), (0, 1, 0));
    assert_eq!(
        env.text("daily/day/Some Clipper Name.md").unwrap(),
        blog("Some: Clipper Name", "p5", "  - blog\n  - x\n")
    );
    assert_eq!(counts(&env.run()), (0, 0, 1));
}

#[test]
fn old_hash_suffixed_slug_is_renamed() {
    let hash = cv_text::url_canon::short_hash("p6");
    let env = Env::new(&[(
        &format!("daily/day/my-post-{hash}.md"),
        &blog("My Post", "p6", "  - blog\n"),
    )]);
    assert_eq!(counts(&env.run()).0, 1);
    assert!(env.text("daily/day/My Post.md").is_some());
}

#[test]
fn malformed_tags_are_retagged_in_place() {
    let env = Env::new(&[(
        "daily/day/T.md",
        &blog("T", "p3", "  - \"#blog\"\n  - a, b\n  - a\n"),
    )]);
    assert_eq!(counts(&env.run()), (0, 1, 0));
    assert_eq!(
        env.text("daily/day/T.md").unwrap(),
        blog("T", "p3", "  - blog\n  - a\n  - b\n")
    );
    assert_eq!(counts(&env.run()), (0, 0, 1));
}

#[test]
fn yaml_comment_tags_are_recovered_from_raw_frontmatter() {
    // E3: `- blog #x #y` parses as `blog` + a comment; the raw line keeps x and y.
    let env = Env::new(&[("daily/day/T.md", &blog("T", "p4", "  - blog #x #y\n"))]);
    assert_eq!(counts(&env.run()), (0, 1, 0));
    assert_eq!(
        env.text("daily/day/T.md").unwrap(),
        blog("T", "p4", "  - blog\n  - x\n  - y\n")
    );
}

#[test]
fn plan_ignores_non_blog_and_falls_back_to_slug() {
    assert!(capture_repair::plan("---\ntags:\n  - x\n---\n").is_none());
    assert!(capture_repair::plan("no frontmatter").is_none());
    let p = capture_repair::plan(
        "---\ntitle: \"(untitled)\"\nsource: \"https://ex.test/a/b\"\ntags:\n  - blog\n---\n",
    )
    .unwrap();
    assert_eq!(p.retagged, None);
    assert_eq!(p.hash_key, "https://ex.test/a/b");
    assert!(!p.name.is_empty());
}
