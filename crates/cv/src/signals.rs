//! `cv signals export [--out <path>] [--weak]` (plan `v0-automation-pipelines` WP-10): the
//! source-eval training export.
//!
//! Ported from Crucible `src/sourceEval/export.ts` (`exportSourceEvalTrainingData` `:106-125`,
//! `collectSourceEvalTrainingExamples` `:127-164`, `buildCaptureBaseExample` `:253-279`,
//! `captureId` `:398-404`) and `captureIndex.ts` (`computeCaptureIndex` `:18-50`,
//! `resolveCaptureSource` `:101-121`, `buildBlogSourceResolver` `:132-151`). The label rules and
//! row type live in `cv_ingest::signals`; the per-note signals in `cv_index::signals`.
//!
//! Read-only on the index (a `Reader`, so it works while the daemon runs) and on the vault
//! (Observations text). **Never writes into the vault**: the default output is
//! `$XDG_DATA_HOME/crucible-vault/source-eval/training-<today>.jsonl` (fallback
//! `$HOME/.local/share/…`), and an `--out` inside the vault is refused.
//!
//! Dropped as Obsidian-induced: the metadata-cache walk (index queries), the in-vault export
//! folder setting (`sourceEvalExportFolder`), and the Markdown source registries (`sources.toml`).
//! Deviations: captures are every indexed note with a `yt-video-id`, `source` or `post-id`
//! property outside the period folders and the metadata roots (Crucible: notes under the daily
//! folder); a blog capture matching no configured blog is keyed by its URL host (Crucible:
//! `metadataBlogKeyForAttribution`); ignored intake items (Crucible `collectIgnoredIntakeItems`,
//! read from intake notes this design no longer writes) are not exported.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use cv_actions::ConfigSet;
use cv_actions::pipelines::{Priority, SourcesConfig};
use cv_index::Query;
use cv_index::signals::{NoteSignals, note_signals, notes_with_any_prop, observation_signals};
use cv_ingest::signals::{
    CaptureInput, CaptureLabel, RowBase, TrainingRow, build_rows, export_file_name,
    normalize_source_tags,
};
use serde_json::{Map, Value, json};

use crate::config::Config;

const USAGE: &str = "usage: cv signals export [--out <path>] [--weak]\n";

/// `cv signals …`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let Some(opts) = parse(args) else {
        eprint!("{USAGE}");
        return 2;
    };
    let r = Config::from_env().and_then(|c| {
        let mut set = ConfigSet::new();
        crate::runner::load_config_dir(&c.config_dir, &mut set);
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        export(&c, &set, |k| std::env::var(k).ok(), &opts, &today)
    });
    match r {
        Ok((path, count)) => {
            println!("path: {}\ncount: {count}", path.display());
            0
        }
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

/// Parsed `export` options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportOpts {
    /// `--out`.
    pub out: Option<PathBuf>,
    /// `--weak` (Crucible `includeWeakLabels`).
    pub weak: bool,
}

fn parse(args: &[String]) -> Option<ExportOpts> {
    let (first, rest) = args.split_first()?;
    if first != "export" {
        return None;
    }
    let mut o = ExportOpts::default();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--weak" => o.weak = true,
            "--out" => o.out = Some(PathBuf::from(it.next()?)),
            _ => return None,
        }
    }
    Some(o)
}

/// `<XDG_DATA_HOME or $HOME/.local/share>/crucible-vault/source-eval`.
pub fn default_dir(lookup: impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    let get = |k: &str| lookup(k).filter(|v| !v.is_empty());
    let data = match get("XDG_DATA_HOME") {
        Some(v) => PathBuf::from(v),
        None => PathBuf::from(get("HOME").ok_or_else(|| anyhow!("HOME is not set"))?)
            .join(".local/share"),
    };
    Ok(data.join("crucible-vault").join("source-eval"))
}

fn within(p: &Path, root: &Path) -> bool {
    let canon = |x: &Path| std::fs::canonicalize(x).unwrap_or_else(|_| x.to_path_buf());
    let root = canon(root);
    // The file may not exist yet: canonicalize its parent.
    let p = match (p.parent(), p.file_name()) {
        (Some(d), Some(n)) if !d.as_os_str().is_empty() => canon(d).join(n),
        _ => canon(p),
    };
    p.starts_with(root)
}

