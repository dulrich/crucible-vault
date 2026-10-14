//! Event starter (pin 9): live `file_changed` notifications → background runs.
//!
//! Port of Crucible's trigger layer (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/TriggerRegistry.ts`,
//! `src/triggers/scopeMatch.ts`, `src/triggers/triggerAdapter.ts`) reduced to what is not
//! Obsidian-induced: selector matching (folder/tag/prop), an origin filter and the storm guards
//! of I5 (`src/orchestration/AGENTS.md:77`). Dropped: the metadata-cache consistency wait
//! (`waitForConsistentCache` — the daemon's index is updated before the event is sent), the
//! `onLayoutReady` start (no startup replay exists, I4), and plugin-managed-path exclusion (the
//! queue no longer lives in the vault).

use cv_actions::{ChangeKindSel, OriginSel, Selector, Starter};
use cv_protocol::{ActionContext, ChangeKind, Notification, Origin};
use cv_state::Lane;
use serde_json::{Map, Value};
use tokio::sync::{broadcast, watch};

use super::{RunRequest, Runner, Trigger};

pub(super) async fn event_loop(
    runner: Runner,
    mut rx: broadcast::Receiver<Notification>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            r = rx.recv() => match r {
                Ok(Notification::FileChanged { path, kind, origin, .. }) => {
                    let rn = runner.clone();
                    let res = tokio::task::spawn_blocking(move || on_change(&rn, &path, kind, origin)).await;
                    if let Err(e) = res {
                        tracing::error!(error = %e, "event starter panicked");
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(k)) => {
                    tracing::warn!(skipped = k, "event starter lagged; events dropped");
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
        }
    }
}

fn kind_sel(k: ChangeKind) -> ChangeKindSel {
    match k {
        ChangeKind::Created => ChangeKindSel::Created,
        ChangeKind::Modified => ChangeKindSel::Modified,
        ChangeKind::Deleted => ChangeKindSel::Deleted,
        ChangeKind::Renamed => ChangeKindSel::Renamed,
    }
}

fn origin_ok(sel: OriginSel, o: Origin) -> bool {
    matches!(
        (sel, o),
        (OriginSel::Any, _)
            | (OriginSel::External, Origin::External)
            | (OriginSel::Daemon, Origin::Daemon)
    )
}

/// Handles one change synchronously; returns the job ids enqueued.
pub(crate) fn on_change(
    runner: &Runner,
    path: &str,
    kind: ChangeKind,
    origin: Origin,
) -> Vec<String> {
    let config = runner.config();
    let mut out = Vec::new();
    let mut reader: Option<cv_index::Reader> = None;
    for action in config.armed_actions() {
        let fires = action.starters.iter().any(|s| match s {
            Starter::Event {
                selector,
                kinds,
                origin: o,
            } => {
                kinds.contains(&kind_sel(kind))
                    && origin_ok(*o, origin)
                    && selector_matches(runner, selector, path, &mut reader)
            }
            _ => false,
        });
        if !fires {
            continue;
        }
        if origin == Origin::Daemon && runner.cycle_blocked(&action.id, path) {
            tracing::info!(action = %action.id, "cycle guard: daemon write does not re-trigger");
            continue;
        }
        let mut params = Map::new();
        super::apply_defaults(action, &mut params);
        let req = RunRequest {
            params,
            context: ActionContext {
                active_path: Some(path.to_owned()),
                ..ActionContext::default()
            },
            trigger: Trigger::Event {
                path: path.to_owned(),
                change: kind_sel_str(kind).to_owned(),
            },
            chain: Vec::new(),
            default_lane: Lane::Background,
            dedupe_key: Some(format!("{}::{path}", action.id)),
        };
        match runner.enqueue(action, req) {
            Ok(id) => out.push(id),
            Err(e) => tracing::warn!(action = %action.id, error = %e, "event run refused"),
        }
    }
    out
}

fn kind_sel_str(k: ChangeKind) -> &'static str {
    match k {
        ChangeKind::Created => "created",
        ChangeKind::Modified => "modified",
        ChangeKind::Deleted => "deleted",
        ChangeKind::Renamed => "renamed",
    }
}

/// Every set selector field must match. Tag/prop look the note up in the index; an index error
/// is a non-match (fail closed: no run).
fn selector_matches(
    runner: &Runner,
    sel: &Selector,
    path: &str,
    reader: &mut Option<cv_index::Reader>,
) -> bool {
    if sel.is_empty() || !sel.folder_matches(path) {
        return false;
    }
    let tag = sel
        .tag
        .as_deref()
        .map(|t| t.trim_start_matches('#'))
        .filter(|t| !t.is_empty());
    let prop = sel.prop.as_ref().filter(|p| !p.key.is_empty());
    if tag.is_none() && prop.is_none() {
        return true;
    }
    if reader.is_none() {
        match cv_index::Reader::open_current(&runner.inner().index_db) {
            Ok(r) => *reader = Some(r),
            Err(e) => {
                tracing::warn!(error = %e, "event selector: index unavailable");
                return false;
            }
        }
    }
    let Some(r) = reader.as_ref() else {
        return false;
    };
    let q = r.query();
    if let Some(t) = tag {
        let ok = q.tags_of(path).is_ok_and(|rows| {
            rows.iter().any(|row| {
                let rt = row.tag.trim_start_matches('#');
                rt == t || rt.strip_prefix(t).is_some_and(|rest| rest.starts_with('/'))
            })
        });
        if !ok {
            return false;
        }
    }
    if let Some(p) = prop {
        let ok = q.props(path).is_ok_and(|rows| {
            rows.iter().any(|row| {
                row.key == p.key
                    && p.value
                        .as_deref()
                        .is_none_or(|want| prop_eq(&row.value, want))
            })
        });
        if !ok {
            return false;
        }
    }
    true
}

fn prop_eq(v: &Value, want: &str) -> bool {
    match v {
        Value::String(s) => s == want,
        Value::Array(a) => a.iter().any(|x| prop_eq(x, want)),
        Value::Null => false,
        other => serde_json::from_str::<Value>(want).is_ok_and(|w| &w == other),
    }
}
