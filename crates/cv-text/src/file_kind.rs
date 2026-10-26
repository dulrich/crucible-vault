//! Plan round-14 WP-3: what kind of file a vault path is, for opening it (one classifier for the
//! editor's tab router and the Reading embed lowering).
//!
//! By extension first: markdown and known text extensions are [`FileKind::Text`]; the image
//! formats the editor decodes are [`FileKind::Image`]; PDFs, the image formats it cannot decode
//! (`svg`, `avif`) and known binary/media/archive/office formats are [`FileKind::External`]. An
//! extensionless path is `Text` (a note link target); an unknown extension is `Text` unless
//! [`sniff`] finds a NUL byte in its first [`SNIFF_LEN`] bytes.

/// What opening a file does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Opens in a text tab.
    Text,
    /// Opens in an image tab (decodable by the editor).
    Image,
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
    "pdf", "epub", "djvu", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx", "odp",
    // Images it does not decode.
    "svg", "avif", "heic", "heif", "tif", "tiff", "ico", "psd", "xcf", "raw", "cr2", "nef",
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
    } else if EXTERNAL_EXTS.contains(&e) {
        Some(FileKind::External)
    } else {
        None
    }
}

/// The unknown-extension fallback: `External` when a NUL byte occurs in the first
/// [`SNIFF_LEN`] bytes of `head`, else `Text`.
pub fn sniff(head: &[u8]) -> FileKind {
    if head.iter().take(SNIFF_LEN).any(|&b| b == 0) {
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
            ("doc.pdf", Some(FileKind::External)),
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
        assert_eq!(classify("a.png", || panic!("no read")), FileKind::Image);
    }
}
