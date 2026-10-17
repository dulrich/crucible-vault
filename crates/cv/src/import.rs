//! `cv import …` (plan `v0-automation-pipelines` WP-3; WP-12 adds `sources` / `ignored`).
//!
//! `cv import sources --from <md>…` prints a `sources.toml` from Crucible's registry tables;
//! `cv import ignored --from <ignored.md>` fills `state.db` `ignored_ids` (daemon stopped).
//!
//! `cv import link-records --from <jobs.sqlite>`: reads Crucible's frozen `link_records` table
//! (opened `mode=ro&immutable=1`, `cv_state::read_crucible_link_records`) and feeds every row
//! through the `state.db` merge (`StateDb::upsert_link_record`), so a re-import is idempotent and
//! merges with anything scanned since. Refuses while a daemon accepts connections on `CV_SOCKET`
//! (state.db has a single writer — the `cv cutover mark` rule, `cutover.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cv_state::{LinkRecordInput, QueueConfig, StateDb};

use crate::config::Config;

const USAGE: &str = "usage: cv import link-records --from <jobs.sqlite>\n       cv import sources --from <md> [--from <md>]   (prints sources.toml)\n       cv import ignored --from <ignored.md>   (daemon stopped)\n";

/// `cv import …`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let r = match (args.first().map(String::as_str), args.get(1..)) {
        (Some("link-records"), rest) => match parse_from(rest) {
            Some(from) => Config::from_env().and_then(|c| link_records(&c, &from)),
            None => return usage(),
        },
        (Some("sources"), Some(rest)) => match parse_froms(rest) {
            Some(froms) => sources(&froms),
            None => return usage(),
        },
        (Some("ignored"), rest) => match parse_from(rest) {
            Some(from) => Config::from_env().and_then(|c| ignored(&c, &from)),
            None => return usage(),
        },
        _ => return usage(),
    };
    match r {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

fn usage() -> i32 {
    eprint!("{USAGE}");
    2
}

/// One or more `--from <path>` pairs.
fn parse_froms(args: &[String]) -> Option<Vec<PathBuf>> {
    let mut out = Vec::new();
    for pair in args.chunks(2) {
        match pair {
            [flag, path] if flag == "--from" => out.push(PathBuf::from(path)),
            _ => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

fn parse_from(args: Option<&[String]>) -> Option<PathBuf> {
    match args? {
        [flag, path] if flag == "--from" => Some(PathBuf::from(path)),
        _ => None,
    }
}

/// Counts of one import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImportCounts {
    pub read: u64,
    pub created: u64,
    pub updated: u64,
    pub unchanged: u64,
}

/// Imports `from`'s `link_records` into `state_db` (no daemon check; see [`link_records`]).
pub fn import_link_records(state_db: &Path, from: &Path) -> Result<ImportCounts> {
    if !from.is_file() {
        bail!("{} is not a file", from.display());
    }
    let rows = cv_state::read_crucible_link_records(from)
        .with_context(|| format!("reading link_records from {}", from.display()))?;
    let mut db = StateDb::open(state_db, crate::now_ms(), QueueConfig::default())
        .with_context(|| format!("opening {}", state_db.display()))?;
    let mut c = ImportCounts::default();
    for r in rows {
        c.read += 1;
        let o = db.upsert_link_record(&LinkRecordInput::from(r))?;
        match (o.created, o.changed) {
            (true, _) => c.created += 1,
            (false, true) => c.updated += 1,
            (false, false) => c.unchanged += 1,
        }
    }
    Ok(c)
}

/// `cv import link-records --from <path>`: refuses while the daemon socket accepts connections.
pub fn link_records(config: &Config, from: &Path) -> Result<String> {
    if std::os::unix::net::UnixStream::connect(&config.socket).is_ok() {
        bail!(
            "something is accepting connections on {}; stop the daemon first — state.db has a single writer",
            config.socket.display()
        );
    }
    let c = import_link_records(&config.state_db, from)?;
    Ok(format!(
        "read {}, created {}, updated {}, unchanged {}",
        c.read, c.created, c.updated, c.unchanged
    ))
}

// ---------------------------------------------------------------------------------------------
// `cv import sources`: Crucible's registry tables → a `sources.toml` document on stdout.
// ---------------------------------------------------------------------------------------------

/// Splits one `| a | b |` row into trimmed cells (`markdownTable.ts` `splitRow`).
fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let Some(inner) = t.strip_prefix('|') else {
        return Vec::new();
    };
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner.split('|').map(|c| c.trim().to_owned()).collect()
}

fn is_delimiter_row(line: &str) -> bool {
    let cells = split_row(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.strip_prefix(':').unwrap_or(c);
            let c = c.strip_suffix(':').unwrap_or(c);
            !c.is_empty() && c.chars().all(|ch| ch == '-')
        })
}

/// `markdownTable.ts` `parseTable`: the first table whose first header cell is `first`
/// (case-insensitive) with at least `min_cols` cells, keyed by lowercased header; rows run until
/// the first non-`|` line or empty first cell.
fn parse_table(content: &str, first: &str, min_cols: usize) -> Vec<BTreeMap<String, String>> {
    let lines: Vec<&str> = content.lines().collect();
    let Some(h) = lines.iter().position(|l| {
        let c = split_row(l);
        c.len() >= min_cols && c[0].eq_ignore_ascii_case(first)
    }) else {
        return Vec::new();
    };
    if !lines.get(h + 1).is_some_and(|l| is_delimiter_row(l)) {
        return Vec::new();
    }
    let headers: Vec<String> = split_row(lines[h])
        .into_iter()
        .map(|c| c.to_lowercase())
        .collect();
    let mut rows = Vec::new();
    for line in &lines[h + 2..] {
        if !line.trim().starts_with('|') {
            break;
        }
        let cells = split_row(line);
        if cells.first().is_none_or(String::is_empty) {
            break;
        }
        rows.push(
            headers
                .iter()
                .enumerate()
                .filter(|(_, k)| !k.is_empty())
                .map(|(i, k)| (k.clone(), cells.get(i).cloned().unwrap_or_default()))
                .collect(),
        );
    }
    rows
}

/// `[name](url)` → `name` (`youtube.ts` `channelDisplayName`).
fn display_name(raw: &str) -> String {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix('[')
        && let Some((name, tail)) = rest.split_once("](")
        && tail.ends_with(')')
        && !name.contains(']')
    {
        return name.trim().to_owned();
    }
    t.to_owned()
}

/// `[x](url)` / `<url>` / raw → url (`blogs.ts` `extractLinkUrl`; a link title is dropped).
fn link_url(raw: &str) -> String {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix('[')
        && let Some((_, tail)) = rest.split_once("](")
        && let Some(inner) = tail.strip_suffix(')')
    {
        let inner = inner.trim();
        let url = inner.split_whitespace().next().unwrap_or("");
        return url.trim_start_matches('<').trim_end_matches('>').to_owned();
    }
    if let Some(inner) = t.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
        return inner.trim().to_owned();
    }
    t.to_owned()
}

