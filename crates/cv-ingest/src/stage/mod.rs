//! The intake stage model (plan `intake-foundations` WP-1): which stages an intake note needs,
//! what each requires, and the evidence that it is done. This module is the only place stage
//! order, prerequisites and evidence rules are written. Pure: the caller gathers [`NoteFacts`]
//! (from the note, `index.db`, `state.db` and `search.db`) and the recorded [`Outcome`]s; nothing
//! here does I/O.
//!
//! **Evidence, not job results.** A stage is [`State::Done`] only from stage-specific evidence in
//! the note's current facts, never from a job having returned Ok:
//!
//! | stage | done when | fact-derived skip | unfinished when |
//! |---|---|---|---|
//! | `auto_file` | the clip no longer sits in the clipper inbox | — | — |
//! | `localize` | no remote image embed is left | — | — |
//! | `image_describe` | every eligible (`_MD5`) image has a description record (a recorded permanent failure counts as attempted) | `no images` | some images attempted, others not |
//! | `yt_metadata` | the note links its `yt-metadata` note | — | — |
//! | `yt_clip_tidy` | no clipped thumbnail/description preamble is left | — | — |
//! | `transcript_refine` | the note carries the `refined` tag | `not youtube`, `no transcript`, `short transcript` | — |
//! | `enrich` | a non-empty `enriched` stamp | — | `enrich-miss` > 0 |
//! | `finalize` | a tracker row is linked to the note | `no tracker row` | — |
//! | `index` | the caller's fact "prepared hash equals indexed hash" is true | — | — |
//!
//! Otherwise, in this order: a recorded skip whose facts fingerprint ([`fingerprint`]) equals
//! the note's current one → [`State::Skipped`] (a stale fingerprint is ignored, so the stage
//! reads pending again); a recorded failure → [`State::Failed`]; a prerequisite (in the kind's
//! set) that is failed or stuck → [`State::Stuck`]; else [`State::Pending`].
//!
//! Nothing writes outcomes yet (plan `intake-stage-jobs` adds the writers).

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
    /// `yt_metadata`: the linked `_yt_metadata` note.
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

    /// The stages that must settle before this one runs (only those in the note kind's
    /// required set apply).
    #[must_use]
    pub fn prerequisites(self) -> &'static [Stage] {
        match self {
            Self::AutoFile | Self::YtMetadata | Self::Finalize => &[],
            // Enrichment of a clip runs on its final path (auto_file module doc, step 3).
            Self::Localize | Self::YtClipTidy => &[Self::AutoFile],
            Self::ImageDescribe => &[Self::Localize],
            Self::TranscriptRefine => &[Self::YtClipTidy],
            Self::Enrich => &[Self::TranscriptRefine],
            // The search hash folds image descriptions and the enriched body.
            Self::Index => &[Self::ImageDescribe, Self::Enrich],
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

/// The note's tracker linkage (what `intake_finalize` leaves in `state.db`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrackerLink {
    /// No tracker row resolves to the note.
    #[default]
    NoRow,
    /// A row resolves by URL or video id but is not linked to the note.
    Unlinked,
    /// A row is linked to the note.
    Linked,
}

/// The plain facts the model judges a note by. The caller gathers them; see the module table
/// for which fact each stage reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteFacts {
    /// The intake kind.
    pub kind: IntakeKind,
    /// The note sits in the clipper inbox, not yet filed.
    pub awaiting_filing: bool,
    /// Remote image embeds left in the body.
    pub remote_images: usize,
    /// Each eligible (`_MD5`, local) image: its md5 and record.
    pub images: Vec<(String, ImageRecord)>,
    /// The note links its `yt-metadata` note.
    pub yt_metadata_linked: bool,
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
    /// Tracker linkage.
    pub tracker: TrackerLink,
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
            remote_images: 0,
            images: Vec::new(),
            yt_metadata_linked: false,
            youtube_preamble: false,
            transcript_of: false,
            transcript_words: None,
            refined: false,
            enriched: false,
            enrich_miss: 0,
            tracker: TrackerLink::NoRow,
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
}

