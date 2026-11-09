//! The [`ConfigSet`]: every `CV_CONFIG_DIR` TOML file WP-9 knows, with hot-reload semantics.
//!
//! - `actions.toml` (`[[action]]`) and `schedules.toml` (`[[schedule]]`) are parsed into the model;
//!   `kinds.toml`, `captures.toml`, `agents.toml`, `providers.toml` are kept as raw tables for the
//!   step validators/executors (WP-10..12) to consult via [`ConfigSet::table`].
//! - `pipelines.toml` and `sources.toml` (plan `v0-automation-pipelines` WP-1) are typed
//!   ([`ConfigSet::pipelines`], [`ConfigSet::sources`]); a file that parses but fails to type-check
//!   keeps its last valid typed value and reports the error, like a syntax error.
//! - A missing file is empty. A file that fails to parse keeps its last valid version and its
//!   error is reported (file + message; never note content).
//! - An action whose definition, starters or steps fail validation is **disarmed** and reported,
//!   never rewritten (I5). Port of Crucible's validation-gated enablement
//!   (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/AGENTS.md:77`, `src/triggers/triggerValidation.ts`);
//!   the plugin-managed-path exclusion and the `onLayoutReady` wait are dropped (the queue is not
//!   in the vault; there is no startup replay, I4).

use std::collections::BTreeMap;

use cv_protocol::{ContextNeed, ParamKind, ParamSpec};
use serde::Deserialize;

use crate::model::{
    Action, LaneChoice, OriginSel, ResourceSpec, Schedule, Selector, Starter, Step,
};
use crate::pipelines::{PipelinesConfig, SourcesConfig};
use crate::vault::VaultConfig;

/// Every config file name WP-9 loads, in load order.
pub const CONFIG_FILES: [&str; 9] = [
    "actions.toml",
    "schedules.toml",
    "kinds.toml",
    "captures.toml",
    "agents.toml",
    "providers.toml",
    "pipelines.toml",
    "sources.toml",
    "vault.toml",
];

