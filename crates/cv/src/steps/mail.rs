//! `track_mail` (plan `auto-intake-sprint` WP-4): the Maildir newsletter intake.
//!
//! An external fetcher (or the user saving `.eml` files) fills a Maildir outside the vault; cv
//! never speaks IMAP/SMTP. Each run scans `<maildir>/new/*` and the loose `<maildir>/*.eml`
//! (sorted by name). Per message:
//!
//! - unparseable → counted `invalid`, **left in place**, logged; never moved or deleted;
//! - already recorded (`tracker_items` `source_kind = "mail"`, `item_id` = Message-ID) → only
//!   moved on;
//! - otherwise the body goes through the supersede engine ([`crate::supersede`], origin `Mail`):
//!   `New` writes a captured note (newsletter kind and tags, `via: email`) and a `Note` row, done
//!   (`status = x`) when the email body is complete and the sender is known; `Kept`/`Replaced`
//!   records a `Skipped` row (hidden from Intake) pointing at the resolved note. The engine owns
//!   finalizing and ticking the resolved note's own rows.
//!
//! Then `new/<f>` moves to `<maildir>/<processed>/<f>:2,S` and a loose file to
//! `<maildir>/processed/`. A failed move is logged and counted (`move_failed`), never fails the
//! step; the recorded row makes a re-run a no-op. No message file is ever deleted.
//!
//! Completeness is evaluated on the delivered email body alone (plan Decisions locked, "audience
//! != completeness"): complete unless a paywall marker (the newsletter row's, united with its
//! linked `[[blog]]` row's) matches. An email never inherits an RSS stub's `complete`.
//!
//! The Maildir is outside the vault, so `std::fs` is correct for it; vault writes go through
//! [`StepCtx`] and the supersede engine. Unknown senders are still captured (key
//! `unknown-sender`, row left open). Outcome vars: `scanned`, `new`, `kept`, `replaced`,
//! `invalid`, `incomplete`, `move_failed` (+ `disabled`).
//!
//! Readiness gate (plan round-16 WP-6): a candidate that fails [`mail::is_complete`] (a multipart
//! without its closing boundary, cut headers — a download still being written) is skipped
//! untouched and counted `incomplete`; whichever event or schedule triggered the scan, it is
//! picked up by a later one.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use cv_actions::pipelines::{BlogSource, NewsletterSource};
use cv_actions::{ConfigSet, StepKind};
use cv_ingest::access;
use cv_ingest::mail::{self, MailPost};
use cv_protocol::TrackerOutcome;
use serde::Serialize;

use super::cancel_source::{mail_keys, orphans_pass};
use super::file::kind_def;
use super::materialize::{join, now};
use super::params::{expand, get_str, get_str_list};
use super::tracker::{Candidate, record_with};
use super::web::create_or_found;
use super::{StepCtx, StepError, StepModule, StepOutcome, lint, localize};
use crate::supersede::{self, Action, Incoming, Origin, SupersedeIndex};

/// `tracker_items.source_kind` of mails.
pub const KIND: &str = "mail";
/// The `source_key` (and Intake source) of mail from no configured newsletter.
pub const UNKNOWN: &str = "unknown-sender";

/// The `track_mail` step (no params; unknown keys disarm, I5).
pub(crate) struct TrackMail;

impl StepModule for TrackMail {
    type Params = ();

    fn parse(kind: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<(), String> {
        super::no_params(kind, t)
    }

    fn run((): (), ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(ctx)
    }
}

/// `track_mail` outcome vars.
#[derive(Default, Serialize)]
struct Vars {
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    disabled: bool,
    scanned: usize,
    new: usize,
    kept: usize,
    replaced: usize,
    invalid: usize,
    incomplete: usize,
    move_failed: usize,
}

/// Where a message was found (decides how it is moved on).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    /// `<maildir>/new/`.
    New,
    /// A loose `<maildir>/*.eml`.
    Loose,
}

/// The files to scan, sorted by name within each group (`new/` first).
fn scan(root: &Path) -> Vec<(PathBuf, Where)> {
    let list = |dir: &Path, loose: bool| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .filter(|p| !loose || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("eml")))
            .collect();
        v.sort();
        v
    };
    let mut out: Vec<(PathBuf, Where)> = list(&root.join("new"), false)
        .into_iter()
        .map(|p| (p, Where::New))
        .collect();
    out.extend(list(root, true).into_iter().map(|p| (p, Where::Loose)));
    out
}

