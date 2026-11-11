//! The Bases-subset expression grammar (no general scripting: a fixed grammar and a fixed
//! function set; any other name is a parse error that becomes a per-view notice).
//!
//! ```text
//! expr    := or
//! or      := and ("||" and)*
//! and     := eq ("&&" eq)*
//! eq      := cmp (("==" | "!=") cmp)*
//! cmp     := add (("<" | "<=" | ">" | ">=") add)*
//! add     := mul (("+" | "-") mul)*
//! mul     := unary (("*" | "/" | "%") unary)*
//! unary   := ("!" | "-") unary | postfix
//! postfix := primary ("." ident ["(" args ")"] | "[" expr "]")*
//! primary := number | string | "true" | "false" | "null" | "[" args "]" | "(" expr ")"
//!          | ident "(" args ")" | ident
//! ```

use thiserror::Error;

use crate::value::Value;

/// A parse failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    #[error("unexpected {0} at byte {1}")]
    Unexpected(String, usize),
    #[error("unterminated string at byte {0}")]
    Unterminated(usize),
    #[error("unknown function `{0}`")]
    UnknownFunction(String),
    #[error("`{0}` takes {1} argument(s), got {2}")]
    Arity(String, String, usize),
}

/// Global functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    If,
    List,
    Number,
    File,
    Files,
    Link,
    Url,
    Today,
    Now,
    Date,
    /// `folder("<key>")`: a configured vault-structure folder (plan public-release WP-2).
    Folder,
}

/// Methods (`recv.name(args)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meth {
    ToString,
    Trim,
    Replace,
    Map,
    Filter,
    Contains,
    StartsWith,
    EndsWith,
    Join,
    Lower,
    Upper,
    Split,
    Round,
    ToFixed,
    HasTag,
    InFolder,
    HasLink,
    AsLink,
    IsEmpty,
    Format,
}

impl Func {
    fn parse(name: &str) -> Option<(Self, usize, usize)> {
        Some(match name {
            "if" => (Self::If, 2, 3),
            "list" => (Self::List, 1, 1),
            "number" => (Self::Number, 1, 1),
            "file" => (Self::File, 1, 1),
            "files" => (Self::Files, 1, 1),
            "link" => (Self::Link, 1, 2),
            "url" => (Self::Url, 1, 2),
            "today" => (Self::Today, 0, 0),
            "now" => (Self::Now, 0, 0),
            "date" => (Self::Date, 1, 1),
            "folder" => (Self::Folder, 1, 1),
            _ => return None,
        })
    }
}

impl Meth {
    fn parse(name: &str) -> Option<(Self, usize, usize)> {
        Some(match name {
            "toString" => (Self::ToString, 0, 0),
            "trim" => (Self::Trim, 0, 0),
            "replace" => (Self::Replace, 2, 2),
            "map" => (Self::Map, 1, 1),
            "filter" => (Self::Filter, 1, 1),
            "contains" => (Self::Contains, 1, 1),
            "startsWith" => (Self::StartsWith, 1, 1),
            "endsWith" => (Self::EndsWith, 1, 1),
            "join" => (Self::Join, 0, 1),
            "lower" => (Self::Lower, 0, 0),
            "upper" => (Self::Upper, 0, 0),
            "split" => (Self::Split, 1, 1),
            "round" => (Self::Round, 0, 1),
            "toFixed" => (Self::ToFixed, 1, 1),
            "hasTag" => (Self::HasTag, 1, usize::MAX),
            "inFolder" => (Self::InFolder, 1, 1),
            "hasLink" => (Self::HasLink, 1, 1),
            "asLink" => (Self::AsLink, 0, 1),
            "isEmpty" => (Self::IsEmpty, 0, 0),
            "format" => (Self::Format, 1, 1),
            _ => return None,
        })
    }
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

/// The AST.
#[derive(Debug, Clone)]
pub enum Expr {
    Lit(Value),
    List(Vec<Expr>),
    Ident(String),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Call(Func, Vec<Expr>),
    Method(Box<Expr>, Meth, Vec<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Bin(BinOp, Box<Expr>, Box<Expr>),
}

impl Expr {
    /// Whether the expression reads the current row (`file`, `note`, `formula`, bare property
    /// names, lambda `value`); row-independent expressions (`this`, literals, `today()`,
    /// `file(..)`) can be folded into SQL.
    pub fn row_dependent(&self) -> bool {
        match self {
            Self::Lit(_) => false,
            Self::List(v) | Self::Call(_, v) => v.iter().any(Self::row_dependent),
            Self::Ident(n) => !matches!(n.as_str(), "this"),
            Self::Member(e, _) | Self::Not(e) | Self::Neg(e) => e.row_dependent(),
            Self::Index(a, b) | Self::Bin(_, a, b) => a.row_dependent() || b.row_dependent(),
            Self::Method(r, _, a) => r.row_dependent() || a.iter().any(Self::row_dependent),
        }
    }

