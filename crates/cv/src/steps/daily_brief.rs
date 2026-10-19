//! `daily_brief` (plan `v0-automation-pipelines` WP-9): the daily note's `# Daily Brief` section.
//!
//! Ports `https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/workflows/DailyBriefLiteWorkflow.ts:11-100`:
//!
//! 1. `pipelines.toml` `[daily_brief] enabled = false` → no-op success (`disabled = true`).
//!    Deviation: Crucible fails when the *daily period* is disabled; here the brief's own
//!    switch gates it, and a missing `[periods.day]` fails as in `capture`.
//! 2. Materialize today's daily note (`captures.toml` `[periods.day]`, the same in-process
//!    [`super::materialize::ensure_note`] `capture` uses — Crucible's poll-until-the-file-appears
//!    loop is Obsidian-induced and dropped).
//! 3. The configured section (default `# Daily Brief`) non-empty → skip, no fetch
//!    (`skipped = true`). No pairs and no places → skip.
//! 4. Fetch every FX pair (Frankfurter) and place (Open-Meteo), one `api_usage` unit per call.
//!    Some fail → the successful lines plus Crucible's inline `*lookup failed (…)*` lines;
//!    **all** fail → no write, the first error's [`super::fetch_disposition`] (deferred on
//!    transient, failed otherwise).
//! 5. Append the block to the section through one CAS write
//!    ([`cv_text::sections::insert_into_section`], append mode, payload `\n<body>\n`); an absent
//!    section is appended at the end of the note, as Crucible does.
//!
//! Params: none (everything comes from `pipelines.toml`). Outcome vars: `fx`, `weather` (lines
//! fetched), `failed` (lookups failed), `skipped`, plus `disabled` when off.

use cv_actions::{ConfigSet, StepKind};
use cv_ingest::brief::{Entry, FxLine, WeatherLine, brief_body};
use cv_providers::web::FetchError;
use cv_providers::web::fx::{self, FxApi};
use cv_providers::web::weather::{self, WeatherApi};
use cv_text::sections::{SectionInsertMode, insert_into_section, is_section_empty};
use serde::Serialize;

use super::materialize::{ensure_note, now, period, stem};
use super::web::{api_base, block_on, client, record_usage};
use super::{StepCtx, StepError, StepModule, StepOutcome, fetch_disposition};

/// The `daily_brief` step (no params; unknown keys disarm, I5).
pub(crate) struct DailyBrief;

impl StepModule for DailyBrief {
    type Params = ();

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<(), String> {
        super::no_params(kind, t)
    }

    fn run((): (), ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(ctx)
    }
}

/// `daily_brief` outcome vars.
#[derive(Serialize)]
struct Vars {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disabled: bool,
    fx: usize,
    weather: usize,
    failed: usize,
    skipped: bool,
}

fn vars(out: &mut StepOutcome, fx: usize, weather: usize, failed: usize, skipped: bool) {
    out.set_vars(&Vars {
        disabled: false,
        fx,
        weather,
        failed,
        skipped,
    });
}

/// Local stamp for the `**FX Rates** _(updated …)_` header (Crucible: `en-CA` + short zone name;
/// chrono has no zone abbreviations, so the numeric offset stands in).
fn stamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%d %H:%M %:z")
        .to_string()
}

/// Executes the step.
pub(crate) fn run(ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let cfg = ctx.config.pipelines().daily_brief.clone();
    let mut out = StepOutcome::default();
    if !cfg.enabled {
        out.set_vars(&Vars {
            disabled: true,
            fx: 0,
            weather: 0,
            failed: 0,
            skipped: true,
        });
        return Ok(out);
    }

    let per = period(ctx.config, "day", None).map_err(StepError::failed)?;
    let today = now().date();
    let path = per.note_path(today)?;
    let title = stem(&path).to_owned();
    ensure_note(ctx, &path, per.template.as_deref(), today, &title)?;
    out.output_paths.push(path.clone());

    let (bytes, hash) = ctx
        .read(&path)?
        .ok_or_else(|| StepError::failed("daily note could not be materialized"))?;
    let content =
        String::from_utf8(bytes).map_err(|_| StepError::failed("daily note is not UTF-8"))?;
    let section = cfg.section.trim();
    if (!section.is_empty() && !is_section_empty(&content, section))
        || (cfg.fx.is_empty() && cfg.weather.is_empty())
    {
        vars(&mut out, 0, 0, 0, true);
        return Ok(out);
    }

    let c = client()?;
    let fx_api = FxApi::new(api_base(fx::SERVICE, fx::DEFAULT_BASE));
    let wx_api = WeatherApi::new(api_base(weather::SERVICE, weather::DEFAULT_BASE));
    let mut first_err: Option<FetchError> = None;
    let keep = |e: FetchError, first: &mut Option<FetchError>| {
        let msg = e.to_string();
        first.get_or_insert(e);
        msg
    };

    let mut fx_entries: Vec<(String, Entry<FxLine>)> = Vec::new();
    for p in &cfg.fx {
        let label = p
            .label
            .clone()
            .unwrap_or_else(|| format!("{}/{}", p.base, p.quote));
        let r = block_on(fx_api.latest(c, &p.base, &p.quote))?;
        record_usage(ctx, fx::SERVICE, 1);
        let e = r
            .map(|r| FxLine {
                label: label.clone(),
                rate: r.rate,
                as_of: r.as_of,
            })
            .map_err(|e| keep(e, &mut first_err));
        fx_entries.push((label, e));
    }
    let mut wx_entries: Vec<(String, Entry<WeatherLine>)> = Vec::new();
    for w in &cfg.weather {
        let r = block_on(wx_api.today(c, w.lat, w.lon))?;
        record_usage(ctx, weather::SERVICE, 1);
        let e = r
            .map(|f| WeatherLine {
                label: w.label.clone(),
                high_c: f.high_c,
                low_c: f.low_c,
                code: f.code,
            })
            .map_err(|e| keep(e, &mut first_err));
        wx_entries.push((w.label.clone(), e));
    }

    let ok_fx = fx_entries.iter().filter(|(_, e)| e.is_ok()).count();
    let ok_wx = wx_entries.iter().filter(|(_, e)| e.is_ok()).count();
    let failed = fx_entries.len() + wx_entries.len() - ok_fx - ok_wx;
    if ok_fx + ok_wx == 0 {
        return Err(first_err.map_or_else(
            || StepError::failed("all daily brief sources failed"),
            |e| fetch_disposition(&e),
        ));
    }

    let body = brief_body(&stamp(), &fx_entries, &wx_entries);
    let payload = format!("\n{}\n", body.trim());
    let next = insert_into_section(&content, section, &payload, SectionInsertMode::Append);
    ctx.write(&path, next.into_bytes(), Some(hash))?;
    vars(&mut out, ok_fx, ok_wx, failed, false);
    Ok(out)
}

#[cfg(test)]
#[path = "daily_brief_tests.rs"]
mod tests;
