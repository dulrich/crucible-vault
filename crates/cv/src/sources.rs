//! `cv sources migrate [--dry-run]` (plan `sources-publication-model` WP-1): rewrites
//! `sources.toml`'s legacy `[[blog]]`/`[[newsletter]]`/`[[serial]]`/`[[youtube]]` rows as the
//! folded `[[publication]]` records the loader already works with. Plain file I/O (no daemon
//! RPC), so it runs with the daemon stopped; prints aggregate counts only.

use std::path::Path;

use anyhow::{Context, Result, bail};
use cv_actions::pipelines::{RowRef, SourcesConfig};
use cv_schema::toml_edit::{DocumentMut, Item};

use crate::config::Config;

/// Usage line (also appended to the top-level `cv --help`).
pub const USAGE: &str = "  cv sources migrate [--dry-run]   rewrite sources.toml legacy rows as [[publication]] records (no daemon needed); prints counts as key=value\n";

/// The leading comment of a migrated file (replaces any generated-by header).
pub const HEADER: &str =
    "# sources.toml: one [[publication]] per publication (written by `cv sources migrate`).\n";

/// `cv sources …`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    let dry_run = match args {
        [m] if m == "migrate" => false,
        [m, d] if m == "migrate" && d == "--dry-run" => true,
        _ => {
            eprint!("usage:\n{USAGE}");
            return 2;
        }
    };
    let r =
        Config::from_env().and_then(|c| migrate_file(&c.config_dir.join("sources.toml"), dry_run));
    match r {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

/// Aggregate counts of one migration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// Records written.
    pub records: usize,
    /// Rows folded into a record another row already started.
    pub merged: usize,
    /// Legacy rows per table.
    pub blog: usize,
    pub newsletter: usize,
    pub serial: usize,
    pub youtube: usize,
}

impl Counts {
    fn render(&self, dry_run: bool) -> String {
        format!(
            "records={}\nmerged={}\nblog={}\nnewsletter={}\nserial={}\nyoutube={}\ndry_run={dry_run}\n",
            self.records, self.merged, self.blog, self.newsletter, self.serial, self.youtube
        )
    }
}

/// The legacy key whose comments carry over to `new` (`link` → `feeds`, `id` → `channel_id`,
/// `channel` → `name`).
fn new_key(table: &str, old: &str) -> String {
    match (table, old) {
        ("blog", "link") => "feeds",
        ("youtube", "id") => "channel_id",
        ("youtube", "channel") => "name",
        (_, k) => k,
    }
    .to_owned()
}

/// The one-line stderr warning when migrating would rename legacy newsletter names (their mail
/// tracker rows are re-keyed only by `cv intake reconcile`, which needs the legacy rows loaded).
fn rename_warning(cfg: &SourcesConfig) -> Option<String> {
    let n = cfg.mail_key_renames().len();
    (n > 0).then(|| {
        format!(
            "cv: warning: {n} legacy newsletter name(s) will be renamed; run `cv intake reconcile` first so mail rows are re-keyed"
        )
    })
}

/// Migrates `text`: `Ok(None)` when it has no legacy rows, else the new file text and counts.
pub fn migrate_text(text: &str) -> Result<Option<(String, Counts)>> {
    let table: toml::Table = text.parse().context("parsing sources.toml")?;
    let cfg = SourcesConfig::from_table(&table)
        .map_err(|e| anyhow::anyhow!("sources.toml does not load: {e}"))?;
    if !cfg.has_legacy_rows() {
        return Ok(None);
    }
    if let Some(w) = rename_warning(&cfg) {
        eprintln!("{w}");
    }
    let recs = cfg.publications();
    for (i, r) in recs.iter().enumerate() {
        if recs[..i].iter().any(|o| o.name.trim() == r.name.trim()) {
            bail!(
                "two publications are both named {:?}; rename one (or set `short`) before migrating",
                r.name
            );
        }
        if !r.has_matcher() {
            bail!(
                "publication {:?} has no feeds, from, list_id, hosts, prefix, channel_id or author; add one or remove the row before migrating",
                r.name
            );
        }
    }
    let mut out = String::from(HEADER);
    for r in recs {
        out.push_str("\n[[publication]]\n");
        out.push_str(&toml::to_string(r).context("serializing a publication")?);
    }
    let out = carry_comments(text, &out, cfg.origins())?;
    // The rewrite must load to the same records.
    let back: toml::Table = out.parse().context("re-parsing the migrated file")?;
    let back = SourcesConfig::from_table(&back)
        .map_err(|e| anyhow::anyhow!("migrated file does not load: {e}"))?;
    if back.publications() != recs {
        bail!("migrated file loads to different records; refusing to write");
    }
    let origins = cfg.origins();
    let legacy = |t: &str| origins.iter().flatten().filter(|o| o.table == t).count();
    let counts = Counts {
        records: recs.len(),
        merged: origins.iter().map(|o| o.len().saturating_sub(1)).sum(),
        blog: legacy("blog"),
        newsletter: legacy("newsletter"),
        serial: legacy("serial"),
        youtube: legacy("youtube"),
    };
    Ok(Some((out, counts)))
}

