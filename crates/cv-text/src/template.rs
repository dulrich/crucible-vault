//! Template token engine.
//!
//! Ported from `https://github.com/dulrich/obsidian-crucible/blob/master/src/utils.ts:91-182` (`applyTemplateString`,
//! `applyAttachmentTemplate`, `collapseWhitespace`, `slugify`); token list per
//! `https://github.com/dulrich/obsidian-crucible/blob/master/AGENTS.md:191-206`. Dropped (Obsidian-induced): the `window.moment()`
//! clock read inside the engine — `now` is an explicit context field, so rendering is pure.
//!
//! Deviation: Crucible applies tokens as sequential regex replaces, so a `{{title}}` inside a
//! user-supplied `value` got expanded. This engine is single-pass over the template; substituted
//! values are never re-scanned. Unknown `{{...}}` tokens are left verbatim, as in Crucible.

use crate::moment::{MomentFormatError, format_moment};
use chrono::NaiveDateTime;
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("bad `{{{{datetime:...}}}}` format: {0}")]
    Format(#[from] MomentFormatError),
}

/// Context for note/capture templates (`applyTemplateString`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateContext {
    /// Target date/time of the note — `{{date}}`, `{{time}}`, `{{datetime:FMT}}`.
    pub target: NaiveDateTime,
    /// Local wall-clock time at invocation — `{{today}}`, `{{now}}`.
    pub now: NaiveDateTime,
    /// `{{title}}`.
    pub title: String,
    /// `{{value}}` / `{{value:oneline}}`.
    pub value: String,
    /// Caller-specific extra tokens (e.g. `source_link`); builtins win on a name clash.
    pub extra: BTreeMap<String, String>,
}

/// Context for attachment folder/name templates (`applyAttachmentTemplate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentTemplateContext {
    /// Local wall-clock time at invocation — every date token and `{{datetime:FMT}}`.
    pub now: NaiveDateTime,
    pub note_basename: String,
    pub note_folder_path: String,
    /// Original attachment filename without extension.
    pub original_name: String,
    /// Extension without the dot.
    pub ext: String,
    pub md5: Option<String>,
}

/// Collapse every whitespace run to one space and trim the ends.
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const DIGITS: &str = "0123456789";
const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";

fn nth(table: &'static str, i: u32) -> Option<&'static str> {
    let i = usize::try_from(i).ok()?;
    table.get(i..i + 1)
}

/// What NFKD followed by stripping U+0300–U+036F yields for `c`, for the characters where that
/// differs from `c` and the result contains ASCII alphanumerics — the fold Crucible's `slugify`
/// performs via `normalize('NFKD').replace(/[̀-ͯ]/g, '')`
/// (`https://github.com/dulrich/obsidian-crucible/blob/master/src/utils.ts:129-136`). Applied after lowercasing, so only lowercase
/// forms are listed. Covered: Latin-1 Supplement and Latin Extended-A letters with canonical
/// decompositions (U+00E0–U+00FF, U+0101–U+017F), compatibility forms U+00AA/U+00BA (ª º),
/// U+0133 (ĳ), U+0140 (ŀ), U+0149 (ŉ), U+017F (ſ), superscript digits (U+00B2, U+00B3, U+00B9,
/// U+2070, U+2074–U+2079), subscript digits U+2080–U+2089, ligatures U+FB00–U+FB06, fullwidth
/// U+FF10–FF19 / U+FF21–FF3A / U+FF41–FF5A. Characters without a decomposition (ß æ ø đ ł þ ħ
/// ŋ œ ŧ ı ĸ) return `None` and stay separators, exactly as in JS.
pub fn fold_nfkd_ascii(c: char) -> Option<&'static str> {
    let cp = u32::from(c);
    Some(match c {
        'ª' | 'à'..='å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' => "d",
        'è'..='ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' => "h",
        'ì'..='ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' => "i",
        'ĳ' => "ij",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' => "l",
        'ŀ' => "l\u{b7}",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ŉ' => "\u{2bc}n",
        'º' | 'ò'..='ö' | 'ō' | 'ŏ' | 'ő' => "o",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ſ' => "s",
        'ţ' | 'ť' => "t",
        'ù'..='ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        '¹' => "1",
        '²' => "2",
        '³' => "3",
        '\u{2070}' => "0",
        '\u{2074}'..='\u{2079}' => nth(DIGITS, cp - 0x2070)?,
        '\u{2080}'..='\u{2089}' => nth(DIGITS, cp - 0x2080)?,
        '\u{fb00}' => "ff",
        '\u{fb01}' => "fi",
        '\u{fb02}' => "fl",
        '\u{fb03}' => "ffi",
        '\u{fb04}' => "ffl",
        '\u{fb05}' | '\u{fb06}' => "st",
        '\u{ff10}'..='\u{ff19}' => nth(DIGITS, cp - 0xff10)?,
        '\u{ff21}'..='\u{ff3a}' => nth(LETTERS, cp - 0xff21)?,
        '\u{ff41}'..='\u{ff5a}' => nth(LETTERS, cp - 0xff41)?,
        _ => return None,
    })
}

