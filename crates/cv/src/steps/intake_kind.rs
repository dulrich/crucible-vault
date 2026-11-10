//! The one intake-kind rule (plan `tn-review-2026-11-10-remediation` WP-R8, review S8): which notes
//! are intake notes and what type each is. `removals_review` types its review population with
//! [`IntakeKinds::kind`]; `intake_enrich`'s backfill picks its candidates from
//! [`IntakeKinds::candidates`] and its refine targets from [`IntakeKinds::youtube`] — both read
//! the same three id sets, loaded once by [`IntakeKinds::load`]: notes with a `post-id` property
//! (blog), with a `message-id` property (mail) and with a `source` property (a YouTube video
//! URL there makes the note a YouTube clip).

use std::collections::{BTreeMap, BTreeSet};

use cv_text::url_canon::is_youtube_video;
use serde_json::Value;

use super::{StepCtx, StepError};

/// The intake types, in report order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    Blog,
    Mail,
    Youtube,
    Clip,
}

impl Kind {
    pub(crate) const ALL: [Kind; 4] = [Kind::Blog, Kind::Mail, Kind::Youtube, Kind::Clip];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Kind::Blog => "blog",
            Kind::Mail => "mail",
            Kind::Youtube => "youtube",
            Kind::Clip => "clip",
        }
    }
}

/// The pure intake classifier: `post-id` → blog, `message-id` → mail, a YouTube `source` URL →
/// youtube, anything else → clip.
pub(crate) fn classify(has_post_id: bool, has_message_id: bool, source: &str) -> Kind {
    if has_post_id {
        Kind::Blog
    } else if has_message_id {
        Kind::Mail
    } else if is_youtube_video(source) {
        Kind::Youtube
    } else {
        Kind::Clip
    }
}

/// [`cv_note::is_blank`]'s rule over the index's `serde_json` property values (there is no
/// `JsValue` conversion at the index boundary): null and a whitespace-only string are blank,
/// everything else is set.
pub(crate) fn is_blank(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        _ => false,
    }
}

/// The three id sets, loaded once from the index.
pub(crate) struct IntakeKinds {
    post: BTreeSet<String>,
    mail: BTreeSet<String>,
    /// Each note's `source` property text.
    source: BTreeMap<String, String>,
}

impl IntakeKinds {
    /// Reads the `post-id`, `message-id` and `source` paths from the index.
    pub(crate) fn load(ctx: &StepCtx<'_>) -> Result<Self, StepError> {
        let reader = ctx.index()?;
        let q = reader.query();
        let post = q.md_paths_with_prop_key("post-id")?.into_iter().collect();
        let mail = q
            .md_paths_with_prop_key("message-id")?
            .into_iter()
            .collect();
        let mut source = BTreeMap::new();
        for r in q.md_props_with_key("source")? {
            source.insert(r.path, cv_index::prop_text(&r.value));
        }
        Ok(Self { post, mail, source })
    }

    /// The type of the note at `path`.
    pub(crate) fn kind(&self, path: &str) -> Kind {
        classify(
            self.post.contains(path),
            self.mail.contains(path),
            self.source.get(path).map_or("", String::as_str),
        )
    }

    /// Notes whose `source` is a YouTube video URL.
    pub(crate) fn youtube(&self) -> BTreeSet<String> {
        self.source
            .iter()
            .filter(|(_, s)| is_youtube_video(s))
            .map(|(p, _)| p.clone())
            .collect()
    }

    /// Every intake note: a blog, a mail or a YouTube clip.
    pub(crate) fn candidates(&self) -> BTreeSet<String> {
        let mut all: BTreeSet<String> = self.post.union(&self.mail).cloned().collect();
        all.extend(self.youtube());
        all
    }
}
