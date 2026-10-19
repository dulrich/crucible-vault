//! Step module `capture_repair` (plan `usability-sprint-captures-links` WP-2): a one-time repair
//! of captured blog notes under a folder — slug-named notes are renamed to the WP-1 title
//! filename (through the writer's rename, so incoming links are rewritten exactly as `move`
//! rewrites them), and malformed `tags` entries are rewritten through
//! [`cv_text::template::normalize_tags`]. Idempotent: a second run renames and retags nothing.
//!
//! Tags are recovered from the **raw** frontmatter lines (audit E3): `  - blog #x #y` is a YAML
//! comment after `blog`, so a parsed read would see only `blog` and lose `x`, `y`.

use cv_actions::{ConfigSet, StepKind};
use cv_lint::{FrontmatterDoc, JsValue};
use serde::Serialize;

use super::{StepCtx, StepError, StepModule, StepOutcome};

pub(crate) struct Params {
    folder: String,
    limit: Option<u64>,
}

fn parse_params(t: &toml::Table) -> Result<Params, String> {
    if let Some(k) = super::params::unknown_key(StepKind::CaptureRepair, t) {
        return Err(format!("capture_repair: unknown param {k:?}"));
    }
    let folder = match t.get("folder") {
        Some(toml::Value::String(s)) if !s.trim_matches('/').is_empty() => {
            s.trim_matches('/').to_owned()
        }
        Some(_) => return Err("capture_repair: folder must be a non-empty string".into()),
        None => return Err("capture_repair: folder is required".into()),
    };
    let limit = match t.get("limit") {
        None => None,
        Some(toml::Value::Integer(v)) if *v > 0 => u64::try_from(*v).ok(),
        Some(_) => return Err("capture_repair: limit must be a positive integer".into()),
    };
    Ok(Params { folder, limit })
}

/// The `capture_repair` step (`folder` required, `limit` optional).
pub(crate) struct CaptureRepair;

impl StepModule for CaptureRepair {
    type Params = Params;

    fn parse(_: StepKind, t: &toml::Table, _config: &ConfigSet) -> Result<Params, String> {
        parse_params(t)
    }

    fn run(p: Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
        run(&p, ctx)
    }
}

/// `capture_repair` outcome vars.
#[derive(Serialize)]
struct Vars {
    scanned: u64,
    renamed: u64,
    retagged: u64,
    skipped: u64,
}

fn unquote(v: &str) -> &str {
    let v = v.trim();
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner;
        }
    }
    v
}

/// The `tags` block in the raw frontmatter: (line range `[start, end)` in `lines`, the raw
/// entries as written — `#…` text YAML would treat as a comment kept verbatim).
fn raw_tags(lines: &[&str]) -> Option<((usize, usize), Vec<String>)> {
    let start = lines.iter().position(|l| {
        l.strip_prefix("tags:")
            .is_some_and(|r| r.is_empty() || r.starts_with([' ', '\t']))
    })?;
    let inline = lines[start]["tags:".len()..].trim();
    let mut items = Vec::new();
    if !inline.is_empty() {
        let inner = inline
            .strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
            .unwrap_or(inline);
        items.extend(
            inner
                .split(',')
                .map(|s| unquote(s).to_owned())
                .filter(|s| !s.is_empty()),
        );
    }
    let mut end = start + 1;
    while let Some(l) = lines.get(end) {
        let item = l
            .trim_start()
            .strip_prefix('-')
            .filter(|r| r.is_empty() || r.starts_with(' '));
        let Some(item) = item else { break };
        items.push(unquote(item).to_owned());
        end += 1;
    }
    Some(((start, end), items))
}

/// What the repair would change in one note.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// The rewritten text when the tags change.
    pub retagged: Option<String>,
    /// The title filename stem (WP-1 `note_name`).
    pub name: String,
    /// The `short_hash` input (post id, else source).
    pub hash_key: String,
    /// The pre-WP-1 `note_slug` stem (only notes named by it are renamed).
    pub old_slug: String,
}

