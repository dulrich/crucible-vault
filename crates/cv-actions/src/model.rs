//! The action model: [`Action`], the closed [`Step`] enum, [`Starter`]s and [`ResourceSpec`].

use cv_protocol::ParamSpec;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Which queue lane a run goes on.
///
/// Lanes port `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/lanes.ts:3-6` (`user` outranks
/// `background`). `Auto` (the default) picks by starter: command runs → user, event/schedule
/// runs → background.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneChoice {
    /// By starter.
    #[default]
    Auto,
    /// Always the user lane.
    User,
    /// Always the background lane.
    Background,
}

/// One configured action.
#[derive(Debug, Clone, PartialEq)]
pub struct Action {
    /// Stable id (`run_action{action_id}`; job type `action:<id>`).
    pub id: String,
    /// Palette label (`ActionInfo.title`).
    pub label: String,
    /// Palette group.
    pub group: Option<String>,
    /// Suggested key bindings (the client binds them).
    pub keys: Vec<String>,
    /// Parameters the client prompts for.
    pub params: Vec<ParamSpec>,
    /// Context the client must supply (`active_path`, `selection`, `value`, `line`).
    pub needs: Vec<cv_protocol::ContextNeed>,
    /// Lane choice.
    pub lane: LaneChoice,
    /// `false`: `run_action` awaits completion (≤10 s) and returns `open_path`.
    pub durable: bool,
    /// Steps, executed sequentially.
    pub steps: Vec<Step>,
    /// How runs start.
    pub starters: Vec<Starter>,
    /// I3 resource-key derivation.
    pub resources: ResourceSpec,
}

impl Action {
    /// Whether any step mutates a note (such a run must derive at least one resource key).
    #[must_use]
    pub fn mutates_note(&self) -> bool {
        self.steps.iter().any(Step::mutates_note)
    }