fn tags(raw: Option<&String>) -> Vec<String> {
    cv_text::template::normalize_tags(raw)
}

fn one_of(raw: Option<&String>, allowed: &[&str], dflt: &str) -> String {
    let v = raw.map(|s| s.trim().to_lowercase()).unwrap_or_default();
    if allowed.contains(&v.as_str()) {
        v
    } else {
        dflt.to_owned()
    }
}

const PRIORITIES: &[&str] = &["high", "normal", "low", "skip", "ignore"];

/// What a `cv import sources` run produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourcesImport {
    /// The `sources.toml` document.
    pub toml: String,
    pub channels: usize,
    pub blogs: usize,
    /// Rows dropped (missing name/id/link).
    pub dropped: usize,
}

fn q(s: &str) -> String {
    toml::Value::String(s.to_owned()).to_string()
}

fn q_list(v: &[String]) -> String {
    let items: Vec<String> = v.iter().map(|s| q(s)).collect();
    format!("[{}]", items.join(", "))
}

/// Converts registry note texts (each a `Tracked Channels.md` and/or `Tracked Feeds.md`)
/// into one `sources.toml` document. Unlike Crucible's parsers, `skip`/`ignore` rows, non-`UC`
/// ids and non-RSS methods are kept (with their values) — the trackers apply that row policy.
pub fn sources_from_markdown(texts: &[String]) -> SourcesImport {
    let mut out = SourcesImport::default();
    let mut doc = String::from(
        "# sources.toml — generated by `cv import sources` from Crucible's registry notes.\n",
    );
    for text in texts {
        for row in parse_table(text, "channel", 2) {
            let name = display_name(row.get("channel").map_or("", String::as_str));
            let id = row.get("id").map_or("", |s| s.trim());
            if name.is_empty() || id.is_empty() {
                out.dropped += 1;
                continue;
            }
            out.channels += 1;
            doc.push_str(&format!(
                "\n[[youtube]]\nchannel = {}\nid = {}\ntags = {}\npriority = {}\n",
                q(&name),
                q(id),
                q_list(&tags(row.get("tags"))),
                q(&one_of(row.get("priority"), PRIORITIES, "normal")),
            ));
        }
    }
    for text in texts {
        for row in parse_table(text, "name", 3) {
            let name = row.get("name").map_or("", |s| s.trim()).to_owned();
            let link = link_url(row.get("link").map_or("", String::as_str));
            if name.is_empty() || link.is_empty() {
                out.dropped += 1;
                continue;
            }
            out.blogs += 1;
            let method = row
                .get("method")
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "rss".into());
            doc.push_str(&format!(
                "\n[[blog]]\nname = {}\nlink = {}\nmethod = {}\ntags = {}\npriority = {}\ncanon = {}\nbody = {}\n",
                q(&name),
                q(&link),
                q(&method),
                q_list(&tags(row.get("tags"))),
                q(&one_of(row.get("priority"), PRIORITIES, "normal")),
                q(&one_of(
                    row.get("canon"),
                    &["auto", "substack", "strip-params", "keep-params"],
                    "auto"
                )),
                q(&one_of(row.get("body"), &["auto", "full", "snippet"], "auto")),
            ));
        }
    }
    out.toml = doc;
    out
}

