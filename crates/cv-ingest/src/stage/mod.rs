//! The intake stage model (plan `intake-foundations` WP-1, amended by plan `intake-stage-jobs`
//! WP-2, D40): which stages an intake note needs, what each requires, the evidence that it is
//! done, and which stage job may run now. This module is the only place stage order,
//! prerequisites, evidence and outcome precedence are written; it is the authority the stage jobs
//! follow. Pure: the caller gathers [`NoteFacts`] (from the note, `index.db`, `state.db`,
//! `search.db` and the config), the recorded [`Outcome`]s and the queue's [`Progress`]; nothing
//! here does I/O.
//!
//! **Evidence, not job results.** A stage is [`State::Done`] only from stage-specific evidence in
//! the note's current facts, never from a job having returned Ok:
//!
//! | stage | action | done when | fact-derived skip | unfinished when |
//! |---|---|---|---|---|
//! | `auto_file` | `stage-file` | `auto_file`'s own eligibility leaves nothing to file | — | — |
//! | `localize` | `stage-localize` | no remote image ref is left | — | — |
//! | `image_describe` | `stage-describe` | every eligible (`_MD5`) image has a description record (a recorded permanent failure counts as attempted) | `no images` | some images attempted, others not |
//! | `yt_metadata` | `stage-yt-metadata` | the note links its `yt-metadata` note and the channel step has run where it is enabled and applies | — | — |
//! | `yt_clip_tidy` | `stage-yt-tidy` | no clipped thumbnail/description preamble is left | — | — |
//! | `transcript_refine` | `stage-refine` | the note carries the `refined` tag | `not youtube`, `no transcript`, `short transcript` | — |
//! | `enrich` | `stage-enrich` | a non-empty `enriched` stamp | — | `enrich-miss` > 0 |
//! | `finalize` | `stage-finalize` | the finalize step's decision is empty | `no tracker row` | — |
//! | `index` | `stage-index` | the caller's fact "prepared hash equals indexed hash" is true | — | — |
//!
//! **Precedence** (D40), per stage: a recorded failure of `auto_file` outranks its evidence (the
//! filing move happens before the filing job's later steps); otherwise done evidence →
//! [`State::Done`]; a fact-derived skip → [`State::Skipped`]; a recorded row whose fingerprint
//! ([`fingerprint`]) equals the note's current one: skipped → [`State::Skipped`], failed or
//! unfinished → [`State::Failed`] / [`State::Unfinished`], or [`State::Exhausted`] once its
//! attempts reach the cap (a row with a stale fingerprint is ignored, so the stage reads pending
//! again); unfinished evidence → [`State::Unfinished`]; a **hard** prerequisite (in the kind's
//! set) that is failed, exhausted or stuck → [`State::Stuck`]; else [`State::Pending`].
//!
//! **Admission** ([`admissible`]): a stage may run when it is pending, unfinished or failed below
//! the cap and every hard prerequisite is done or skipped. Index's prerequisites are **soft**:
//! Index is admitted last, only once no other required stage of the note can make progress now
//! (admissible, or with a job queued or running that no open breaker holds — the queue facts are
//! the caller's [`Progress`]).

use std::fmt::Write as _;

use crate::transcript::MIN_REFINE_WORDS;

#[cfg(test)]
mod tests;

/// An intake note's type (the `cv` binary's `intake_kind` classifier: `post-id` → blog,
/// `message-id` → mail, a YouTube `source` → youtube, anything else → clip).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntakeKind {
    Blog,
    Mail,
    Youtube,
    Clip,
}

impl IntakeKind {
    /// Every kind, in report order.
    pub const ALL: [IntakeKind; 4] = [Self::Blog, Self::Mail, Self::Youtube, Self::Clip];

