//! Plan editor-feedback-round-2 WP-9: image-aware Ctrl+V for the source editor.
//!
//! iced 0.14's clipboard API is text-only, so non-text targets go through a system binary:
//! `wl-paste`/`wl-copy` (wl-clipboard) in a Wayland session, else `xclip` (X11; plan
//! "Dependency posture"). The session rule — `WAYLAND_DISPLAY` set means Wayland — is the
//! daemon's `clipboard_cmd` rule (`crates/cv/src/config.rs`, D-record premise correction).
//! The decision is a pure function over a [`ClipboardReader`] so tests feed canned
//! `TARGETS`/bytes and never spawn a binary.

use std::process::{Command, Stdio};

use crate::daemon::DaemonClient;

/// Reads one target of the CLIPBOARD selection as raw bytes. The pseudo-target `TARGETS`
/// yields the offered MIME types/atoms, one per line.
pub trait ClipboardReader {
    /// `Err` carries a human-readable reason (binary missing, non-zero exit, …).
    fn read(&self, target: &str) -> Result<Vec<u8>, String>;
}

/// The production clipboard binary, chosen per session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemClipboard {
    /// X11: `xclip -selection clipboard`.
    Xclip,
    /// Wayland: `wl-paste` / `wl-copy`.
    WlClipboard,
}

impl SystemClipboard {
    /// The session's clipboard binary: Wayland when `WAYLAND_DISPLAY` is set and non-empty.
    pub fn detect() -> Self {
        Self::for_wayland_display(std::env::var_os("WAYLAND_DISPLAY").as_deref())
    }

    /// [`Self::detect`] over an explicit `WAYLAND_DISPLAY` value (testable).
    pub fn for_wayland_display(v: Option<&std::ffi::OsStr>) -> Self {
        match v {
            Some(v) if !v.is_empty() => Self::WlClipboard,
            _ => Self::Xclip,
        }
    }

    fn bin(self, write: bool) -> &'static str {
        match (self, write) {
            (Self::Xclip, _) => "xclip",
            (Self::WlClipboard, false) => "wl-paste",
            (Self::WlClipboard, true) => "wl-copy",
        }
    }

    /// Read argv: `TARGETS` lists the offered types, any other target reads its bytes.
    fn read_args(self, target: &str) -> Vec<&str> {
        match (self, target) {
            (Self::Xclip, t) => vec!["-selection", "clipboard", "-t", t, "-o"],
            (Self::WlClipboard, "TARGETS") => vec!["--list-types"],
            (Self::WlClipboard, t) => vec!["--no-newline", "-t", t],
        }
    }

    fn write_args(self, mime: &str) -> Vec<&str> {
        match self {
            Self::Xclip => vec!["-selection", "clipboard", "-t", mime, "-i"],
            Self::WlClipboard => vec!["-t", mime],
        }
    }
}

impl ClipboardReader for SystemClipboard {
    fn read(&self, target: &str) -> Result<Vec<u8>, String> {
        let bin = self.bin(false);
        let out = Command::new(bin)
            .args(self.read_args(target))
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|e| format!("{bin} unavailable: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(format!("{bin} -t {target} failed ({})", out.status))
        }
    }
}

/// Plan editor-round-4 WP-4: writes bytes to the CLIPBOARD selection under one target.
pub trait ClipboardWriter {
    /// `Err` carries a human-readable reason (binary missing, non-zero exit, …).
    fn write(&self, mime: &str, bytes: &[u8]) -> Result<(), String>;
}

/// The production writer: `xclip -selection clipboard -t <mime> -i` or `wl-copy -t <mime>`.
///
/// Lifecycle (audit E4): stdin piped, all bytes written, stdin explicitly dropped (EOF), then
/// the child is `wait()`ed (reaped). Both `xclip -i` and `wl-copy` fork a selection owner and
/// the parent exits promptly (wl-copy measured 2026-11-07), so the wait does not block on the
/// selection's lifetime. Blocking: call off the UI thread.
impl ClipboardWriter for SystemClipboard {
    fn write(&self, mime: &str, bytes: &[u8]) -> Result<(), String> {
        use std::io::Write;
        let bin = self.bin(true);
        let mut child = Command::new(bin)
            .args(self.write_args(mime))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{bin} unavailable: {e}"))?;
        let written = match child.stdin.take() {
            Some(mut stdin) => {
                let r = stdin.write_all(bytes);
                drop(stdin);
                r.map_err(|e| format!("{bin} write failed: {e}"))
            }
            None => Err(format!("{bin} stdin unavailable")),
        };
        let status = child
            .wait()
            .map_err(|e| format!("{bin} wait failed: {e}"))?;
        written?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{bin} -t {mime} failed ({status})"))
        }
    }
}

/// The MIME type for an image file path, by extension.
pub fn image_mime(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        "avif" => "image/avif",
        _ => return None,
    })
}

/// Copy image: reads the vault file `path` through the daemon and writes its original bytes
/// under its MIME type (no transcoding). `Ok` carries the status line. Blocking.
pub fn copy_image(
    daemon: &dyn DaemonClient,
    writer: &dyn ClipboardWriter,
    path: &str,
) -> Result<String, String> {
    let mime = image_mime(path).unwrap_or("application/octet-stream");
    let (bytes, _) = daemon
        .read_file(path)
        .map_err(|e| format!("copy image: read failed: {e}"))?;
    writer
        .write(mime, &bytes)
        .map_err(|e| format!("copy image failed: {e}"))?;
    Ok(format!("copied image {path}"))
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

/// Parses `xclip -t TARGETS -o` / `wl-paste --list-types` output (one type per line).
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
/// HTML-only remote image as `![](src)`, else the ordinary text paste. Any clipboard-binary failure
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

    /// Firefox "Copy Image" TARGETS (docs/spikes/firefox-paste/FINDINGS.md).
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

    #[test]
    fn session_picks_clipboard_binary() {
        use std::ffi::OsStr;
        let w = SystemClipboard::for_wayland_display(Some(OsStr::new("wayland-1")));
        assert_eq!(w, SystemClipboard::WlClipboard);
        assert_eq!(
            SystemClipboard::for_wayland_display(Some(OsStr::new(""))),
            SystemClipboard::Xclip
        );
        assert_eq!(
            SystemClipboard::for_wayland_display(None),
            SystemClipboard::Xclip
        );
    }

    #[test]
    fn wl_clipboard_argv() {
        let w = SystemClipboard::WlClipboard;
        assert_eq!(
            (w.bin(false), w.read_args("TARGETS")),
            ("wl-paste", vec!["--list-types"])
        );
        assert_eq!(
            w.read_args("image/png"),
            vec!["--no-newline", "-t", "image/png"]
        );
        assert_eq!(
            (w.bin(true), w.write_args("image/png")),
            ("wl-copy", vec!["-t", "image/png"])
        );
        let x = SystemClipboard::Xclip;
        assert_eq!(
            x.read_args("TARGETS"),
            vec!["-selection", "clipboard", "-t", "TARGETS", "-o"]
        );
        assert_eq!(
            x.write_args("image/png"),
            vec!["-selection", "clipboard", "-t", "image/png", "-i"]
        );
    }
}