/// Runs the export; returns the written path and the row count.
pub fn export(
    config: &Config,
    set: &ConfigSet,
    lookup: impl Fn(&str) -> Option<String>,
    opts: &ExportOpts,
    today: &str,
) -> Result<(PathBuf, usize)> {
    let path = match &opts.out {
        Some(p) => p.clone(),
        None => default_dir(lookup)?.join(export_file_name(today)),
    };
    if within(&path, &config.vault) || within(&path, &config.protected_vault) {
        bail!(
            "{} is inside the vault; the export never writes there",
            path.display()
        );
    }
    let reader = cv_index::Reader::open_current(&config.index_db)
        .with_context(|| format!("opening the index at {}", config.index_db.display()))?;
    let rows = collect(&reader.query(), &config.vault, set, opts.weak, today)?;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut body = Vec::new();
    for r in &rows {
        serde_json::to_writer(&mut body, &row_json(r))?;
        body.push(b'\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(&body))
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
    Ok((path, rows.len()))
}

/// One row as Crucible's `SourceEvalTrainingExample` JSON (field order kept).
fn row_json(r: &TrainingRow) -> Value {
    let b = &r.base;
    json!({
        "id": b.id,
        "source_type": b.source_type,
        "source_key": b.source_key,
        "source_name": b.source_name,
        "source_tags": b.source_tags,
        "source_priority": b.source_priority,
        "title": b.title,
        "description": b.description,
        "author": b.author,
        "published": b.published,
        "word_count": b.word_count,
        "duration_seconds": b.duration_seconds,
        "label": {
            "importance": r.label.importance,
            "urgent": r.label.urgent,
            "tags": r.label.tags,
        },
        "label_source": r.label_source.as_str(),
        "rated": r.rated,
    })
}

fn priority_str(p: Priority) -> &'static str {
    match p {
        Priority::High => "high",
        Priority::Normal => "normal",
        Priority::Low => "low",
        Priority::Skip => "skip",
        Priority::Ignore => "ignore",
    }
}

/// `captures.toml` `[periods.P].folder` (trimmed), if set.
fn period_folder(set: &ConfigSet, p: &str) -> Option<String> {
    set.table("captures.toml")
        .get("periods")?
        .get(p)?
        .get("folder")?
        .as_str()
        .map(|s| s.trim_matches('/').to_owned())
        .filter(|s| !s.is_empty())
}

fn under(path: &str, folder: &str) -> bool {
    !folder.is_empty()
        && path
            .strip_prefix(folder)
            .is_some_and(|r| r.starts_with('/'))
}

/// `firstString`: the first non-empty string (or first string of a list), wikilink stripped.
fn first_string(values: &[Option<&Value>]) -> Option<String> {
    values.iter().flatten().find_map(|v| {
        let s = match v {
            Value::String(s) => Some(s.as_str()),
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .find(|s| !s.trim().is_empty()),
            _ => None,
        }?;
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let s = s.strip_prefix("[[").unwrap_or(s);
        let s = s.strip_suffix("]]").unwrap_or(s);
        Some(s.rsplit('|').next().unwrap_or(s).trim().to_owned())
    })
}

fn number(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        _ => None,
    }
}

fn str_prop<'a>(fm: &'a Map<String, Value>, k: &str) -> Option<&'a str> {
    fm.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// `coerceVideoId`: a bare 11-char id or a YouTube URL.
fn video_id(v: &str) -> Option<String> {
    let v = v.trim();
    if v.len() == 11
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Some(v.to_owned());
    }
    cv_text::url_canon::extract_video_id_from_url(v)
}

fn host(u: &str) -> Option<String> {
    url::Url::parse(u)
        .ok()?
        .host_str()
        .map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
}

struct SourceInfo {
    key: String,
    name: String,
    tags: Vec<String>,
    priority: &'static str,
}