    /// The kind's name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blog => "blog",
            Self::Mail => "mail",
            Self::Youtube => "youtube",
            Self::Clip => "clip",
        }
    }

    /// The stages this kind requires, in [`Stage::ALL`] order.
    #[must_use]
    pub fn required(self) -> &'static [Stage] {
        use Stage::{
            AutoFile, Enrich, Finalize, ImageDescribe, Index, Localize, TranscriptRefine,
            YtClipTidy, YtMetadata,
        };
        match self {
            Self::Blog | Self::Mail => &[Localize, ImageDescribe, Enrich, Finalize, Index],
            Self::Youtube => &[
                AutoFile,
                Localize,
                ImageDescribe,
                YtMetadata,
                YtClipTidy,
                TranscriptRefine,
                Enrich,
                Finalize,
                Index,
            ],
            // A plain web clip is not an enrich candidate; a link-only transcript clip
            // (`transcript-of`) is refined like a video, otherwise refine reads skipped.
            Self::Clip => &[
                AutoFile,
                Localize,
                ImageDescribe,
                TranscriptRefine,
                Finalize,
                Index,
            ],
        }
    }
}

/// One intake stage (the existing intake steps), in evaluation order: every prerequisite comes
/// before the stages that need it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// `auto_file`: dedup and file a fresh clip by kind.
    AutoFile,
    /// `localize`: download remote attachments to `_MD5` files.
    Localize,
    /// `image_describe`: describe localized images.
    ImageDescribe,
    /// `yt_metadata`: the linked `_yt_metadata` note (and the channel step).
    YtMetadata,
    /// `yt_clip_tidy`: strip the clipped thumbnail and description.
    YtClipTidy,
    /// `transcript_refine`: refine the `## Transcript` section.
    TranscriptRefine,
    /// `intake_enrich`: the structure + cleanup pass.
    Enrich,
    /// `intake_finalize`: link and tick the tracker rows.
    Finalize,
    /// `search_upsert`: the note's search source is current.
    Index,
}

impl Stage {
    /// Every stage, in evaluation (and report) order.
    pub const ALL: [Stage; 9] = [
        Self::AutoFile,
        Self::Localize,
        Self::ImageDescribe,
        Self::YtMetadata,
        Self::YtClipTidy,
        Self::TranscriptRefine,
        Self::Enrich,
        Self::Finalize,
        Self::Index,
    ];

    /// The stage's stored / wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoFile => "auto_file",
            Self::Localize => "localize",
            Self::ImageDescribe => "image_describe",
            Self::YtMetadata => "yt_metadata",
            Self::YtClipTidy => "yt_clip_tidy",
            Self::TranscriptRefine => "transcript_refine",
            Self::Enrich => "enrich",
            Self::Finalize => "finalize",
            Self::Index => "index",
        }
    }

    /// The stage named `s` ([`Self::as_str`]).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.as_str() == s)
    }

    /// The id of the action that runs this stage as one job (plan `intake-stage-jobs`, the
    /// stage-to-job table).
    #[must_use]
    pub fn action_id(self) -> &'static str {
        match self {
            Self::AutoFile => "stage-file",
            Self::Localize => "stage-localize",
            Self::ImageDescribe => "stage-describe",
            Self::YtMetadata => "stage-yt-metadata",
            Self::YtClipTidy => "stage-yt-tidy",
            Self::TranscriptRefine => "stage-refine",
            Self::Enrich => "stage-enrich",
            Self::Finalize => "stage-finalize",
            Self::Index => "stage-index",
        }
    }

    /// The stage whose action is `id` ([`Self::action_id`]).
    #[must_use]
    pub fn for_action(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.action_id() == id)
    }

    /// The **hard** prerequisites: stages that must be done or skipped before this one runs, and
    /// whose failure or exhaustion makes this one stuck (only those in the note kind's required
    /// set apply).
    #[must_use]
    pub fn prerequisites(self) -> &'static [Stage] {
        match self {
            Self::AutoFile | Self::YtMetadata | Self::Finalize | Self::Index => &[],
            // Enrichment of a clip runs on its final path (auto_file module doc, step 3).
            Self::Localize | Self::YtClipTidy => &[Self::AutoFile],
            Self::ImageDescribe => &[Self::Localize],
            Self::TranscriptRefine => &[Self::YtClipTidy],
            Self::Enrich => &[Self::TranscriptRefine],
        }
    }

    /// The **soft** prerequisites: they only order (see [`admissible`]); a failed one never makes
    /// this stage stuck. The search hash folds image descriptions and the enriched body.
    #[must_use]
    pub fn soft_prerequisites(self) -> &'static [Stage] {
        match self {
            Self::Index => &[Self::ImageDescribe, Self::Enrich],
            _ => &[],
        }
    }
}