/// Copies comments on the source rows' keys onto the same keys of the generated records (first
/// row's comment wins per key).
fn carry_comments(src: &str, generated: &str, origins: &[Vec<RowRef>]) -> Result<String> {
    let old: DocumentMut = src.parse().context("parsing sources.toml")?;
    let mut new: DocumentMut = generated.parse().context("parsing the migrated file")?;
    let Some(arr) = new
        .get_mut("publication")
        .and_then(Item::as_array_of_tables_mut)
    else {
        return Ok(generated.to_owned());
    };
    for (rec, rows) in arr.iter_mut().zip(origins) {
        for row in rows {
            let Some(t) = old
                .get(row.table)
                .and_then(Item::as_array_of_tables)
                .and_then(|a| a.get(row.index))
            else {
                continue;
            };
            for (k, item) in t.iter() {
                let nk = new_key(row.table, k);
                let prefix = t
                    .key(k)
                    .and_then(|key| key.leaf_decor().prefix())
                    .and_then(|p| p.as_str())
                    .filter(|p| p.contains('#'))
                    .map(str::to_owned);
                let suffix = item
                    .as_value()
                    .and_then(|v| v.decor().suffix())
                    .and_then(|s| s.as_str())
                    .filter(|s| s.contains('#'))
                    .map(str::to_owned);
                if let Some(p) = prefix
                    && let Some(mut key) = rec.key_mut(&nk)
                    && key
                        .leaf_decor()
                        .prefix()
                        .and_then(|p| p.as_str())
                        .is_none_or(|p| !p.contains('#'))
                {
                    key.leaf_decor_mut().set_prefix(p);
                }
                if let Some(s) = suffix
                    && let Some(v) = rec.get_mut(&nk).and_then(Item::as_value_mut)
                    && v.decor()
                        .suffix()
                        .and_then(|s| s.as_str())
                        .is_none_or(|s| !s.contains('#'))
                {
                    v.decor_mut().set_suffix(s);
                }
            }
        }
    }
    Ok(new.to_string())
}

/// Migrates the file at `path` (atomically: temp file + rename); `dry_run` writes nothing.
/// Returns the stdout text.
pub fn migrate_file(path: &Path, dry_run: bool) -> Result<String> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let Some((out, counts)) = migrate_text(&text)? else {
        return Ok("nothing to migrate\n".into());
    };
    if !dry_run {
        let tmp = path.with_extension("toml.migrate-tmp");
        std::fs::write(&tmp, &out).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::File::open(&tmp)
            .and_then(|f| f.sync_all())
            .with_context(|| format!("syncing {}", tmp.display()))?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    }
    Ok(counts.render(dry_run))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = "# generated by an old tool\n[[blog]]\nname = \"B\"\n# the feed\nlink = \"https://b.example/feed\"\ntags = [\"a\"]\n[[newsletter]]\nname = \"B\"\nfrom = [\"@b.example\"] # sender\n[[serial]]\nname = \"S\"\nauthor = \"Ann\"\nkind = \"fanfic\"\n[[youtube]]\nchannel = \"Y\"\nid = \"UCy\"\n";

    fn load(s: &str) -> SourcesConfig {
        SourcesConfig::from_table(&s.parse().unwrap()).unwrap()
    }

    #[test]
    fn migrate_output_reloads_equal_and_keeps_comments() {
        let (out, counts) = migrate_text(LEGACY).unwrap().unwrap();
        assert!(out.starts_with(HEADER));
        assert!(!out.contains("generated by an old tool"));
        assert!(out.contains("# the feed\nfeeds = ") && out.contains("# sender"));
        assert_eq!(load(&out).publications(), load(LEGACY).publications());
        assert_eq!(
            counts,
            Counts {
                records: 3,
                merged: 1,
                blog: 1,
                newsletter: 1,
                serial: 1,
                youtube: 1
            }
        );
        assert!(migrate_text(&out).unwrap().is_none(), "second run no-op");
    }

    #[test]
    fn migrate_file_dry_run_writes_nothing_and_second_run_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sources.toml");
        std::fs::write(&p, LEGACY).unwrap();
        let dry = migrate_file(&p, true).unwrap();
        assert!(dry.contains("records=3") && dry.contains("dry_run=true"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), LEGACY);
        migrate_file(&p, false).unwrap();
        let once = std::fs::read_to_string(&p).unwrap();
        assert_ne!(once, LEGACY);
        assert_eq!(migrate_file(&p, false).unwrap(), "nothing to migrate\n");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), once);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn rename_warning_only_while_legacy_newsletter_names_differ() {
        let renamed = "[[blog]]\nname = \"Blog\"\nshort = \"Pub\"\nlink = \"https://b.example/feed\"\n[[newsletter]]\nname = \"Letter\"\nshort = \"Pub\"\nfrom = [\"@b.example\"]\n";
        let w = rename_warning(&load(renamed)).expect("a renamed newsletter warns");
        assert!(w.contains("1 legacy newsletter") && w.contains("cv intake reconcile"));
        assert_eq!(
            rename_warning(&load(LEGACY)),
            None,
            "same-name newsletter: no rename"
        );
        let (out, _) = migrate_text(renamed).unwrap().unwrap();
        assert_eq!(
            rename_warning(&load(&out)),
            None,
            "migrated file: nothing to rename"
        );
    }

    #[test]
    fn migrate_refuses_matcherless_and_colliding_names() {
        assert!(migrate_text("[[serial]]\nname = \"S\"\nkind = \"fanfic\"\n").is_err());
        let e = migrate_text(
            "[[blog]]\nname = \"X\"\nshort = \"Xb\"\nlink = \"https://x/f\"\n[[youtube]]\nchannel = \"X\"\nid = \"UC1\"\n",
        )
        .unwrap_err();
        assert!(e.to_string().contains("both named"));
    }
}