/// Attributes one note (`resolveCaptureSource`) and builds its base row
/// (`buildCaptureBaseExample`); `None` = no source or no id (Crucible skips both).
fn base_row(
    q: &Query<'_>,
    n: &NoteSignals,
    sources: &SourcesConfig,
    yt_root: &str,
    hosts: &BTreeMap<String, cv_text::url_canon::CanonMethod>,
) -> Result<Option<RowBase>> {
    let fm = &n.props;
    let mut yt_meta = Map::new();
    let (source_type, id, info) = if let Some(vid) = str_prop(fm, "yt-video-id").and_then(video_id)
    {
        for row in q.files_with_prop("videoId", &json!(vid))? {
            if under(&row.path, yt_root) {
                for p in q.props(&row.path)? {
                    yt_meta.insert(p.key, p.value);
                }
                break;
            }
        }
        let channel = str_prop(&yt_meta, "channelId")
            .map(str::to_owned)
            .or_else(|| channel_from_link(fm.get("yt-metadata"), yt_root));
        let Some(channel) = channel else {
            return Ok(None);
        };
        let info = match sources.youtube.iter().find(|s| s.id == channel) {
            Some(s) => SourceInfo {
                key: s.id.clone(),
                name: s.channel.clone(),
                tags: s.tags.clone(),
                priority: priority_str(s.priority),
            },
            None => SourceInfo {
                name: channel.clone(),
                key: channel,
                tags: Vec::new(),
                priority: "normal",
            },
        };
        ("youtube", vid, info)
    } else {
        let source = str_prop(fm, "source");
        let raw_post = str_prop(fm, "post-id");
        let opts = cv_text::url_canon::PostIdOptions {
            method: None,
            host_rules: hosts.clone(),
        };
        let id = match (raw_post, source) {
            (Some(p), _) => p.to_owned(),
            (None, Some(s)) => cv_text::url_canon::post_id_from_url(s, &opts),
            (None, None) => return Ok(None),
        };
        let blog_name = str_prop(fm, "blog").map(|s| s.to_lowercase());
        let src_host = source.and_then(host).or_else(|| host(&id));
        let configured = sources
            .blog
            .iter()
            .find(|b| blog_name.as_deref() == Some(b.name.trim().to_lowercase().as_str()))
            .or_else(|| {
                sources
                    .blog
                    .iter()
                    .find(|b| src_host.is_some() && host(&b.link) == src_host)
            });
        let info = match configured {
            Some(b) => SourceInfo {
                key: b.link.clone(),
                name: b.name.clone(),
                tags: b.tags.clone(),
                priority: priority_str(b.priority),
            },
            None => {
                let Some(h) = src_host else {
                    return Ok(None);
                };
                SourceInfo {
                    name: h.clone(),
                    key: h,
                    tags: Vec::new(),
                    priority: "normal",
                }
            }
        };
        ("blog", id, info)
    };
    if id.is_empty() {
        return Ok(None);
    }
    let author = if source_type == "youtube" {
        first_string(&[yt_meta.get("channelTitle"), fm.get("channel")])
    } else {
        first_string(&[fm.get("author"), fm.get("authors"), fm.get("blog")])
    }
    .or_else(|| Some(info.name.clone()));
    Ok(Some(RowBase {
        id,
        source_type: source_type.to_owned(),
        source_key: info.key,
        source_tags: normalize_source_tags(&info.tags),
        source_priority: info.priority.to_owned(),
        title: str_prop(fm, "title").map_or_else(|| n.name.clone(), str::to_owned),
        description: first_string(&[
            fm.get("description"),
            fm.get("summary"),
            yt_meta.get("description"),
        ]),
        author,
        published: first_string(&[fm.get("published"), yt_meta.get("publishedAt")]),
        word_count: number(fm.get("word-count")),
        duration_seconds: number(fm.get("duration_seconds"))
            .or_else(|| number(yt_meta.get("duration_seconds"))),
        source_name: info.name,
    }))
}

/// `parseYtMetadataChannelFromLink`: `[[<root>/<channel>/…]]` → `<channel>` (not `_unavailable`).
fn channel_from_link(v: Option<&Value>, root: &str) -> Option<String> {
    let link = first_string(&[v])?;
    let link = link.split(['|', '#']).next().unwrap_or("").trim();
    let rest = link.strip_prefix(root)?.strip_prefix('/')?;
    let channel = rest.split('/').next()?.trim();
    (rest.contains('/') && !channel.is_empty() && channel != "_unavailable")
        .then(|| channel.to_owned())
}