/// `cv import sources --from …`: read-only on its inputs; prints the document (stdout) and the
/// counts (stderr), so `> sources.toml` captures only TOML.
pub fn sources(froms: &[PathBuf]) -> Result<String> {
    let mut texts = Vec::new();
    for f in froms {
        texts.push(std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?);
    }
    let r = sources_from_markdown(&texts);
    if r.channels + r.blogs == 0 {
        bail!("no registry table (`| Channel | ID |…` or `| Name | Link | Method |…`) found");
    }
    let parsed: toml::Table = r.toml.parse().context("generated TOML does not parse")?;
    cv_actions::SourcesConfig::from_table(&parsed).map_err(|e| anyhow::anyhow!(e))?;
    eprintln!(
        "cv: {} channels, {} blogs, {} rows dropped (missing name/id/link)",
        r.channels, r.blogs, r.dropped
    );
    Ok(r.toml.trim_end().to_owned())
}

// ---------------------------------------------------------------------------------------------
// `cv import ignored`: Crucible's `_crucible/orchestration/ignored.md` → state.db `ignored_ids`.
// ---------------------------------------------------------------------------------------------

/// `(kind, id)` pairs of an `ignored.md` (`ignoredIds.ts` `readIgnored`): `- <id>` bullets under
/// `## Videos` (kind `youtube`; a URL becomes its video id) and `## Blogs` (kind `blog`; a URL
/// becomes its `post_id_from_url` id).
pub fn parse_ignored(text: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let mut section: Option<&'static str> = None;
    for line in text.lines() {
        let t = line.trim();
        match t {
            "## Videos" => section = Some("youtube"),
            "## Blogs" => section = Some("blog"),
            _ if t.starts_with("## ") => section = None,
            _ => {
                let Some(kind) = section else { continue };
                let Some(v) = t.strip_prefix('-') else {
                    continue;
                };
                if !v.starts_with(char::is_whitespace) {
                    continue;
                }
                let v = v.trim();
                if v.is_empty() {
                    continue;
                }
                let is_url = v.len() > 7
                    && (v[..7].eq_ignore_ascii_case("http://")
                        || v.get(..8)
                            .is_some_and(|p| p.eq_ignore_ascii_case("https://")));
                let id = match (kind, is_url) {
                    ("youtube", true) => cv_text::url_canon::extract_video_id_from_url(v),
                    ("youtube", false) => Some(v.to_owned()),
                    _ => Some(cv_text::url_canon::post_id_from_url(
                        v,
                        &cv_text::url_canon::PostIdOptions::default(),
                    )),
                };
                if let Some(id) = id.filter(|i| !i.is_empty()) {
                    out.push((kind, id));
                }
            }
        }
    }
    out
}

