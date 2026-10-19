//! Step module `capture`: render a `{{value}}` template and append/prepend/replace it into a
//! target note (whole note or a `## Section`).
//!
//! Ported from Crucible `src/captures.ts:31-168` (`executeCapture`: target resolution, the
//! `daily-next` target that materializes a missing note first, template rendering, the two
//! empty-body guards, frontmatter stripping, whole-note replace/prepend/append, section insert),
//! `src/captures.ts:206-218` (`resolveTargetSection`, `targetSectionMode = source`) and
//! `src/captureCommands.ts:50-58,140-146` (the `line` source and the source-section lookup).
//! Dropped (Obsidian-induced): the `setMaterializing` flag, `Notice` toasts, the editor/dialog
//! value prompts (the client supplies `value`/`selection`/`line` in the action context, or
//! the palette prompts for a `value` param), and the `TFile instanceof` checks.
//!
//! # Step params
//!
//! A step may name a `[[capture]]` entry of `captures.toml` with `capture = "NAME"`; the
//! entry's keys are the defaults and the step's own keys override them.
//!
//! | key | type | default | meaning |
//! |---|---|---|---|
//! | `capture` | string | — | `[[capture]] name` in `captures.toml` |
//! | `target` | `daily` \| `daily_next` \| `weekly` \| `monthly` \| `active` \| `path` | `active` | where to write (`day`/`week`/`month` accepted) |
//! | `path` | string (placeholders) | — | target for `target = "path"` |
//! | `mode` | `append` \| `prepend` \| `replace` \| `section` | `append` | `section` = append into `section` |
//! | `section` | string, e.g. `"## Log"` | — (whole note) | heading anchoring the insert (missing section is appended) |
//! | `section_mode` | `fixed` \| `source` | `fixed` | `source`: use the heading above the source line when the target has it |
//! | `format` | `{{…}}` template | `"{{value}}"` | `{{value}}`, `{{value:oneline}}`, `{{date}}`, `{{time}}`, `{{source_link}}`, `{{vars.NAME}}`, `{{param.NAME}}` |
//! | `source` | `value` \| `line` | `value` | `line`: the task line at `(context.active_path, context.line)` (0-based), fail closed when not a task |
//! | `value` | string (placeholders) | `params.value`, then `context.value`, then `context.selection` | the captured value |
//! | `name`, `label` | string | — | descriptive (captures.toml entries) |
//!
//! Period targets materialize a missing note first (period settings in `captures.toml`
//! `[periods.*]`, see [`super::materialize`]); `daily_next` is the day after the source daily note,
//! never earlier than today (`cv_text::resolve_next_daily_date`).
//!
//! Idempotence (E2): before inserting, the step checks whether the exact rendered block already
//! sits at the target position (end of note/section for append, start for prepend, identical
//! result for replace) and then writes nothing. Consequence: capturing the identical block twice
//! in a row at the same position is a no-op.
//!
//! Outputs: `vars.captured_path`, `output_paths` when written.

use std::sync::LazyLock;

use chrono::NaiveDateTime;
use cv_actions::{ConfigSet, StepKind};
use cv_text::sections::{SectionInsertMode, find_section_range, insert_into_section};
use regex::Regex;
use serde::Serialize;

use cv_text::vpath::stem;

use super::materialize::{canonical_period, ensure_note, now, period};
use super::params::{check_keys, check_placeholders, expand, get_str, value_text};
use super::{StepCtx, StepError, StepModule, StepOutcome};

/// Open checkbox (`src/markForwarded.ts:3`).
pub(super) static OPEN_TASK: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^(\s*[-*+]\s+\[) (\]\s+.*)$").ok());
/// Already-forwarded checkbox.
pub(super) static FORWARDED_TASK: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^(\s*[-*+]\s+\[)>(\]\s+.*)$").ok());

fn re(r: &LazyLock<Option<Regex>>) -> Result<&Regex, StepError> {
    r.as_ref()
        .ok_or_else(|| StepError::failed("internal regex failed to compile"))
}