/// Every training row of the vault.
pub fn collect(
    q: &Query<'_>,
    vault: &Path,
    set: &ConfigSet,
    weak: bool,
    today: &str,
) -> Result<Vec<TrainingRow>> {
    let pipelines = set.pipelines();
    let sources = set.sources();
    let yt_root = pipelines.youtube.metadata_root.trim_matches('/').to_owned();
    let x_root = pipelines.x.metadata_root.trim_matches('/').to_owned();
    let periods: BTreeSet<String> = ["day", "week", "month"]
        .iter()
        .filter_map(|p| period_folder(set, p))
        .collect();
    let hosts = cv_text::url_canon::build_blog_canon_host_map(
        sources
            .blog
            .iter()
            .map(|b| (b.link.as_str(), b.canon.method())),
    );
    let observations = match (weak, period_folder(set, "month")) {
        (true, Some(m)) => observation_signals(q, vault, &m)?,
        (true, None) => {
            tracing::warn!("signals: captures.toml has no [periods.month]; no observation signals");
            BTreeMap::new()
        }
        (false, _) => BTreeMap::new(),
    };
    let mut caps = Vec::new();
    for path in notes_with_any_prop(q, &["yt-video-id", "source", "post-id"])? {
        if under(&path, &yt_root) || under(&path, &x_root) {
            continue;
        }
        let Some(n) = note_signals(q, &path)? else {
            continue;
        };
        if periods.contains(&n.folder) {
            continue;
        }
        let Some(base) = base_row(q, &n, sources, &yt_root, &hosts)? else {
            continue;
        };
        caps.push(CaptureInput {
            base,
            observed_months: observations.get(&path).map_or(0, |o| o.months),
            label: n.label.map(|l| CaptureLabel {
                importance: l.importance,
                urgent: l.urgent,
                rated: l.rated,
                tags: l.tags,
            }),
            eval_skip: n.eval_skip,
            tags: n.tags,
        });
    }
    Ok(build_rows(&caps, weak, today))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const CAPTURES: &str = "[periods.day]\nfolder = \"daily\"\n[periods.month]\nfolder = \"monthly\"\nformat = \"YYYY-MM\"\n";
    const SOURCES: &str = "[[youtube]]\nchannel = \"Chan A\"\nid = \"UCa\"\ntags = [\"AI\"]\npriority = \"high\"\n[[blog]]\nname = \"Blog B\"\nlink = \"https://b.example/feed\"\n";

    fn setup() -> (Tmp, Config, ConfigSet) {
        let root = std::env::temp_dir().join(format!("cv-wp10-signals-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tmp = Tmp(root.clone());
        let vault = root.join("vault");
        let files: &[(&str, &str)] = &[
            (
                "_yt_metadata/UCa/v.md",
                "---\nvideoId: aaaaaaaaaa1\nchannelId: UCa\nchannelTitle: Chan A Official\nduration_seconds: 600\npublishedAt: 2026-09-01\n---\n",
            ),
            (
                "c/human.md",
                "---\nyt-video-id: aaaaaaaaaa1\ntitle: Human\neval-importance: 3\neval-urgent: true\neval-rated: 2026-10-01\ntags: [gold, misc]\n---\n",
            ),
            (
                "c/slop.md",
                "---\nsource: https://b.example/p/slop?utm_source=x\nword-count: 120\ntags: [low-value]\n---\n",
            ),
            (
                "c/observed.md",
                "---\nsource: https://b.example/p/obs\n---\n",
            ),
            (
                "c/plain.md",
                "---\nsource: https://b.example/p/plain\n---\n",
            ),
            (
                "c/skipped.md",
                "---\nsource: https://b.example/p/skip\neval-skip: true\neval-importance: 2\n---\n",
            ),
            (
                "daily/2026-10-01.md",
                "---\nsource: https://b.example/p/daily\ntags: [gold]\n---\n",
            ),
            (
                "monthly/2026-10.md",
                "# Observations\n- [[observed]]\n  - q\n",
            ),
        ];
        for (rel, text) in files {
            let p = vault.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let index = root.join("index.db");
        drop(cv_index::Index::create(&vault, &index).unwrap());
        let (home, idx, v) = (
            root.join("home").to_string_lossy().into_owned(),
            index.to_string_lossy().into_owned(),
            vault.to_string_lossy().into_owned(),
        );
        let config = Config::resolve(|k| match k {
            "HOME" => Some(home.clone()),
            "CV_SOCKET" => Some("/nonexistent.sock".into()),
            "CV_INDEX_DB" => Some(idx.clone()),
            "CV_VAULT" => Some(v.clone()),
            "CV_PROTECTED_VAULT" => Some(v.clone()),
            _ => None,
        })
        .unwrap();
        let mut set = ConfigSet::new();
        set.reload(
            &BTreeMap::from([
                ("captures.toml".to_owned(), Some(CAPTURES.to_owned())),
                ("sources.toml".to_owned(), Some(SOURCES.to_owned())),
            ]),
            &crate::steps::validate,
        );
        (tmp, config, set)
    }

    fn rows(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn export_human_and_weak_rows_to_the_data_dir() {
        let (tmp, config, set) = setup();
        let data = tmp.0.join("data").to_string_lossy().into_owned();
        let env = |k: &str| (k == "XDG_DATA_HOME").then(|| data.clone());
        let today = "2026-10-15";
        let (path, count) = export(&config, &set, env, &ExportOpts::default(), today).unwrap();
        assert_eq!(
            path,
            tmp.0
                .join("data/crucible-vault/source-eval/training-2026-10-15.jsonl")
        );
        assert_eq!(count, 1);
        let r = &rows(&path)[0];
        let keys: Vec<&str> = r.as_object().unwrap().keys().map(String::as_str).collect();
        let mut want = [
            "id",
            "source_type",
            "source_key",
            "source_name",
            "source_tags",
            "source_priority",
            "title",
            "description",
            "author",
            "published",
            "word_count",
            "duration_seconds",
            "label",
            "label_source",
            "rated",
        ];
        let mut got = keys.clone();
        got.sort_unstable();
        want.sort_unstable();
        assert_eq!(got, want);
        assert_eq!(
            *r,
            json!({
                "id": "aaaaaaaaaa1", "source_type": "youtube", "source_key": "UCa",
                "source_name": "Chan A", "source_tags": ["#ai"], "source_priority": "high",
                "title": "Human", "description": null, "author": "Chan A Official",
                "published": "2026-09-01", "word_count": null, "duration_seconds": 600,
                "label": {"importance": 3, "urgent": true, "tags": ["gold"]},
                "label_source": "human", "rated": "2026-10-01",
            })
        );

        let weak = ExportOpts {
            out: Some(tmp.0.join("out/w.jsonl")),
            weak: true,
        };
        let (path, count) = export(&config, &set, env, &weak, today).unwrap();
        assert_eq!(
            count, 3,
            "human + slop + observed; plain/skip/daily excluded"
        );
        let all = rows(&path);
        let by_id = |suffix: &str| {
            all.iter()
                .find(|r| r["id"].as_str().unwrap().ends_with(suffix))
                .unwrap()
                .clone()
        };
        let slop = by_id("/p/slop");
        assert_eq!(slop["id"], "https://b.example/p/slop");
        assert_eq!(slop["label"]["importance"], 0);
        assert_eq!(slop["source_key"], "https://b.example/feed");
        assert_eq!(
            (slop["word_count"].clone(), slop["rated"].clone()),
            (json!(120), json!(today))
        );
        assert_eq!(by_id("/p/obs")["label"]["importance"], 4);
        assert_eq!(by_id("/p/obs")["label_source"], "weak");

        // Never into the vault.
        let bad = ExportOpts {
            out: Some(config.vault.join("x.jsonl")),
            weak: false,
        };
        assert!(export(&config, &set, env, &bad, today).is_err());
        assert!(!config.vault.join("x.jsonl").exists());
        assert_eq!(
            parse(&["export".into(), "--weak".into()]),
            Some(ExportOpts {
                out: None,
                weak: true
            })
        );
        assert_eq!(parse(&["export".into(), "--bogus".into()]), None);
        let home = |k: &str| (k == "HOME").then(|| "/h".to_owned());
        assert_eq!(
            default_dir(home).unwrap(),
            PathBuf::from("/h/.local/share/crucible-vault/source-eval")
        );
    }
}