    /// Whether the expression reads the property `owner.field` (`file.serial`, or the indexed
    /// form `file["serial"]`). Walks the AST, so the text only appearing inside a string literal
    /// is not a reference.
    /// Pushes the literal key of every `folder("<key>")` call in the expression onto `out`.
    pub fn folder_keys(&self, out: &mut Vec<String>) {
        match self {
            Self::Lit(_) | Self::Ident(_) => {}
            Self::Call(Func::Folder, v) => {
                if let [Self::Lit(Value::Str(k))] = v.as_slice() {
                    out.push(k.clone());
                }
                v.iter().for_each(|e| e.folder_keys(out));
            }
            Self::List(v) | Self::Call(_, v) => v.iter().for_each(|e| e.folder_keys(out)),
            Self::Member(e, _) | Self::Not(e) | Self::Neg(e) => e.folder_keys(out),
            Self::Index(a, b) | Self::Bin(_, a, b) => {
                a.folder_keys(out);
                b.folder_keys(out);
            }
            Self::Method(r, _, a) => {
                r.folder_keys(out);
                a.iter().for_each(|e| e.folder_keys(out));
            }
        }
    }

    pub fn references(&self, owner: &str, field: &str) -> bool {
        match self {
            Self::Lit(_) => false,
            Self::Ident(_) => false,
            Self::Member(e, f) => {
                (f == field && matches!(&**e, Self::Ident(o) if o == owner))
                    || e.references(owner, field)
            }
            Self::Index(a, b) => {
                let direct = matches!(&**a, Self::Ident(o) if o == owner)
                    && matches!(&**b, Self::Lit(Value::Str(k)) if k == field);
                direct || a.references(owner, field) || b.references(owner, field)
            }
            Self::List(v) | Self::Call(_, v) => v.iter().any(|e| e.references(owner, field)),
            Self::Not(e) | Self::Neg(e) => e.references(owner, field),
            Self::Bin(_, a, b) => a.references(owner, field) || b.references(owner, field),
            Self::Method(r, _, a) => {
                r.references(owner, field) || a.iter().any(|e| e.references(owner, field))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    End,
}

const OPS: &[&str] = &[
    "==", "!=", "<=", ">=", "&&", "||", "<", ">", "!", "+", "-", "*", "/", "%", "(", ")", "[", "]",
    ".", ",",
];

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, ParseError> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut i = 0;
    while let Some(&(pos, c)) = chars.get(i) {
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let start = i;
            while chars
                .get(i)
                .is_some_and(|(_, c)| c.is_ascii_digit() || *c == '.')
            {
                // `1.round()` is not used; a `.` followed by a non-digit ends the number.
                if chars[i].1 == '.' && !chars.get(i + 1).is_some_and(|(_, c)| c.is_ascii_digit()) {
                    break;
                }
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |(p, _)| *p);
            let text = &src[chars[start].0..end];
            let n = text
                .parse()
                .map_err(|_| ParseError::Unexpected(format!("number `{text}`"), pos))?;
            out.push((Tok::Num(n), pos));
        } else if c == '"' || c == '\'' {
            let mut s = String::new();
            i += 1;
            loop {
                let Some(&(_, d)) = chars.get(i) else {
                    return Err(ParseError::Unterminated(pos));
                };
                i += 1;
                if d == c {
                    break;
                }
                if d == '\\' {
                    let Some(&(_, e)) = chars.get(i) else {
                        return Err(ParseError::Unterminated(pos));
                    };
                    i += 1;
                    s.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        other => other,
                    });
                } else {
                    s.push(d);
                }
            }
            out.push((Tok::Str(s), pos));
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while chars
                .get(i)
                .is_some_and(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '$')
            {
                i += 1;
            }
            let end = chars.get(i).map_or(src.len(), |(p, _)| *p);
            out.push((Tok::Ident(src[chars[start].0..end].to_string()), pos));
        } else {
            let rest = &src[pos..];
            let op = OPS
                .iter()
                .find(|o| rest.starts_with(**o))
                .ok_or_else(|| ParseError::Unexpected(format!("`{c}`"), pos))?;
            out.push((Tok::Op(op), pos));
            i += op.chars().count();
        }
    }
    out.push((Tok::End, src.len()));
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        self.toks.get(self.i).map_or(&Tok::End, |t| &t.0)
    }

