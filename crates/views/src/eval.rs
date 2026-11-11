//! The expression evaluator. Total over values: a type mismatch yields null/false, never an
//! error; only index I/O errors propagate. Runtime problems a definition author must fix
//! (unknown or cyclic formula) are collected as notices.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};

use crate::db::{Db, FileInfo, TaskInfo};
use crate::expr::{BinOp, Expr, Func, Meth};
use crate::value::{Value, loose_eq, parse_date, partial_cmp};

type R<T> = rusqlite::Result<T>;

/// The `file.*` property ids offered as view columns, in display order — each resolves in the
/// `Value::File` arm of `Ev::member` below (the editor's property-id picker lists these; a test
/// pins that every one resolves).
pub const FILE_PROPERTY_IDS: [&str; 10] = [
    "file.name",
    "file.basename",
    "file.path",
    "file.folder",
    "file.ext",
    "file.size",
    "file.mtime",
    "file.ctime",
    "file.tags",
    "file.links",
];

/// Per-run environment shared by every row.
pub struct Env<'a> {
    pub db: &'a Db<'a>,
    /// Formula name → parsed expression or its parse error.
    pub formulas: &'a BTreeMap<String, Result<Expr, String>>,
    pub this: Option<Arc<FileInfo>>,
    pub today: NaiveDate,
    pub now: NaiveDateTime,
    pub notices: RefCell<Vec<String>>,
    /// Note path → the name of the first `[[serial]]` sources row (config order) the note matches,
    /// supplied by the daemon (plan round-11-feedback-todos Decision 5; the matcher is
    /// `cv_actions::pipelines::SerialSource::matches`, never restated here). Exposed as
    /// `file.serial` (the row name, or null); `None` = no data (every `file.serial` is null).
    pub serial: Option<&'a BTreeMap<String, String>>,
    /// `folder("<key>")` values (vault-structure folders from the daemon's typed config, plan
    /// public-release WP-2); an unknown key or `None` is null (an unknown literal key is a load notice,
    /// review S6).
    pub folders: Option<&'a crate::Folders>,
}

impl Env<'_> {
    pub fn notice(&self, n: String) {
        let mut v = self.notices.borrow_mut();
        if !v.contains(&n) {
            v.push(n);
        }
    }
}

/// Lambda scope: `value` is the innermost element, `outer` the enclosing lambda's element.
struct Scope<'s> {
    value: Value,
    parent: Option<&'s Scope<'s>>,
}

/// Evaluation for one row (`file`/`note`/`formula` bound to `file`, or unbound when `None`).
pub struct RowCtx<'a> {
    env: &'a Env<'a>,
    file: Option<Arc<FileInfo>>,
    /// A non-file row (`tracker_items`, plan round-5 WP-10): its fields are bare identifiers and
    /// `note.<field>`; `file` is null.
    rec: Option<Arc<BTreeMap<String, Value>>>,
    memo: RefCell<HashMap<String, Value>>,
    busy: RefCell<HashSet<String>>,
}

/// Milliseconds since the epoch as a local date-time.
pub fn ms_date(ms: i64) -> Value {
    DateTime::from_timestamp_millis(ms).map_or(Value::Null, |d| {
        Value::Date(d.with_timezone(&Local).naive_local(), false)
    })
}

/// A task as `{status, text, heading, line}`.
pub fn task_value(t: &TaskInfo) -> Value {
    let mut m = BTreeMap::new();
    m.insert("status".to_string(), Value::Str(t.status.clone()));
    m.insert("text".to_string(), Value::Str(t.text.clone()));
    m.insert(
        "heading".to_string(),
        t.heading.clone().map_or(Value::Null, Value::Str),
    );
    #[allow(clippy::cast_precision_loss)]
    m.insert("line".to_string(), Value::Num(t.line as f64));
    Value::Object(Arc::new(m))
}

fn to_list(v: &Value) -> Arc<Vec<Value>> {
    match v {
        Value::List(l) => Arc::clone(l),
        Value::Null => Arc::new(Vec::new()),
        other => Arc::new(vec![other.clone()]),
    }
}

fn tag_matches(tag: &str, want: &str) -> bool {
    let tag = tag.trim_start_matches('#').to_lowercase();
    let want = want.trim_start_matches('#').to_lowercase();
    !want.is_empty() && (tag == want || tag.strip_prefix(&want).is_some_and(|r| r.starts_with('/')))
}

fn in_folder(f: &FileInfo, folder: &str) -> bool {
    let folder = folder.trim_matches('/');
    folder.is_empty()
        || f.folder == folder
        || f.folder
            .strip_prefix(folder)
            .is_some_and(|r| r.starts_with('/'))
}