    /// Whether a running job of this action holds its active note (plan round-18 Decision 1): iff
    /// `resources.note` and an `agent` step precedes a `capture` step whose **effective** mode is
    /// `replace`. The effective mode is the step's own `mode`, else the named preset's `mode`
    /// (`preset_mode(name)`, from `captures.toml`), else the capture default `append` — the same
    /// overlay `cv`'s capture step resolves (`steps/capture.rs` `Capture::parse`). An unresolvable
    /// preset never holds unless the step itself says `replace`. A `run_action` child is
    /// classified by its own steps, so `run_action` steps are ignored here.
    #[must_use]
    pub fn holds_note(&self, preset_mode: impl Fn(&str) -> Option<String>) -> bool {
        if !self.resources.note {
            return false;
        }
        let mut agent_seen = false;
        for step in &self.steps {
            match step.kind {
                StepKind::Agent => agent_seen = true,
                StepKind::Capture if agent_seen => {
                    let str_of = |k: &str| step.params.get(k).and_then(toml::Value::as_str);
                    let mode = str_of("mode")
                        .map(str::to_owned)
                        .or_else(|| str_of("capture").and_then(&preset_mode));
                    if mode.as_deref() == Some("replace") {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }

    /// Whether the action can be started by `run_action` (has a command starter).
    #[must_use]
    pub fn has_command_starter(&self) -> bool {
        self.starters.iter().any(|s| matches!(s, Starter::Command))
    }
}

/// Generates [`StepKind`] (variants, [`StepKind::ALL`], [`StepKind::as_str`],
/// [`StepKind::mutates_note`]) from one table: `Variant = "toml_type", mutates;`.
macro_rules! step_kinds {
    ($( $(#[$doc:meta])* $v:ident = $name:literal, $mutates:literal; )*) => {
        /// Step kind tag (the TOML `type` value).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub enum StepKind {
            $( $(#[$doc])* #[serde(rename = $name)] $v, )*
        }

        impl StepKind {
            /// Every kind.
            pub const ALL: [StepKind; [$($name),*].len()] = [$(StepKind::$v),*];

            /// The TOML `type` string.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self { $(StepKind::$v => $name,)* }
            }

            /// Whether a step of this kind may write a note (replaces Crucible's `mutating`
            /// flag, `src/AGENTS.md:28`). Fails closed: anything that might write counts.
            #[must_use]
            pub fn mutates_note(self) -> bool {
                match self { $(StepKind::$v => $mutates,)* }
            }
        }
    };
}

// A2: every pipeline step mutates (fails closed) except `link_scan`, which writes only
// `state.db`. `x_discover`/`x_backfill` ensure metadata inline and append to source notes'
// `x-metadata` lists (plan `v0-automation-pipelines` WP-5), so they mutate.
step_kinds! {
    Guard = "guard", false;
    Capture = "capture", true;
    Materialize = "materialize", true;
    Move = "move", true;
    Copy = "copy", true;
    SetProps = "set_props", true;
    RemoveProps = "remove_props", true;
    UpsertTags = "upsert_tags", true;
    Lint = "lint", true;
    Localize = "localize", true;
    Agent = "agent", true;
    MarkForwarded = "mark_forwarded", true;
    /// Enqueue another action as a child job (replaces Crucible sub-chains, `src/chains.ts`).
    RunAction = "run_action", false;
    SearchUpsert = "search_upsert", false;
    SearchDelete = "search_delete", false;
    // A2 (plan v0-automation-pipelines WP-1).
    LinkScan = "link_scan", false;
    YtMetadata = "yt_metadata", true;
    YtChannelEnrich = "yt_channel_enrich", true;
    XDiscover = "x_discover", true;
    XMetadata = "x_metadata", true;
    XBackfill = "x_backfill", true;
    ImageDescribe = "image_describe", true;
    TrackYoutube = "track_youtube", true;
    TrackBlogs = "track_blogs", true;
    /// Maildir newsletter intake (plan `auto-intake-sprint` WP-4).
    TrackMail = "track_mail", true;
    /// Clip dedup + kind auto-filing (plan `auto-intake-sprint` WP-5).
    AutoFile = "auto_file", true;
    /// One-shot reconcile of tracker rows (plan `auto-intake-sprint` WP-6); plan
    /// `intake-followups-mail-youtube` WP-5 widened it to retag, re-clean and re-key mail notes.
    IntakeReconcile = "intake_reconcile", true;
    /// Links and ticks the tracker rows a filed note satisfies (plan
    /// `intake-followups-mail-youtube` WP-4); writes `state.db` only, never a note.
    IntakeFinalize = "intake_finalize", false;
    IgnoreVideo = "ignore_video", true;
    DailyBrief = "daily_brief", true;
    /// On-demand duplicate scan writing `possible-duplicate` (plan `round-12-feedback-todos` WP-6).
    DupesScan = "dupes_scan", true;
    /// One duplicate-pair verdict (`not-duplicates` | `keep`) (plan `round-12-feedback-todos` WP-7).
    DupesResolve = "dupes_resolve", true;
    /// Cancels the open intake of one source (plan `cancel-source-intake` WP-1); writes `state.db` only.
    CancelSourceIntake = "cancel_source_intake", false;
    /// Daily vault tidiness: orphaned-attachment sweep, trash ledger seed and retention expiry
    /// (plan `tidiness-trash-orphans` WP-2). Trashes files, so it counts as mutating.
    Tidy = "tidy", true;
    /// One LLM structure + cleanup pass over an intake note, folding (never deleting) what the
    /// cleanup marks (plan `intake-auto-enrich` WP-3).
    IntakeEnrich = "intake_enrich", true;
    /// Refines a YouTube clip's `## Transcript` section into cleaned paragraphs with `###`
    /// chapter headings (plan `youtube-transcript-auto-refine` WP-1).
    TranscriptRefine = "transcript_refine", true;
    /// Deletes a YouTube clip's thumbnail embed and clipped description ahead of `## Transcript`
    /// (plan `round-19-yt-cleanup-intake` WP-2).
    YtClipTidy = "yt_clip_tidy", true;
}

/// The shared queue resource key of every supersede-engine caller (plan auto-intake-sprint WP-3):
/// no two intake jobs resolve and commit concurrently.
pub const INTAKE_IDENTITY_KEY: &str = "intake::identity";

/// The shared queue resource key of every attachment writer and the orphan sweep (plan
/// tidiness-trash-orphans WP-2, audit B4): `tidy` never interleaves with a localize run between
/// its attachment selection and its note commit. Held by `tidy`, `localize`, `track_mail` and
/// `track_blogs` (the steps that run the localize attachment writer).
pub const ATTACHMENTS_KEY: &str = "vault::attachments";

impl StepKind {
    /// Parses a TOML `type` string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Queue resource keys the step itself always needs, whatever the action config says (plan
    /// auto-intake-sprint WP-3, audit M3): every supersede-engine caller holds
    /// [`INTAKE_IDENTITY_KEY`] from resolve through create/record; every attachment writer and
    /// the orphan sweep hold [`ATTACHMENTS_KEY`].
    #[must_use]
    pub fn intrinsic_resource_keys(self) -> &'static [&'static str] {
        match self {
            StepKind::TrackBlogs | StepKind::TrackMail => &[ATTACHMENTS_KEY, INTAKE_IDENTITY_KEY],
            StepKind::Localize | StepKind::Tidy => &[ATTACHMENTS_KEY],
            StepKind::AutoFile | StepKind::IntakeReconcile | StepKind::IntakeFinalize => {
                &[INTAKE_IDENTITY_KEY]
            }
            _ => &[],
        }
    }
}

/// A typed step: its kind plus its params (the TOML table minus `type`), parsed and validated by
/// the kind's owning module in `cv`'s `steps/`. `run_action` params are checked here at load and
/// read through [`Step::run_action`].
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// The kind (`type`).
    pub kind: StepKind,
    /// The params table.
    pub params: toml::Table,
}

/// The parsed params of a `run_action` step.
#[derive(Debug, Clone, PartialEq)]
pub struct RunAction {
    /// Target action id.
    pub action_id: String,
    /// Params passed to the child run.
    pub params: Map<String, Value>,
}

impl RunAction {
    /// Parses a `run_action` step's params (`action`, optional `params` table).
    pub fn parse(t: &toml::Table) -> Result<Self, String> {
        let action_id = match t.get("action") {
            Some(toml::Value::String(s)) if !s.is_empty() => s.clone(),
            _ => return Err("run_action step needs a non-empty `action`".into()),
        };
        let params = match t.get("params") {
            None => Map::new(),
            Some(toml::Value::Table(p)) => match serde_json::to_value(p) {
                Ok(Value::Object(m)) => m,
                _ => return Err("run_action `params` is not representable".into()),
            },
            Some(_) => return Err("run_action `params` must be a table".into()),
        };
        if let Some(k) = t
            .keys()
            .find(|k| !matches!(k.as_str(), "action" | "params"))
        {
            return Err(format!("run_action step: unknown key {k:?}"));
        }
        Ok(RunAction { action_id, params })
    }
}

impl Step {
    /// The step's kind.
    #[must_use]
    pub fn kind(&self) -> StepKind {
        self.kind
    }

    /// Whether the step may write a note ([`StepKind::mutates_note`]).
    #[must_use]
    pub fn mutates_note(&self) -> bool {
        self.kind.mutates_note()
    }

    /// The parsed `run_action` params.
    pub fn run_action(&self) -> Result<RunAction, String> {
        RunAction::parse(&self.params)
    }

    /// Parses one `[[action.steps]]` table (`type` + params).
    pub fn from_table(mut t: toml::Table) -> Result<Self, String> {
        let kind = match t.remove("type") {
            Some(toml::Value::String(s)) => {
                StepKind::parse(&s).ok_or_else(|| format!("unknown step type {s:?}"))?
            }
            Some(_) => return Err("step `type` must be a string".into()),
            None => return Err("step is missing `type`".into()),
        };
        let step = Step { kind, params: t };
        if kind == StepKind::RunAction {
            step.run_action()?;
        }
        Ok(step)
    }
}

/// Origin filter of an event starter (default `external`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginSel {
    /// Only edits made outside the daemon.
    #[default]
    External,
    /// Only the daemon's own writes.
    Daemon,
    /// Both.
    Any,
}

/// A frontmatter property selector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropSelector {
    /// Property key.
    pub key: String,
    /// Required value (string form); absent = key present.
    #[serde(default)]
    pub value: Option<String>,
}

/// Event selector; every set field must match. All empty ⇒ the action is disarmed (I5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selector {
    /// Folder prefix (vault-relative, no trailing slash).
    #[serde(default)]
    pub folder: Option<String>,
    /// Tag (exact or nested parent, without `#`).
    #[serde(default)]
    pub tag: Option<String>,
    /// Frontmatter property.
    #[serde(default)]
    pub prop: Option<PropSelector>,
}

impl Selector {
    /// Whether no field constrains anything (I5: such an event action never arms).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let blank = |s: &Option<String>| s.as_deref().is_none_or(|v| v.trim().is_empty());
        blank(&self.folder)
            && blank(&self.tag)
            && self.prop.as_ref().is_none_or(|p| p.key.trim().is_empty())
    }