    fn pos(&self) -> usize {
        self.toks.get(self.i).map_or(0, |t| t.1)
    }

    fn bump(&mut self) -> Tok {
        let t = self.peek().clone();
        self.i += 1;
        t
    }

    fn eat(&mut self, op: &str) -> bool {
        if matches!(self.peek(), Tok::Op(o) if *o == op) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn unexpected(&self) -> ParseError {
        let what = match self.peek() {
            Tok::Num(n) => format!("number {n}"),
            Tok::Str(_) => "string".into(),
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Op(o) => format!("`{o}`"),
            Tok::End => "end of expression".into(),
        };
        ParseError::Unexpected(what, self.pos())
    }

    fn expect(&mut self, op: &str) -> Result<(), ParseError> {
        if self.eat(op) {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn binary(
        &mut self,
        ops: &[(&str, BinOp)],
        next: fn(&mut Self) -> Result<Expr, ParseError>,
    ) -> Result<Expr, ParseError> {
        let mut l = next(self)?;
        'outer: loop {
            for (s, op) in ops {
                if self.eat(s) {
                    let r = next(self)?;
                    l = Expr::Bin(*op, Box::new(l), Box::new(r));
                    continue 'outer;
                }
            }
            return Ok(l);
        }
    }

    fn or(&mut self) -> Result<Expr, ParseError> {
        self.binary(&[("||", BinOp::Or)], Self::and)
    }
    fn and(&mut self) -> Result<Expr, ParseError> {
        self.binary(&[("&&", BinOp::And)], Self::eq)
    }
    fn eq(&mut self) -> Result<Expr, ParseError> {
        self.binary(&[("==", BinOp::Eq), ("!=", BinOp::Ne)], Self::cmp)
    }
    fn cmp(&mut self) -> Result<Expr, ParseError> {
        self.binary(
            &[
                ("<=", BinOp::Le),
                (">=", BinOp::Ge),
                ("<", BinOp::Lt),
                (">", BinOp::Gt),
            ],
            Self::add,
        )
    }
    fn add(&mut self) -> Result<Expr, ParseError> {
        self.binary(&[("+", BinOp::Add), ("-", BinOp::Sub)], Self::mul)
    }
    fn mul(&mut self) -> Result<Expr, ParseError> {
        self.binary(
            &[("*", BinOp::Mul), ("/", BinOp::Div), ("%", BinOp::Rem)],
            Self::unary,
        )
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.eat("!") {
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if self.eat("-") {
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        self.postfix()
    }

    fn args(&mut self, close: &str) -> Result<Vec<Expr>, ParseError> {
        let mut v = Vec::new();
        if self.eat(close) {
            return Ok(v);
        }
        loop {
            v.push(self.or()?);
            if self.eat(close) {
                return Ok(v);
            }
            self.expect(",")?;
        }
    }

    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut e = self.primary()?;
        loop {
            if self.eat(".") {
                let Tok::Ident(name) = self.bump() else {
                    self.i -= 1;
                    return Err(self.unexpected());
                };
                if self.eat("(") {
                    let args = self.args(")")?;
                    let (m, lo, hi) =
                        Meth::parse(&name).ok_or(ParseError::UnknownFunction(name.clone()))?;
                    arity(&name, lo, hi, args.len())?;
                    e = Expr::Method(Box::new(e), m, args);
                } else {
                    e = Expr::Member(Box::new(e), name);
                }
            } else if self.eat("[") {
                let idx = self.or()?;
                self.expect("]")?;
                e = Expr::Index(Box::new(e), Box::new(idx));
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        match self.bump() {
            Tok::Num(n) => Ok(Expr::Lit(Value::Num(n))),
            Tok::Str(s) => Ok(Expr::Lit(Value::Str(s))),
            Tok::Op("(") => {
                let e = self.or()?;
                self.expect(")")?;
                Ok(e)
            }
            Tok::Op("[") => Ok(Expr::List(self.args("]")?)),
            Tok::Ident(name) => match name.as_str() {
                "true" => Ok(Expr::Lit(Value::Bool(true))),
                "false" => Ok(Expr::Lit(Value::Bool(false))),
                "null" => Ok(Expr::Lit(Value::Null)),
                _ if self.eat("(") => {
                    let args = self.args(")")?;
                    let (f, lo, hi) =
                        Func::parse(&name).ok_or(ParseError::UnknownFunction(name.clone()))?;
                    arity(&name, lo, hi, args.len())?;
                    Ok(Expr::Call(f, args))
                }
                _ => Ok(Expr::Ident(name)),
            },
            _ => {
                self.i -= 1;
                Err(self.unexpected())
            }
        }
    }
}

fn arity(name: &str, lo: usize, hi: usize, n: usize) -> Result<(), ParseError> {
    if n < lo || n > hi {
        let want = if lo == hi {
            lo.to_string()
        } else if hi == usize::MAX {
            format!("{lo}+")
        } else {
            format!("{lo}-{hi}")
        };
        return Err(ParseError::Arity(name.to_string(), want, n));
    }
    Ok(())
}

/// Parses one expression.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    let mut p = Parser {
        toks: lex(src)?,
        i: 0,
    };
    let e = p.or()?;
    if *p.peek() != Tok::End {
        return Err(p.unexpected());
    }
    Ok(e)
}

/// A column/sort/group property id (`file.name`, `formula.x`, `note.word-count`, bare
/// `word-count` = `note.word-count`) as an expression.
pub fn property(id: &str) -> Expr {
    let note = |k: &str| {
        Expr::Index(
            Box::new(Expr::Ident("note".into())),
            Box::new(Expr::Lit(Value::Str(k.to_string()))),
        )
    };
    match id.split_once('.') {
        Some(("file", f)) => Expr::Member(Box::new(Expr::Ident("file".into())), f.to_string()),
        Some(("formula", f)) => {
            Expr::Member(Box::new(Expr::Ident("formula".into())), f.to_string())
        }
        Some(("note", k)) => note(k),
        _ => note(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar_shapes() {
        assert!(matches!(
            parse("a || b && c").unwrap(),
            Expr::Bin(BinOp::Or, _, _)
        ));
        assert!(matches!(
            parse("!list(x).contains(\"d\")").unwrap(),
            Expr::Not(_)
        ));
        assert!(matches!(
            parse("file.folder.split(\"/\")[1]").unwrap(),
            Expr::Index(..)
        ));
        assert!(matches!(
            parse("1 + 2 * 3").unwrap(),
            Expr::Bin(BinOp::Add, _, _)
        ));
        assert!(matches!(parse("'a\\'b'").unwrap(), Expr::Lit(Value::Str(s)) if s == "a'b"));
        assert!(matches!(parse("[1, 2]").unwrap(), Expr::List(v) if v.len() == 2));
        assert!(matches!(
            parse("x.round(1)").unwrap(),
            Expr::Method(_, Meth::Round, _)
        ));
        assert!(matches!(parse("3.5").unwrap(), Expr::Lit(Value::Num(n)) if n == 3.5));
    }

    #[test]
    fn errors() {
        assert_eq!(
            parse("bogus(1)").unwrap_err(),
            ParseError::UnknownFunction("bogus".into())
        );
        assert_eq!(
            parse("x.frobnicate()").unwrap_err(),
            ParseError::UnknownFunction("frobnicate".into())
        );
        assert!(matches!(parse("if(1)"), Err(ParseError::Arity(..))));
        assert!(matches!(parse("\"abc"), Err(ParseError::Unterminated(0))));
        assert!(matches!(parse("1 +"), Err(ParseError::Unexpected(..))));
        assert!(matches!(parse("a b"), Err(ParseError::Unexpected(..))));
        assert!(matches!(parse("a # b"), Err(ParseError::Unexpected(..))));
    }

    #[test]
    fn row_dependence() {
        assert!(
            !parse("\"daily/day/\" + this.file.name")
                .unwrap()
                .row_dependent()
        );
        assert!(
            !parse("file(\"x.md\").properties.y")
                .unwrap()
                .row_dependent()
        );
        assert!(parse("note.x").unwrap().row_dependent());
        assert!(parse("file.name").unwrap().row_dependent());
    }

    #[test]
    fn property_ids() {
        assert!(matches!(property("file.name"), Expr::Member(_, f) if f == "name"));
        assert!(matches!(property("formula.tags"), Expr::Member(_, f) if f == "tags"));
        assert!(matches!(property("note.word-count"), Expr::Index(..)));
        assert!(matches!(property("word-count"), Expr::Index(..)));
    }

    #[test]
    fn every_base_vector_parses() {
        for v in super::tests_support::BASE_VECTORS {
            if let Err(e) = parse(v) {
                panic!("{v}: {e}");
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// Every formula and filter string in the six live `.base` files (config, not note
    /// content; `works#Unread` represented by one of its 20 generated clauses).
    pub const BASE_VECTORS: &[&str] = &[
        r##"list(note.tags).map(value.toString().replace("#", "")).filter(value != "clippings" && value != "using").join(", ")"##,
        r##"(number(note["word-count"]) / number(file("_custom/Settings.md").properties.reading_wpm)).round(1)"##,
        r##"if(note.read == true, "✅", "❌")"##,
        r##"if(note.created, note.created, file.ctime)"##,
        r##"if(note.read == true, 0, number(note["word-count"]))"##,
        r##"file.inFolder("daily/day")"##,
        r##"(this["author-filter"] && this["author-filter"].toString().trim() != "" || list(this["source-prefix-filter"]).filter(value.toString().trim() != "").length > 0) && (!(this["author-filter"] && this["author-filter"].toString().trim() != "") || list(note.author).map(value.toString().trim().replace("#", "")).contains(this["author-filter"].toString().trim().replace("#", ""))) && (list(this["source-prefix-filter"]).filter(value.toString().trim() != "").length == 0 || list(this["source-prefix-filter"]).filter(value.toString().trim() != "").filter(note.source.toString().trim().startsWith(value.toString().trim())).length > 0)"##,
        r##"file.folder == "daily/day/" + this.file.name"##,
        r##"list(note.tags).map(value.toString().replace("#", "")).filter(value != "clippings" && value != "refined" && value != "using").join(", ")"##,
        r##"if(note.read == true, 0, (number(note["word-count"]) / number(file("_custom/Settings.md").properties.reading_wpm)).round(1))"##,
        r##"if(file.hasTag("using"), "👈", "")"##,
        r##"file.inFolder("projects/research") || file.hasTag("research")"##,
        r##"file.path != this.file.path"##,
        r##"file.folder == "topic_b""##,
        r##"file.folder == "stack""##,
        r##"file.folder == "_custom/prompts""##,
        r##"link(file.path, if(note["project-title"], note["project-title"], if(note.title, note.title, file.folder.split("/")[1])))"##,
        r##"if(file(file.folder + "/NOTES.md"), link(file.folder + "/NOTES.md", "NOTES"), "")"##,
        r##"if(file(file.folder + "/README.md"), link(file.folder + "/README.md", "README"), "")"##,
        r##"list(note["pending-items"]).filter(value.toString().trim() != "").length"##,
        r##"if(note["project-status"], note["project-status"].toString().trim().lower(), "active")"##,
        r##"file.name == "PROJECT""##,
        r##"file.folder.startsWith("projects/")"##,
        r##"file.folder.split("/").length == 2"##,
        r##"formula.status == "active""##,
        r##"formula.status == "blocked""##,
        r##"formula.status == "paused""##,
        r##"if(list(note.tags).map(value.toString().replace("#", "")).contains("blog"), "blog", if(list(note.tags).map(value.toString().replace("#", "")).contains("news"), "news", if(list(note.tags).map(value.toString().replace("#", "")).contains("transcript"), "transcript", if(list(note.tags).map(value.toString().replace("#", "")).contains("fanfic"), "fanfic", "other"))))"##,
        r##"file.path.startsWith("daily/day/" + this.file.name)"##,
        r##"note.read == false"##,
        r##"!list(note.tags).map(value.toString().replace("#", "")).contains("daily")"##,
        r##"note.created"##,
        r##"if(note["work-status"], note["work-status"].toString().trim() == "active", true)"##,
        r##"note["work-status"] == "complete""##,
        r##"note.read != true"##,
        r##"(file("core/works/X Y.md").properties["author-filter"] && file("core/works/X Y.md").properties["author-filter"].toString().trim() != "" || list(file("core/works/X Y.md").properties["source-prefix-filter"]).filter(value.toString().trim() != "").length > 0) && (!(file("core/works/X Y.md").properties["author-filter"] && file("core/works/X Y.md").properties["author-filter"].toString().trim() != "") || list(note.author).map(value.toString().trim().replace("#", "")).contains(file("core/works/X Y.md").properties["author-filter"].toString().trim().replace("#", ""))) && (list(file("core/works/X Y.md").properties["source-prefix-filter"]).filter(value.toString().trim() != "").length == 0 || list(file("core/works/X Y.md").properties["source-prefix-filter"]).filter(value.toString().trim() != "").filter(note.source.toString().trim().startsWith(value.toString().trim())).length > 0)"##,
    ];
}