/// One eligible image's description record, as `state.db` holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageRecord {
    /// A usable description.
    Described,
    /// A recorded permanent failure (attempted; not retried).
    Failed,
    /// No record: never attempted (or a retryable legacy failure).
    Missing,
}

/// The finalize fact: what the finalize step's read-only decision says for the note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackerLink {
    /// No tracker row resolves to the note.
    #[default]
    NoRow,
    /// Rows resolve and the decision is not empty (a link to add, evidence that differs from
    /// what a row holds, or a tick).
    Pending,
    /// Rows resolve and the decision is empty: nothing left to do.
    Settled,
}

/// The plain facts the model judges a note by. The caller gathers them; see the module table
/// for which fact each stage reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteFacts {
    /// The intake kind.
    pub kind: IntakeKind,
    /// `auto_file`'s eligibility, read without writing: the note still has filing left to do.
    pub awaiting_filing: bool,
    /// The remote image refs left in the body (their set is the localize fingerprint).
    pub remote_refs: Vec<String>,
    /// Each eligible (`_MD5`, local) image: its md5 and record.
    pub images: Vec<(String, ImageRecord)>,
    /// The note links its `yt-metadata` note.
    pub yt_metadata_linked: bool,
    /// The channel step is enabled, applies to the note's channel, and has not run.
    pub yt_channel_pending: bool,
    /// The clipped thumbnail/description preamble is still present.
    pub youtube_preamble: bool,
    /// The note carries a non-blank `transcript-of` (a link-only post's transcript clip).
    pub transcript_of: bool,
    /// Unmanaged words of the `## Transcript` section; `None` = no section.
    pub transcript_words: Option<usize>,
    /// The note carries the `refined` tag.
    pub refined: bool,
    /// A non-empty `enriched` stamp.
    pub enriched: bool,
    /// The `enrich-miss` count (absent = 0).
    pub enrich_miss: u32,
    /// The finalize decision.
    pub tracker: TrackerLink,
    /// The note's prepared search content hash (empty when not prepared).
    pub prepared_hash: String,
    /// The note's prepared search hash equals the indexed source hash.
    pub index_current: bool,
}

impl NoteFacts {
    /// Facts of a note with nothing done yet (tests and callers fill the rest).
    #[must_use]
    pub fn new(kind: IntakeKind) -> Self {
        Self {
            kind,
            awaiting_filing: false,
            remote_refs: Vec::new(),
            images: Vec::new(),
            yt_metadata_linked: false,
            yt_channel_pending: false,
            youtube_preamble: false,
            transcript_of: false,
            transcript_words: None,
            refined: false,
            enriched: false,
            enrich_miss: 0,
            tracker: TrackerLink::NoRow,
            prepared_hash: String::new(),
            index_current: false,
        }
    }
}

/// What a stage outcome row records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// The stage judged itself not applicable to the facts fingerprinted with it.
    Skipped,
    /// The stage failed (`attempts` times).
    Failed,
    /// A run returned Ok but left work behind (`attempts` such runs).
    Unfinished,
}

impl OutcomeKind {
    /// The stored name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::Unfinished => "unfinished",
        }
    }

    /// The kind named `s`; an unknown stored kind is `None` (ignored by the model).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skipped" => Some(Self::Skipped),
            "failed" => Some(Self::Failed),
            "unfinished" => Some(Self::Unfinished),
            _ => None,
        }
    }
}

/// One recorded stage outcome (a `state.db` `stage_outcomes` row, parsed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The stage.
    pub stage: Stage,
    /// Skip, failure or unfinished run.
    pub kind: OutcomeKind,
    /// Why (a skip's reason is what the report shows).
    pub reason: String,
    /// Attempts so far.
    pub attempts: u32,
    /// [`fingerprint`] of the facts the outcome was judged on.
    pub fingerprint: String,
}

/// A stage's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Stage-specific evidence says done.
    Done,
    /// Should run; nothing explains why it has not.
    Pending,
    /// Not applicable to the current facts.
    Skipped(String),
    /// A run left work behind (below the attempt cap).
    Unfinished,
    /// Recorded failures (below the attempt cap).
    Failed(u32),
    /// A failed or unfinished stage whose attempts reached the cap: only a reset retries it.
    Exhausted(u32),
    /// Blocked behind a failed, exhausted or stuck hard prerequisite.
    Stuck,
}

