//! Plan editor-feedback-round-2 WP-9: image-aware Ctrl+V for the source editor.
//!
//! iced 0.14's clipboard API is text-only, so non-text targets are read through the `xclip`
//! system binary (X11; plan "Dependency posture"). The decision is a pure function over a
//! [`ClipboardReader`] so tests feed canned `TARGETS`/bytes and never spawn `xclip`.

use std::process::{Command, Stdio};

use crate::daemon::DaemonClient;

/// Reads one target of the CLIPBOARD selection as raw bytes.
pub trait ClipboardReader {
    /// `Err` carries a human-readable reason (binary missing, non-zero exit, …).
    fn read(&self, target: &str) -> Result<Vec<u8>, String>;
}

/// The production reader: `xclip -selection clipboard -t <target> -o`.
pub struct Xclip;

impl ClipboardReader for Xclip {
    fn read(&self, target: &str) -> Result<Vec<u8>, String> {
        let out = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", target, "-o"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("xclip unavailable: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!("xclip -t {target} failed ({})", out.status))
        }
    }
}

/// What a Ctrl+V should do, before any daemon call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Image bytes to store as an attachment with extension `ext`.
    Image { ext: String, bytes: Vec<u8> },
    /// Literal text to insert (an `![](url)` remote embed).
    Insert(String),
    /// Fall back to the ordinary text paste; `notice` explains a degraded path.
    Text { notice: Option<String> },
}

/// What the UI applies once the paste has been resolved (after any daemon write).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteOutcome {
    /// Insert this text at the cursor.
    Insert(String),
    /// Ordinary text paste, with an optional status notice.
    Text { notice: Option<String> },
    /// The attachment write failed; nothing is inserted.
    Failed(String),
}

/// Parses `xclip -t TARGETS -o` output (one atom per line).
pub fn parse_targets(raw: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(raw)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// File extension for an `image/<subtype>` MIME type.
pub fn image_ext(mime: &str) -> Option<String> {
    let sub = mime.strip_prefix("image/")?.to_ascii_lowercase();
    let ext = match sub.as_str() {
        "jpeg" | "pjpeg" => "jpg".to_owned(),
        "x-icon" | "vnd.microsoft.icon" => "ico".to_owned(),
        "svg+xml" => "svg".to_owned(),
        s => s.strip_prefix("x-").unwrap_or(s).to_owned(),
    };
    let ok = (1..=8).contains(&ext.len())
        && ext
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    ok.then_some(ext)
}

/// The first `<img src="http…">` in an HTML fragment.
pub fn html_img_src(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<img") {
        let tag_start = from + i;
        let tag_end = lower[tag_start..]
            .find('>')
            .map_or(lower.len(), |e| tag_start + e);
        let tag = &lower[tag_start..tag_end];
        if let Some(s) = tag.find("src=") {
            let v_start = tag_start + s + 4;
            let rest = &html[v_start..tag_end];
            let val = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or(""),
                _ => rest.split_whitespace().next().unwrap_or(""),
            };
            let val = val.trim();
            let l = val.to_ascii_lowercase();
            if l.starts_with("http://") || l.starts_with("https://") {
                return Some(val.to_owned());
            }
        }
        from = tag_end;
    }
    None
}

/// The paste decision: image bytes (PNG preferred, else the first `image/*`), else an
/// HTML-only remote image as `![](src)`, else the ordinary text paste. Any `xclip` failure
/// degrades to text paste with a notice.
pub fn decide(reader: &dyn ClipboardReader) -> Decision {
    let targets = match reader.read("TARGETS") {
        Ok(raw) => parse_targets(&raw),
        Err(e) => {
            return Decision::Text {
                notice: Some(format!("image paste unavailable ({e}); pasted text")),
            };
        }
    };
    let has = |t: &str| targets.iter().any(|x| x == t);
    let image = if has("image/png") {
        Some("image/png".to_owned())
    } else {
        targets
            .iter()
            .find(|t| t.starts_with("image/") && image_ext(t).is_some())
            .cloned()
    };
    if let Some(mime) = image {
        let ext = image_ext(&mime).unwrap_or_else(|| "png".into());
        return match reader.read(&mime) {
            Ok(bytes) if !bytes.is_empty() => Decision::Image { ext, bytes },
            Ok(_) => Decision::Text {
                notice: Some(format!("clipboard {mime} was empty; pasted text")),
            },
            Err(e) => Decision::Text {
                notice: Some(format!("image paste failed ({e}); pasted text")),
            },
        };
    }
    let has_text = has("text/plain") || has("UTF8_STRING") || has("text/plain;charset=utf-8");
    if has("text/html") && !has_text {
        match reader.read("text/html") {
            Ok(raw) => {
                if let Some(src) = html_img_src(&String::from_utf8_lossy(&raw)) {
                    return Decision::Insert(format!("![]({src})"));
                }
            }
            Err(e) => {
                return Decision::Text {
                    notice: Some(format!("clipboard html read failed ({e}); pasted text")),
                };
            }
        }
    }
    Decision::Text { notice: None }
}

