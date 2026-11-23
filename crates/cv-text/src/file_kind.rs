//! Plan round-14 WP-3: what kind of file a vault path is, for opening it (one classifier for the
//! editor's tab router and the Reading embed lowering).
//!
//! By extension first: markdown and known text extensions are [`FileKind::Text`]; the image
//! formats the editor decodes are [`FileKind::Image`]; `pdf` is [`FileKind::Pdf`] (plan round-15
//! WP-3); the image formats it cannot decode
//! (`svg`, `avif`) and known binary/media/archive/office formats are [`FileKind::External`]. An
//! extensionless path is `Text` (a note link target); an unknown extension is `Text` unless
//! [`sniff`] finds a NUL byte or invalid UTF-8 in its first [`SNIFF_LEN`] bytes.

/// What opening a file does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Opens in a text tab.
    Text,
    /// Opens in an image tab (decodable by the editor).
    Image,
    /// Plan round-15 WP-3: opens in a PDF tab (pages rendered by the editor).
    Pdf,
    /// Opens with the system opener; never loaded into a text buffer.
    External,
}

/// Known text extensions (beyond the extensionless default).
pub const TEXT_EXTS: &[&str] = &[
    "md", "markdown", "txt", "text", "base", "canvas", "json", "jsonl", "toml", "yaml", "yml",
    "csv", "tsv", "css", "js", "mjs", "ts", "html", "htm", "xml", "sh", "bash", "py", "rs", "ini",
    "conf", "cfg", "log", "org", "rst", "tex", "bib", "sql", "lua",
];

/// Extensions the editor decodes into an image tab (the `image` codecs it links).
pub const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

/// Extensions always handed to the system opener.
pub const EXTERNAL_EXTS: &[&str] = &[
    // Documents the editor cannot render.
    "epub", "djvu", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp",
    // Images it does not decode.
    "svg", "avif", "heic", "heif", "tif", "tiff", "ico", "psd", "xcf", "raw", "cr2", "nef", "pgm",
    "ppm", "pbm", "pnm", "pam", "tga", "qoi", "exr", "hdr", "dds", "jxl",
    // Audio / video.
    "mp3", "m4a", "wav", "flac", "ogg", "oga", "opus", "aac", "webm", "mp4", "m4v", "mkv", "mov",
    "avi", "3gp", // Archives / binaries / fonts / databases.
    "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "jar", "exe", "dll", "so", "bin",
    "iso", "dmg", "apk", "deb", "rpm", "sqlite", "db", "woff", "woff2", "ttf", "otf",
];

/// How many leading bytes [`sniff`] inspects.
pub const SNIFF_LEN: usize = 8192;

/// The lower-cased extension of `path`'s file name (`None` when it has none).
fn ext(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// The kind by extension alone; `None` = unknown extension (decide with [`sniff`]).
pub fn by_extension(path: &str) -> Option<FileKind> {
    let Some(e) = ext(path) else {
        return Some(FileKind::Text);
    };
    let e = e.as_str();
    if TEXT_EXTS.contains(&e) {
        Some(FileKind::Text)
    } else if IMAGE_EXTS.contains(&e) {
        Some(FileKind::Image)
    } else if e == "pdf" {
        Some(FileKind::Pdf)
    } else if EXTERNAL_EXTS.contains(&e) {
        Some(FileKind::External)
    } else {
        None
    }
}

/// The unknown-extension fallback: `External` when the first [`SNIFF_LEN`] bytes of `head`
/// hold a NUL byte or are not UTF-8 (a multi-byte character cut off at the window end is
/// fine), else `Text`. Binary formats with long NUL-free runs (a white-topped `P5` PGM is all
/// `0xFF`) fail the UTF-8 check; loading one into a text buffer pins the UI thread.
pub fn sniff(head: &[u8]) -> FileKind {
    let window = &head[..head.len().min(SNIFF_LEN)];
    let not_utf8 = std::str::from_utf8(window).is_err_and(|e| e.error_len().is_some());
    if not_utf8 || window.contains(&0) {
        FileKind::External
    } else {
        FileKind::Text
    }
}

/// The kind of `path`, calling `read` (its bytes, `None` = unreadable) only for an unknown
/// extension.
pub fn classify(path: &str, read: impl FnOnce() -> Option<Vec<u8>>) -> FileKind {
    by_extension(path).unwrap_or_else(|| read().map_or(FileKind::Text, |b| sniff(&b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_table() {
        for (p, k) in [
            ("a.md", Some(FileKind::Text)),
            ("Notes/A Note", Some(FileKind::Text)),
            ("x/notes.TXT", Some(FileKind::Text)),
            ("v/x.base", Some(FileKind::Text)),
            ("att/p.PNG", Some(FileKind::Image)),
            ("att/p.jpeg", Some(FileKind::Image)),
            ("att/p.webp", Some(FileKind::Image)),
            ("doc.pdf", Some(FileKind::Pdf)),
            ("x/Doc.PDF", Some(FileKind::Pdf)),
            ("doc.docx", Some(FileKind::External)),
            ("i.svg", Some(FileKind::External)),
            ("i.avif", Some(FileKind::External)),
            ("song.mp3", Some(FileKind::External)),
            (".hidden", Some(FileKind::Text)),
            ("dir.v2/blob.qqq", None),
        ] {
            assert_eq!(by_extension(p), k, "{p}");
        }
    }

    #[test]
    fn nul_sniff_decides_unknown_extensions() {
        assert_eq!(
            classify("a.qqq", || Some(b"plain\ntext".to_vec())),
            FileKind::Text
        );
        assert_eq!(
            classify("a.qqq", || Some(vec![1, 0, 2])),
            FileKind::External
        );
        assert_eq!(classify("a.qqq", || None), FileKind::Text);
        // A NUL past the window is not looked at; a known extension never reads.
        let mut late = vec![b'a'; SNIFF_LEN];
        late.push(0);
        assert_eq!(sniff(&late), FileKind::Text);
        // NUL-free binary (a white-topped P5 PGM) is not UTF-8; a cut-off char at the window end is.
        let mut pgm = b"P5\n1072 1448\n255\n".to_vec();
        pgm.resize(SNIFF_LEN * 2, 0xFF);
        assert_eq!(sniff(&pgm), FileKind::External);
        let mut cut = vec![b'a'; SNIFF_LEN - 1];
        cut.extend_from_slice("é".as_bytes());
        assert_eq!(sniff(&cut), FileKind::Text);
        assert_eq!(sniff("naïve ✓".as_bytes()), FileKind::Text);
        assert_eq!(by_extension("shot.pgm"), Some(FileKind::External));
        assert_eq!(classify("a.png", || panic!("no read")), FileKind::Image);
    }
}
