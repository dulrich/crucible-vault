//! Example-config load and step-param validation tests (was `wp10_tests.rs`). Synthetic notes in a temp
//! vault, the example config from `config/examples/`, steps driven
//! through `steps::validate`/`steps::execute` via [`super::testkit`].

use cv_actions::{ConfigSet, Step};

use super::testkit::example_config;

// ---- config ----------------------------------------------------------------------------------

#[test]
fn example_config_arms_every_action() {
    let c = example_config();
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    assert_eq!(c.armed_actions().count(), 36);
    for id in [
        "period.open",
        "note.create",
        "period.fill-created",
        "capture.daylog",
        "capture.forward-task",
        "capture.replace-note",
        "capture.monthly-observation",
        "file-as-kind",
        "refine-transcript",
        "search-refresh",
    ] {
        assert!(c.armed(id).is_some(), "{id} armed");
    }
    let kinds = c.table("kinds.toml")["kinds"].as_table().unwrap();
    assert_eq!(kinds.len(), 10);
    assert_eq!(
        c.table("captures.toml")["capture"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
}

#[test]
fn validation_rejects_bad_params() {
    let c = example_config();
    let v = |src: &str| {
        let step = Step::from_table(src.parse::<toml::Table>().unwrap()).unwrap();
        super::validate(&step, &c)
    };
    assert!(v("type = \"materialize\"\nperiod = \"year\"").is_err());
    assert!(v("type = \"materialize\"\nperiod = \"day\"").is_ok());
    let no_periods = ConfigSet::new();
    let step =
        Step::from_table("type = \"materialize\"\nperiod = \"day\"".parse().unwrap()).unwrap();
    assert!(
        super::validate(&step, &no_periods).is_err(),
        "period without settings"
    );
    assert!(v("type = \"capture\"\ntarget = \"nowhere\"").is_err());
    assert!(v("type = \"capture\"\ncapture = \"nope\"").is_err());
    assert!(v("type = \"capture\"\nmode = \"section\"").is_err());
    assert!(v("type = \"upsert_tags\"").is_err());
    assert!(v("type = \"move\"\nkind = \"nope\"").is_err());
    assert!(v("type = \"guard\"\nbogus = 1").is_err());
    assert!(v("type = \"lint\"\nsteps = { nope = true }").is_err());
    // Bare steps the runner tests use stay valid.
    assert!(v("type = \"guard\"").is_ok());
    assert!(v("type = \"set_props\"").is_ok());
}