/// Plans the repair of one note; `None` when it is not a blog capture (no `blog` tag).
pub fn plan(text: &str) -> Option<Plan> {
    let doc = FrontmatterDoc::new(text);
    let raw = doc.raw_block()?;
    let lines: Vec<&str> = raw.lines().collect();
    let ((start, end), items) = raw_tags(&lines)?;
    let norm = cv_text::template::normalize_tags(&items);
    if !norm.iter().any(|t| t == "blog") {
        return None;
    }
    let retagged = (norm != items).then(|| {
        let mut out: Vec<String> = lines[..start].iter().map(|s| (*s).to_owned()).collect();
        out.push("tags:".into());
        out.extend(norm.iter().map(|t| format!("  - {t}")));
        out.extend(lines[end..].iter().map(|s| (*s).to_owned()));
        let mut block = out.join("\n");
        if raw.ends_with('\n') {
            block.push('\n');
        }
        // `raw` borrows from `doc`'s own copy of the text.
        let full = doc.text();
        let off = raw.as_ptr() as usize - full.as_ptr() as usize;
        format!("{}{block}{}", &full[..off], &full[off + raw.len()..])
    });
    let field = |k: &str| match doc.get(k) {
        Ok(Some(JsValue::Str(s))) => s,
        Ok(Some(JsValue::Num(n))) => n.to_string(),
        _ => String::new(),
    };
    let (title, source, post_id) = (field("title"), field("source"), field("post-id"));
    Some(Plan {
        retagged,
        name: note_name(&title, &source),
        old_slug: old_slug(&title, &source),
        hash_key: if post_id.is_empty() { source } else { post_id },
    })
}

/// The pre-WP-1 `cv_ingest::blogs::note_slug` (`51ecbed`): slugified title, else the
/// canonical-url slug; ≤ 80 chars; else `post`.
fn old_slug(title: &str, source: &str) -> String {
    let mut s = cv_text::template::slugify(title);
    if s.is_empty() || title == "(untitled)" {
        s = cv_text::url_canon::slug_for_canonical(source);
    }
    if s.len() > 80 {
        s.truncate(80);
        s = s.trim_end_matches('-').to_owned();
    }
    if s.is_empty() { "post".into() } else { s }
}

/// `cv_ingest::blogs::note_name` over the capture's frontmatter: the title filename, else the
/// canonical-url slug (≤ 80 chars), else `post`.
fn note_name(title: &str, source: &str) -> String {
    if !title.is_empty() && title != "(untitled)" {
        let t = cv_text::template::title_filename(title);
        if !t.is_empty() {
            return t;
        }
    }
    let mut s = cv_text::url_canon::slug_for_canonical(source);
    if s.len() > 80 {
        s.truncate(80);
        s = s.trim_end_matches('-').to_owned();
    }
    if s.is_empty() { "post".into() } else { s }
}

fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_owned()
    } else {
        format!("{folder}/{name}")
    }
}

/// Executes one `capture_repair` step.
fn run(p: &Params, ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    let files = super::util::md_files_under(ctx, &p.folder);
    let scanned = files.len() as u64;
    let (mut renamed, mut retagged, mut changed_notes) = (0_u64, 0_u64, 0_u64);
    let mut out = StepOutcome::default();
    for path in files {
        if p.limit.is_some_and(|l| changed_notes >= l) {
            break;
        }
        let Some((bytes, mut hash)) = ctx.read(&path)? else {
            continue;
        };
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        let Some(plan) = plan(&text) else {
            continue;
        };
        let mut touched = false;
        if let Some(new_text) = plan.retagged {
            hash = ctx.write(&path, new_text.into_bytes(), Some(hash))?;
            retagged += 1;
            touched = true;
            out.push_output(path.clone());
        }
        let (dir, base) = cv_text::vpath::split(&path);
        let first = format!("{}.md", plan.name);
        let alt = format!(
            "{} {}.md",
            plan.name,
            cv_text::url_canon::short_hash(&plan.hash_key)
        );
        let sh = cv_text::url_canon::short_hash(&plan.hash_key);
        let slug_named =
            base == format!("{}.md", plan.old_slug) || base == format!("{}-{sh}.md", plan.old_slug);
        if slug_named && base != first && base != alt {
            let dst = [join(dir, &first), join(dir, &alt)]
                .into_iter()
                .find(|d| matches!(ctx.read(d), Ok(None)));
            if let Some(dst) = dst {
                let (s2, d2) = (path.clone(), dst.clone());
                let r = ctx.with_writer(&[path.as_str(), dst.as_str()], move |vw| {
                    vw.rename(&s2, &d2, hash)
                })??;
                renamed += 1;
                touched = true;
                out.output_paths.retain(|o| o != &path);
                out.push_output(dst);
                for w in r.rewritten {
                    out.push_output(w);
                }
            } else {
                tracing::warn!("capture_repair: both rename targets taken; left as-is");
            }
        }
        if touched {
            changed_notes += 1;
        }
    }
    let skipped = scanned - changed_notes;
    tracing::info!(
        folder = %p.folder,
        scanned,
        renamed,
        retagged,
        skipped,
        "capture repair"
    );
    out.set_vars(&Vars {
        scanned,
        renamed,
        retagged,
        skipped,
    });
    Ok(out)
}