/// Step validation callback (`cv`'s `steps::validate`): `Err(reason)` disarms the action.
pub type StepValidator<'a> = &'a dyn Fn(&Step, &ConfigSet) -> Result<(), String>;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionDef {
    id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    params: Vec<ParamSpec>,
    #[serde(default)]
    needs: Vec<String>,
    #[serde(default)]
    lane: LaneChoice,
    #[serde(default = "yes")]
    durable: bool,
    /// Plan round-20-transcript-heading-auto-holds WP-2: explicit hold opt-in.
    #[serde(default)]
    hold: bool,
    /// Plan job-slots-fast-lane WP-1: the action may use the reserved fast job slot.
    #[serde(default)]
    fast: bool,
    #[serde(default)]
    steps: Vec<toml::Table>,
    #[serde(default)]
    starters: Option<Vec<StarterDef>>,
    #[serde(default)]
    resources: ResourceSpec,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum StarterDef {
    Command,
    Event {
        #[serde(default)]
        selector: Selector,
        #[serde(default)]
        kinds: Vec<cv_protocol::ChangeKind>,
        #[serde(default)]
        origin: OriginSel,
    },
    Schedule {
        #[serde(default)]
        daily: Option<String>,
        #[serde(default)]
        every: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScheduleDef {
    action: String,
    #[serde(default)]
    daily: Option<String>,
    #[serde(default)]
    every: Option<String>,
}

/// Loaded configuration: raw tables, parsed actions, disarm reasons and errors.
#[derive(Debug, Clone, Default)]
pub struct ConfigSet {
    tables: BTreeMap<String, toml::Table>,
    file_errors: BTreeMap<String, String>,
    entry_errors: Vec<String>,
    actions: BTreeMap<String, Action>,
    disarmed: BTreeMap<String, String>,
    empty: toml::Table,
    pipelines: PipelinesConfig,
    sources: SourcesConfig,
    vault: VaultConfig,
    typed_errors: BTreeMap<String, String>,
}

impl ConfigSet {
    /// An empty set (no files).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads `files` (name → contents; absent/`None` = missing = empty) on top of the current
    /// state, keeping each unparseable file's last valid version, then re-derives and re-validates
    /// every action.
    pub fn reload(&mut self, files: &BTreeMap<String, Option<String>>, validate: StepValidator) {
        for name in CONFIG_FILES {
            match files.get(name).and_then(Option::as_deref) {
                None => {
                    self.tables.insert(name.to_owned(), toml::Table::new());
                    self.file_errors.remove(name);
                }
                Some(text) => match text.parse::<toml::Table>() {
                    Ok(t) => {
                        self.tables.insert(name.to_owned(), t);
                        self.file_errors.remove(name);
                    }
                    Err(e) => {
                        let msg = e.message().to_owned();
                        let at = e
                            .span()
                            .map(|s| format!(" (line {})", line_of(text, s.start)))
                            .unwrap_or_default();
                        self.file_errors
                            .insert(name.to_owned(), format!("{name}: {msg}{at}"));
                    }
                },
            }
        }
        self.retype();
        self.rebuild(validate);
    }

    /// Re-derives the typed files; a type error keeps the last valid value (and reports).
    fn retype(&mut self) {
        match PipelinesConfig::from_table(self.table("pipelines.toml")) {
            Ok(p) => {
                self.pipelines = p;
                self.typed_errors.remove("pipelines.toml");
            }
            Err(e) => {
                self.typed_errors
                    .insert("pipelines.toml".into(), format!("pipelines.toml: {e}"));
            }
        }
        match SourcesConfig::from_table(self.table("sources.toml")) {
            Ok(s) => {
                self.sources = s;
                self.typed_errors.remove("sources.toml");
            }
            Err(e) => {
                self.typed_errors
                    .insert("sources.toml".into(), format!("sources.toml: {e}"));
            }
        }
        match VaultConfig::from_table(self.table("vault.toml")) {
            Ok(v) => {
                self.vault = v;
                self.typed_errors.remove("vault.toml");
            }
            Err(e) => {
                self.typed_errors
                    .insert("vault.toml".into(), format!("vault.toml: {e}"));
            }
        }
    }

    /// The typed `vault.toml` (default = vault `.trash/`, 30-day retention).
    #[must_use]
    pub fn vault(&self) -> &VaultConfig {
        &self.vault
    }

    /// `vault.toml` `[structure] app_folder` (default `_crucible`).
    #[must_use]
    pub fn app_folder(&self) -> &str {
        self.vault.structure.app_folder()
    }

    /// The shared exclude list (plan public-release WP-2, C#9): `vault.toml` `[structure]
    /// exclude` when set (entries trimmed of slashes, blanks dropped), else `.trash`, `_custom`,
    /// the app folder and the configured `pipelines.toml` metadata roots (`[youtube]` and `[x]`
    /// `metadata_root`), deduplicated in that order. Folder prefixes: a path is excluded when it
    /// is an entry or lies under one.
    #[must_use]
    pub fn exclude_folders(&self) -> Vec<String> {
        let raw: Vec<String> = match &self.vault.structure.exclude {
            Some(v) => v.clone(),
            None => vec![
                ".trash".into(),
                "_custom".into(),
                self.app_folder().to_owned(),
                self.pipelines.youtube.metadata_root.clone(),
                self.pipelines.x.metadata_root.clone(),
            ],
        };
        let mut out: Vec<String> = Vec::new();
        for e in &raw {
            if let Some(f) = crate::vault::folder_key(Some(e))
                && !out.iter().any(|o| o == f)
            {
                out.push(f.to_owned());
            }
        }
        out
    }

    /// `captures.toml` `[periods.day].folder` (slashes trimmed; `daily/day` when unset or blank).
    #[must_use]
    pub fn day_folder(&self) -> String {
        crate::vault::day_folder_of(self.table("captures.toml"))
    }

    /// The typed `pipelines.toml` (default = every pipeline off).
    #[must_use]
    pub fn pipelines(&self) -> &PipelinesConfig {
        &self.pipelines
    }

    /// The typed `sources.toml` (default = no sources).
    #[must_use]
    pub fn sources(&self) -> &SourcesConfig {
        &self.sources
    }

    fn rebuild(&mut self, validate: StepValidator) {
        self.entry_errors.clear();
        self.actions.clear();
        self.disarmed.clear();
        let actions_table = self.table("actions.toml").clone();
        let entries = match actions_table.get("action") {
            None => Vec::new(),
            Some(toml::Value::Array(a)) => a.clone(),
            Some(_) => {
                self.entry_errors
                    .push("actions.toml: `action` must be an array of tables".into());
                Vec::new()
            }
        };
        for (i, entry) in entries.into_iter().enumerate() {
            let id_hint = entry
                .get("id")
                .and_then(toml::Value::as_str)
                .map(str::to_owned);
            match parse_action(entry) {
                Ok(a) => {
                    if self.actions.contains_key(&a.id) {
                        self.entry_errors
                            .push(format!("actions.toml: duplicate action id {:?}", a.id));
                        self.disarmed
                            .insert(a.id.clone(), "duplicate action id".into());
                    } else {
                        self.actions.insert(a.id.clone(), a);
                    }
                }
                Err(e) => match id_hint.filter(|s| !s.is_empty()) {
                    Some(id) => {
                        self.disarmed.insert(id, e);
                    }
                    None => self
                        .entry_errors
                        .push(format!("actions.toml: action #{}: {e}", i + 1)),
                },
            }
        }
        self.apply_schedules();
        // Structural disarms, then step validation (which may consult the whole set).
        let ids: Vec<String> = self.actions.keys().cloned().collect();
        for id in &ids {
            if let Some(a) = self.actions.get(id)
                && let Err(e) = check_action(a)
            {
                self.disarmed.entry(id.clone()).or_insert(e);
            }
        }
        for id in &ids {
            if self.disarmed.contains_key(id) {
                continue;
            }
            let Some(a) = self.actions.get(id) else {
                continue;
            };
            let mut failure = None;
            for (n, step) in a.steps.iter().enumerate() {
                if let Err(e) = validate(step, self) {
                    failure = Some(format!("step {} ({}): {e}", n + 1, step.kind().as_str()));
                    break;
                }
            }
            if let Some(f) = failure {
                self.disarmed.insert(id.clone(), f);
            }
        }
    }

    fn apply_schedules(&mut self) {
        let table = self.table("schedules.toml").clone();
        let entries = match table.get("schedule") {
            None => return,
            Some(toml::Value::Array(a)) => a.clone(),
            Some(_) => {
                self.entry_errors
                    .push("schedules.toml: `schedule` must be an array of tables".into());
                return;
            }
        };
        for (i, entry) in entries.into_iter().enumerate() {
            let def: ScheduleDef = match entry.try_into() {
                Ok(d) => d,
                Err(e) => {
                    self.entry_errors
                        .push(format!("schedules.toml: schedule #{}: {e}", i + 1));
                    continue;
                }
            };
            let sched = match parse_schedule(def.daily.as_deref(), def.every.as_deref()) {
                Ok(s) => s,
                Err(e) => {
                    self.disarmed
                        .entry(def.action.clone())
                        .or_insert(format!("schedules.toml: {e}"));
                    continue;
                }
            };
            match self.actions.get_mut(&def.action) {
                Some(a) => a.starters.push(Starter::Schedule(sched)),
                None => self.entry_errors.push(format!(
                    "schedules.toml: schedule #{} names unknown action {:?}",
                    i + 1,
                    def.action
                )),
            }
        }
    }

    /// A loaded file's table (last valid version; empty when missing).
    #[must_use]
    pub fn table(&self, file: &str) -> &toml::Table {
        self.tables.get(file).unwrap_or(&self.empty)
    }

    /// The `mode` of the `[[capture]]` preset `name` in `captures.toml` (`None` when the preset
    /// is unknown or sets no mode) — the input of [`Action::holds_note`].
    #[must_use]
    pub fn capture_preset_mode(&self, name: &str) -> Option<String> {
        self.table("captures.toml")
            .get("capture")?
            .as_array()?
            .iter()
            .filter_map(toml::Value::as_table)
            .find(|e| e.get("name").and_then(toml::Value::as_str) == Some(name))?
            .get("mode")?
            .as_str()
            .map(str::to_owned)
    }

    /// Whether a running job of `action` holds its note, with presets from this config.
    #[must_use]
    pub fn action_holds_note(&self, action: &Action) -> bool {
        action.holds_note(|n| self.capture_preset_mode(n))
    }

    /// Every current error: file parse errors, unattributable entry errors, and one line per
    /// disarmed action.
    #[must_use]
    pub fn errors(&self) -> Vec<String> {
        let mut out: Vec<String> = self.file_errors.values().cloned().collect();
        out.extend(self.typed_errors.values().cloned());
        out.extend(self.entry_errors.iter().cloned());
        out.extend(
            self.disarmed
                .iter()
                .map(|(id, why)| format!("action {id:?} disarmed: {why}")),
        );
        out
    }

    /// Any parsed action (armed or not).
    #[must_use]
    pub fn action(&self, id: &str) -> Option<&Action> {
        self.actions.get(id)
    }

    /// Whether `id` is defined at all (parsed or disarmed at parse time).
    #[must_use]
    pub fn is_known(&self, id: &str) -> bool {
        self.actions.contains_key(id) || self.disarmed.contains_key(id)
    }

    /// The action if it is armed.
    #[must_use]
    pub fn armed(&self, id: &str) -> Option<&Action> {
        if self.disarmed.contains_key(id) {
            None
        } else {
            self.actions.get(id)
        }
    }

    /// Why `id` is disarmed, if it is.
    #[must_use]
    pub fn disarm_reason(&self, id: &str) -> Option<&str> {
        self.disarmed.get(id).map(String::as_str)
    }

    /// Armed actions in id order.
    pub fn armed_actions(&self) -> impl Iterator<Item = &Action> {
        self.actions
            .values()
            .filter(|a| !self.disarmed.contains_key(&a.id))
    }
}

fn line_of(text: &str, byte: usize) -> usize {
    text.get(..byte).map_or(0, |s| s.matches('\n').count()) + 1
}

fn parse_schedule(daily: Option<&str>, every: Option<&str>) -> Result<Schedule, String> {
    match (daily, every) {
        (Some(d), None) => Schedule::parse_daily(d),
        (None, Some(e)) => Schedule::parse_every(e),
        _ => Err("a schedule needs exactly one of `daily` or `every`".into()),
    }
}

fn parse_action(entry: toml::Value) -> Result<Action, String> {
    let def: ActionDef = entry
        .try_into()
        .map_err(|e: toml::de::Error| e.message().to_owned())?;
    if def.id.trim().is_empty() {
        return Err("empty action id".into());
    }
    let steps = def
        .steps
        .into_iter()
        .enumerate()
        .map(|(n, t)| Step::from_table(t).map_err(|e| format!("step {}: {e}", n + 1)))
        .collect::<Result<Vec<_>, _>>()?;
    if def.fast
        && let Some(n) = steps.iter().position(|s| s.kind().calls_model())
    {
        return Err(format!(
            "fast = true cannot have a model-calling step (step {}: {}); the fast slot must stay short",
            n + 1,
            steps[n].kind().as_str()
        ));
    }
    if def.hold && !def.resources.note {
        return Err(
            "hold = true needs resources.note (an action that keys no note has no note to hold)"
                .into(),
        );
    }
    let starters = match def.starters {
        None => vec![Starter::Command],
        Some(defs) => defs
            .into_iter()
            .map(|s| match s {
                StarterDef::Command => Ok(Starter::Command),
                StarterDef::Event {
                    selector,
                    kinds,
                    origin,
                } => Ok(Starter::Event {
                    selector,
                    kinds,
                    origin,
                }),
                StarterDef::Schedule { daily, every } => {
                    parse_schedule(daily.as_deref(), every.as_deref()).map(Starter::Schedule)
                }
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    Ok(Action {
        label: def.label.unwrap_or_else(|| def.id.clone()),
        id: def.id,
        group: def.group,
        keys: def.keys,
        params: def.params,
        needs: def
            .needs
            .iter()
            .map(|n| ContextNeed::parse(n).ok_or_else(|| format!("unknown need {n:?}")))
            .collect::<Result<Vec<_>, _>>()?,
        lane: def.lane,
        durable: def.durable,
        hold: def.hold,
        fast: def.fast,
        steps,
        starters,
        resources: def.resources,
    })
}

/// Structural checks (I5): non-empty steps, non-empty event selectors and kinds, sane params.
fn check_action(a: &Action) -> Result<(), String> {
    if a.steps.is_empty() {
        return Err("no steps".into());
    }
    for s in &a.starters {
        if let Starter::Event {
            selector, kinds, ..
        } = s
        {
            if selector.is_empty() {
                return Err("event starter has an empty selector (I5)".into());
            }
            if kinds.is_empty() {
                return Err("event starter lists no kinds".into());
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in &a.params {
        if p.name.is_empty() || !seen.insert(p.name.as_str()) {
            return Err(format!("param name {:?} is empty or duplicated", p.name));
        }
        if p.kind == ParamKind::Enum && p.choices.is_empty() {
            return Err(format!("enum param {:?} has no choices", p.name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
