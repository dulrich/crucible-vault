//! `cv media audit [--vault <path>] [--state-db <path>] [--strict] [--verbose]` (plan
//! remote-media-sprint WP-4, Decision 6): a read-only count of what is left to localize.
//!
//! Walks the vault filesystem (every `.md` outside `_custom/` and the dot-dirs, the same set the
//! `localize` step's `scope = "vault"` covers) and counts, outside fenced code, by kind: remote
//! image embeds, unconverted video/post/iframe embeds, remote audio and PDF embeds, and unmanaged
//! (non-`_MD5`) local image refs; plus describe coverage over the `_MD5` image files against
//! `state.db`'s `image_descriptions` (opened read-only, never migrated). Every count is also
//! split by top-level folder. **Output is counts only** — never note text or note paths (folder
//! names and, for the failed-describe exception, md5s and failure text).
//!
//! Exit 0; with `--strict` exit 1 when any acceptance count is non-zero: remote image embeds,
//! unconverted video/post/iframe embeds, audio/PDF embeds while `[remote_media]` says `localize`
//! for them, unmanaged local image refs, undescribed images, or describe coverage unavailable
//! (`state.db` absent: nothing can be proved). Images whose describe failed permanently under a
//! cv provider (`failed` with `providerId`) are a reported exception, not undescribed; so is an
//! unmanaged local image ref whose file is nowhere in the vault (no basename match), which is
//! counted apart from the unmanaged refs.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use cv_actions::vault::{MediaMode, VaultConfig};
use cv_search::prepare::localized_image_md5;
use cv_state::StateReader;
use cv_text::attachments::{parse_attachment_refs, unmanaged_local_image_refs};
use cv_text::remote_media::{EffectiveKind, RemoteKind, effective_kind, scan_html_media};
use cv_text::vpath::basename;

use crate::config::Config;
use crate::steps::util::vault_files;

const USAGE: &str = "usage: cv media audit [--vault <path>] [--state-db <path>] [--strict] [--verbose]\n\n  Read-only: counts remote images, unconverted video/post/iframe embeds, remote audio/pdf,\n  unmanaged local images and undescribed images across the vault, by kind and top-level folder.\n  Prints counts only (never note text or paths). Exit 0; --strict exits 1 when any acceptance\n  count is non-zero. --verbose lists permanently failed describes by md5.\n";

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif"];

/// Parsed options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Opts {
    vault: Option<PathBuf>,
    state_db: Option<PathBuf>,
    strict: bool,
    verbose: bool,
}

fn parse(args: &[String]) -> Result<Option<Opts>, String> {
    let mut o = Opts::default();
    let mut it = args.iter();
    match it.next().map(String::as_str) {
        Some("audit") => {}
        Some("--help" | "-h" | "help") | None => return Ok(None),
        Some(other) => return Err(format!("unknown media command {other:?}")),
    }
    while let Some(a) = it.next() {
        match a.as_str() {
            "--help" | "-h" => return Ok(None),
            "--strict" => o.strict = true,
            "--verbose" => o.verbose = true,
            "--vault" => {
                o.vault = Some(PathBuf::from(it.next().ok_or("--vault needs a path")?));
            }
            "--state-db" => {
                o.state_db = Some(PathBuf::from(it.next().ok_or("--state-db needs a path")?));
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(Some(o))
}

/// `cv media …`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    match parse(args) {
        Ok(None) => {
            print!("{USAGE}");
            0
        }
        Err(e) => {
            eprintln!("cv: {e}");
            eprint!("{USAGE}");
            2
        }
        Ok(Some(o)) => match audit_command(&o) {
            Ok((text, ok)) => {
                print!("{text}");
                i32::from(!ok)
            }
            Err(e) => {
                eprintln!("cv: {e:#}");
                1
            }
        },
    }
}

fn audit_command(o: &Opts) -> Result<(String, bool)> {
    // The socket is unused here; a missing XDG_RUNTIME_DIR must not stop a read-only audit.
    let cfg = Config::resolve(|k| {
        std::env::var(k)
            .ok()
            .or_else(|| (k == "CV_SOCKET").then(|| "/nonexistent".to_owned()))
    })?;
    let vault = o.vault.clone().unwrap_or_else(|| cfg.vault.clone());
    let state_db = o.state_db.clone().unwrap_or_else(|| cfg.state_db.clone());
    if !vault.is_dir() {
        bail!("vault {} is not a directory", vault.display());
    }
    let config_dir = match (&o.vault, std::env::var("CV_CONFIG_DIR").ok()) {
        (_, Some(d)) if !d.is_empty() => PathBuf::from(d),
        (Some(v), _) => v.join("_custom").join("crucible-vault"),
        (None, _) => cfg.config_dir.clone(),
    };
    let (modes, mode_note) = load_modes(&config_dir);
    let reader = if state_db.is_file() {
        Some(StateReader::open(&state_db).map_err(|e| anyhow!("state.db: {e}"))?)
    } else {
        None
    };
    let report = audit(&vault, reader.as_ref(), modes)?;
    let mut text = render(&report, o.verbose);
    if let Some(n) = mode_note {
        text.push_str(&n);
    }
    let ok = !o.strict || report.acceptance_ok();
    if o.strict {
        text.push_str(if ok {
            "strict: ok\n"
        } else {
            "strict: FAILED (non-zero acceptance counts)\n"
        });
    }
    Ok((text, ok))
}

/// The `[remote_media] audio`/`pdf` modes that decide whether a remote ref is acceptable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Modes {
    audio_forbidden: bool,
    pdf_forbidden: bool,
}