/// The destination of a handled message: `new/<f>` → `<processed>/<f>:2,S`; loose →
/// `processed/<f>`. Never an existing file (a clash gets a `-dupN` suffix).
fn destination(root: &Path, processed: &str, file: &Path, origin: Where) -> PathBuf {
    let name = file
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let (dir, stem, suffix) = match origin {
        Where::New => {
            let base = name.split(":2,").next().unwrap_or(&name).to_owned();
            (root.join(processed), base, ":2,S")
        }
        Where::Loose => (root.join("processed"), name, ""),
    };
    let mut dest = dir.join(format!("{stem}{suffix}"));
    let mut n = 1;
    while dest.exists() {
        dest = dir.join(format!("{stem}-dup{n}{suffix}"));
        n += 1;
    }
    dest
}

fn move_file(root: &Path, processed: &str, file: &Path, origin: Where) -> std::io::Result<()> {
    let dest = destination(root, processed, file, origin);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::rename(file, dest)
}

/// The sender's domain (`ann@news.example` → `news.example`).
fn sender_host(addr: &str) -> Option<String> {
    addr.rsplit_once('@')
        .map(|(_, d)| d.trim().to_lowercase())
        .filter(|d| !d.is_empty())
}

/// The note folder (the kind's `folder`, default `{period.day}`) and tags (kind tags, then the
/// row's, deduplicated).
pub(crate) fn note_target(
    ctx: &StepCtx<'_>,
    kind: &str,
    extra: &[String],
) -> Result<(String, Vec<String>, bool), StepError> {
    let (folder, mut tags, defined) = match kind_def(ctx.config, kind) {
        Ok(def) => (
            get_str(def, "folder")
                .map_err(StepError::failed)?
                .unwrap_or_else(|| "{period.day}".into()),
            get_str_list(def, "tags").map_err(StepError::failed)?,
            true,
        ),
        Err(_) => ("{period.day}".into(), vec![kind.to_owned()], false),
    };
    if tags.is_empty() {
        tags.push(kind.to_owned());
    }
    for t in cv_text::template::normalize_tags(extra) {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    Ok((expand(ctx, &folder)?, tags, defined))
}

/// Where a captured mail note points: its canonical `source` and its row's `publication`.
#[derive(Clone, Copy)]
struct NoteRefs<'a> {
    source: Option<&'a str>,
    publication: Option<&'a str>,
    /// The note's kind (decides the unticked `trim-label`).
    kind: &'a str,
}

/// A mail note's captured `description`: the cleaned body's first 40 words (reconcile's
/// description repair recomputes it the same way).
pub(crate) fn mail_description(body_md: &str) -> String {
    body_md
        .split_whitespace()
        .take(40)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The captured note's text (the filed-blog frontmatter order, plus `via` and `message-id`).
fn captured_note(
    post: &MailPost,
    refs: NoteRefs<'_>,
    title: &str,
    body_md: &str,
    tags: &[String],
    created: &str,
    complete: bool,
) -> String {
    use cv_ingest::fm::json_string;
    // `words` is the cleaned body's word count (the same count `handle` records).
    let words = body_md.split_whitespace().count();
    let mut fm = vec!["---".to_owned(), format!("title: {}", json_string(title))];
    if let Some(u) = refs.source {
        fm.push(format!("source: {u}"));
    }
    fm.push(format!("message-id: {}", json_string(&post.message_id)));
    if post.from_name.is_empty() {
        fm.push("author: []".into());
    } else {
        fm.push("author:".into());
        fm.push(format!(
            "  - {}",
            json_string(&format!("[[{}]]", post.from_name))
        ));
    }
    if let Some(p) = refs.publication {
        fm.push(cv_ingest::fm::publication_line(p));
    }
    fm.push(match post.date.as_deref().and_then(|d| d.get(..10)) {
        Some(d) => format!("published: {d}"),
        None => "published:".into(),
    });
    fm.push(format!("created: {created}"));
    fm.push(format!(
        "description: {}",
        json_string(&mail_description(body_md))
    ));
    fm.push(format!("word-count: {words}"));
    fm.push(format!("complete: {complete}"));
    fm.push("via: email".into());
    if super::file::wants_trim_label(refs.kind) {
        // A fresh note never has the key: unticked, for the user to tick once trimmed.
        fm.push("trim-label: false".into());
    }
    fm.push("tags:".into());
    for t in tags {
        fm.push(format!("  - {t}"));
    }
    fm.push("read: false".into());
    fm.push("---".into());
    let mut out = fm.join("\n");
    out.push('\n');
    if !body_md.is_empty() {
        out.push('\n');
        out.push_str(body_md);
        out.push('\n');
    }
    out
}

/// Whether the note at `path` carries `message_id` (so a re-created file is recognised).
fn is_message_note(
    ctx: &StepCtx<'_>,
    path: &str,
    message_id: &str,
) -> Result<Option<bool>, StepError> {
    Ok(ctx.read(path)?.map(|(b, _)| {
        let t = String::from_utf8_lossy(&b);
        let want = cv_ingest::fm::json_string(message_id);
        t.lines().any(|l| {
            l.strip_prefix("message-id:")
                .is_some_and(|v| v.trim() == want)
        })
    }))
}

/// `<folder>/<title>.md`, else `<title> <hash>.md`, else numbered: the first path that is free or
/// already this message's note. `(path, exists)`.
fn note_path(
    ctx: &StepCtx<'_>,
    folder: &str,
    title: &str,
    message_id: &str,
) -> Result<(String, bool), StepError> {
    let stem = cv_text::template::title_filename(title);
    let stem = if stem.is_empty() {
        "Newsletter".to_owned()
    } else {
        stem
    };
    let hash = cv_text::url_canon::short_hash(message_id);
    let mut tries = vec![join(folder, &format!("{stem}.md"))];
    tries.push(join(folder, &format!("{stem} {hash}.md")));
    tries.extend((2..20).map(|n| join(folder, &format!("{stem} {hash} {n}.md"))));
    for p in tries {
        match is_message_note(ctx, &p, message_id)? {
            None => return Ok((p, false)),
            Some(true) => return Ok((p, true)),
            Some(false) => {}
        }
    }
    Err(StepError::failed("no free note name for the newsletter"))
}

/// Counters and shared inputs of one run.
struct Run<'a> {
    rows: &'a [NewsletterSource],
    complete_percent: u32,
    sidx: SupersedeIndex,
    sup_cfg: cv_actions::pipelines::SupersedeConfig,
}