/// The `[[capture]]` entry named `name` in `captures.toml`.
fn preset_def<'c>(config: &'c ConfigSet, name: &str) -> Result<&'c toml::Table, String> {
    let entries = match config.table("captures.toml").get("capture") {
        None => &[][..],
        Some(toml::Value::Array(a)) => a.as_slice(),
        Some(_) => return Err("captures.toml `capture` must be an array of tables".into()),
    };
    let def = entries
        .iter()
        .filter_map(toml::Value::as_table)
        .find(|e| e.get("name").and_then(toml::Value::as_str) == Some(name))
        .ok_or_else(|| format!("capture {name:?} is not defined in captures.toml"))?;
    check_keys(def, StepKind::Capture, "[[capture]]")?;
    Ok(def)
}

/// Parsed `capture` params: the named `[[capture]]` entry overlaid by the step's keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Params {
    pub target: Target,
    pub mode: Mode,
    pub path: Option<String>,
    pub section: Option<String>,
    /// `section_mode = "source"`.
    pub section_from_source: bool,
    /// `None` = `{{value}}`.
    pub format: Option<String>,
    /// `source = "line"`.
    pub source_line: bool,
    pub value: Option<String>,
}

impl Params {
    /// The params of the `[[capture]]` preset `name` alone (inline captures, e.g. the tracker).
    pub(crate) fn preset(config: &ConfigSet, name: &str) -> Result<Self, String> {
        parse_merged(preset_def(config, name)?, config)
    }
}

/// The `capture` step.
pub(crate) struct Capture;

impl StepModule for Capture {
    type Params = Params;

