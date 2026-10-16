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
    pub needs: Vec<String>,
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

    /// Whether the action can be started by `run_action` (has a command starter).
    #[must_use]
    pub fn has_command_starter(&self) -> bool {
        self.starters.iter().any(|s| matches!(s, Starter::Command))
    }
}

/// Step kind tag (the TOML `type` value).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Guard,
    Capture,
    Materialize,
    Move,
    Copy,
    SetProps,
    RemoveProps,
    UpsertTags,
    Lint,
    Localize,
    Agent,
    MarkForwarded,
    RunAction,
    SearchUpsert,
    SearchDelete,
    // A2 (plan v0-automation-pipelines WP-1).
    LinkScan,
    YtMetadata,
    YtChannelEnrich,
    XDiscover,
    XMetadata,
    XBackfill,
    ImageDescribe,
    TrackYoutube,
    TrackBlogs,
    IgnoreVideo,
    DailyBrief,
}

impl StepKind {
    /// Every kind.
    pub const ALL: [StepKind; 26] = [
        StepKind::Guard,
        StepKind::Capture,
        StepKind::Materialize,
        StepKind::Move,
        StepKind::Copy,
        StepKind::SetProps,
        StepKind::RemoveProps,
        StepKind::UpsertTags,
        StepKind::Lint,
        StepKind::Localize,
        StepKind::Agent,
        StepKind::MarkForwarded,
        StepKind::RunAction,
        StepKind::SearchUpsert,
        StepKind::SearchDelete,
        StepKind::LinkScan,
        StepKind::YtMetadata,
        StepKind::YtChannelEnrich,
        StepKind::XDiscover,
        StepKind::XMetadata,
        StepKind::XBackfill,
        StepKind::ImageDescribe,
        StepKind::TrackYoutube,
        StepKind::TrackBlogs,
        StepKind::IgnoreVideo,
        StepKind::DailyBrief,
    ];

    /// The TOML `type` string.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Guard => "guard",
            StepKind::Capture => "capture",
            StepKind::Materialize => "materialize",
            StepKind::Move => "move",
            StepKind::Copy => "copy",
            StepKind::SetProps => "set_props",
            StepKind::RemoveProps => "remove_props",
            StepKind::UpsertTags => "upsert_tags",
            StepKind::Lint => "lint",
            StepKind::Localize => "localize",
            StepKind::Agent => "agent",
            StepKind::MarkForwarded => "mark_forwarded",
            StepKind::RunAction => "run_action",
            StepKind::SearchUpsert => "search_upsert",
            StepKind::SearchDelete => "search_delete",
            StepKind::LinkScan => "link_scan",
            StepKind::YtMetadata => "yt_metadata",
            StepKind::YtChannelEnrich => "yt_channel_enrich",
            StepKind::XDiscover => "x_discover",
            StepKind::XMetadata => "x_metadata",
            StepKind::XBackfill => "x_backfill",
            StepKind::ImageDescribe => "image_describe",
            StepKind::TrackYoutube => "track_youtube",
            StepKind::TrackBlogs => "track_blogs",
            StepKind::IgnoreVideo => "ignore_video",
            StepKind::DailyBrief => "daily_brief",
        }
    }

    /// Parses a TOML `type` string.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }
}