impl OutcomeKind {
    /// The stored name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    /// The kind named `s`; an unknown stored kind is `None` (ignored by the model).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "skipped" => Some(Self::Skipped),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One recorded stage outcome (a `state.db` `stage_outcomes` row, parsed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The stage.
    pub stage: Stage,
    /// Skip or failure.
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
    /// A run left work behind.
    Unfinished,
    /// Recorded failures.
    Failed(u32),
    /// Blocked behind a failed or stuck prerequisite.
    Stuck,
}

impl State {
    /// The state's name (`done` | `pending` | `skipped` | `unfinished` | `failed` | `stuck`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Pending => "pending",
            Self::Skipped(_) => "skipped",
            Self::Unfinished => "unfinished",
            Self::Failed(_) => "failed",
            Self::Stuck => "stuck",
        }
    }

    /// Done or skipped: nothing left to do.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        matches!(self, Self::Done | Self::Skipped(_))
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

/// The fingerprint of the facts `stage` is judged on: `v1:` + 16 hex digits of the FNV-1a 64
/// hash of a canonical text of those facts (images as their sorted md5 set). Stable across runs
/// and builds; opaque to callers (store it, compare it for equality).
#[must_use]
pub fn fingerprint(stage: Stage, f: &NoteFacts) -> String {
    let mut c = format!("{}|{}|", stage.as_str(), f.kind.as_str());
    let _ = match stage {
        Stage::AutoFile => write!(c, "inbox={}", f.awaiting_filing),
        Stage::Localize => write!(c, "remote={}", f.remote_images),
        Stage::ImageDescribe => {
            let mut md5s: Vec<&str> = f.images.iter().map(|(m, _)| m.as_str()).collect();
            md5s.sort_unstable();
            md5s.dedup();
            write!(c, "images={}", md5s.join(","))
        }
        Stage::YtMetadata => write!(c, "linked={}", f.yt_metadata_linked),
        Stage::YtClipTidy => write!(c, "preamble={}", f.youtube_preamble),
        Stage::TranscriptRefine | Stage::Enrich => write!(
            c,
            "of={}|transcript={}",
            f.transcript_of,
            f.transcript_words
                .map_or_else(|| "none".to_owned(), |w| w.to_string())
        ),
        Stage::Finalize => write!(c, "tracker={:?}", f.tracker),
        Stage::Index => Ok(()),
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
        Stage::Localize => done(f.remote_images == 0),
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
        Stage::YtMetadata => done(f.yt_metadata_linked),
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
            TrackerLink::Linked => Evidence::Done,
            TrackerLink::NoRow => Evidence::Skip("no tracker row"),
            TrackerLink::Unlinked => Evidence::None,
        },
        Stage::Index => done(f.index_current),
    }
}

/// The state of every stage `facts.kind` requires, in [`Stage::ALL`] order, given the recorded
/// `outcomes` (rows for stages the kind does not require, and unknown rows, are ignored; with
/// several rows for one stage the first wins).
#[must_use]
pub fn stage_states(facts: &NoteFacts, outcomes: &[Outcome]) -> Vec<(Stage, State)> {
    let required = facts.kind.required();
    let mut out: Vec<(Stage, State)> = Vec::with_capacity(required.len());
    for &stage in required {
        let state = match evidence(stage, facts) {
            Evidence::Done => State::Done,
            Evidence::Skip(r) => State::Skipped(r.to_owned()),
            Evidence::Unfinished => State::Unfinished,
            Evidence::None => {
                let recorded = outcomes.iter().find(|o| o.stage == stage);
                let blocked = stage.prerequisites().iter().any(|p| {
                    out.iter()
                        .any(|(s, st)| s == p && matches!(st, State::Failed(_) | State::Stuck))
                });
                match recorded {
                    Some(o)
                        if o.kind == OutcomeKind::Skipped
                            && o.fingerprint == fingerprint(stage, facts) =>
                    {
                        State::Skipped(o.reason.clone())
                    }
                    Some(o) if o.kind == OutcomeKind::Failed => State::Failed(o.attempts.max(1)),
                    _ if blocked => State::Stuck,
                    _ => State::Pending,
                }
            }
        };
        out.push((stage, state));
    }
    out
}
