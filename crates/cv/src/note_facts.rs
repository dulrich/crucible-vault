//! The intake note facts (plan `intake-stage-jobs` WP-2): the one builder that turns a note, the
//! index, `state.db`, `search.db` and the config into the [`cv_ingest::stage`] model's inputs.
//! The `cv intake pending` report, the runner's stage guard ([`crate::runner::ModelGuard`]), and
//! later admission and the convergence sweep all read stage states through it; nothing here
//! writes.
//!
//! - [`candidates`]: every intake note path in scope (the population the report and the sweep
//!   iterate; the window is the caller's).
//! - [`note_view`]: one note's kind, id, [`NoteFacts`] and recorded outcomes ([`NoteView`]);
//!   [`NoteView::states`] judges them under an attempt cap ([`ATTEMPT_CAP`] until the cap's config
//!   home lands).
//!
//! **Facts**, per note: the remote image refs (`cv media audit`'s image embeds, as a set), the
//! `_MD5` image embeds and their `image_descriptions` records (a `failed` record is attempted),
//! the `yt-metadata` link and the channel step ([`crate::steps::youtube::channel_step_pending`]),
//! `transcript-of`, `enriched` and `enrich-miss`, the `## Transcript` eligibility
//! (`cv_ingest::transcript`), the `refined` tag, the `yt_clip_tidy` preamble shape,
//! `auto_file`'s own eligibility ([`crate::steps::auto_file::filing_pending`]), the finalize
//! step's read-only decision ([`crate::steps::intake_finalize::decide`]: empty → settled), and the
//! `index` fact: the note prepared exactly as the search feed prepares it, its hash compared
//! with `search.db`'s ([`IndexedHashes`]; skipped when the caller passes none).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Result;
use cv_actions::ConfigSet;
use cv_index::{LinkKind, Reader};
use cv_ingest::stage::{
    ImageRecord, IntakeKind, NoteFacts, Outcome, OutcomeKind, Stage, State, TrackerLink,
    stage_states,
};
use cv_note::FrontmatterDoc;
use cv_search::IndexedHashes;
use cv_search::prepare::localized_image_md5;
use cv_state::IntakeRead;

use crate::steps::intake_kind::{self, Kind};
use crate::steps::search::NotePrep;

/// The attempt cap a failed or unfinished stage is exhausted at, until `[intake_converge]` in
/// `pipelines.toml` holds it (plan `intake-stage-jobs` WP-5).
pub const ATTEMPT_CAP: u32 = 3;

/// Where the facts are read from.
pub struct FactEnv<'a> {
    /// The vault root (notes are read from the filesystem).
    pub vault: &'a Path,
    /// The active config (`auto_file`'s kind rules, the finalize decision, the channel step).
    pub config: &'a ConfigSet,
    /// The index, current generation.
    pub reader: &'a Reader,
    /// `state.db`, when there is one (none: no image records, rows or outcomes).
    pub state: Option<&'a dyn IntakeRead>,
    /// `search.db`'s indexed hashes; `None` leaves the index fact unread (not current, no hash).
    pub indexed: Option<&'a IndexedHashes>,
}

/// One intake note, as the model sees it.
#[derive(Debug, Clone)]
pub struct NoteView {
    /// The note's path.
    pub path: String,
    /// Its live note id, when bound.
    pub note_id: Option<i64>,
    /// The facts.
    pub facts: NoteFacts,
    /// Its recorded stage outcomes.
    pub outcomes: Vec<Outcome>,
}

impl NoteView {
    /// The intake kind.
    #[must_use]
    pub fn kind(&self) -> IntakeKind {
        self.facts.kind
    }

    /// Every required stage's state under attempt cap `cap`.
    #[must_use]
    pub fn states(&self, cap: u32) -> Vec<(Stage, State)> {
        stage_states(&self.facts, &self.outcomes, cap)
    }
}

fn kind_of(k: Kind) -> IntakeKind {
    match k {
        Kind::Blog => IntakeKind::Blog,
        Kind::Mail => IntakeKind::Mail,
        Kind::Youtube => IntakeKind::Youtube,
        Kind::Clip => IntakeKind::Clip,
    }
}

/// Markdown notes outside `.trash/`, the index's skipped dirs and `_`/`.`-prefixed top folders.
#[must_use]
pub fn in_scope(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
        && !cv_index::is_skipped_path(path)
        && path
            .split_once('/')
            .is_none_or(|(top, _)| !top.starts_with(['_', '.']))
        && !path.starts_with(['_', '.'])
}

/// Every in-scope note with a `post-id`, `message-id` or `source` property, in path order.
pub fn candidates(reader: &Reader) -> Result<Vec<String>> {
    let q = reader.query();
    let mut paths: BTreeSet<String> = q.md_paths_with_prop_key("post-id")?.into_iter().collect();
    paths.extend(q.md_paths_with_prop_key("message-id")?);
    paths.extend(q.md_props_with_key("source")?.into_iter().map(|r| r.path));
    Ok(paths.into_iter().filter(|p| in_scope(p)).collect())
}

/// A non-blank property's text.
pub(crate) fn text_prop(props: &[cv_index::PropRow], key: &str) -> Option<String> {
    props
        .iter()
        .find(|p| p.key == key)
        .filter(|p| !intake_kind::is_blank(&p.value))
        .map(|p| cv_index::prop_text(&p.value))
}