/// A typed step. Every A1 variant is declared up front (WP-10..13 fill disjoint modules); A2 adds
/// more. Each variant except [`Step::RunAction`] carries its params opaquely (the TOML table minus
/// `type`), parsed and validated by its owning module in `cv`'s `steps/`.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Guard(toml::Table),
    Capture(toml::Table),
    Materialize(toml::Table),
    Move(toml::Table),
    Copy(toml::Table),
    SetProps(toml::Table),
    RemoveProps(toml::Table),
    UpsertTags(toml::Table),
    Lint(toml::Table),
    Localize(toml::Table),
    Agent(toml::Table),
    MarkForwarded(toml::Table),
    /// Enqueue another action as a child job (replaces Crucible sub-chains, `src/chains.ts`).
    RunAction {
        /// Target action id.
        action_id: String,
        /// Params passed to the child run.
        params: Map<String, Value>,
    },
    SearchUpsert(toml::Table),
    SearchDelete(toml::Table),
    // A2 pipeline steps (plan v0-automation-pipelines; bodies in WP-3..10).
    LinkScan(toml::Table),
    YtMetadata(toml::Table),
    YtChannelEnrich(toml::Table),
    XDiscover(toml::Table),
    XMetadata(toml::Table),
    XBackfill(toml::Table),
    ImageDescribe(toml::Table),
    TrackYoutube(toml::Table),
    TrackBlogs(toml::Table),
    IgnoreVideo(toml::Table),
    DailyBrief(toml::Table),
}

impl Step {
    /// The step's kind.
    #[must_use]
    pub fn kind(&self) -> StepKind {
        match self {
            Step::Guard(_) => StepKind::Guard,
            Step::Capture(_) => StepKind::Capture,
            Step::Materialize(_) => StepKind::Materialize,
            Step::Move(_) => StepKind::Move,
            Step::Copy(_) => StepKind::Copy,
            Step::SetProps(_) => StepKind::SetProps,
            Step::RemoveProps(_) => StepKind::RemoveProps,
            Step::UpsertTags(_) => StepKind::UpsertTags,
            Step::Lint(_) => StepKind::Lint,
            Step::Localize(_) => StepKind::Localize,
            Step::Agent(_) => StepKind::Agent,
            Step::MarkForwarded(_) => StepKind::MarkForwarded,
            Step::RunAction { .. } => StepKind::RunAction,
            Step::SearchUpsert(_) => StepKind::SearchUpsert,
            Step::SearchDelete(_) => StepKind::SearchDelete,
            Step::LinkScan(_) => StepKind::LinkScan,
            Step::YtMetadata(_) => StepKind::YtMetadata,
            Step::YtChannelEnrich(_) => StepKind::YtChannelEnrich,
            Step::XDiscover(_) => StepKind::XDiscover,
            Step::XMetadata(_) => StepKind::XMetadata,
            Step::XBackfill(_) => StepKind::XBackfill,
            Step::ImageDescribe(_) => StepKind::ImageDescribe,
            Step::TrackYoutube(_) => StepKind::TrackYoutube,
            Step::TrackBlogs(_) => StepKind::TrackBlogs,
            Step::IgnoreVideo(_) => StepKind::IgnoreVideo,
            Step::DailyBrief(_) => StepKind::DailyBrief,
        }
    }

    /// The opaque params table (`None` for [`Step::RunAction`]).
    #[must_use]
    pub fn table(&self) -> Option<&toml::Table> {
        match self {
            Step::Guard(t)
            | Step::Capture(t)
            | Step::Materialize(t)
            | Step::Move(t)
            | Step::Copy(t)
            | Step::SetProps(t)
            | Step::RemoveProps(t)
            | Step::UpsertTags(t)
            | Step::Lint(t)
            | Step::Localize(t)
            | Step::Agent(t)
            | Step::MarkForwarded(t)
            | Step::SearchUpsert(t)
            | Step::SearchDelete(t)
            | Step::LinkScan(t)
            | Step::YtMetadata(t)
            | Step::YtChannelEnrich(t)
            | Step::XDiscover(t)
            | Step::XMetadata(t)
            | Step::XBackfill(t)
            | Step::ImageDescribe(t)
            | Step::TrackYoutube(t)
            | Step::TrackBlogs(t)
            | Step::IgnoreVideo(t)
            | Step::DailyBrief(t) => Some(t),
            Step::RunAction { .. } => None,
        }
    }