/// Counts of an `ignored` import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IgnoredCounts {
    pub videos: u64,
    pub blogs: u64,
    pub inserted: u64,
    pub existing: u64,
}

/// Imports `from`'s ids into `state_db` (no daemon check; see [`ignored`]).
pub fn import_ignored(state_db: &Path, from: &Path) -> Result<IgnoredCounts> {
    let text =
        std::fs::read_to_string(from).with_context(|| format!("reading {}", from.display()))?;
    let ids = parse_ignored(&text);
    let mut db = StateDb::open(state_db, crate::now_ms(), QueueConfig::default())
        .with_context(|| format!("opening {}", state_db.display()))?;
    let mut c = IgnoredCounts::default();
    let now = crate::now_ms();
    for (kind, id) in ids {
        if kind == "youtube" {
            c.videos += 1;
        } else {
            c.blogs += 1;
        }
        if db.insert_ignored_id(kind, &id, "import", now)? {
            c.inserted += 1;
        } else {
            c.existing += 1;
        }
    }
    Ok(c)
}

/// `cv import ignored --from <path>`: refuses while the daemon socket accepts connections.
pub fn ignored(config: &Config, from: &Path) -> Result<String> {
    if std::os::unix::net::UnixStream::connect(&config.socket).is_ok() {
        bail!(
            "something is accepting connections on {}; stop the daemon first — state.db has a single writer",
            config.socket.display()
        );
    }
    let c = import_ignored(&config.state_db, from)?;
    Ok(format!(
        "videos {}, blogs {}, inserted {}, already present {}",
        c.videos, c.blogs, c.inserted, c.existing
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNELS_MD: &str = "# Tracked Channels\n\nintro\n\n| Channel | ID | Tags | Priority |\n|---------|----|------|----------|\n| [Alpha](https://example.com/a) | UCaaaaaaaaaaaaaaaaaaaaaa | ai, research | HIGH |\n| Beta \"B\" | UCbbbbbbbbbbbbbbbbbbbbbb | | skip |\n| Gamma | xx-not-uc | | weird |\n|  | UCmissing | | |\n\nafter\n";
    const BLOGS_MD: &str = "| Name | Link | Method | Tags | Priority | Canon | Body |\n|:-----|------|-------:|------|----------|-------|------|\n| Ex | [feed](<https://example.com/feed.xml>) | RSS | x | normal | keep-params | full |\n| Old | <https://example.org/rss> | Scrape | | ignore | | |\n| NoLink |  | RSS | | | | |\n";
    const FIVE_COL_MD: &str = "| Name | Link | Method | Tags | Priority |\n|---|---|---|---|---|\n| Five | https://example.net/atom | rss | | |\n";

    #[test]
    fn sources_import_parses_crucible_tables() {
        let r =
            sources_from_markdown(&[CHANNELS_MD.to_owned(), format!("{BLOGS_MD}\n{FIVE_COL_MD}")]);
        // An empty first cell ends a table (Crucible `parseTable`), so `UCmissing` is never read.
        assert_eq!((r.channels, r.blogs, r.dropped), (3, 2, 1), "{}", r.toml);
        let t: toml::Table = r.toml.parse().unwrap();
        let c = cv_actions::SourcesConfig::from_table(&t).unwrap();
        assert_eq!(c.youtube[0].channel, "Alpha");
        assert_eq!(c.youtube[0].tags, ["ai", "research"]);
        assert_eq!(c.youtube[0].priority, cv_actions::pipelines::Priority::High);
        assert_eq!(c.youtube[1].channel, "Beta \"B\"");
        assert_eq!(c.youtube[1].priority, cv_actions::pipelines::Priority::Skip);
        assert_eq!(c.youtube[2].id, "xx-not-uc");
        assert_eq!(
            c.youtube[2].priority,
            cv_actions::pipelines::Priority::Normal
        );
        assert_eq!(c.blog[0].link, "https://example.com/feed.xml");
        assert_eq!(
            c.blog[0].canon,
            cv_actions::pipelines::BlogCanon::KeepParams
        );
        assert_eq!(c.blog[0].body, cv_actions::pipelines::BodyMode::Full);
        assert_eq!(c.blog[1].method, "scrape");
        assert_eq!(c.blog[1].link, "https://example.org/rss");
        // The 5-column table in the same note is not reached (first table only, as Crucible).
        assert!(!r.toml.contains("Five"));
        let five = sources_from_markdown(&[FIVE_COL_MD.to_owned()]);
        assert_eq!(five.blogs, 1);
        assert!(five.toml.contains("canon = \"auto\""));
        assert_eq!(sources_from_markdown(&["no table".into()]).channels, 0);
    }

    #[test]
    fn ignored_import_is_idempotent() {
        let md = "# Ignored ingestion IDs\n\n## Videos\n\n- dQw4w9WgXcQ\n- https://www.youtube.com/watch?v=aaaaaaaaaaa\n-nospace\n\n## Blogs\n\n- https://example.com/p/post/?utm_source=x\n- bare-id\n\n## Other\n\n- not-me\n";
        let ids = parse_ignored(md);
        assert_eq!(
            ids.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            ["youtube", "youtube", "blog", "blog"]
        );
        assert_eq!(ids[1].1, "aaaaaaaaaaa");
        assert!(!ids[2].1.contains("utm_source"), "{}", ids[2].1);
        let dir = std::env::temp_dir().join(format!("cv-wp12-ignored-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("ignored.md");
        std::fs::write(&src, md).unwrap();
        let db = dir.join("state.db");
        let a = import_ignored(&db, &src).unwrap();
        assert_eq!((a.videos, a.blogs, a.inserted, a.existing), (2, 2, 4, 0));
        let b = import_ignored(&db, &src).unwrap();
        assert_eq!((b.inserted, b.existing), (0, 4));
        assert_eq!(
            std::fs::read_to_string(&src).unwrap(),
            md,
            "source untouched"
        );
        let s = StateDb::open(&db, crate::now_ms(), QueueConfig::default()).unwrap();
        assert!(s.is_ignored("youtube", "dQw4w9WgXcQ").unwrap());
        assert!(s.is_ignored("blog", "bare-id").unwrap());
        drop(s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_subcommand_args() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            parse_froms(&a(&["--from", "a", "--from", "b"]))
                .unwrap()
                .len(),
            2
        );
        assert!(parse_froms(&a(&["--from"])).is_none());
        assert!(parse_froms(&a(&[])).is_none());
        assert_eq!(run(&a(&["sources"])), 2);
        assert_eq!(run(&a(&["ignored", "--from"])), 2);
        assert_eq!(run(&a(&["sources", "--from", "/nonexistent/x.md"])), 1);
    }

    #[test]
    fn args_and_missing_source() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(parse_from(Some(&a(&["--from", "x"]))).is_some());
        assert!(parse_from(Some(&a(&["--from"]))).is_none());
        assert_eq!(run(&a(&["link-records"])), 2);
        assert_eq!(run(&a(&["nope"])), 2);
        let dir = std::env::temp_dir().join(format!("cv-wp3-import-{}", std::process::id()));
        let err = import_link_records(&dir.join("state.db"), &dir.join("missing.sqlite"));
        assert!(err.is_err());
        assert!(
            !dir.join("state.db").exists(),
            "no state.db for a bad source"
        );
    }
}
