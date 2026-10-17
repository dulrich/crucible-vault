//! Schema drift test (plan editor-settings-tab WP-1): holds `cv-schema` to the real parsers.
//!
//! For each config file and each step kind, a suite of valid alternative samples (each honouring
//! the exclusivity rules) must pass both the schema's `validate` and the real parser —
//! `ConfigSet::reload` + `cv::steps::validate`, `ProvidersConfig::from_toml`, `views::def::BaseDef`
//! — and together the samples must exercise every field the schema declares. A schema field the
//! parser rejects therefore fails here. `bookmarks.toml` and `editor.toml` are parsed by the
//! editor crate (iced), which `cv` cannot depend on; they are not covered by this test.

use std::collections::{BTreeMap, BTreeSet};

use cv_actions::{ConfigSet, StepKind};
use cv_providers::ProvidersConfig;
use cv_schema::{ConfigDoc, covered_paths, schema_for, schema_paths, step_schema, validate};

const PROVIDERS: &[&str] = &[
    r#"
[[provider]]
id = "local"
kind = "openai-compatible"
name = "Local"
base_url = "http://127.0.0.1:1234/v1"
api_key_env = "LOCAL_KEY"
api_key_file = "local-key"
max_concurrent_requests = 2
local = true
max_tokens = 1000
cwd = "/tmp"
timeout_seconds = 30
models = [{ id = "m1" }, { id = "e1", capabilities = ["embed", "rerank"] }, { id = "c1", capabilities = ["chat"] }]
"#,
    r#"
[[provider]]
id = "local"
kind = "ollama"
models = [{ id = "m1" }]

[[provider]]
id = "codex"
kind = "cli"
command = "codex"
args = ["exec"]
extra_args = ["--skip-git-repo-check"]
"#,
];

