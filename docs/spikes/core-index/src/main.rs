//! Throwaway spike: vault index + daemon + URI intake. See FINDINGS.md.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use mdview::frontmatter::{JsValue, parse_frontmatter, split_frontmatter};
use mdview::glyphs::Tier;
use mdview::lex::lex;
use mdview::token::Token;
use regex::Regex;
use rusqlite::{Connection, params};

type R<T> = Result<T, Box<dyn std::error::Error>>;

const SCHEMA: &str = "
PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
CREATE TABLE IF NOT EXISTS files(id INTEGER PRIMARY KEY, path TEXT UNIQUE NOT NULL, folder TEXT, name TEXT, ext TEXT, is_md INT, size INT, mtime INT, has_fm INT);
CREATE TABLE IF NOT EXISTS props(file_id INT, key TEXT, value TEXT);
CREATE INDEX IF NOT EXISTS props_k ON props(key, file_id);
CREATE TABLE IF NOT EXISTS links(src_id INT, kind TEXT, raw TEXT, target TEXT, fragment TEXT, resolved_id INT);
CREATE INDEX IF NOT EXISTS links_src ON links(src_id); CREATE INDEX IF NOT EXISTS links_dst ON links(resolved_id);
CREATE TABLE IF NOT EXISTS tags(file_id INT, tag TEXT, source TEXT);
CREATE INDEX IF NOT EXISTS tags_t ON tags(tag); CREATE INDEX IF NOT EXISTS tags_f ON tags(file_id); CREATE INDEX IF NOT EXISTS props_f ON props(file_id);
CREATE TABLE IF NOT EXISTS blocks(file_id INT, kind TEXT, label TEXT); CREATE INDEX IF NOT EXISTS blocks_f ON blocks(file_id);
CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(body, content='', contentless_delete=1);
";

fn now_ns() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

/// userIgnoreFilters (`_resources/`): not indexed, but still a link-resolution target (Obsidian behaviour).
fn target_only(rel: &str) -> bool {
    rel.starts_with("_resources/")
}

fn ignored(rel: &str) -> bool {
    rel.starts_with(".obsidian/") || rel.starts_with(".git/") || rel.starts_with(".trash/")
}

// ---------- parsing ----------
#[derive(Default)]
struct Parsed {
    has_fm: bool,
    props: Vec<(String, String)>,
    links: Vec<(String, String, String, Option<String>)>, // kind, raw, target, fragment
    tags: Vec<(String, String)>,
    blocks: Vec<(String, String)>,
    body: String,
}