enum Handled {
    New,
    Kept,
    Replaced,
    /// Seen before: nothing to record.
    Seen,
}

/// The newsletter row a message belongs to (plan intake-followups-mail-youtube WP-2): an explicit
/// `[[newsletter]]` match always wins; otherwise a Substack message (`mail::substack_host`) mirrors
/// the first `[[blog]]` row whose feed host equals it, as a synthesized row named for the blog
/// (kind `blog`, no extra tags or markers; the blog row's own markers apply downstream).
/// WP-5's reconcile re-match of already-recorded rows reuses this.
pub(crate) fn resolve_newsletter<'a>(
    post: &MailPost,
    rows: &'a [NewsletterSource],
    blogs: &[BlogSource],
) -> Option<Cow<'a, NewsletterSource>> {
    if let Some(r) =
        mail::match_newsletter(post, rows, |r| (r.from.as_slice(), r.list_id.as_deref()))
    {
        return Some(Cow::Borrowed(r));
    }
    let host = mail::substack_host(post)?;
    let blog = blogs
        .iter()
        .find(|b| cv_text::url_canon::http_host(&b.link).is_some_and(|h| h == host))?;
    Some(Cow::Owned(NewsletterSource {
        name: blog.name.clone(),
        short: blog.short.clone(),
        from: Vec::new(),
        list_id: None,
        blog: Some(blog.name.clone()),
        kind: "blog".into(),
        tags: Vec::new(),
        paywall_markers: Vec::new(),
        hosts: Vec::new(),
        works: false,
    }))
}

/// The message body as markdown before any cleanup (the HTML part converted, else the trimmed
/// plain text): the input of the paywall-marker verdict and of `clean_body`. The HTML passes
/// through `strip_html_chrome` first (styling, spacers and small images only, none of which can
/// carry a paywall marker), so the marker verdict sees the post-strip, pre-`clean_body` markdown.
pub(crate) fn raw_markdown(post: &MailPost) -> String {
    let html = post
        .html
        .clone()
        .or_else(|| post.text.clone())
        .unwrap_or_default();
    if post.html.is_some() {
        cv_ingest::html_to_markdown(&cv_ingest::mail::strip_html_chrome(&html))
    } else {
        html.trim().to_owned()
    }
}