impl State {
    /// The state's name (`done` | `pending` | `skipped` | `unfinished` | `failed` | `exhausted`
    /// | `stuck`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Pending => "pending",
            Self::Skipped(_) => "skipped",
            Self::Unfinished => "unfinished",
            Self::Failed(_) => "failed",
            Self::Exhausted(_) => "exhausted",
            Self::Stuck => "stuck",
        }
    }

    /// Done or skipped: nothing left to do.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        matches!(self, Self::Done | Self::Skipped(_))
    }

    /// Work a stage job may still do (pending, unfinished or failed below the cap).
    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self, Self::Pending | Self::Unfinished | Self::Failed(_))
    }
}

/// FNV-1a 64.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

fn sorted_set(items: impl Iterator<Item = String>) -> String {
    let mut v: Vec<String> = items.collect();
    v.sort_unstable();
    v.dedup();
    v.join(",")
}

/// The fingerprint of the facts `stage` is judged on: `v1:` + 16 hex digits of the FNV-1a 64
/// hash of a canonical text of those facts (images and remote refs as their sorted sets, Index
/// as the prepared content hash). `auto_file`'s is the kind alone: its failure must survive the
/// filing move that changes the note's place. Stable across runs and builds; opaque to callers
/// (store it, compare it for equality).
#[must_use]
pub fn fingerprint(stage: Stage, f: &NoteFacts) -> String {
    let mut c = format!("{}|{}|", stage.as_str(), f.kind.as_str());
    let _ = match stage {
        Stage::AutoFile => Ok(()),
        Stage::Localize => write!(c, "remote={}", sorted_set(f.remote_refs.iter().cloned())),
        Stage::ImageDescribe => write!(
            c,
            "images={}",
            sorted_set(f.images.iter().map(|(m, _)| m.clone()))
        ),
        Stage::YtMetadata => write!(
            c,
            "linked={}|channel={}",
            f.yt_metadata_linked, f.yt_channel_pending
        ),
        Stage::YtClipTidy => write!(c, "preamble={}", f.youtube_preamble),
        Stage::TranscriptRefine | Stage::Enrich => write!(
            c,
            "of={}|transcript={}",
            f.transcript_of,
            f.transcript_words
                .map_or_else(|| "none".to_owned(), |w| w.to_string())
        ),
        Stage::Finalize => write!(c, "tracker={:?}", f.tracker),
        Stage::Index => write!(c, "hash={}", f.prepared_hash),
    };
    format!("v1:{:016x}", fnv1a(&c))
}