/// Lowercase ASCII slug (`https://github.com/dulrich/obsidian-crucible/blob/master/src/utils.ts:129-136`): lowercase, fold per
/// [`fold_nfkd_ascii`], drop combining marks U+0300–U+036F, runs of non-`[a-z0-9]` become `-`,
/// edges trimmed.
pub fn slugify(value: &str) -> String {
    let lower = value.to_lowercase();
    let mut out = String::new();
    let mut pending_dash = false;
    let mut push = |c: char| {
        if ('\u{300}'..='\u{36f}').contains(&c) {
            return;
        }
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(c);
        } else {
            pending_dash = true;
        }
    };
    for c in lower.chars() {
        match fold_nfkd_ascii(c) {
            Some(s) => s.chars().for_each(&mut push),
            None => push(c),
        }
    }
    out
}

/// Obsidian Web Clipper-style note basename for `title`: drops characters illegal in filenames
/// (`/ \ : * ? " < > |`) and those that break wikilinks (`# ^ [ ]`) plus control characters,
/// collapses whitespace runs to one space, caps at 120 chars on a char boundary, and trims edge
/// spaces and trailing dots. An empty result is `""` (the caller falls back).
pub fn title_filename(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '#' | '^' | '[' | ']'
            )
        })
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((i, _)) = out.char_indices().nth(120) {
        out.truncate(i);
    }
    out.trim_end_matches(['.', ' ']).to_owned()
}

/// Normalizes raw tag strings: each is split on commas and whitespace, a leading `#` is
/// stripped, empties are dropped, and duplicates are removed keeping first-seen order
/// (`["#a #b, c", "a"]` → `a`, `b`, `c`).
pub fn normalize_tags<I, S>(raw: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for s in raw {
        for t in s
            .as_ref()
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(|t| t.trim_start_matches('#'))
        {
            if !t.is_empty() && !out.iter().any(|x| x == t) {
                out.push(t.to_owned());
            }
        }
    }
    out
}

static INPUT_ALIAS: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\{\{input(:[^}]*)?\}\}").ok());

/// Rewrite the agent-prompt alias `{{input}}` / `{{input:MOD}}` to `{{value}}` / `{{value:MOD}}`.
/// Per `https://github.com/dulrich/obsidian-crucible/blob/master/src/agents.ts:83` and `src/AGENTS.md:59` (modifier suffixes included).
pub fn rewrite_input_alias(template: &str) -> String {
    match INPUT_ALIAS.as_ref() {
        Some(re) => re.replace_all(template, "{{value$1}}").into_owned(),
        None => template.to_owned(),
    }
}