/// Records and (maybe) captures one parsed message. The caller holds `intake::identity`.
fn handle(
    ctx: &mut StepCtx<'_>,
    run: &mut Run<'_>,
    post: &MailPost,
    out: &mut StepOutcome,
) -> Result<Handled, StepError> {
    let nl = resolve_newsletter(post, run.rows, &ctx.config.sources().blog);
    let nl = nl.as_deref();
    let key = nl.map_or(UNKNOWN, |n| n.name.as_str()).to_owned();
    for k in [key.as_str(), UNKNOWN] {
        if ctx
            .state()
            .tracker_item(KIND, k, &post.message_id)?
            .is_some()
        {
            return Ok(Handled::Seen);
        }
    }
    let blog_row = nl.and_then(|n| n.blog.as_deref()).and_then(|b| {
        ctx.config
            .sources()
            .blog
            .iter()
            .find(|r| r.name == b)
            .cloned()
    });
    // The one post identity (plan intake-round-2-clean-dedupe-reading WP-2): publication-proven,
    // token-free, rebased onto the blog row's host. `None` leaves the Message-ID as the identity.
    let canon = mail::canonical_post_url(post, blog_row.as_ref().map(|b| b.link.as_str()));
    let mut markers: Vec<String> = nl.map(|n| n.paywall_markers.clone()).unwrap_or_default();
    if let Some(b) = &blog_row {
        markers.extend(b.paywall_markers.iter().cloned());
    }
    let body_md = raw_markdown(post);
    // The paywall-marker verdict is taken on the pre-cleanup body: removing an "Upgrade to paid"
    // chrome line must never turn a partial message complete (plan intake-followups-mail-youtube
    // WP-1, audit M1). Everything after (words, description, the captured or replacing body) is
    // the cleaned markdown.
    let marker = access::marker_hit(&body_md, &markers);
    let body_md = mail::clean_body(&body_md);
    // Per-sender boilerplate (plan round-10-feedback-todos WP-2): a matched newsletter learns
    // this message's pre-strip body once, then strips its anchored head/tail runs. Words and
    // completeness see the stripped body (boilerplate is not content). Unknown senders: neither.
    let body_md = if nl.is_some() {
        super::reconcile_boilerplate::learn_and_strip(ctx, &key, &post.message_id, &body_md)?
    } else {
        body_md
    };
    let words = body_md.split_whitespace().count();
    let complete = access::completeness(
        None,
        u32::try_from(words).unwrap_or(u32::MAX),
        None,
        words > 0,
        marker,
        run.complete_percent,
    );
    let title = if post.subject.is_empty() {
        "(no subject)".to_owned()
    } else {
        post.subject.clone()
    };
    let blog_key = blog_row.as_ref().map(|b| b.link.trim().to_owned());
    let inc = Incoming {
        own_path: None,
        urls: canon.iter().cloned().collect(),
        source: blog_key.as_deref().map(|k| ("blog", k)),
        authors: Some(post.from_name.clone())
            .filter(|n| !n.is_empty())
            .into_iter()
            .collect(),
        host: sender_host(&post.from_addr),
        published: post
            .date
            .as_deref()
            .and_then(|d| d.get(..10))
            .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
        body_md: &body_md,
        access: None,
        complete,
        publication: nl.map(NewsletterSource::publication),
        origin: Origin::Mail,
    };
    let resolved = supersede::resolve(ctx, &run.sidx, &run.sup_cfg, &inc)?;
    let mut cand = Candidate {
        source_kind: KIND.into(),
        source_key: key.clone(),
        source_name: nl.map_or_else(
            || {
                format!("Unknown sender {}", post.from_addr)
                    .trim()
                    .to_owned()
            },
            |n| n.name.clone(),
        ),
        item_id: post.message_id.clone(),
        title: title.clone(),
        url: canon.clone().unwrap_or_default(),
        duration_s: None,
        word_count: i64::try_from(words).ok(),
        access: resolved.access.clone(),
        complete: resolved.complete.or(complete),
        full_word_count: None,
    };
    if resolved.action != Action::New {
        cand.complete = resolved.complete;
        record_with(
            ctx,
            &cand,
            TrackerOutcome::Skipped,
            None,
            None,
            resolved.path.clone(),
        )?;
        if resolved.action == Action::Replaced
            && let Some(p) = &resolved.path
        {
            out.push_output(p.clone());
            if let Err(e) = localize::run(
                &localize::Params::note(p.clone(), &ctx.config.vault().remote_media),
                ctx,
            ) {
                tracing::warn!(path = %p, error = %e, "track_mail: inline localize failed");
            }
            return Ok(Handled::Replaced);
        }
        return Ok(Handled::Kept);
    }
    // New: capture a note.
    let kind = nl.map_or("blog", |n| n.kind.as_str());
    let extra = nl.map(|n| n.tags.clone()).unwrap_or_default();
    let (folder, tags, kind_defined) = note_target(ctx, kind, &extra)?;
    let (path, exists) = note_path(ctx, &folder, &title, &post.message_id)?;
    if !exists {
        let created = now().format("%Y-%m-%d").to_string();
        let refs = NoteRefs {
            source: canon.as_deref(),
            publication: nl.map(NewsletterSource::publication),
            kind,
        };
        let text = captured_note(
            post,
            refs,
            &title,
            &body_md,
            &tags,
            &created,
            complete.unwrap_or(false),
        );
        if create_or_found(ctx, &path, text.into_bytes())? {
            out.push_output(path.clone());
            if let Err(e) = localize::run(
                &localize::Params::note(path.clone(), &ctx.config.vault().remote_media),
                ctx,
            ) {
                tracing::warn!(path = %path, error = %e, "track_mail: inline localize failed");
            }
            lint::run(
                &lint::Params::note(path.clone(), kind_defined.then(|| kind.to_owned())),
                ctx,
            )?;
            // Final form: one enrich pass (plan intake-auto-enrich WP-3; child job, not a starter,
            // so it never races the inline localize/lint above).
            super::intake_enrich::enqueue_for(ctx, &path);
        }
    }
    if let Some(u) = &canon {
        run.sidx.insert_identity(&path, u);
    }
    cand.access = None;
    cand.complete = complete;
    let inserted = record_with(ctx, &cand, TrackerOutcome::Note, None, None, Some(path))?;
    // Auto-done when complete; an unknown sender is left open for a human to look at.
    if inserted && nl.is_some() && complete == Some(true) {
        ctx.state()
            .finalize_tracker_item(KIND, &key, &post.message_id, None, true)?;
    }
    Ok(Handled::New)
}