    /// Whether `path` is inside the folder selector (`true` when unset).
    #[must_use]
    pub fn folder_matches(&self, path: &str) -> bool {
        match self.folder.as_deref().map(|f| f.trim_matches('/')) {
            None | Some("") => true,
            Some(f) => path.strip_prefix(f).is_some_and(|r| r.starts_with('/')),
        }
    }
}

/// When a schedule fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// Daily at local `minute_of_day` (0..1440).
    Daily {
        /// Minutes after local midnight.
        minute_of_day: u32,
    },
    /// Every `period_ms`, aligned to the Unix epoch.
    Every {
        /// Period in milliseconds (> 0).
        period_ms: i64,
    },
}

impl Schedule {
    /// Parses `daily = "HH:MM"`.
    pub fn parse_daily(s: &str) -> Result<Self, String> {
        let (h, m) = s
            .split_once(':')
            .ok_or_else(|| format!("daily {s:?}: expected HH:MM"))?;
        let h: u32 = h.parse().map_err(|_| format!("daily {s:?}: bad hour"))?;
        let m: u32 = m.parse().map_err(|_| format!("daily {s:?}: bad minute"))?;
        if h > 23 || m > 59 {
            return Err(format!("daily {s:?}: out of range"));
        }
        Ok(Schedule::Daily {
            minute_of_day: h * 60 + m,
        })
    }