    fn parse(kind: StepKind, t: &toml::Table, config: &ConfigSet) -> Result<Params, String> {
        check_keys(t, kind, "capture")?;
        let Some(name) = get_str(t, "capture")? else {
            return parse_merged(t, config);
        };
        let mut merged = preset_def(config, &name)?.clone();
        for (k, v) in t {
            merged.insert(k.clone(), v.clone());
        }
        parse_merged(&merged, config)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(&p, ctx)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Period(&'static str),
    DailyNext,
    Active,
    Path,
}

pub(crate) fn parse_target(s: &str) -> Result<Target, String> {
    match s {
        "active" => Ok(Target::Active),
        "path" => Ok(Target::Path),
        "daily_next" | "daily-next" => Ok(Target::DailyNext),
        other => canonical_period(other)
            .map(Target::Period)
            .ok_or_else(|| format!("capture: unknown target {other:?}")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Append,
    Prepend,
    Replace,
}

pub(crate) fn parse_mode(s: &str) -> Result<(Mode, bool), String> {
    match s {
        "append" => Ok((Mode::Append, false)),
        "prepend" => Ok((Mode::Prepend, false)),
        "replace" => Ok((Mode::Replace, false)),
        "section" => Ok((Mode::Append, true)),
        other => Err(format!("capture: unknown mode {other:?}")),
    }
}

/// Parses the effective (preset-merged) params.
fn parse_merged(t: &toml::Table, config: &ConfigSet) -> Result<Params, String> {
    for k in [
        "target",
        "path",
        "mode",
        "section",
        "section_mode",
        "format",
        "source",
        "value",
        "name",
        "label",
    ] {
        get_str(t, k)?;
    }
    let target = parse_target(&get_str(t, "target")?.unwrap_or_else(|| "active".into()))?;
    let path = get_str(t, "path")?;
    match target {
        Target::Period(p) => period(config, p, None).map(|_| ())?,
        Target::DailyNext => period(config, "day", None).map(|_| ())?,
        Target::Path => match &path {
            Some(p) if !p.trim().is_empty() => check_placeholders(p)?,
            _ => return Err("capture target=path needs `path`".into()),
        },
        Target::Active => {}
    }
    let (mode, needs_section) =
        parse_mode(&get_str(t, "mode")?.unwrap_or_else(|| "append".into()))?;
    let section = get_str(t, "section")?;
    if needs_section && section.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return Err("capture mode=section needs `section`".into());
    }
    let section_from_source = match get_str(t, "section_mode")?.as_deref() {
        None | Some("fixed") => false,
        Some("source") => true,
        Some(o) => return Err(format!("capture: unknown section_mode {o:?}")),
    };
    let source_line = match get_str(t, "source")?.as_deref() {
        None | Some("value") => false,
        Some("line") => true,
        Some(o) => return Err(format!("capture: unknown source {o:?}")),
    };
    let value = get_str(t, "value")?;
    if let Some(v) = &value {
        check_placeholders(v)?;
    }
    Ok(Params {
        target,
        mode,
        path,
        section,
        section_from_source,
        format: get_str(t, "format")?,
        source_line,
        value,
    })
}

/// The task line at `(active_path, line)` and the heading above it.
fn source_line(ctx: &StepCtx<'_>) -> Result<(String, Option<String>), StepError> {
    let path = ctx
        .context
        .active_path
        .as_deref()
        .filter(|p| !p.is_empty())
        .ok_or_else(|| StepError::failed("capture source=line needs an active path"))?;
    let line = ctx
        .context
        .line
        .ok_or_else(|| StepError::failed("capture source=line needs a context line"))?;
    let (bytes, _) = ctx
        .read(path)?
        .ok_or_else(|| StepError::failed("capture source note not found"))?;
    let text = String::from_utf8(bytes).map_err(|_| StepError::failed("source not UTF-8"))?;
    let lines: Vec<&str> = text.split('\n').collect();
    let idx = usize::try_from(line).map_err(|_| StepError::failed("line out of range"))?;
    let raw = lines
        .get(idx)
        .map(|l| l.trim_end_matches('\r'))
        .ok_or_else(|| StepError::failed("capture source line out of range"))?;
    // Fail closed: only a task (open, or already forwarded by an earlier run) is forwarded; a
    // forwarded line is captured as the open task it was.
    let open = if re(&OPEN_TASK)?.is_match(raw) {
        raw.to_owned()
    } else if re(&FORWARDED_TASK)?.is_match(raw) {
        re(&FORWARDED_TASK)?.replace(raw, "$1 $2").into_owned()
    } else {
        return Err(StepError::failed("capture source line is not a task"));
    };
    let heading = Regex::new(r"^#{1,6}\s+\S")
        .map_err(|e| StepError::failed(e.to_string()))
        .map(|h| {
            lines[..=idx]
                .iter()
                .rev()
                .map(|l| l.trim())
                .find(|l| h.is_match(l))
                .map(str::to_owned)
        })?;
    Ok((open.trim().to_owned(), heading))
}

/// Whether `block` already sits at the end of `hay` (line-boundary aligned).
fn ends_with_block(hay: &str, block: &str) -> bool {
    let h = hay.trim_end();
    h == block || h.ends_with(&format!("\n{block}"))
}

/// Whether `block` already sits at the start of `hay`.
fn starts_with_block(hay: &str, block: &str) -> bool {
    let h = hay.trim_start();
    h == block || h.starts_with(&format!("{block}\n"))
}

/// The new note text, or `None` when the block is already in place (E2) / nothing to do.
fn apply(
    existing: &str,
    content: &str,
    content_raw: &str,
    mode: Mode,
    section: Option<&str>,
) -> Option<String> {
    if let Some(sec) = section {
        if let Some(r) = find_section_range(existing, sec) {
            let lines: Vec<&str> = existing.split('\n').collect();
            let body = lines[r.header_index + 1..r.end_index].join("\n");
            let present = match mode {
                Mode::Append => ends_with_block(&body, content),
                Mode::Prepend => starts_with_block(&body, content),
                Mode::Replace => body.trim() == content,
            };
            if present {
                return None;
            }
        }
        let m = match mode {
            Mode::Append => SectionInsertMode::Append,
            Mode::Prepend => SectionInsertMode::Prepend,
            Mode::Replace => SectionInsertMode::Replace,
        };
        let out = insert_into_section(existing, sec, content, m);
        return (out != existing).then_some(out);
    }
    // (block through its closing fence line — BOM included, body offset); Crucible's
    // `FRONTMATTER_REGEX` (`src/utils.ts:28`) is replaced by the canonical `cv_note::span` (B10).
    let yaml = cv_note::span(existing).map(|r| (&existing[..r.end], r.end));
    let out = match mode {
        Mode::Replace => match yaml {
            Some((block, _)) => format!("{}\n\n{content}", block.trim_end()),
            None => content_raw.to_owned(),
        },
        Mode::Prepend => {
            let (head, body) = match yaml {
                Some((block, end)) => (
                    format!("{}\n\n", block.trim_end()),
                    existing[end..].trim_start(),
                ),
                None => (String::new(), existing.trim_start()),
            };
            if starts_with_block(body, content) {
                return None;
            }
            let sep = if body.is_empty() { "" } else { "\n\n" };
            format!("{head}{content}{sep}{body}")
        }
        Mode::Append => {
            let body = yaml.map_or(existing, |(_, end)| &existing[end..]);
            if !body.trim().is_empty() && ends_with_block(body, content) {
                return None;
            }
            let head = existing.trim_end();
            if head.is_empty() {
                content.to_owned()
            } else {
                format!("{head}\n\n{content}")
            }
        }
    };
    (out != existing).then_some(out)
}

/// Executes one capture step.
pub(crate) fn run(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let target = p.target;
    let mode = p.mode;
    let format = p.format.clone().unwrap_or_else(|| "{{value}}".into());

    // Value.
    let mut source_heading = None;
    let value = if p.source_line {
        let (line, heading) = source_line(ctx)?;
        source_heading = heading;
        line
    } else if let Some(v) = &p.value {
        expand(ctx, v)?
    } else {
        ctx.params
            .get("value")
            .map(value_text)
            .filter(|v| !v.is_empty())
            .or_else(|| ctx.context.value.clone())
            .or_else(|| ctx.context.selection.clone())
            .unwrap_or_default()
    };

    // Target (period notes are materialized when missing).
    let n = now();
    let mut template_date: NaiveDateTime = n;
    let path = match target {
        Target::Active => ctx
            .context
            .active_path
            .clone()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| StepError::failed("capture target=active needs an active path"))?,
        Target::Path => {
            let path = expand(ctx, p.path.as_deref().unwrap_or_default())?;
            let path = path.trim().trim_matches('/').to_owned();
            if path.is_empty() {
                return Err(StepError::failed("capture: `path` expanded to nothing"));
            }
            path
        }
        Target::Period(_) | Target::DailyNext => {
            let pname = match target {
                Target::Period(p) => p,
                _ => "day",
            };
            let per = period(ctx.config, pname, None).map_err(StepError::failed)?;
            let date = if target == Target::DailyNext {
                let src = ctx.context.active_path.as_deref().map(stem);
                let d = cv_text::resolve_next_daily_date(src, &per.format, n)
                    .map_err(|e| StepError::failed(e.to_string()))?;
                template_date = d.and_time(chrono::NaiveTime::MIN);
                d
            } else {
                n.date()
            };
            let path = per.note_path(date)?;
            let title = stem(&path).to_owned();
            ensure_note(ctx, &path, per.template.as_deref(), date, &title)?;
            path
        }
    };

    let mut out = StepOutcome::default();
    out.set_vars(&Vars {
        captured_path: path.clone(),
    });

    let rendered = super::materialize::render(ctx, &format, template_date, stem(&path), &value)?;
    let content_raw = rendered.trim();
    if content_raw.is_empty() {
        return Ok(out);
    }
    let content = content_raw[cv_note::span(content_raw).map_or(0, |r| r.end)..]
        .trim()
        .to_owned();
    // Crucible's second guard (YAML-only value + replace = no-op, never blank the body). For
    // the other modes an empty body has nothing to insert either: no-op (deviation, see report).
    if content.is_empty() {
        return Ok(out);
    }

    if ctx.read(&path)?.is_none() {
        return Err(StepError::failed("capture target note not found"));
    }
    let fixed = p.section.clone().filter(|x| !x.trim().is_empty());
    // Re-appliable on a concurrent edit (SR7): the section and the "already present" check
    // are decided on the text read for each attempt.
    let written = ctx.modify(&path, |existing| {
        let section = if p.section_from_source {
            source_heading
                .clone()
                .filter(|h| find_section_range(existing, h).is_some())
                .or_else(|| fixed.clone())
        } else {
            fixed.clone()
        };
        Ok(apply(
            existing,
            &content,
            content_raw,
            mode,
            section.as_deref(),
        ))
    })?;
    if written.is_some() {
        out.push_output(path);
    }
    Ok(out)
}

/// `capture` outcome vars.
#[derive(Serialize)]
struct Vars {
    captured_path: String,
}