/// Single-pass `{{token}}` scanner; `resolve` returns `None` to leave a token verbatim. The one
/// exposed template engine (the agent step renders its prompts through it, WP-R10).
pub fn render<F>(template: &str, mut resolve: F) -> Result<String, TemplateError>
where
    F: FnMut(&str) -> Result<Option<String>, TemplateError>,
{
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            return Ok(out);
        };
        let inner = &after[..close];
        match resolve(inner)? {
            Some(v) => out.push_str(&v),
            None => {
                out.push_str("{{");
                out.push_str(inner);
                out.push_str("}}");
            }
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Render a note/capture template.
pub fn apply_template(template: &str, ctx: &TemplateContext) -> Result<String, TemplateError> {
    render(template, |tok| {
        if let Some(fmt) = tok.strip_prefix("datetime:") {
            return Ok(Some(format_moment(&ctx.target, fmt)?));
        }
        Ok(match tok {
            "date" => Some(format_moment(&ctx.target, "YYYY-MM-DD")?),
            "time" => Some(format_moment(&ctx.target, "HH:mm")?),
            "today" => Some(format_moment(&ctx.now, "YYYY-MM-DD")?),
            "now" => Some(format_moment(&ctx.now, "YYYY-MM-DDTHH:mm:ss")?),
            "value:oneline" => Some(collapse_whitespace(&ctx.value)),
            "value" => Some(ctx.value.clone()),
            "title" => Some(ctx.title.clone()),
            other => ctx.extra.get(other).cloned(),
        })
    })
}

/// Render an attachment folder/name template.
pub fn apply_attachment_template(
    template: &str,
    ctx: &AttachmentTemplateContext,
) -> Result<String, TemplateError> {
    render(template, |tok| {
        if let Some(fmt) = tok.strip_prefix("datetime:") {
            return Ok(Some(format_moment(&ctx.now, fmt)?));
        }
        Ok(match tok {
            "folder" => Some(ctx.note_folder_path.clone()),
            "slug" => Some(slugify(&ctx.note_basename)),
            "name" | "title" => Some(ctx.note_basename.clone()),
            "ext" => Some(ctx.ext.clone()),
            "original" => Some(ctx.original_name.clone()),
            "md5" => Some(ctx.md5.clone().unwrap_or_default()),
            "date" | "today" => Some(format_moment(&ctx.now, "YYYY-MM-DD")?),
            "time" => Some(format_moment(&ctx.now, "HH:mm")?),
            "now" => Some(format_moment(&ctx.now, "YYYY-MM-DDTHH:mm:ss")?),
            _ => None,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
    }

    fn ctx(value: &str) -> TemplateContext {
        TemplateContext {
            target: dt(2026, 10, 14, 9, 5, 0),
            now: dt(2026, 10, 13, 15, 30, 7),
            title: "Note".into(),
            value: value.into(),
            extra: BTreeMap::new(),
        }
    }

    // Vectors from https://github.com/dulrich/obsidian-crucible/blob/master/tests/templateTokens.test.mjs.
    #[test]
    fn collapse_whitespace_vectors() {
        assert_eq!(collapse_whitespace("a  b\n\nc\td"), "a b c d");
        assert_eq!(
            collapse_whitespace("  leading and trailing \n"),
            "leading and trailing"
        );
        assert_eq!(collapse_whitespace(""), "");
    }

    #[test]
    fn value_oneline_collapses_multi_paragraph() {
        let v = "First paragraph of the\nobservation.\n\nSecond paragraph here.";
        assert_eq!(
            apply_template("- {{value:oneline}}", &ctx(v)).unwrap(),
            "- First paragraph of the observation. Second paragraph here."
        );
    }

    #[test]
    fn value_preserves_newlines_alongside_oneline() {
        let v = "line one\n\nline two";
        assert_eq!(
            apply_template("{{value:oneline}}|{{value}}", &ctx(v)).unwrap(),
            "line one line two|line one\n\nline two"
        );
    }

    #[test]
    fn date_tokens_use_target_and_now() {
        let out = apply_template(
            "{{date}} {{time}} {{today}} {{now}} {{title}} {{datetime:MMMM YYYY}}",
            &ctx(""),
        )
        .unwrap();
        assert_eq!(
            out,
            "2026-10-14 09:05 2026-10-13 2026-10-13T15:30:07 Note October 2026"
        );
    }

    #[test]
    fn extra_unknown_and_unterminated() {
        let mut c = ctx("v");
        c.extra.insert("source_link".into(), "[[a|b]]".into());
        c.extra.insert("title".into(), "shadowed".into());
        assert_eq!(
            apply_template("{{source_link}} {{nope}} {{title}} {{open", &c).unwrap(),
            "[[a|b]] {{nope}} Note {{open"
        );
    }

    #[test]
    fn values_are_not_rescanned() {
        assert_eq!(
            apply_template("{{value}}", &ctx("{{title}}")).unwrap(),
            "{{title}}"
        );
    }

    #[test]
    fn bad_datetime_format_is_error() {
        assert!(matches!(
            apply_template("{{datetime:Qo}}", &ctx("")),
            Err(TemplateError::Format(_))
        ));
    }

    #[test]
    fn input_alias_with_modifiers() {
        assert_eq!(
            rewrite_input_alias("{{input}} / {{input:oneline}} / {{inputs}}"),
            "{{value}} / {{value:oneline}} / {{inputs}}"
        );
    }

    #[test]
    fn attachment_tokens() {
        let a = AttachmentTemplateContext {
            now: dt(2026, 10, 13, 15, 30, 7),
            note_basename: "My Note: Draft 2".into(),
            note_folder_path: "projects/x".into(),
            original_name: "IMG_01".into(),
            ext: "png".into(),
            md5: None,
        };
        let out = apply_attachment_template("{{folder}}/{{slug}}-{{name}}-{{title}}-{{original}}.{{ext}}|{{md5}}|{{date}}|{{datetime:YYYY}}", &a)
            .unwrap();
        assert_eq!(
            out,
            "projects/x/my-note-draft-2-My Note: Draft 2-My Note: Draft 2-IMG_01.png||2026-10-13|2026"
        );
    }

    #[test]
    fn title_filename_vectors() {
        assert_eq!(
            title_filename("  A/B: C*? \"x\" <y> | #t ^b [[l]]  "),
            "AB C x y t b l"
        );
        assert_eq!(title_filename("Tabs\tand\nlines\u{7}x"), "Tabs and lines x");
        assert_eq!(title_filename("Trailing dots... "), "Trailing dots");
        assert_eq!(title_filename("Café — déjà vu"), "Café — déjà vu");
        assert_eq!(title_filename("///"), "");
        let long = "é".repeat(200);
        assert_eq!(title_filename(&long).chars().count(), 120);
    }

    #[test]
    fn normalize_tags_splits_strips_dedupes() {
        assert_eq!(
            normalize_tags(["#a #b #c", "b, d,,#a", "  ", "##e"]),
            ["a", "b", "c", "d", "e"]
        );
        assert!(normalize_tags(Vec::<String>::new()).is_empty());
    }

    #[test]
    fn slugify_vectors() {
        assert_eq!(slugify("  Hello, World!  "), "hello-world");
        assert_eq!(slugify("---"), "");
    }

    #[test]
    fn slugify_nfkd_fold() {
        assert_eq!(slugify("Àéñç Őž"), "aenc-oz");
        assert_eq!(slugify("Crème Brûlée"), "creme-brulee");
        assert_eq!(slugify("e\u{301}t\u{e9}"), "ete");
        assert_eq!(slugify("\u{ff21}\u{ff42}\u{ff13}"), "ab3");
        assert_eq!(
            slugify("\u{fb00}\u{fb01}\u{fb02}\u{fb03}\u{fb04}\u{fb05}\u{fb06}"),
            "fffiflffifflstst"
        );
        assert_eq!(
            slugify("x\u{2070}\u{b9}\u{b2}\u{b3}\u{2074}\u{2079} y\u{2080}\u{2089}"),
            "x012349-y09"
        );
        assert_eq!(slugify("ª º"), "a-o");
        assert_eq!(slugify("İstanbul"), "istanbul");
        assert_eq!(slugify("aßbæcødđełfþg"), "a-b-c-d-e-f-g");
    }
}