    /// Parses `every = "<n>m" | "<n>h"`.
    pub fn parse_every(s: &str) -> Result<Self, String> {
        let (num, unit) = s.split_at(s.len().saturating_sub(1));
        let n: i64 = num
            .parse()
            .map_err(|_| format!("every {s:?}: expected <n>m or <n>h"))?;
        let unit_ms = match unit {
            "m" => 60_000,
            "h" => 3_600_000,
            _ => return Err(format!("every {s:?}: unit must be m or h")),
        };
        if n <= 0 {
            return Err(format!("every {s:?}: must be positive"));
        }
        Ok(Schedule::Every {
            period_ms: n.saturating_mul(unit_ms),
        })
    }

    /// The most recent due time `<= now_ms`. `local_offset_ms` is the local UTC offset (daily
    /// schedules fire at local wall-clock time).
    #[must_use]
    pub fn most_recent_due(&self, now_ms: i64, local_offset_ms: i64) -> i64 {
        match *self {
            Schedule::Every { period_ms } => now_ms - now_ms.rem_euclid(period_ms),
            Schedule::Daily { minute_of_day } => {
                const DAY: i64 = 86_400_000;
                let local = now_ms + local_offset_ms;
                let today = local - local.rem_euclid(DAY) + i64::from(minute_of_day) * 60_000;
                let due_local = if today <= local { today } else { today - DAY };
                due_local - local_offset_ms
            }
        }
    }

    /// The next due time strictly after `now_ms`.
    #[must_use]
    pub fn next_due(&self, now_ms: i64, local_offset_ms: i64) -> i64 {
        match *self {
            Schedule::Every { period_ms } => self.most_recent_due(now_ms, 0) + period_ms,
            Schedule::Daily { .. } => self.most_recent_due(now_ms, local_offset_ms) + 86_400_000,
        }
    }
}