    /// Whether the step may write a note (replaces Crucible's `mutating` flag, `src/AGENTS.md:28`).
    /// Fails closed: anything that might write counts.
    /// A2: every pipeline step mutates (fails closed) except `x_discover` and `x_backfill`,
    /// which only enqueue `x_metadata` runs.
    #[must_use]
    pub fn mutates_note(&self) -> bool {
        match self.kind() {
            StepKind::Guard
            | StepKind::RunAction
            | StepKind::SearchUpsert
            | StepKind::SearchDelete
            | StepKind::XDiscover
            | StepKind::XBackfill => false,
            StepKind::Capture
            | StepKind::Materialize
            | StepKind::Move
            | StepKind::Copy
            | StepKind::SetProps
            | StepKind::RemoveProps
            | StepKind::UpsertTags
            | StepKind::Lint
            | StepKind::Localize
            | StepKind::Agent
            | StepKind::MarkForwarded
            | StepKind::LinkScan
            | StepKind::YtMetadata
            | StepKind::YtChannelEnrich
            | StepKind::XMetadata
            | StepKind::ImageDescribe
            | StepKind::TrackYoutube
            | StepKind::TrackBlogs
            | StepKind::IgnoreVideo
            | StepKind::DailyBrief => true,
        }
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
        Ok(match kind {
            StepKind::Guard => Step::Guard(t),
            StepKind::Capture => Step::Capture(t),
            StepKind::Materialize => Step::Materialize(t),
            StepKind::Move => Step::Move(t),
            StepKind::Copy => Step::Copy(t),
            StepKind::SetProps => Step::SetProps(t),
            StepKind::RemoveProps => Step::RemoveProps(t),
            StepKind::UpsertTags => Step::UpsertTags(t),
            StepKind::Lint => Step::Lint(t),
            StepKind::Localize => Step::Localize(t),
            StepKind::Agent => Step::Agent(t),
            StepKind::MarkForwarded => Step::MarkForwarded(t),
            StepKind::SearchUpsert => Step::SearchUpsert(t),
            StepKind::SearchDelete => Step::SearchDelete(t),
            StepKind::LinkScan => Step::LinkScan(t),
            StepKind::YtMetadata => Step::YtMetadata(t),
            StepKind::YtChannelEnrich => Step::YtChannelEnrich(t),
            StepKind::XDiscover => Step::XDiscover(t),
            StepKind::XMetadata => Step::XMetadata(t),
            StepKind::XBackfill => Step::XBackfill(t),
            StepKind::ImageDescribe => Step::ImageDescribe(t),
            StepKind::TrackYoutube => Step::TrackYoutube(t),
            StepKind::TrackBlogs => Step::TrackBlogs(t),
            StepKind::IgnoreVideo => Step::IgnoreVideo(t),
            StepKind::DailyBrief => Step::DailyBrief(t),
            StepKind::RunAction => {
                let action_id = match t.remove("action") {
                    Some(toml::Value::String(s)) if !s.is_empty() => s,
                    _ => return Err("run_action step needs a non-empty `action`".into()),
                };
                let params = match t.remove("params") {
                    None => Map::new(),
                    Some(toml::Value::Table(p)) => match serde_json::to_value(p) {
                        Ok(Value::Object(m)) => m,
                        _ => return Err("run_action `params` is not representable".into()),
                    },
                    Some(_) => return Err("run_action `params` must be a table".into()),
                };
                if let Some(k) = t.keys().next() {
                    return Err(format!("run_action step: unknown key {k:?}"));
                }
                Step::RunAction { action_id, params }
            }
        })
    }
}

/// Event kinds an event starter listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKindSel {
    Created,
    Modified,
    Deleted,
    Renamed,
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
        kinds: Vec<ChangeKindSel>,
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
        assert!(s.table().unwrap().contains_key("x"));
        let t: toml::Table =
            toml::from_str("type = \"run_action\"\naction = \"b\"\nparams = { k = \"v\" }")
                .unwrap();
        assert_eq!(
            Step::from_table(t).unwrap(),
            Step::RunAction {
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
        assert!(!Step::from_table(t).unwrap().mutates_note());
        let t: toml::Table = toml::from_str("type = \"nope\"").unwrap();
        assert!(Step::from_table(t).is_err());
    }
}