/// Moment-style `YYYY MM DD HH mm ss` tokens to chrono.
fn date_format(fmt: &str) -> String {
    let mut out = String::new();
    let mut rest = fmt;
    while !rest.is_empty() {
        let tok = [
            ("YYYY", "%Y"),
            ("MM", "%m"),
            ("DD", "%d"),
            ("HH", "%H"),
            ("mm", "%M"),
            ("ss", "%S"),
        ]
        .into_iter()
        .find(|(t, _)| rest.starts_with(t));
        if let Some((t, c)) = tok {
            out.push_str(c);
            rest = &rest[t.len()..];
        } else if let Some(ch) = rest.chars().next() {
            if ch == '%' {
                out.push_str("%%");
            } else {
                out.push(ch);
            }
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

fn arith(op: BinOp, a: &Value, b: &Value) -> Value {
    if op == BinOp::Add {
        let numeric = |v: &Value| matches!(v, Value::Num(_) | Value::Bool(_) | Value::Null);
        match (a, b) {
            (Value::List(x), Value::List(y)) => {
                return Value::list(x.iter().chain(y.iter()).cloned().collect());
            }
            _ if numeric(a) && numeric(b) => {}
            _ => return Value::Str(a.to_text() + &b.to_text()),
        }
    }
    let (x, y) = (a.to_num(), b.to_num());
    Value::Num(match op {
        BinOp::Add => x + y,
        BinOp::Sub => x - y,
        BinOp::Mul => x * y,
        BinOp::Div => x / y,
        _ => x % y,
    })
}

impl<'a> RowCtx<'a> {
    pub fn new(env: &'a Env<'a>, file: Option<Arc<FileInfo>>) -> Self {
        Self {
            env,
            file,
            rec: None,
            memo: RefCell::default(),
            busy: RefCell::default(),
        }
    }

    /// A context bound to a non-file record row.
    pub fn record(env: &'a Env<'a>, rec: Arc<BTreeMap<String, Value>>) -> Self {
        Self {
            rec: Some(rec),
            ..Self::new(env, None)
        }
    }

    /// The opaque [`crate::result::TrackerKey`] of a `tracker_items` record row (`None` for
    /// file rows or a record lacking the key fields).
    pub fn record_key(&self) -> Option<String> {
        let rec = self.rec.as_ref()?;
        let s = |k: &str| match rec.get(k) {
            Some(Value::Str(v)) => Some(v.clone()),
            _ => None,
        };
        Some(
            crate::result::TrackerKey {
                source_kind: s("source_kind")?,
                source_key: s("source_key")?,
                item_id: s("item_id")?,
            }
            .encode(),
        )
    }

    /// Plan round-13 WP-4 (Decision 1, audit M1): the selection value of a `sources` record
    /// row — its `name` when its `row` is `serial` (the daemon's `[[serial]]` rows, whose name
    /// is exactly the `file.serial` value of their entries); `None` for every other row.
    pub fn select_value(&self) -> Option<String> {
        let rec = self.rec.as_ref()?;
        match (rec.get("row"), rec.get("name")) {
            (Some(Value::Str(r)), Some(Value::Str(n))) if r == "serial" => Some(n.clone()),
            _ => None,
        }
    }

    /// Evaluates `e` for this row.
    pub fn eval(&self, e: &Expr) -> R<Value> {
        self.ev(e, None)
    }

    /// Evaluates `e` with lambda `value` bound to `v`.
    pub fn eval_with(&self, e: &Expr, v: Value) -> R<Value> {
        self.ev(
            e,
            Some(&Scope {
                value: v,
                parent: None,
            }),
        )
    }

    fn prop(&self, f: &FileInfo, key: &str) -> R<Value> {
        Ok(self
            .env
            .db
            .props(f)?
            .get(key)
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn formula(&self, name: &str) -> R<Value> {
        if let Some(v) = self.memo.borrow().get(name) {
            return Ok(v.clone());
        }
        let Some(def) = self.env.formulas.get(name) else {
            self.env.notice(format!("unknown formula `{name}`"));
            return Ok(Value::Null);
        };
        let Ok(e) = def else {
            return Ok(Value::Null);
        };
        if !self.busy.borrow_mut().insert(name.to_string()) {
            self.env
                .notice(format!("formula `{name}` refers to itself"));
            return Ok(Value::Null);
        }
        let v = self.ev(e, None);
        self.busy.borrow_mut().remove(name);
        let v = v?;
        self.memo.borrow_mut().insert(name.to_string(), v.clone());
        Ok(v)
    }

    fn ev(&self, e: &Expr, sc: Option<&Scope<'_>>) -> R<Value> {
        Ok(match e {
            Expr::Lit(v) => v.clone(),
            Expr::List(items) => Value::list(
                items
                    .iter()
                    .map(|i| self.ev(i, sc))
                    .collect::<R<Vec<_>>>()?,
            ),
            Expr::Ident(n) => match n.as_str() {
                "file" => self.file.clone().map_or(Value::Null, Value::File),
                "note" => match (&self.file, &self.rec) {
                    (Some(f), _) => Value::Note(Arc::clone(f)),
                    (None, Some(r)) => Value::Object(Arc::clone(r)),
                    (None, None) => Value::Null,
                },
                "formula" => Value::Formulas,
                "this" => self.env.this.clone().map_or(Value::Null, Value::File),
                "value" => sc.map_or(Value::Null, |s| s.value.clone()),
                "outer" => sc
                    .and_then(|s| s.parent)
                    .map_or(Value::Null, |s| s.value.clone()),
                other => match (&self.file, &self.rec) {
                    (Some(f), _) => self.prop(f, other)?,
                    (None, Some(r)) => r.get(other).cloned().unwrap_or(Value::Null),
                    (None, None) => Value::Null,
                },
            },
            Expr::Member(r, name) => {
                let r = self.ev(r, sc)?;
                self.member(&r, name)?
            }
            Expr::Index(r, i) => {
                let r = self.ev(r, sc)?;
                let i = self.ev(i, sc)?;
                match (&r, &i) {
                    (Value::List(l), Value::Num(n)) => {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                        let idx = if *n < 0.0 {
                            l.len() as f64 + n.trunc()
                        } else {
                            n.trunc()
                        };
                        if idx >= 0.0 {
                            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
                            l.get(idx as usize).cloned().unwrap_or(Value::Null)
                        } else {
                            Value::Null
                        }
                    }
                    (Value::List(_), _) | (Value::Null, _) => Value::Null,
                    _ => self.member(&r, &i.to_text())?,
                }
            }
            Expr::Call(f, args) => self.call(*f, args, sc)?,
            Expr::Method(r, m, args) => {
                let r = self.ev(r, sc)?;
                self.method(&r, *m, args, sc)?
            }
            Expr::Not(x) => Value::Bool(!self.ev(x, sc)?.truthy()),
            Expr::Neg(x) => Value::Num(-self.ev(x, sc)?.to_num()),
            Expr::Bin(op, a, b) => {
                let a = self.ev(a, sc)?;
                match op {
                    BinOp::And => Value::Bool(a.truthy() && self.ev(b, sc)?.truthy()),
                    BinOp::Or => Value::Bool(a.truthy() || self.ev(b, sc)?.truthy()),
                    _ => {
                        let b = self.ev(b, sc)?;
                        match op {
                            BinOp::Eq => Value::Bool(loose_eq(&a, &b)),
                            BinOp::Ne => Value::Bool(!loose_eq(&a, &b)),
                            BinOp::Lt => {
                                Value::Bool(partial_cmp(&a, &b).is_some_and(|o| o.is_lt()))
                            }
                            BinOp::Le => {
                                Value::Bool(partial_cmp(&a, &b).is_some_and(|o| o.is_le()))
                            }
                            BinOp::Gt => {
                                Value::Bool(partial_cmp(&a, &b).is_some_and(|o| o.is_gt()))
                            }
                            BinOp::Ge => {
                                Value::Bool(partial_cmp(&a, &b).is_some_and(|o| o.is_ge()))
                            }
                            _ => arith(*op, &a, &b),
                        }
                    }
                }
            }
        })
    }

    fn member(&self, r: &Value, name: &str) -> R<Value> {
        let db = self.env.db;
        Ok(match r {
            Value::File(f) => match name {
                "file" => r.clone(),
                "name" => Value::Str(f.name.clone()),
                "basename" => Value::Str(f.name.clone()),
                "path" => Value::Str(f.path.clone()),
                "folder" => Value::Str(f.folder.clone()),
                "ext" => Value::Str(f.ext.clone()),
                #[allow(clippy::cast_precision_loss)]
                "size" => Value::Num(f.size as f64),
                "mtime" => ms_date(f.mtime),
                "ctime" => ms_date(f.ctime),
                "serial" => self
                    .env
                    .serial
                    .and_then(|m| m.get(&f.path))
                    .map_or(Value::Null, |n| Value::Str(n.clone())),
                "tags" => Value::list(
                    db.tags(f)?
                        .iter()
                        .map(|t| Value::Str(format!("#{t}")))
                        .collect(),
                ),
                "links" => Value::List(db.links(f)?),
                "backlinks" => Value::List(db.backlinks(f)?),
                "tasks" => Value::list(db.tasks(f)?.iter().map(task_value).collect()),
                "properties" => Value::Object(db.props(f)?),
                other => self.prop(f, other)?,
            },
            Value::Note(f) => self.prop(f, name)?,
            Value::Formulas => self.formula(name)?,
            Value::Object(o) => o.get(name).cloned().unwrap_or(Value::Null),
            #[allow(clippy::cast_precision_loss)]
            Value::List(l) if name == "length" => Value::Num(l.len() as f64),
            #[allow(clippy::cast_precision_loss)]
            Value::Str(s) if name == "length" => Value::Num(s.chars().count() as f64),
            Value::Link { path, label } => match name {
                "path" => Value::Str(path.clone()),
                "label" => Value::Str(label.clone()),
                _ => Value::Null,
            },
            Value::Date(d, _) => {
                use chrono::Datelike;
                match name {
                    "year" => Value::Num(f64::from(d.year())),
                    "month" => Value::Num(f64::from(d.month())),
                    "day" => Value::Num(f64::from(d.day())),
                    _ => Value::Null,
                }
            }
            _ => Value::Null,
        })
    }

    fn file_at(&self, v: &Value) -> R<Value> {
        if let Value::File(_) = v {
            return Ok(v.clone());
        }
        let Some(p) = v.as_path() else {
            return Ok(Value::Null);
        };
        let p = p.trim_start_matches('/');
        if let Some(f) = self.env.db.file(p)? {
            return Ok(Value::File(f));
        }
        if !p.ends_with(".md")
            && let Some(f) = self.env.db.file(&format!("{p}.md"))?
        {
            return Ok(Value::File(f));
        }
        Ok(Value::Null)
    }

    fn call(&self, f: Func, args: &[Expr], sc: Option<&Scope<'_>>) -> R<Value> {
        let arg =
            |i: usize| -> R<Value> { args.get(i).map_or(Ok(Value::Null), |e| self.ev(e, sc)) };
        Ok(match f {
            Func::If => {
                if arg(0)?.truthy() {
                    arg(1)?
                } else {
                    arg(2)?
                }
            }
            Func::List => Value::List(to_list(&arg(0)?)),
            Func::Number => Value::Num(arg(0)?.to_num()),
            Func::File => self.file_at(&arg(0)?)?,
            Func::Files => Value::List(self.env.db.folder_files(&arg(0)?.to_text())?),
            Func::Link => {
                let target = arg(0)?;
                let path = target.as_path().unwrap_or_default();
                let label = if args.len() > 1 {
                    arg(1)?.to_text()
                } else {
                    let base = path.rsplit('/').next().unwrap_or(&path);
                    base.strip_suffix(".md").unwrap_or(base).to_string()
                };
                Value::Link { path, label }
            }
            Func::Url => {
                let url = arg(0)?.to_text();
                let label = if args.len() > 1 {
                    arg(1)?.to_text()
                } else {
                    String::new()
                };
                let label = if label.is_empty() { url.clone() } else { label };
                Value::Url { url, label }
            }
            Func::Folder => {
                let key = arg(0)?.to_text();
                match (self.env.folders, crate::FolderKey::parse(&key)) {
                    (Some(f), Some(k)) => Value::Str(f.get(k).to_owned()),
                    _ => Value::Null,
                }
            }
            Func::Today => Value::date(self.env.today),
            Func::Now => Value::Date(self.env.now, false),
            Func::Date => match arg(0)? {
                d @ Value::Date(..) => d,
                Value::Str(s) => parse_date(&s).map_or(Value::Null, |(d, o)| Value::Date(d, o)),
                _ => Value::Null,
            },
        })
    }

    fn method(&self, r: &Value, m: Meth, args: &[Expr], sc: Option<&Scope<'_>>) -> R<Value> {
        let arg =
            |i: usize| -> R<Value> { args.get(i).map_or(Ok(Value::Null), |e| self.ev(e, sc)) };
        let text = || r.to_text();
        Ok(match m {
            Meth::ToString => Value::Str(text()),
            Meth::Trim => Value::Str(text().trim().to_string()),
            Meth::Lower => Value::Str(text().to_lowercase()),
            Meth::Upper => Value::Str(text().to_uppercase()),
            Meth::Replace => {
                let from = arg(0)?.to_text();
                let to = arg(1)?.to_text();
                if from.is_empty() {
                    Value::Str(text())
                } else {
                    Value::Str(text().replace(&from, &to))
                }
            }
            Meth::Map | Meth::Filter => {
                let Some(body) = args.first() else {
                    return Ok(Value::Null);
                };
                let mut out = Vec::new();
                for item in to_list(r).iter() {
                    let inner = Scope {
                        value: item.clone(),
                        parent: sc,
                    };
                    let v = self.ev(body, Some(&inner))?;
                    if m == Meth::Map {
                        out.push(v);
                    } else if v.truthy() {
                        out.push(item.clone());
                    }
                }
                Value::list(out)
            }
            Meth::Contains => {
                let x = arg(0)?;
                Value::Bool(match r {
                    Value::List(l) => l.iter().any(|v| loose_eq(v, &x)),
                    Value::Null => false,
                    _ => text().contains(&x.to_text()),
                })
            }
            Meth::StartsWith => Value::Bool(text().starts_with(&arg(0)?.to_text())),
            Meth::EndsWith => Value::Bool(text().ends_with(&arg(0)?.to_text())),
            Meth::Join => {
                let sep = if args.is_empty() {
                    ",".to_string()
                } else {
                    arg(0)?.to_text()
                };
                Value::Str(
                    to_list(r)
                        .iter()
                        .map(Value::to_text)
                        .collect::<Vec<_>>()
                        .join(&sep),
                )
            }
            Meth::Split => {
                let sep = arg(0)?.to_text();
                let t = text();
                if sep.is_empty() {
                    Value::list(t.chars().map(|c| Value::Str(c.to_string())).collect())
                } else {
                    Value::list(t.split(&sep).map(|s| Value::Str(s.to_string())).collect())
                }
            }
            Meth::Round => {
                let n = r.to_num();
                let d = if args.is_empty() {
                    0.0
                } else {
                    arg(0)?.to_num()
                };
                let f = 10f64.powf(d.clamp(0.0, 12.0).trunc());
                Value::Num((n * f).round() / f)
            }
            Meth::ToFixed => {
                let n = r.to_num();
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let d = arg(0)?.to_num().clamp(0.0, 12.0) as usize;
                if n.is_finite() {
                    Value::Str(format!("{n:.d$}"))
                } else {
                    Value::Null
                }
            }
            Meth::HasTag => match r {
                Value::File(f) | Value::Note(f) => {
                    let tags = self.env.db.tags(f)?;
                    let mut hit = false;
                    for i in 0..args.len() {
                        let want = arg(i)?.to_text();
                        if tags.iter().any(|t| tag_matches(t, &want)) {
                            hit = true;
                            break;
                        }
                    }
                    Value::Bool(hit)
                }
                _ => Value::Bool(false),
            },
            Meth::InFolder => match r {
                Value::File(f) | Value::Note(f) => Value::Bool(in_folder(f, &arg(0)?.to_text())),
                _ => Value::Bool(false),
            },
            Meth::HasLink => match r {
                Value::File(f) | Value::Note(f) => {
                    let target = arg(0)?;
                    let want = target.as_path().unwrap_or_default();
                    let links = self.env.db.links(f)?;
                    Value::Bool(!want.is_empty() && links.iter().any(|l| {
                        matches!(l, Value::Link { path, label } if *path == want || *label == want)
                    }))
                }
                _ => Value::Bool(false),
            },
            Meth::AsLink => match r {
                Value::File(f) | Value::Note(f) => Value::Link {
                    path: f.path.clone(),
                    label: if args.is_empty() {
                        f.name.clone()
                    } else {
                        arg(0)?.to_text()
                    },
                },
                _ => Value::Null,
            },
            Meth::IsEmpty => Value::Bool(match r {
                Value::Null => true,
                Value::Str(s) => s.is_empty(),
                Value::List(l) => l.is_empty(),
                Value::Object(o) => o.is_empty(),
                _ => false,
            }),
            Meth::Format => {
                let d = match r {
                    Value::Date(d, _) => Some(*d),
                    Value::Str(s) => parse_date(s).map(|(d, _)| d),
                    _ => None,
                };
                match d {
                    Some(d) => {
                        use std::fmt::Write as _;
                        let mut s = String::new();
                        if write!(s, "{}", d.format(&date_format(&arg(0)?.to_text()))).is_err() {
                            Value::Null
                        } else {
                            Value::Str(s)
                        }
                    }
                    None => Value::Null,
                }
            }
        })
    }
}