/// How a run starts.
#[derive(Debug, Clone, PartialEq)]
pub enum Starter {
    /// The `run_action` RPC (palette / key binding / CLI).
    Command,
    /// A live watcher event (never a startup replay, I4).
    Event {
        /// Selector (non-empty, I5).
        selector: Selector,
        /// Kinds listened to (non-empty).
        kinds: Vec<cv_protocol::ChangeKind>,
        /// Origin filter.
        origin: OriginSel,
    },
    /// Daily or interval schedule; a missed run catches up once.
    Schedule(Schedule),
}

/// I3 resource-key derivation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSpec {
    /// Include the triggering/active note path (default true).
    #[serde(default = "yes")]
    pub note: bool,
    /// Extra `kind::id` templates; `{path}` and `{param.NAME}` are substituted.
    #[serde(default)]
    pub keys: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Default for ResourceSpec {
    fn default() -> Self {
        Self {
            note: true,
            keys: Vec::new(),
        }
    }
}

impl ResourceSpec {
    /// Derives the keys for one run. A template naming an absent value is an error (fail
    /// closed, never a key with a hole in it).
    pub fn derive(
        &self,
        path: Option<&str>,
        params: &Map<String, Value>,
    ) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        if self.note
            && let Some(p) = path.filter(|p| !p.is_empty())
        {
            out.push(p.to_owned());
        }
        for t in &self.keys {
            out.push(expand(t, path, params)?);
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}

fn expand(t: &str, path: Option<&str>, params: &Map<String, Value>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = t;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("resource key {t:?}: unclosed `{{`"))?;
        let name = &after[..end];
        let value = if name == "path" {
            path.filter(|p| !p.is_empty()).map(str::to_owned)
        } else if let Some(p) = name.strip_prefix("param.") {
            match params.get(p) {
                Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
                Some(Value::Number(n)) => Some(n.to_string()),
                Some(Value::Bool(b)) => Some(b.to_string()),
                _ => None,
            }
        } else {
            return Err(format!(
                "resource key {t:?}: unknown placeholder {{{name}}}"
            ));
        };
        let v = value.ok_or_else(|| format!("resource key {t:?}: {{{name}}} has no value"))?;
        out.push_str(&v);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn action(note: bool, steps: &[&str]) -> Action {
        Action {
            id: "a".into(),
            label: "A".into(),
            group: None,
            keys: Vec::new(),
            params: Vec::new(),
            needs: Vec::new(),
            lane: LaneChoice::Auto,
            durable: true,
            steps: steps
                .iter()
                .map(|s| Step::from_table(toml::from_str(s).unwrap()).unwrap())
                .collect(),
            starters: Vec::new(),
            resources: ResourceSpec {
                note,
                keys: Vec::new(),
            },
        }
    }

    /// Plan round-18 Decision 1: the hold predicate table.
    #[test]
    fn holds_note_only_for_agent_then_effective_replace() {
        let presets = |n: &str| match n {
            "replace-note" => Some("replace".to_owned()),
            "log" => Some("append".to_owned()),
            _ => None,
        };
        let agent = "type = \"agent\"\nagent = \"x\"";
        let cases: &[(bool, &[&str], bool)] = &[
            // agent → explicit replace.
            (
                true,
                &[agent, "type = \"capture\"\nmode = \"replace\""],
                true,
            ),
            // agent → preset-supplied replace.
            (
                true,
                &[agent, "type = \"capture\"\ncapture = \"replace-note\""],
                true,
            ),
            // agent → preset replace overridden by the step's own append.
            (
                true,
                &[
                    agent,
                    "type = \"capture\"\ncapture = \"replace-note\"\nmode = \"append\"",
                ],
                false,
            ),
            // replace without an agent (capture.replace-note).
            (
                true,
                &["type = \"capture\"\ncapture = \"replace-note\""],
                false,
            ),
            // capture before the agent.
            (
                true,
                &["type = \"capture\"\nmode = \"replace\"", agent],
                false,
            ),
            // note = false.
            (
                false,
                &[agent, "type = \"capture\"\nmode = \"replace\""],
                false,
            ),
            // command-only / non-capture actions.
            (true, &["type = \"guard\"", agent], false),
            // agent → default (append) and unknown preset.
            (true, &[agent, "type = \"capture\""], false),
            (
                true,
                &[agent, "type = \"capture\"\ncapture = \"nope\""],
                false,
            ),
            (
                true,
                &[agent, "type = \"capture\"\ncapture = \"log\""],
                false,
            ),
        ];
        for (i, (note, steps, want)) in cases.iter().enumerate() {
            assert_eq!(action(*note, steps).holds_note(presets), *want, "case {i}");
        }
    }