fn load_modes(config_dir: &Path) -> (Modes, Option<String>) {
    let defaults = Modes {
        audio_forbidden: false,
        pdf_forbidden: false,
    };
    let path = config_dir.join("vault.toml");
    let Ok(src) = std::fs::read_to_string(&path) else {
        return (defaults, None);
    };
    let parsed = src
        .parse::<toml::Table>()
        .map_err(|e| e.message().to_owned())
        .and_then(|t| VaultConfig::from_table(&t));
    match parsed {
        Ok(c) => (
            Modes {
                audio_forbidden: c.remote_media.audio == MediaMode::Localize,
                pdf_forbidden: c.remote_media.pdf == MediaMode::Localize,
            },
            None,
        ),
        Err(_) => (
            defaults,
            Some("note: vault.toml is invalid; audio/pdf modes assumed `link`\n".to_owned()),
        ),
    }
}

/// Per-kind counts, in total and by top-level folder.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Tally {
    total: u64,
    by_folder: BTreeMap<String, u64>,
}

impl Tally {
    fn add(&mut self, folder: &str, n: u64) {
        if n > 0 {
            self.total += n;
            *self.by_folder.entry(folder.to_owned()).or_default() += n;
        }
    }
}

/// Describe coverage over the vault's `_MD5` image files.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Coverage {
    /// Distinct md5s among the `_MD5` image files.
    images: u64,
    /// Records by kind (`vision`, `svg-text`, `imported`, `failed`).
    by_kind: BTreeMap<String, u64>,
    undescribed: Tally,
    /// `failed` with `providerId`: md5 and failure text.
    exception: Vec<(String, String)>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Report {
    notes: u64,
    remote_images: Tally,
    video: Tally,
    audio: Tally,
    pdf: Tally,
    unmanaged: Tally,
    /// Unmanaged local image refs whose file is nowhere in the vault (no basename match): a
    /// reported exception, outside acceptance — localize cannot adopt a file that does not exist.
    missing: Tally,
    /// `None` = `state.db` absent (coverage unavailable).
    coverage: Option<Coverage>,
    /// The `_MD5` image count even when coverage is unavailable.
    managed_images: u64,
    modes: Option<(bool, bool)>,
}

impl Report {
    fn acceptance_ok(&self) -> bool {
        let (audio_forbidden, pdf_forbidden) = self.modes.unwrap_or((false, false));
        self.remote_images.total == 0
            && self.video.total == 0
            && !(audio_forbidden && self.audio.total > 0)
            && !(pdf_forbidden && self.pdf.total > 0)
            && self.unmanaged.total == 0
            && self
                .coverage
                .as_ref()
                .is_some_and(|c| c.undescribed.total == 0)
    }
}

fn top_folder(rel: &str) -> &str {
    rel.split_once('/').map_or("(root)", |(f, _)| f)
}

/// Counts of one note's remote refs by kind: (images, video, audio, pdf).
fn note_counts(text: &str) -> (u64, u64, u64, u64) {
    let (mut img, mut vid, mut aud, mut pdf) = (0_u64, 0_u64, 0_u64, 0_u64);
    let mut count = |k: EffectiveKind| match k {
        EffectiveKind::Kind(RemoteKind::Image | RemoteKind::Unknown) => img += 1,
        EffectiveKind::IdlessVideoPage
        | EffectiveKind::Kind(
            RemoteKind::VideoPage { .. }
            | RemoteKind::VideoFile
            | RemoteKind::Post
            | RemoteKind::Embed,
        ) => vid += 1,
        EffectiveKind::Kind(RemoteKind::AudioFile) => aud += 1,
        EffectiveKind::Kind(RemoteKind::Pdf) => pdf += 1,
    };
    for m in parse_attachment_refs(text) {
        if m.is_embed && m.is_remote && m.syntax == cv_text::attachments::Syntax::Md {
            count(effective_kind(&m.link, None));
        }
    }
    for h in scan_html_media(text) {
        count(effective_kind(&h.src, Some(h.tag)));
    }
    (img, vid, aud, pdf)
}