/// A config-error line when mail intake is enabled with a maildir the daemon cannot write
/// (missing, not a directory, or a create+remove probe fails, e.g. `EROFS` under the unit
/// sandbox; round-23 WP-2), else `None`. A disabled or unset maildir is not an error. The probe
/// file is `.cv-write-probe-<pid>` (a dot name the scan ignores), removed at once.
pub(crate) fn maildir_write_error(cfg: &cv_actions::pipelines::MailPipeline) -> Option<String> {
    let dir = cfg.maildir.trim();
    if !cfg.enabled || dir.is_empty() {
        return None;
    }
    let root = Path::new(dir);
    if !root.is_dir() {
        return Some(format!("mail.maildir {dir}: not an existing directory"));
    }
    let probe = root.join(format!(".cv-write-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            None
        }
        Err(e) => Some(format!(
            "mail.maildir {dir}: not writable ({e}); track-mail cannot move messages (unit ReadWritePaths? migration 0034)"
        )),
    }
}

/// Executes the step.
pub(crate) fn run(ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let mut out = StepOutcome::default();
    // First, so the disabled / no-maildir path runs it too: DB-only.
    orphans_pass(ctx, KIND, &mail_keys(ctx.config));
    let cfg = ctx.config.pipelines().mail.clone();
    if !cfg.enabled || cfg.maildir.trim().is_empty() {
        out.set_vars(&Vars {
            disabled: true,
            ..Vars::default()
        });
        return Ok(out);
    }
    let root = PathBuf::from(cfg.maildir.trim());
    let mut vars = Vars::default();
    let mut run = Run {
        rows: &ctx.config.sources().newsletter.clone(),
        complete_percent: ctx.config.pipelines().blogs.complete_percent,
        sidx: SupersedeIndex::build(ctx).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "track_mail: supersede index unavailable");
            SupersedeIndex::default()
        }),
        sup_cfg: ctx.config.pipelines().supersede.clone(),
    };
    for (file, origin) in scan(&root) {
        vars.scanned += 1;
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) => {
                vars.invalid += 1;
                tracing::warn!(file = %file.display(), error = %e, "track_mail: unreadable message left in place");
                continue;
            }
        };
        // Consumer gate (round-16 WP-6): a message still being written (unclosed multipart,
        // cut headers) is neither handled nor moved; a later scan picks it up.
        if !mail::is_complete(&bytes) {
            vars.incomplete += 1;
            tracing::debug!(file = %file.display(), "track_mail: incomplete message left for a later scan");
            continue;
        }
        let Some(post) = mail::parse_eml(&bytes) else {
            vars.invalid += 1;
            tracing::warn!(file = %file.display(), "track_mail: unparseable message left in place");
            continue;
        };
        match handle(ctx, &mut run, &post, &mut out)? {
            Handled::New => vars.new += 1,
            Handled::Kept => vars.kept += 1,
            Handled::Replaced => vars.replaced += 1,
            Handled::Seen => {}
        }
        if let Err(e) = move_file(&root, &cfg.processed, &file, origin) {
            vars.move_failed += 1;
            tracing::warn!(file = %file.display(), error = %e, "track_mail: move failed");
        }
    }
    out.output_paths.sort();
    out.output_paths.dedup();
    out.set_vars(&vars);
    Ok(out)
}