/// Runs [`decide`] and, for an image, stores it through the daemon as an attachment of
/// `note_path`, yielding `![[<basename>]]`. Blocking: call off the UI thread.
pub fn resolve(
    reader: &dyn ClipboardReader,
    daemon: Option<&dyn DaemonClient>,
    note_path: &str,
) -> PasteOutcome {
    match decide(reader) {
        Decision::Insert(s) => PasteOutcome::Insert(s),
        Decision::Text { notice } => PasteOutcome::Text { notice },
        Decision::Image { ext, bytes } => {
            let Some(d) = daemon else {
                return PasteOutcome::Failed("image paste: no daemon connection".into());
            };
            match d.write_attachment(note_path, &ext, &bytes) {
                Ok(path) => {
                    let name = path.rsplit('/').next().unwrap_or(&path);
                    PasteOutcome::Insert(format!("![[{name}]]"))
                }
                Err(e) => PasteOutcome::Failed(format!("image paste failed: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct Canned(HashMap<&'static str, Result<Vec<u8>, String>>);
    impl ClipboardReader for Canned {
        fn read(&self, target: &str) -> Result<Vec<u8>, String> {
            self.0
                .get(target)
                .cloned()
                .unwrap_or_else(|| Err(format!("no {target}")))
        }
    }
    fn canned(pairs: &[(&'static str, Result<&[u8], &str>)]) -> Canned {
        Canned(
            pairs
                .iter()
                .map(|(k, v)| (*k, v.map(<[u8]>::to_vec).map_err(str::to_owned)))
                .collect(),
        )
    }

    /// Firefox "Copy Image" TARGETS (runs/spike/firefox-paste/FINDINGS.md).
    const FIREFOX: &[u8] = b"TIMESTAMP\nTARGETS\nMULTIPLE\nSAVE_TARGETS\nimage/png\nimage/jpeg\nimage/bmp\nimage/x-bmp\nimage/x-MS-bmp\nimage/x-icon\nimage/x-ico\nimage/x-win-bitmap\nimage/vnd.microsoft.icon\napplication/ico\nimage/ico\nimage/icon\ntext/ico\nimage/tiff\nimage/webp\naudio/x-riff\n";

    #[test]
    fn firefox_targets_pick_png() {
        let r = canned(&[("TARGETS", Ok(FIREFOX)), ("image/png", Ok(b"PNGDATA"))]);
        assert_eq!(
            decide(&r),
            Decision::Image {
                ext: "png".into(),
                bytes: b"PNGDATA".to_vec()
            }
        );
    }

    #[test]
    fn non_png_image_uses_subtype_ext() {
        let r = canned(&[
            ("TARGETS", Ok(b"TARGETS\nimage/jpeg\n")),
            ("image/jpeg", Ok(b"J")),
        ]);
        assert!(matches!(decide(&r), Decision::Image { ext, .. } if ext == "jpg"));
        assert_eq!(image_ext("image/x-icon").as_deref(), Some("ico"));
        assert_eq!(
            image_ext("image/vnd.microsoft.icon").as_deref(),
            Some("ico")
        );
        assert_eq!(image_ext("image/x-bmp").as_deref(), Some("bmp"));
        assert_eq!(image_ext("image/webp").as_deref(), Some("webp"));
    }

    #[test]
    fn html_only_remote_image_inserts_markdown_embed() {
        let html: &[u8] =
            b"<meta charset=\"utf-8\"><img src=\"https://ex.com/a.png?x=1\" alt=\"\">";
        let r = canned(&[
            ("TARGETS", Ok(b"TARGETS\ntext/html\n")),
            ("text/html", Ok(html)),
        ]);
        assert_eq!(
            decide(&r),
            Decision::Insert("![](https://ex.com/a.png?x=1)".into())
        );
    }

    #[test]
    fn plain_text_present_means_text_paste() {
        let r = canned(&[
            (
                "TARGETS",
                Ok(b"TARGETS\ntext/html\nUTF8_STRING\ntext/plain\n"),
            ),
            ("text/html", Ok(b"<img src=\"https://ex.com/a.png\">")),
        ]);
        assert_eq!(decide(&r), Decision::Text { notice: None });
    }

    #[test]
    fn xclip_failure_falls_back_to_text_with_notice() {
        let r = canned(&[("TARGETS", Err("xclip unavailable: not found"))]);
        assert!(matches!(decide(&r), Decision::Text { notice: Some(n) } if n.contains("xclip")));
        let r = canned(&[
            ("TARGETS", Ok(b"image/png\n")),
            ("image/png", Err("xclip -t image/png failed")),
        ]);
        assert!(matches!(decide(&r), Decision::Text { notice: Some(_) }));
    }

    #[test]
    fn html_img_src_variants() {
        assert_eq!(
            html_img_src("<IMG alt='x' SRC='http://h/p.jpg'>").as_deref(),
            Some("http://h/p.jpg")
        );
        assert_eq!(html_img_src("<img src=\"data:image/png;base64,AA\">"), None);
        assert_eq!(html_img_src("<p>no image</p>"), None);
    }
}