static RE_CODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?ms)^(```|~~~).*?^(```|~~~)[^\n]*$|`[^`\n]*`").unwrap_or_else(|_| unreachable!()));
static RE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|\s)#([\p{L}\p{N}_/\-]+)").unwrap_or_else(|_| unreachable!()));
static RE_FN_DEF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\[\^([^\]\s]+)\]:").unwrap_or_else(|_| unreachable!()));
static RE_FN_REF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\^([^\]\s]+)\][^:]").unwrap_or_else(|_| unreachable!()));
static RE_CALLOUT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*>\s*\[!([A-Za-z0-9_-]+)\]").unwrap_or_else(|_| unreachable!()));

fn walk_tokens(ts: &[Token], out: &mut Parsed) {
    for t in ts {
        match t {
            Token::Wikilink { slug, fragment, embed, alias, tokens } => {
                let kind = if *embed { "embed" } else { "wiki" };
                let mut raw = slug.clone();
                if let Some(f) = fragment { raw.push('#'); raw.push_str(f); }
                if let Some(a) = alias { raw.push('|'); raw.push_str(a); }
                out.links.push((kind.into(), raw, slug.clone(), fragment.clone()));
                walk_tokens(tokens, out);
            }
            Token::Link { href, tokens, .. } => {
                push_md(out, "md", href);
                walk_tokens(tokens, out);
            }
            Token::Image { href, .. } => push_md(out, "mdimg", href),
            Token::Heading { tokens, .. } | Token::Blockquote { tokens } | Token::Paragraph { tokens }
            | Token::Strong { tokens, .. } | Token::Em { tokens, .. } | Token::Del { tokens, .. } => walk_tokens(tokens, out),
            Token::Text { tokens: Some(tokens), .. } => walk_tokens(tokens, out),
            Token::List { items, .. } => items.iter().for_each(|i| walk_tokens(&i.tokens, out)),
            Token::Table { header, rows, .. } => {
                header.iter().chain(rows.iter().flatten()).for_each(|c| walk_tokens(&c.tokens, out));
            }
            _ => {}
        }
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut v = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 1 && s.is_char_boundary(i + 3) {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) { v.push(x); i += 3; continue; }
        }
        v.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&v).into_owned()
}

fn push_md(out: &mut Parsed, kind: &str, href: &str) {
    if href.contains("://") || href.starts_with("mailto:") { return; }
    let (path, frag) = match href.split_once('#') { Some((p, f)) => (p, Some(f.to_string())), None => (href, None) };
    out.links.push((kind.into(), href.into(), pct_decode(path), frag));
}

fn js_to_json(v: &JsValue) -> serde_json::Value {
    match v {
        JsValue::Null => serde_json::Value::Null,
        JsValue::Bool(b) => (*b).into(),
        JsValue::Num(n) => serde_json::Number::from_f64(*n).map(serde_json::Value::Number).unwrap_or(serde_json::Value::Null),
        JsValue::Str(s) => s.clone().into(),
        JsValue::Arr(a) => a.iter().map(js_to_json).collect(),
        JsValue::Obj(o) => o.entries().into_iter().map(|(k, v)| (k.to_string(), js_to_json(v))).collect::<serde_json::Map<_, _>>().into(),
    }
}

fn parse(src: &str) -> Parsed {
    let mut p = Parsed::default();
    let (fm, body) = split_frontmatter(src);
    if let Some(fm) = fm {
        p.has_fm = true;
        if let Ok(JsValue::Obj(o)) = parse_frontmatter(&fm) {
            for (k, v) in o.entries() {
                if k == "tags" {
                    let list: Vec<String> = match v {
                        JsValue::Str(s) => s.split([',', ' ']).filter(|x| !x.is_empty()).map(String::from).collect(),
                        JsValue::Arr(a) => a.iter().filter_map(|x| if let JsValue::Str(s) = x { Some(s.clone()) } else { None }).collect(),
                        _ => vec![],
                    };
                    for t in list { p.tags.push((t.trim_start_matches('#').to_string(), "fm".into())); }
                }
                // frontmatter wikilinks (Obsidian indexes "[[x]]" property values as links)
                for s in fm_strings(v) {
                    if let Some(inner) = s.strip_prefix("[[").and_then(|x| x.strip_suffix("]]")) {
                        let (t, _) = inner.split_once('|').unwrap_or((inner, ""));
                        let (t, f) = match t.split_once('#') { Some((a, b)) => (a, Some(b.to_string())), None => (t, None) };
                        p.links.push(("fm".into(), s.clone(), t.into(), f));
                    }
                }
                p.props.push((k.to_string(), js_to_json(v).to_string()));
            }
        }
    }
    walk_tokens(&lex(body, Tier::Unicode), &mut p);
    let stripped = RE_CODE.replace_all(body, " ");
    for c in RE_TAG.captures_iter(&stripped) {
        let t = &c[1];
        if t.chars().all(|ch| ch.is_ascii_digit()) { continue; }
        p.tags.push((t.to_string(), "inline".into()));
    }
    for c in RE_FN_DEF.captures_iter(&stripped) { p.blocks.push(("fndef".into(), c[1].into())); }
    for c in RE_FN_REF.captures_iter(&stripped) { p.blocks.push(("fnref".into(), c[1].into())); }
    for c in RE_CALLOUT.captures_iter(&stripped) { p.blocks.push(("callout".into(), c[1].to_lowercase())); }
    p.body = body.to_string();
    p
}

fn fm_strings(v: &JsValue) -> Vec<String> {
    match v { JsValue::Str(s) => vec![s.clone()], JsValue::Arr(a) => a.iter().flat_map(fm_strings).collect(), _ => vec![] }
}

// ---------- resolution ----------
#[derive(Default)]
struct Resolver {
    by_path: HashMap<String, i64>,           // lowercased rel path -> id
    by_base: HashMap<String, Vec<(String, i64)>>, // lowercased basename (md: sans .md) -> (path, id)
}

impl Resolver {
    fn load(c: &Connection) -> R<Self> {
        let mut r = Resolver::default();
        let mut s = c.prepare("SELECT id, path, is_md FROM files")?;
        let rows = s.query_map([], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, String>(1)?, x.get::<_, i64>(2)?)))?;
        for row in rows { let (id, p, md) = row?; r.add(id, &p, md != 0); }
        Ok(r)
    }
    fn add(&mut self, id: i64, path: &str, md: bool) {
        let lp = path.to_lowercase();
        self.by_path.insert(lp.clone(), id);
        let base = lp.rsplit('/').next().unwrap_or(&lp).to_string();
        let key = if md { base.strip_suffix(".md").unwrap_or(&base).to_string() } else { base };
        self.by_base.entry(key).or_default().push((path.to_string(), id));
    }
    fn resolve(&self, src_path: &str, src_id: i64, target: &str) -> Option<i64> {
        let t = target.trim().trim_start_matches('/').to_lowercase();
        if t.is_empty() { return Some(src_id); }
        let dir = src_path.rsplit_once('/').map(|(d, _)| d.to_lowercase());
        let mut cands = vec![t.clone(), format!("{t}.md")];
        if let Some(d) = &dir { cands.push(normalize(&format!("{d}/{t}"))); cands.push(normalize(&format!("{d}/{t}.md"))); }
        for c in &cands { if let Some(id) = self.by_path.get(c) { return Some(*id); } }
        let base = t.rsplit('/').next().unwrap_or(&t);
        let base = base.strip_suffix(".md").unwrap_or(base);
        let hits = self.by_base.get(base)?;
        // path-suffix filter when the link has folders
        let hits: Vec<_> = if t.contains('/') { hits.iter().filter(|(p, _)| { let lp = p.to_lowercase(); lp.ends_with(&t) || lp.ends_with(&format!("{t}.md")) }).collect() } else { hits.iter().collect() };
        hits.iter().min_by_key(|(p, _)| (p.len(), p.clone())).map(|(_, id)| *id)
    }
}

fn normalize(p: &str) -> String {
    let mut out: Vec<&str> = vec![];
    for seg in p.split('/') {
        match seg { "" | "." => {}, ".." => { out.pop(); }, s => out.push(s) }
    }
    out.join("/")
}

// ---------- writing ----------
fn file_meta(root: &Path, rel: &str) -> (String, String, String, i64, i64) {
    let full = root.join(rel);
    let md = std::fs::metadata(&full).ok();
    let size = md.as_ref().map(|m| m.len() as i64).unwrap_or(0);
    let mtime = md.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
    let (folder, file) = rel.rsplit_once('/').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((String::new(), rel.to_string()));
    let (name, ext) = file.rsplit_once('.').map(|(a, b)| (a.to_string(), b.to_lowercase())).unwrap_or((file.clone(), String::new()));
    (folder, name, ext, size, mtime)
}

fn upsert_file(tx: &Connection, root: &Path, rel: &str) -> R<(i64, bool)> {
    let (folder, name, ext, size, mtime) = file_meta(root, rel);
    let is_md = ext == "md" && !target_only(rel);
    tx.execute("INSERT INTO files(path,folder,name,ext,is_md,size,mtime,has_fm) VALUES(?1,?2,?3,?4,?5,?6,?7,0) ON CONFLICT(path) DO UPDATE SET size=?6, mtime=?7",
        params![rel, folder, name, ext, is_md as i64, size, mtime])?;
    let id: i64 = tx.query_row("SELECT id FROM files WHERE path=?1", [rel], |r| r.get(0))?;
    Ok((id, is_md))
}

fn write_parsed(tx: &Connection, id: i64, rel: &str, p: &Parsed, res: &Resolver) -> R<()> {
    for t in ["props", "tags", "blocks"] { tx.execute(&format!("DELETE FROM {t} WHERE file_id=?1"), [id])?; }
    tx.execute("DELETE FROM links WHERE src_id=?1", [id])?;
    tx.execute("DELETE FROM fts WHERE rowid=?1", [id])?;
    tx.execute("UPDATE files SET has_fm=?2 WHERE id=?1", params![id, p.has_fm as i64])?;
    let mut s = tx.prepare_cached("INSERT INTO props VALUES(?1,?2,?3)")?;
    for (k, v) in &p.props { s.execute(params![id, k, v])?; }
    let mut s = tx.prepare_cached("INSERT INTO links VALUES(?1,?2,?3,?4,?5,?6)")?;
    for (k, raw, t, f) in &p.links { s.execute(params![id, k, raw, t, f, res.resolve(rel, id, t)])?; }
    let mut s = tx.prepare_cached("INSERT INTO tags VALUES(?1,?2,?3)")?;
    for (t, src) in &p.tags { s.execute(params![id, t, src])?; }
    let mut s = tx.prepare_cached("INSERT INTO blocks VALUES(?1,?2,?3)")?;
    for (k, l) in &p.blocks { s.execute(params![id, k, l])?; }
    tx.execute("INSERT INTO fts(rowid, body) VALUES(?1, ?2)", params![id, p.body])?;
    Ok(())
}

fn full_index(root: &Path, db: &Path) -> R<()> {
    let t0 = Instant::now();
    let _ = std::fs::remove_file(db);
    let mut c = Connection::open(db)?;
    c.execute_batch(SCHEMA)?;
    let mut rels = vec![];
    for e in walkdir::WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if !e.file_type().is_file() { continue; }
        let rel = e.path().strip_prefix(root)?.to_string_lossy().replace('\\', "/");
        if ignored(&rel) { continue; }
        rels.push(rel);
    }
    let t_walk = t0.elapsed();
    let tx = c.transaction()?;
    let mut mds = vec![];
    for rel in &rels { let (id, md) = upsert_file(&tx, root, rel)?; if md { mds.push((id, rel.clone())); } }
    let res = Resolver::load(&tx)?;
    let mut t_parse = std::time::Duration::ZERO;
    for (id, rel) in &mds {
        let src = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
        let tp = Instant::now();
        let p = parse(&src);
        t_parse += tp.elapsed();
        write_parsed(&tx, *id, rel, &p, &res)?;
    }
    tx.commit()?;
    eprintln!("index: files={} md={} walk={:?} parse={:?} total={:?}", rels.len(), mds.len(), t_walk, t_parse, t0.elapsed());
    Ok(())
}

fn reindex_one(c: &mut Connection, root: &Path, rel: &str) -> R<()> {
    let full = root.join(rel);
    let tx = c.transaction()?;
    if !full.exists() {
        if let Ok(id) = tx.query_row("SELECT id FROM files WHERE path=?1", [rel], |r| r.get::<_, i64>(0)) {
            for t in ["props", "tags", "blocks"] { tx.execute(&format!("DELETE FROM {t} WHERE file_id=?1"), [id])?; }
            tx.execute("DELETE FROM links WHERE src_id=?1", [id])?;
            tx.execute("UPDATE links SET resolved_id=NULL WHERE resolved_id=?1", [id])?;
            tx.execute("DELETE FROM fts WHERE rowid=?1", [id])?;
            tx.execute("DELETE FROM files WHERE id=?1", [id])?;
        }
        tx.commit()?;
        return Ok(());
    }
    let existed: bool = tx.query_row("SELECT count(*) FROM files WHERE path=?1", [rel], |r| r.get::<_, i64>(0))? > 0;
    let (id, md) = upsert_file(&tx, root, rel)?;
    let res = Resolver::load(&tx)?;
    if md {
        let src = std::fs::read_to_string(&full).unwrap_or_default();
        write_parsed(&tx, id, rel, &parse(&src), &res)?;
    }
    if !existed {
        // a new file may satisfy previously unresolved links
        let mut s = tx.prepare("SELECT l.rowid, f.path, l.src_id, l.target FROM links l JOIN files f ON f.id=l.src_id WHERE l.resolved_id IS NULL")?;
        let rows: Vec<(i64, String, i64, String)> = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.filter_map(Result::ok).collect();
        drop(s);
        for (rid, sp, sid, t) in rows { if let Some(x) = res.resolve(&sp, sid, &t) { tx.execute("UPDATE links SET resolved_id=?1 WHERE rowid=?2", params![x, rid])?; } }
    }
    tx.commit()?;
    Ok(())
}

fn daemon(root: &Path, db: &Path) -> R<()> {
    use notify::{EventKind, RecursiveMode, Watcher};
    let mut c = Connection::open(db)?;
    c.execute_batch(SCHEMA)?;
    let root = root.canonicalize()?;
    let (txc, rx) = std::sync::mpsc::channel();
    let mut w = notify::recommended_watcher(txc)?;
    w.watch(&root, RecursiveMode::Recursive)?;
    eprintln!("daemon: watching pid={}", std::process::id());
    let mut last: HashMap<String, (i64, i64)> = HashMap::new();
    for ev in rx {
        let ev = match ev { Ok(e) => e, Err(e) => { eprintln!("watch error {e}"); continue; } };
        let t_ev = now_ns();
        let ti = Instant::now();
        if !matches!(ev.kind, EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)) { continue; }
        for p in &ev.paths {
            let Ok(rel) = p.strip_prefix(&root) else { continue };
            let rel = rel.to_string_lossy().to_string();
            if ignored(&rel) || p.is_dir() { continue; }
            let (_, _, _, size, mtime) = file_meta(&root, &rel);
            if p.exists() && last.get(&rel) == Some(&(size, mtime)) { continue; } // dup event, unchanged
            last.insert(rel.clone(), (size, mtime));
            if let Err(e) = reindex_one(&mut c, &root, &rel) { eprintln!("reindex error {e}"); continue; }
            // machine-readable: committed epoch ns, event-received epoch ns, handler us, path hash only
            println!("COMMIT {} {} {} {:?}", now_ns(), t_ev, ti.elapsed().as_micros(), ev.kind);
        }
    }
    Ok(())
}

// ---------- URI ----------
fn uri(url: &str, vaults_root: &Path) -> R<()> {
    let rest = url.strip_prefix("obsidian://").ok_or("not an obsidian:// url")?;
    let (action, query) = rest.split_once('?').unwrap_or((rest, ""));
    let mut q: HashMap<String, String> = HashMap::new();
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        q.insert(pct_decode(k), pct_decode(v));
    }
    if action.trim_end_matches('/') != "new" { return Err(format!("unsupported action {action}").into()); }
    let vault = q.get("vault").ok_or("missing vault")?;
    if vault.contains('/') || vault.starts_with('.') { return Err("bad vault name".into()); }
    let root = vaults_root.join(vault);
    if !root.is_dir() { return Err(format!("unknown vault {vault}").into()); }
    let file = q.get("file").or(q.get("name")).cloned().unwrap_or_else(|| "Untitled".into());
    let mut rel = normalize(&file.replace('\\', "/"));
    if rel.is_empty() { return Err("empty file".into()); }
    if !rel.to_lowercase().ends_with(".md") { rel.push_str(".md"); }
    let content = if q.contains_key("clipboard") {
        std::env::var("CVX_CLIPBOARD").map_err(|_| "clipboard requested; spike reads CVX_CLIPBOARD stand-in")?
    } else { q.get("content").cloned().unwrap_or_default() };
    let stem = rel.strip_suffix(".md").unwrap_or(&rel).to_string();
    let mut target = root.join(&rel);
    let mut n = 1;
    while target.exists() { rel = format!("{stem} {n}.md"); target = root.join(&rel); n += 1; }
    if let Some(parent) = target.parent() { std::fs::create_dir_all(parent)?; }
    let tmp = target.with_extension("md.tmp");
    std::fs::write(&tmp, content.as_bytes())?;
    std::fs::rename(&tmp, &target)?;
    println!("created {rel} bytes={} silent={}", content.len(), q.contains_key("silent"));
    Ok(())
}

fn main() -> R<()> {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| a.get(i).cloned().ok_or_else(|| format!("missing arg {i}"));
    match a.get(1).map(String::as_str) {
        Some("index") => full_index(&PathBuf::from(arg(2)?), &PathBuf::from(arg(3)?)),
        Some("daemon") => daemon(&PathBuf::from(arg(2)?), &PathBuf::from(arg(3)?)),
        Some("uri") => uri(&arg(2)?, &PathBuf::from(std::env::var("CVX_VAULTS").map_err(|_| "set CVX_VAULTS")?)),
        Some("parse") => { let s = std::fs::read_to_string(arg(2)?)?; let p = parse(&s); println!("links={:?}\ntags={:?}\nblocks={:?}\nprops={:?}", p.links, p.tags, p.blocks, p.props); Ok(()) }
        _ => Err("usage: cvx index|daemon <vault> <db> | uri <url> | parse <file>".into()),
    }
}