    #[test]
    fn schedules_parse_and_due_times() {
        assert_eq!(
            Schedule::parse_daily("07:30").unwrap(),
            Schedule::Daily { minute_of_day: 450 }
        );
        assert!(Schedule::parse_daily("24:00").is_err());
        assert_eq!(
            Schedule::parse_every("15m").unwrap(),
            Schedule::Every { period_ms: 900_000 }
        );
        assert!(Schedule::parse_every("0h").is_err());
        assert!(Schedule::parse_every("5s").is_err());
        let e = Schedule::Every { period_ms: 1000 };
        assert_eq!(e.most_recent_due(2500, 0), 2000);
        assert_eq!(e.next_due(2500, 0), 3000);
        let d = Schedule::Daily { minute_of_day: 60 };
        // 00:30 UTC day 1 → due is 01:00 of day 0.
        let now = 86_400_000 + 30 * 60_000;
        assert_eq!(d.most_recent_due(now, 0), 3_600_000);
        // At UTC+1 it is 01:30 local: due 01:00 local = 00:00 UTC day 1.
        assert_eq!(d.most_recent_due(now, 3_600_000), 86_400_000);
    }

    #[test]
    fn resource_keys_derive_and_fail_closed() {
        let spec = ResourceSpec {
            note: true,
            keys: vec!["kind::{param.id}".into()],
        };
        let p = json!({"id": "x"});
        let m = p.as_object().unwrap();
        assert_eq!(
            spec.derive(Some("a.md"), m).unwrap(),
            vec!["a.md".to_owned(), "kind::x".to_owned()]
        );
        assert!(spec.derive(None, &Map::new()).is_err());
        assert!(ResourceSpec::default().derive(None, m).unwrap().is_empty());
    }

    #[test]
    fn selector_emptiness_and_folder() {
        assert!(Selector::default().is_empty());
        let s = Selector {
            folder: Some("inbox".into()),
            ..Selector::default()
        };
        assert!(!s.is_empty());
        assert!(s.folder_matches("inbox/a.md"));
        assert!(!s.folder_matches("inboxes/a.md"));
        assert!(!s.folder_matches("inbox"));
    }

    #[test]
    fn step_tables_parse() {
        let t: toml::Table = toml::from_str("type = \"set_props\"\nx = 1").unwrap();
        let s = Step::from_table(t).unwrap();
        assert_eq!(s.kind(), StepKind::SetProps);
        assert!(s.params.contains_key("x"));
        let t: toml::Table =
            toml::from_str("type = \"run_action\"\naction = \"b\"\nparams = { k = \"v\" }")
                .unwrap();
        assert_eq!(
            Step::from_table(t).unwrap().run_action().unwrap(),
            RunAction {
                action_id: "b".into(),
                params: json!({"k": "v"}).as_object().unwrap().clone()
            }
        );
        for k in StepKind::ALL {
            assert_eq!(StepKind::parse(k.as_str()), Some(k));
        }
        let t: toml::Table = toml::from_str("type = \"track_blogs\"").unwrap();
        let s = Step::from_table(t).unwrap();
        assert_eq!(s.kind(), StepKind::TrackBlogs);
        assert!(s.mutates_note());
        let t: toml::Table = toml::from_str("type = \"x_backfill\"").unwrap();
        assert!(Step::from_table(t).unwrap().mutates_note());
        let t: toml::Table = toml::from_str("type = \"link_scan\"").unwrap();
        assert!(!Step::from_table(t).unwrap().mutates_note());
        let t: toml::Table = toml::from_str("type = \"nope\"").unwrap();
        assert!(Step::from_table(t).is_err());
    }
}