/// What the note's own facts say about `stage`, before recorded outcomes.
enum Evidence {
    Done,
    Skip(&'static str),
    Unfinished,
    None,
}

fn evidence(stage: Stage, f: &NoteFacts) -> Evidence {
    let done = |b: bool| if b { Evidence::Done } else { Evidence::None };
    match stage {
        Stage::AutoFile => done(!f.awaiting_filing),
        Stage::Localize => done(f.remote_refs.is_empty()),
        Stage::ImageDescribe => {
            if f.images.is_empty() {
                return Evidence::Skip("no images");
            }
            let missing = f
                .images
                .iter()
                .filter(|(_, r)| *r == ImageRecord::Missing)
                .count();
            if missing == 0 {
                Evidence::Done
            } else if missing < f.images.len() {
                Evidence::Unfinished
            } else {
                Evidence::None
            }
        }
        Stage::YtMetadata => done(f.yt_metadata_linked && !f.yt_channel_pending),
        Stage::YtClipTidy => done(!f.youtube_preamble),
        Stage::TranscriptRefine => {
            if f.refined {
                Evidence::Done
            } else if f.kind != IntakeKind::Youtube && !f.transcript_of {
                Evidence::Skip("not youtube")
            } else {
                match f.transcript_words {
                    None | Some(0) => Evidence::Skip("no transcript"),
                    Some(w) if w < MIN_REFINE_WORDS => Evidence::Skip("short transcript"),
                    Some(_) => Evidence::None,
                }
            }
        }
        Stage::Enrich => {
            if f.enriched {
                Evidence::Done
            } else if f.enrich_miss > 0 {
                Evidence::Unfinished
            } else {
                Evidence::None
            }
        }
        Stage::Finalize => match f.tracker {
            TrackerLink::Settled => Evidence::Done,
            TrackerLink::NoRow => Evidence::Skip("no tracker row"),
            TrackerLink::Pending => Evidence::None,
        },
        Stage::Index => done(f.index_current),
    }
}

/// A failed or unfinished row's state against the cap.
fn counted(kind: OutcomeKind, attempts: u32, cap: u32) -> State {
    let n = attempts.max(1);
    if n >= cap.max(1) {
        State::Exhausted(n)
    } else if kind == OutcomeKind::Failed {
        State::Failed(n)
    } else {
        State::Unfinished
    }
}

/// The state of every stage `facts.kind` requires, in [`Stage::ALL`] order, given the recorded
/// `outcomes` and the attempt `cap` (a failed or unfinished row at `cap` attempts or more is
/// [`State::Exhausted`]). Rows for stages the kind does not require, and unknown rows, are
/// ignored; with several rows for one stage the first wins. See the module doc for the
/// precedence.
#[must_use]
pub fn stage_states(facts: &NoteFacts, outcomes: &[Outcome], cap: u32) -> Vec<(Stage, State)> {
    let required = facts.kind.required();
    let mut out: Vec<(Stage, State)> = Vec::with_capacity(required.len());
    for &stage in required {
        let recorded = outcomes.iter().find(|o| o.stage == stage);
        let current = recorded.filter(|o| o.fingerprint == fingerprint(stage, facts));
        let ev = evidence(stage, facts);
        let state = match (ev, recorded) {
            // A recorded failed filing outranks the "nothing left to file" evidence.
            (_, Some(o)) if stage == Stage::AutoFile && o.kind == OutcomeKind::Failed => {
                counted(o.kind, o.attempts, cap)
            }
            (Evidence::Done, _) => State::Done,
            (Evidence::Skip(r), _) => State::Skipped(r.to_owned()),
            (ev, _) => {
                let blocked = stage.prerequisites().iter().any(|p| {
                    out.iter().any(|(s, st)| {
                        s == p
                            && matches!(st, State::Failed(_) | State::Exhausted(_) | State::Stuck)
                    })
                });
                match current {
                    Some(o) if o.kind == OutcomeKind::Skipped => State::Skipped(o.reason.clone()),
                    Some(o) => counted(o.kind, o.attempts, cap),
                    None if matches!(ev, Evidence::Unfinished) => State::Unfinished,
                    None if blocked => State::Stuck,
                    None => State::Pending,
                }
            }
        };
        out.push((stage, state));
    }
    out
}

/// The queue's view of a note's stage jobs, handed in by the caller (the model stays pure).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    /// Stages whose job is queued or running and no open breaker holds it.
    pub active: Vec<Stage>,
    /// Stages whose job (or whose service) waits behind an open breaker.
    pub held: Vec<Stage>,
}

/// Whether `stage`'s own state and hard prerequisites admit it (the non-Index rule).
fn hard_admissible(states: &[(Stage, State)], stage: Stage) -> bool {
    let Some((_, st)) = states.iter().find(|(s, _)| *s == stage) else {
        return false;
    };
    st.is_open()
        && stage.prerequisites().iter().all(|p| {
            states
                .iter()
                .find(|(s, _)| s == p)
                .is_none_or(|(_, st)| st.is_settled())
        })
}

/// Whether the job of `stage` may run now for a note in `states` ([`stage_states`]): the stage
/// is pending, unfinished or failed below the cap and every hard prerequisite is done or
/// skipped; Index additionally waits while any other required stage can still make progress now
/// (admissible or `progress.active`, and not `progress.held`). A stage outside `states` (not
/// required by the kind) is never admissible.
#[must_use]
pub fn admissible(states: &[(Stage, State)], stage: Stage, progress: &Progress) -> bool {
    if !hard_admissible(states, stage) {
        return false;
    }
    if stage != Stage::Index {
        return true;
    }
    !states.iter().any(|(s, _)| {
        *s != Stage::Index
            && !progress.held.contains(s)
            && (progress.active.contains(s) || hard_admissible(states, *s))
    })
}
