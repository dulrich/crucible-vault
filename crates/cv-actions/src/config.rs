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
#[path = "config_tests.rs"]
mod tests;