fn is_image_file(rel: &str) -> bool {
    rel.rsplit_once('.')
        .is_some_and(|(_, e)| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

fn audit(vault: &Path, state: Option<&StateReader>, modes: Modes) -> Result<Report> {
    let mut r = Report {
        modes: Some((modes.audio_forbidden, modes.pdf_forbidden)),
        ..Report::default()
    };
    // md5 -> top-level folder of its first file.
    let mut md5s: BTreeMap<String, String> = BTreeMap::new();
    // Unmanaged refs per note (folder, basenames), resolved once every file is known.
    let mut pending: Vec<(String, Vec<String>)> = Vec::new();
    // Basenames of every non-note file, `_custom/` included (an embed may resolve there).
    let mut file_names: HashSet<String> = vault_files(&vault.join("_custom"))
        .iter()
        .map(|f| basename(f).to_owned())
        .collect();
    for rel in vault_files(vault) {
        let folder = top_folder(&rel).to_owned();
        if rel.ends_with(".md") {
            let Ok(bytes) = std::fs::read(vault.join(&rel)) else {
                continue;
            };
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            r.notes += 1;
            let (img, vid, aud, pdf) = note_counts(text);
            r.remote_images.add(&folder, img);
            r.video.add(&folder, vid);
            r.audio.add(&folder, aud);
            r.pdf.add(&folder, pdf);
            pending.push((folder, unmanaged_local_image_refs(text)));
            continue;
        }
        file_names.insert(basename(&rel).to_owned());
        if is_image_file(&rel)
            && let Some((md5, _)) = localized_image_md5(&rel)
        {
            md5s.entry(md5).or_insert(folder);
        }
    }
    for (folder, names) in &pending {
        let found = names
            .iter()
            .filter(|n| file_names.contains(n.as_str()))
            .count() as u64;
        r.unmanaged.add(folder, found);
        r.missing.add(folder, names.len() as u64 - found);
    }
    r.managed_images = md5s.len() as u64;
    if let Some(st) = state {
        let mut cov = Coverage {
            images: md5s.len() as u64,
            ..Coverage::default()
        };
        let keys: Vec<String> = md5s.keys().cloned().collect();
        let mut recorded = BTreeSet::new();
        for chunk in keys.chunks(500) {
            for d in st
                .image_descriptions_for(chunk)
                .map_err(|e| anyhow!("state.db: {e}"))?
            {
                *cov.by_kind.entry(d.kind.clone()).or_default() += 1;
                let raw: serde_json::Value = serde_json::from_str(&d.raw).unwrap_or_default();
                if d.kind == "failed" && crate::steps::image_describe::is_retryable(&d) {
                    // The describe step retries it (legacy length or no provenance): still
                    // undescribed.
                } else if d.kind == "failed" {
                    let f = raw.get("failure").and_then(|v| v.as_str()).unwrap_or("");
                    cov.exception
                        .push((d.md5.clone(), f.chars().take(80).collect()));
                    recorded.insert(d.md5);
                } else {
                    recorded.insert(d.md5);
                }
            }
        }
        for (md5, folder) in &md5s {
            if !recorded.contains(md5) {
                cov.undescribed.add(folder, 1);
            }
        }
        r.coverage = Some(cov);
    }
    Ok(r)
}

fn section(out: &mut String, label: &str, t: &Tally) {
    out.push_str(&format!("{label}: {}\n", t.total));
    for (f, n) in &t.by_folder {
        out.push_str(&format!("  {f}: {n}\n"));
    }
}

fn render(r: &Report, verbose: bool) -> String {
    let mut o = String::new();
    o.push_str(&format!("notes scanned: {}\n", r.notes));
    section(&mut o, "remote image embeds", &r.remote_images);
    section(&mut o, "unconverted video/post/iframe embeds", &r.video);
    let (af, pf) = r.modes.unwrap_or((false, false));
    section(
        &mut o,
        &format!(
            "remote audio embeds ({})",
            if af {
                "forbidden"
            } else {
                "allowed: link mode"
            }
        ),
        &r.audio,
    );
    section(
        &mut o,
        &format!(
            "remote pdf embeds ({})",
            if pf {
                "forbidden"
            } else {
                "allowed: link mode"
            }
        ),
        &r.pdf,
    );
    section(&mut o, "unmanaged local image refs", &r.unmanaged);
    section(
        &mut o,
        "reported exception (local image ref, file missing from vault)",
        &r.missing,
    );
    o.push_str(&format!("managed _MD5 images: {}\n", r.managed_images));
    match &r.coverage {
        None => o.push_str("describe coverage: unavailable (state.db not found)\n"),
        Some(c) => {
            let recorded: u64 = c.by_kind.values().sum();
            o.push_str(&format!("describe records: {recorded}\n"));
            for (k, n) in &c.by_kind {
                o.push_str(&format!("  {k}: {n}\n"));
            }
            section(&mut o, "undescribed images", &c.undescribed);
            o.push_str(&format!(
                "reported exception (describe failed permanently): {}\n",
                c.exception.len()
            ));
            if verbose {
                for (md5, f) in &c.exception {
                    o.push_str(&format!("  {md5}: {f}\n"));
                }
            }
        }
    }
    o
}

#[cfg(test)]
#[path = "media_audit_tests.rs"]
mod tests;
