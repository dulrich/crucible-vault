//! `yt_clip_tidy` tests (plan `round-19-yt-cleanup-intake` WP-2): synthetic notes over the
//! shipped example config.

use serde_json::{Value, json};

use super::testkit::{active, example_env};

const STEP: &str = "type = \"yt_clip_tidy\"";
const FM: &str = "---\nsource: https://www.youtube.com/watch?v=dQw4w9WgXcQ\n---\n";
const ENRICH: &str = "%% cv-enrich %%\n> [!abstract]- Summary\n> gist\n\n%% /cv-enrich %%\n";
const TAIL: &str = "## Transcript\n\n**0:01** hi\n";

fn clip(fm: &str) -> String {
    format!(
        "{fm}{ENRICH}\n[![](Clippings/_attachments/t.jpg)](https://www.youtube.com/watch?v=dQw4w9WgXcQ)\n\nDescription.\n\n{TAIL}"
    )
}

#[test]
fn strips_embed_and_description_then_is_idempotent() {
    let e = example_env(&[("Clips/v.md", clip(FM).as_str())]);
    let o = e.run(STEP, json!({}), active("Clips/v.md")).unwrap();
    assert_eq!(o.vars.get("stripped"), Some(&Value::Bool(true)));
    assert_eq!(
        e.read("Clips/v.md").unwrap(),
        format!("{FM}{ENRICH}\n{TAIL}")
    );
    let o = e.run(STEP, json!({}), active("Clips/v.md")).unwrap();
    assert_eq!(o.vars.get("stripped"), Some(&Value::Bool(false)));
}

#[test]
fn non_youtube_source_is_untouched() {
    let other = clip("---\nsource: https://ex.test/post\n---\n");
    let e = example_env(&[("Clips/o.md", other.as_str())]);
    let o = e.run(STEP, json!({}), active("Clips/o.md")).unwrap();
    assert_eq!(o.vars.get("stripped"), Some(&Value::Bool(false)));
    assert_eq!(e.read("Clips/o.md").unwrap(), other);
}

#[test]
fn a_param_disarms() {
    let step = cv_actions::Step::from_table(
        "type = \"yt_clip_tidy\"\nx = 1"
            .parse::<toml::Table>()
            .unwrap(),
    )
    .unwrap();
    assert!(super::validate(&step, &super::testkit::example_config()).is_err());
}