/// The intake kind of a note with properties `props` (`None`: not an intake note).
#[must_use]
pub fn intake_kind(props: &[cv_index::PropRow]) -> Option<IntakeKind> {
    let has = |k: &str| props.iter().any(|p| p.key == k);
    if !(has("post-id") || has("message-id") || has("source")) {
        return None;
    }
    let source = props
        .iter()
        .find(|p| p.key == "source")
        .map(|p| cv_index::prop_text(&p.value))
        .unwrap_or_default();
    Some(kind_of(intake_kind::classify(
        has("post-id"),
        has("message-id"),
        &source,
    )))
}

/// Parses stored rows; an unknown stage or outcome kind is skipped (written by a newer build).
#[must_use]
pub fn parse_outcomes(rows: &[cv_state::StageOutcomeRow]) -> Vec<Outcome> {
    rows.iter()
        .filter_map(|r| {
            Some(Outcome {
                stage: Stage::parse(&r.stage)?,
                kind: OutcomeKind::parse(&r.outcome)?,
                reason: r.reason.clone(),
                attempts: r.attempts,
                fingerprint: r.fingerprint.clone(),
            })
        })
        .collect()
}

/// The note at `path` as the model sees it; `None` when it is out of scope, not indexed, not an
/// intake note, or gone from the vault.
pub fn note_view(env: &FactEnv<'_>, path: &str) -> Result<Option<NoteView>> {
    if !in_scope(path) {
        return Ok(None);
    }
    let q = env.reader.query();
    if q.file(path)?.is_none() {
        return Ok(None);
    }
    let props = q.props(path)?;
    let Some(kind) = intake_kind(&props) else {
        return Ok(None);
    };
    let bytes = match std::fs::read(env.vault.join(path)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("reading {path}: {e}")),
    };
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let facts = facts(env, path, kind, &text, &props, &q.outgoing(path)?)?;
    let note_id = match env.state {
        Some(s) => s.live_note_id(path)?,
        None => None,
    };
    let outcomes = match (env.state, note_id) {
        (Some(s), Some(id)) => parse_outcomes(&s.stage_outcomes_for(&[id])?),
        _ => Vec::new(),
    };
    Ok(Some(NoteView {
        path: path.to_owned(),
        note_id,
        facts,
        outcomes,
    }))
}

fn facts(
    env: &FactEnv<'_>,
    path: &str,
    kind: IntakeKind,
    text: &str,
    props: &[cv_index::PropRow],
    links: &[cv_index::LinkRow],
) -> Result<NoteFacts> {
    let mut f = NoteFacts::new(kind);
    let body = FrontmatterDoc::new(text).body().to_owned();
    f.awaiting_filing = matches!(kind, IntakeKind::Clip | IntakeKind::Youtube)
        && crate::steps::auto_file::filing_pending(env.config, path, text);
    f.remote_refs = crate::media_audit::remote_image_refs(text);
    let mut md5s = BTreeSet::new();
    for l in links {
        if !matches!(l.kind, LinkKind::Embed | LinkKind::MdImage) || l.external {
            continue;
        }
        let target = l.resolved_path.as_deref().unwrap_or(&l.target);
        if crate::media_audit::is_image_file(target)
            && let Some((md5, _)) = localized_image_md5(target)
        {
            md5s.insert(md5);
        }
    }
    let md5s: Vec<String> = md5s.into_iter().collect();
    let records = match env.state {
        Some(s) if !md5s.is_empty() => s.image_descriptions_for(&md5s)?,
        _ => Vec::new(),
    };
    f.images = md5s
        .iter()
        .map(|m| {
            let r = match records.iter().find(|r| &r.md5 == m) {
                None => ImageRecord::Missing,
                Some(r) if r.kind == "failed" => ImageRecord::Failed,
                Some(_) => ImageRecord::Described,
            };
            (m.clone(), r)
        })
        .collect();
    let metadata = text_prop(props, "yt-metadata");
    f.yt_metadata_linked = metadata.is_some();
    f.yt_channel_pending = metadata
        .as_deref()
        .is_some_and(|l| crate::steps::youtube::channel_step_pending(env.config, env.vault, l));
    f.youtube_preamble = crate::steps::yt_clip_tidy::tidy(text).is_some();
    f.transcript_of = text_prop(props, "transcript-of").is_some();
    f.transcript_words = match cv_ingest::transcript::eligibility(&body) {
        cv_ingest::transcript::Eligibility::NoSection => None,
        e => Some(e.words()),
    };
    f.refined = crate::steps::transcript_refine::is_refined(text);
    f.enriched = text_prop(props, "enriched").is_some();
    f.enrich_miss = text_prop(props, "enrich-miss")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    f.tracker = match env.state {
        None => TrackerLink::NoRow,
        Some(s) => {
            let d = crate::steps::intake_finalize::decide(env.config, s, path, text, None)
                .map_err(|e| anyhow::anyhow!("finalize decision for {path}: {e}"))?;
            if d.rows.is_empty() {
                TrackerLink::NoRow
            } else if d.is_empty() {
                TrackerLink::Settled
            } else {
                TrackerLink::Pending
            }
        }
    };
    if let Some(indexed) = env.indexed {
        let vault = env.vault;
        let state = env.state;
        let prep = NotePrep::new(
            Box::new(move |p| match std::fs::read(vault.join(p)) {
                Ok(b) => Ok(Some(b)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(crate::steps::StepError::failed(e.to_string())),
            }),
            env.reader,
            Box::new(move |md5| {
                state
                    .and_then(|s| s.image_descriptions_for(&[md5.to_owned()]).ok())
                    .and_then(|mut v| v.pop())
            }),
        );
        if let Some(n) = prep
            .local_note(path)
            .map_err(|e| anyhow::anyhow!("preparing {path}: {e}"))?
        {
            f.index_current = indexed.is_current(&n.prepared);
            f.prepared_hash = n.prepared.content_hash;
        }
    }
    Ok(f)
}