const AGENTS: &[&str] = &[r#"
[a1]
name = "Agent one"
system = "Be brief."
prompt = "Summarize: {{content}}"
input = "{{value}}"
model = "local:m1"
max_tokens = 100

[a2]
system_file = "_custom/system.md"
prompt_file = "_custom/prompt.md"
max_tokens = 5
"#];

const KINDS: &[&str] = &[r#"
[kinds.k1]
label = "Kind one"
tags = ["one", "two"]
folder = "{period.day}"
cleanup_transcript = true
props = { source = "web", rank = 1 }

[kinds.k2]
tags = "single"
"#];

const CAPTURES: &[&str] = &[
    r###"
[periods.day]
folder = "daily/day"
format = "YYYY-MM-DD"
template = "_custom/day.md"
asset_folder = true

[periods.week]
folder = "daily/week"

[periods.month]
folder = "daily/month"
format = "YYYY-MM"

[[capture]]
name = "c1"
label = "Log line"
target = "path"
path = "logs/{param.topic}.md"
mode = "section"
section = "## Log"
section_mode = "source"
format = "- {{value}}"
source = "line"
value = "{vars.v}"

[[capture]]
name = "c2"
target = "weekly"
mode = "prepend"
"###,
    r#"
[periods.week]
folder = "w"
format = "GGGG-[W]WW"
template = "_custom/week.md"
asset_folder = false

[periods.month]
folder = "m"
template = "_custom/month.md"
asset_folder = true
"#,
];

/// Step-kind samples: `(type, params lines)`.
const STEPS: &[(&str, &str)] = &[
    (
        "guard",
        "path = \"{path}\"\ntag = \"t\"\nnot_tag = [\"n\"]\nprop = [\"p\"]\nnot_prop = \"q\"\nfolder = \"f\"\nnot_folder = [\"g\"]",
    ),
    ("guard", ""),
    ("capture", "capture = \"c1\""),
    (
        "capture",
        "target = \"path\"\npath = \"x/{param.p}.md\"\nmode = \"section\"\nsection = \"## Log\"\nsection_mode = \"source\"\nformat = \"- {{value}}\"\nsource = \"line\"\nvalue = \"{vars.v}\"\nname = \"n\"\nlabel = \"l\"",
    ),
    ("capture", "target = \"daily_next\"\nmode = \"replace\""),
    (
        "materialize",
        "period = \"day\"\ndate = \"{param.date}\"\ntemplate = \"_custom/t.md\"\nfolder = \"daily\"\nformat = \"YYYY-MM-DD\"\nonly_if_empty = true",
    ),
    ("materialize", "path = \"notes/{param.title}\""),
    ("materialize", "target = \"active\""),
    ("move", "path = \"{path}\"\nto = \"archive\"\nname = \"n\""),
    ("move", "kind = \"k1\""),
    ("copy", "path = \"{path}\"\nto = \"copies\"\nname = \"c\""),
    (
        "set_props",
        "path = \"{path}\"\nprops = { a = \"x\", b = 1 }\nif_empty = true\nkind = \"k1\"",
    ),
    ("remove_props", "path = \"{path}\"\nprops = [\"a\"]"),
    (
        "upsert_tags",
        "path = \"{path}\"\ntags = [\"t\", \"{param.x}\"]\nkind = \"k1\"",
    ),
    ("upsert_tags", "tags = \"x\""),
    (
        "lint",
        "path = \"{path}\"\nkind = \"k1\"\ncleanup_transcript = true\nfrontmatter_insert = \"a: b\"\ncreated_key = \"created\"\nmodified_key = \"updated\"\nblank_line_after_yaml = true\nyaml_key_priority = [\"title\"]\nsteps = { insert-keys = false }",
    ),
    (
        "localize",
        "path = \"{path}\"\nmode = \"repair\"\nfolder_template = \"{{folder}}/_att\"\nname_template = \"{{md5}}.{{ext}}\"\nimages = true\naudio = true\nvideo = false\npdf = true\nimages_whitelist = [\"png\"]\naudio_whitelist = [\"mp3\"]\nvideo_whitelist = [\"mp4\"]\npdf_whitelist = [\"pdf\"]\nconvert_images = true\nimage_format = \"jpeg\"\nimage_quality = 80\ntimeout_secs = 10\nmax_bytes = 1000",
    ),
    ("localize", "image_quality = 0.8"),
    (
        "localize",
        "scope = \"folder\"\nfolder = \"Clippings\"\nlimit = 50",
    ),
    (
        "agent",
        "agent = \"a1\"\nmodel = \"local:m1\"\noutput_var = \"out\"\nmax_tokens = 10\noutput = \"replace\"",
    ),
    ("agent", "agent = \"a2\"\nmodel = \"{param.model}\""),
    ("mark_forwarded", "path = \"{path}\"\nline = 0"),
    ("run_action", "action = \"s\"\nparams = { x = \"y\" }"),
    (
        "search_upsert",
        "companion_url = \"http://127.0.0.1:4801\"\nvault_id = \"v\"\nembed = \"local:e1\"\npath = \"a.md\"",
    ),
    ("search_upsert", ""),
    (
        "search_delete",
        "companion_url = \"https://example.invalid\"\nvault_id = \"v\"\nembed = \"local:e1\"\npath = \"a.md\"",
    ),
    // A2 stubs (plan v0-automation-pipelines WP-1): no params until their WPs define them.
    ("link_scan", ""),
    (
        "link_scan",
        "scope = \"vault\"\nexclude = [\"_crucible\", \"Archive/\"]",
    ),
    ("yt_metadata", ""),
    ("yt_metadata", "video_id = \"{vars.video_id}\""),
    ("yt_channel_enrich", ""),
    (
        "yt_channel_enrich",
        "channel_id = \"{vars.channel_id}\"\nforce = true\nmax_age_days = 7",
    ),
    ("x_discover", ""),
    ("x_discover", "limit = 10"),
    ("x_metadata", ""),
    ("x_metadata", "status_id = \"{vars.status_id}\""),
    ("x_backfill", ""),
    ("x_backfill", "limit = 25"),
    ("image_describe", ""),
    (
        "image_describe",
        "scope = \"vault\"\nlimit = 5\nprune_transient = true",
    ),
    ("track_youtube", "limit_per_source = 25"),
    ("track_blogs", ""),
    ("ignore_video", "mode = \"manual\""),
    ("daily_brief", ""),
    ("capture_repair", "folder = \"daily/day\"\nlimit = 10"),
];

const ACTION_FULL: &str = r#"
[[action]]
id = "full"
label = "Full"
group = "G"
keys = ["Ctrl+Alt+F"]
needs = ["active_path", "selection", "value", "line"]
lane = "background"
durable = false
resources = { note = false, keys = ["note::{path}", "p::{param.kind}"] }

[[action.params]]
name = "kind"
label = "Kind"
kind = "enum"
required = true
choices = ["k1"]
default = "k1"

[[action.params]]
name = "title"
label = "Title"
kind = "text"

[[action.steps]]
type = "guard"

[[action.starters]]
type = "command"

[[action.starters]]
type = "event"
kinds = ["created", "modified", "deleted", "renamed"]
origin = "any"
selector = { folder = "inbox", tag = "t", prop = { key = "k", value = "v" } }

[[action.starters]]
type = "event"
kinds = ["created"]
selector = { prop = { key = "k" } }

[[action.starters]]
type = "schedule"
daily = "07:30"

[[action.starters]]
type = "schedule"
every = "15m"

[[action]]
id = "lean"
lane = "user"
[[action.steps]]
type = "guard"
"#;

/// Plan v0-automation-pipelines WP-12: every `PipelinesConfig` section.
const PIPELINES: &[&str] = &[r##"
[youtube]
enabled = true
metadata_root = "_yt_metadata"
channel_enrich = true

[x]
enabled = true
metadata_root = "_x_metadata"
inbox = "Clippings"

[blogs]
enabled = true
automatable_window = 3

[image_describe]
enabled = true
provider = "local"
model = "vl"
max_tokens = 800

[daily_brief]
enabled = true
section = "# Daily Brief"
fx = [{ base = "EUR", quote = "USD", label = "EUR/USD" }]
weather = [{ label = "Home", lat = 0.0, lon = 0.0 }]

[scorer]
url = "http://127.0.0.1:9000/v1/systemone"
model = "local/importance"

[ignore_shorts]
channels = ["UCxxxxxxxxxxxxxxxxxxxxxx"]
max_seconds = 90

[queue]
retention_days = 14
"##];

const SOURCES: &[&str] = &[r#"
[[youtube]]
channel = "Example"
id = "UCxxxxxxxxxxxxxxxxxxxxxx"
tags = ["a"]
priority = "high"

[[blog]]
name = "Example"
link = "https://example.com/feed.xml"
method = "rss"
tags = ["a"]
priority = "skip"
canon = "strip-params"
body = "full"
"#];

const SCHEDULES: &[&str] = &[
    "[[schedule]]\naction = \"s\"\ndaily = \"06:00\"\n\n[[schedule]]\naction = \"s\"\nevery = \"2h\"\n",
];

const VIEWS: &[&str] = &[r#"
filters = 'file.inFolder("daily/day")'

[formulas]
words = 'note.words'

[properties."note.status"]
displayName = "Status"

[[views]]
type = "table"
name = "Table"
filters = { and = ['note.status == "done"', { or = ["note.a", { not = ["note.b"] }] }] }
order = ["file.name", "note.status"]
sort = [{ property = "file.name", direction = "DESC" }]
groupBy = { property = "note.status", direction = "asc" }
summaries = { "note.words" = "Sum" }
limit = 10
columnSize = { "file.name" = 200 }

[[views]]
type = "pivot"
name = "Pivot"
rows = 'file.name'
rows_label = "Note"
rows_direction = "DESC"
columns = [{ label = "Done", when = 'note.status == "done"' }, { label = "Rest" }]
value = 'note.words'
aggregate = "count"
display = 'value'
total_column = "Total"
total_row = "Total"
average_row = "Average"

[[views]]
type = "tasks"
name = "Tasks"
status = [" "]
require_text = false
task_filter = 'value.text != ""'
task_group = "file"
"#];

/// Runs the real loader + step validators; returns every error.
fn reload(files: &BTreeMap<&str, String>) -> Vec<String> {
    let map: BTreeMap<String, Option<String>> = files
        .iter()
        .map(|(k, v)| ((*k).to_owned(), Some(v.clone())))
        .collect();
    let mut c = ConfigSet::new();
    c.reload(&map, &cv::steps::validate);
    c.errors()
}

fn support() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("providers.toml", PROVIDERS[0].to_owned()),
        ("agents.toml", AGENTS[0].to_owned()),
        ("kinds.toml", KINDS[0].to_owned()),
        ("captures.toml", CAPTURES[0].to_owned()),
        ("actions.toml", step_action("guard", "")),
    ])
}

fn step_action(kind: &str, params: &str) -> String {
    format!("[[action]]\nid = \"s\"\n[[action.steps]]\ntype = \"{kind}\"\n{params}\n")
}

/// Actions that make the loader consult every entry of a support file (the step validators read
/// `captures.toml` / `agents.toml` / `kinds.toml` only through steps that name them).
fn exercising_actions(file: &str, text: &str) -> String {
    let doc = ConfigDoc::parse(text).unwrap();
    let mut steps = vec![("guard".to_owned(), String::new())];
    match file {
        "captures.toml" => {
            for p in doc.keys(&cv_schema::Path::parse("periods").unwrap()) {
                steps.push(("materialize".into(), format!("period = \"{p}\"")));
            }
            for i in 0..doc.len(&cv_schema::Path::parse("capture").unwrap()) {
                let name = doc
                    .get_str(&cv_schema::Path::parse(&format!("capture[{i}].name")).unwrap())
                    .unwrap()
                    .to_owned();
                steps.push(("capture".into(), format!("capture = \"{name}\"")));
            }
        }
        "agents.toml" => {
            for id in doc.keys(&cv_schema::Path::root()) {
                steps.push((
                    "agent".into(),
                    format!("agent = \"{id}\"\nmodel = \"local:m1\""),
                ));
            }
        }
        "kinds.toml" => {
            for k in doc.keys(&cv_schema::Path::parse("kinds").unwrap()) {
                steps.push(("move".into(), format!("kind = \"{k}\"")));
            }
        }
        _ => {}
    }
    let mut out = String::new();
    for (n, (kind, params)) in steps.iter().enumerate() {
        out.push_str(&format!(
            "[[action]]\nid = \"x{n}\"\n[[action.steps]]\ntype = \"{kind}\"\n{params}\n\n"
        ));
    }
    out
}

fn schema_errors(file: &str, text: &str) -> Vec<String> {
    let doc = ConfigDoc::parse(text).unwrap_or_else(|e| panic!("{file}: {e}"));
    validate(&doc, schema_for(file).unwrap())
        .iter()
        .map(ToString::to_string)
        .collect()
}

fn assert_covered(file: &str, samples: &[String]) {
    let node = schema_for(file).unwrap();
    let mut got = BTreeSet::new();
    for s in samples {
        got.extend(covered_paths(&ConfigDoc::parse(s).unwrap(), node));
    }
    let missing: Vec<_> = schema_paths(node).difference(&got).cloned().collect();
    assert!(
        missing.is_empty(),
        "{file}: schema fields no sample exercises: {missing:?}"
    );
}

#[test]
fn every_step_kind_has_a_schema() {
    for k in StepKind::ALL {
        assert!(step_schema(k.as_str()).is_some(), "{}", k.as_str());
    }
    assert_eq!(cv_schema::schemas::STEP.variants.len(), StepKind::ALL.len());
    for v in cv_schema::schemas::STEP.variants {
        assert!(
            StepKind::parse(v.tag).is_some(),
            "schema-only step kind {}",
            v.tag
        );
    }
}

#[test]
fn support_files_parse_and_validate() {
    for (file, samples) in [
        ("providers.toml", PROVIDERS),
        ("agents.toml", AGENTS),
        ("kinds.toml", KINDS),
        ("captures.toml", CAPTURES),
        ("pipelines.toml", PIPELINES),
        ("sources.toml", SOURCES),
    ] {
        for s in samples {
            assert_eq!(
                schema_errors(file, s),
                Vec::<String>::new(),
                "{file} schema:\n{s}"
            );
            let mut files = support();
            files.insert(file, (*s).to_owned());
            files.insert("actions.toml", exercising_actions(file, s));
            assert_eq!(reload(&files), Vec::<String>::new(), "{file} parser:\n{s}");
        }
        let owned: Vec<String> = samples.iter().map(|s| (*s).to_owned()).collect();
        assert_covered(file, &owned);
    }
    for s in PROVIDERS {
        ProvidersConfig::from_toml(s).unwrap_or_else(|e| panic!("{e}\n{s}"));
    }
}

#[test]
fn actions_and_step_samples_parse_and_validate() {
    let mut samples = vec![ACTION_FULL.to_owned()];
    samples.extend(STEPS.iter().map(|(k, p)| step_action(k, p)));
    for s in &samples {
        assert_eq!(
            schema_errors("actions.toml", s),
            Vec::<String>::new(),
            "schema:\n{s}"
        );
        let mut files = support();
        files.insert("actions.toml", s.clone());
        assert_eq!(reload(&files), Vec::<String>::new(), "parser:\n{s}");
    }
    assert_covered("actions.toml", &samples);
}

#[test]
fn schedules_parse_and_validate() {
    for s in SCHEDULES {
        assert_eq!(schema_errors("schedules.toml", s), Vec::<String>::new());
        let mut files = support();
        files.insert("schedules.toml", (*s).to_owned());
        assert_eq!(reload(&files), Vec::<String>::new(), "{s}");
    }
    let owned: Vec<String> = SCHEDULES.iter().map(|s| (*s).to_owned()).collect();
    assert_covered("schedules.toml", &owned);
}

#[test]
fn views_parse_compile_and_validate() {
    for s in VIEWS {
        assert_eq!(schema_errors("views/v.toml", s), Vec::<String>::new());
        let def: views::def::BaseDef = toml::from_str(s).unwrap_or_else(|e| panic!("{e}"));
        let mut notices = Vec::new();
        let base = views::def::Base::compile("v", &def, &mut notices);
        assert_eq!(notices, Vec::<String>::new());
        for v in &base.views {
            assert_eq!(v.errors, Vec::<String>::new());
        }
    }
    let owned: Vec<String> = VIEWS.iter().map(|s| (*s).to_owned()).collect();
    assert_covered("views/v.toml", &owned);
}

/// The harness detects drift: an invented field is rejected by each strict parser.
#[test]
fn invented_fields_are_rejected() {
    let mut files = support();
    files.insert(
        "actions.toml",
        "[[action]]\nid = \"s\"\ninvented = 1\n[[action.steps]]\ntype = \"guard\"\n".into(),
    );
    assert!(reload(&files).iter().any(|e| e.contains("invented")));
    files.insert("actions.toml", step_action("lint", "invented = 1"));
    assert!(reload(&files).iter().any(|e| e.contains("invented")));
    files.insert("actions.toml", exercising_actions("agents.toml", AGENTS[0]));
    files.insert("agents.toml", format!("{}invented = 1\n", AGENTS[0]));
    assert!(reload(&files).iter().any(|e| e.contains("invented")));
    assert!(ProvidersConfig::from_toml(&format!("{}invented = 1\n", PROVIDERS[0])).is_err());
    assert!(toml::from_str::<views::def::BaseDef>("invented = 1\n").is_err());
    let mut files = support();
    files.insert("pipelines.toml", format!("{}invented = 1\n", PIPELINES[0]));
    assert!(reload(&files).iter().any(|e| e.contains("invented")));
    let mut files = support();
    files.insert("sources.toml", format!("{}invented = 1\n", SOURCES[0]));
    assert!(reload(&files).iter().any(|e| e.contains("invented")));
    // The dropped cadence keys (cadences live in schedules.toml).
    let mut files = support();
    files.insert("pipelines.toml", "[youtube]\nschedule = \"3h\"\n".into());
    assert!(reload(&files).iter().any(|e| e.contains("schedule")));
}
