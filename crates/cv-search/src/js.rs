//! JavaScript string semantics over UTF-16 code units (audit item E4).
//!
//! Crucible's chunker runs on JS strings, which are sequences of UTF-16 code units: `.length`,
//! `slice`, `charCodeAt`, index arithmetic, default `Array.prototype.sort()` and non-`u`-flag
//! regexes all operate on code units. Byte compatibility therefore means working on `Vec<u16>`
//! wherever the TypeScript indexes, and treating a slice that splits a surrogate pair exactly as
//! JS does: the lone surrogate is kept (it is hashed as itself and serialized as a `\uXXXX`
//! escape, as `JSON.stringify` does since ES2019).

/// A JS string: UTF-16 code units, possibly containing lone surrogates.
pub type JsStr = Vec<u16>;

/// `s` as UTF-16 code units.
#[must_use]
pub fn js(s: &str) -> JsStr {
    s.encode_utf16().collect()
}

/// Lossy conversion back to Rust text (a lone surrogate becomes U+FFFD). Only for places
/// where the value leaves the byte-compatible core (logs, embedding inputs).
#[must_use]
pub fn lossy(s: &[u16]) -> String {
    String::from_utf16_lossy(s)
}

/// JS `WhiteSpace` + `LineTerminator` — the set `\s` and `String.prototype.trim` use.
#[must_use]
pub fn is_ws(c: u16) -> bool {
    matches!(
        c,
        0x09..=0x0D
            | 0x20
            | 0xA0
            | 0x1680
            | 0x2000..=0x200A
            | 0x2028
            | 0x2029
            | 0x202F
            | 0x205F
            | 0x3000
            | 0xFEFF
    )
}

/// JS `LineTerminator` — what `.` does not match and multiline `^`/`$` anchor on.
#[must_use]
pub fn is_term(c: u16) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

/// `String.prototype.trim`.
#[must_use]
pub fn trim(s: &[u16]) -> &[u16] {
    let start = s.iter().position(|&c| !is_ws(c)).unwrap_or(s.len());
    let end = s.iter().rposition(|&c| !is_ws(c)).map_or(start, |i| i + 1);
    &s[start..end.max(start)]
}

/// Length of the run of `pred` code units starting at `from`.
#[must_use]
pub fn run(s: &[u16], from: usize, pred: impl Fn(u16) -> bool) -> usize {
    s.get(from..)
        .map_or(0, |rest| rest.iter().take_while(|&&c| pred(c)).count())
}

/// `s.startsWith(prefix)`.
#[must_use]
pub fn starts_with(s: &[u16], prefix: &str) -> bool {
    let p = js(prefix);
    s.len() >= p.len() && s[..p.len()] == p[..]
}

/// `s.split(/\r?\n/)`.
#[must_use]
pub fn split_lines(s: &[u16]) -> Vec<&[u16]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, &c) in s.iter().enumerate() {
        if c == u16::from(b'\n') {
            let end = if i > start && s[i - 1] == u16::from(b'\r') {
                i - 1
            } else {
                i
            };
            out.push(&s[start..end]);
            start = i + 1;
        }
    }
    out.push(&s[start..]);
    out
}

/// `s.split(sep)` where `sep` matches maximal runs of code units satisfying `pred` that are at
/// least `min` long (`/\n{2,}/` → `min = 2`, `/[,\s]+/` → `min = 1`).
#[must_use]
pub fn split_runs(s: &[u16], min: usize, pred: impl Fn(u16) -> bool) -> Vec<&[u16]> {
    let mut out = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < s.len() {
        let n = run(s, i, &pred);
        if n >= min {
            out.push(&s[start..i]);
            i += n;
            start = i;
        } else {
            i += n.max(1);
        }
    }
    out.push(&s[start..]);
    out
}

/// `s.split(ch)` for a single code unit.
#[must_use]
pub fn split_char(s: &[u16], ch: u16) -> Vec<&[u16]> {
    s.split(|&c| c == ch).collect()
}

/// `String.prototype.toLowerCase`: full Unicode lowercasing (including the context-dependent
/// final sigma, which `str::to_lowercase` also implements) over the valid parts; lone
/// surrogates pass through unchanged, as in JS.
#[must_use]
pub fn to_lower(s: &[u16]) -> JsStr {
    let mut out = Vec::with_capacity(s.len());
    let mut valid = String::new();
    for r in char::decode_utf16(s.iter().copied()) {
        match r {
            Ok(c) => valid.push(c),
            Err(e) => {
                out.extend(valid.to_lowercase().encode_utf16());
                valid.clear();
                out.push(e.unpaired_surrogate());
            }
        }
    }
    out.extend(valid.to_lowercase().encode_utf16());
    out
}

/// `JSON.stringify(s)` (well-formed variant): escapes `"`, `\`, control characters and lone
/// surrogates; everything else is emitted verbatim.
pub fn write_json_string(out: &mut String, s: &[u16]) {
    out.push('"');
    for r in char::decode_utf16(s.iter().copied()) {
        match r {
            Ok('"') => out.push_str("\\\""),
            Ok('\\') => out.push_str("\\\\"),
            Ok('\u{08}') => out.push_str("\\b"),
            Ok('\u{0C}') => out.push_str("\\f"),
            Ok('\n') => out.push_str("\\n"),
            Ok('\r') => out.push_str("\\r"),
            Ok('\t') => out.push_str("\\t"),
            Ok(c) if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            Ok(c) => out.push(c),
            Err(e) => out.push_str(&format!("\\u{:04x}", e.unpaired_surrogate())),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trim_and_ws_cover_js_set() {
        assert_eq!(trim(&js("\u{FEFF}\u{3000} a b\u{2029}\t")), &js("a b")[..]);
        assert!(trim(&js(" \n ")).is_empty());
        assert!(!is_ws(0x200B));
    }

    #[test]
    fn split_lines_matches_crlf_regex() {
        let s = js("a\r\nb\rc\n\nd");
        let parts: Vec<String> = split_lines(&s).iter().map(|p| lossy(p)).collect();
        assert_eq!(parts, ["a", "b\rc", "", "d"]);
    }

    #[test]
    fn split_runs_matches_js_split() {
        let s = js("a\n\n\nb\nc\n\nd");
        let parts: Vec<String> = split_runs(&s, 2, |c| c == 10)
            .iter()
            .map(|p| lossy(p))
            .collect();
        assert_eq!(parts, ["a", "b\nc", "d"]);
        let t = js(",a, b,,");
        let parts: Vec<String> = split_runs(&t, 1, |c| c == 44 || is_ws(c))
            .iter()
            .map(|p| lossy(p))
            .collect();
        assert_eq!(parts, ["", "a", "b", ""]);
    }

    #[test]
    fn lone_surrogates_survive_lower_and_json() {
        let s = vec![0x41, 0xD83D, 0x03A3];
        assert_eq!(to_lower(&s), vec![0x61, 0xD83D, 0x03C3]);
        let mut out = String::new();
        write_json_string(&mut out, &[0x22, 0xD835, 0x0A, 0x01]);
        assert_eq!(out, "\"\\\"\\ud835\\n\\u0001\"");
    }
}
