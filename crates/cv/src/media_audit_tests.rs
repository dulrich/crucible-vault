//! `cv media audit` over a synthetic vault and state.db (plan remote-media-sprint WP-4).
use super::*;
use cv_state::{ImageDescription, QueueConfig, StateDb};

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "cccccccccccccccccccccccccccccccc";
const D: &str = "dddddddddddddddddddddddddddddddd";
const E: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

fn write(root: &Path, rel: &str, body: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn rec(md5: &str, kind: &str, provider: Option<&str>) -> ImageDescription {
    let mut raw = serde_json::json!({"md5": md5, "kind": kind, "failure": "decode failed"});
    if let Some(p) = provider {
        raw["providerId"] = serde_json::json!(p);
    }
    ImageDescription {
        md5: md5.into(),
        kind: kind.into(),
        narrative: String::new(),
        extraction: String::new(),
        description_hash: "0".into(),
        described_at: "2026-10-29T00:00:00Z".into(),
        raw: raw.to_string(),
    }
}

const NOTE: &str = "SECRET-TITLE\n\
![](https://x.test/a.png)\n\
<img src=\"https://x.test/b.jpg\">\n\
![](https://www.youtube.com/watch?v=dQw4w9WgXcQ)\n\
<iframe src=\"https://player.example/x\"></iframe>\n\
<video src=\"https://x.test/v.mp4\"></video>\n\
![](https://x.test/s.mp3)\n\
<audio src=\"https://x.test/t.ogg\"></audio>\n\
![](https://x.test/d.pdf)\n\
![[local.png]]\n\
![[gone.png]]\n\
```\n![](https://x.test/fenced.png)\n![[fenced.png]]\n```\n";

struct Fixture {
    dir: tempfile::TempDir,
    state_db: PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let v = dir.path().join("vault");
    write(&v, "Clippings/n.md", NOTE.as_bytes());
    write(&v, "root.md", b"![](https://x.test/r.png)\n");
    write(&v, "pics/local.png", b"x");
    write(&v, "_custom/c.md", b"![](https://x.test/custom.png)\n");
    write(&v, ".trash/t.md", b"![](https://x.test/trash.png)\n");
    for (rel, md5) in [
        ("att/", A),
        ("att/", B),
        ("att/", C),
        ("Clippings/", D),
        ("_custom/", E),
    ] {
        write(&v, &format!("{rel}{md5}_MD5.png"), b"x");
    }
    let state_db = dir.path().join("state.db");
    let mut st = StateDb::open(&state_db, 1, QueueConfig::default()).unwrap();
    st.put_image_description(&rec(A, "vision", Some("p")))
        .unwrap();
    // B: a cv-era permanent failure (reported exception); C: a Crucible-era failure (retried).
    st.put_image_description(&rec(B, "failed", Some("p")))
        .unwrap();
    st.put_image_description(&rec(C, "failed", None)).unwrap();
    drop(st);
    Fixture { dir, state_db }
}

fn args(f: &Fixture, extra: &[&str]) -> Vec<String> {
    let mut a = vec![
        "audit".to_owned(),
        "--vault".into(),
        f.dir.path().join("vault").to_string_lossy().into_owned(),
        "--state-db".into(),
        f.state_db.to_string_lossy().into_owned(),
    ];
    a.extend(extra.iter().map(|s| (*s).to_owned()));
    a
}

fn report(f: &Fixture, modes: Modes) -> Report {
    let st = StateReader::open(&f.state_db).unwrap();
    audit(&f.dir.path().join("vault"), Some(&st), modes).unwrap()
}

const LINK: Modes = Modes {
    audio_forbidden: false,
    pdf_forbidden: false,
};

#[test]
fn counts_each_kind_and_excludes_fences_custom_and_dot_dirs() {
    let f = fixture();
    let r = report(&f, LINK);
    assert_eq!(r.notes, 2);
    // markdown a.png + <img> b.jpg in Clippings; r.png at the root.
    assert_eq!(r.remote_images.total, 3);
    assert_eq!(r.remote_images.by_folder["Clippings"], 2);
    assert_eq!(r.remote_images.by_folder["(root)"], 1);
    // youtube page + <iframe> + <video>.
    assert_eq!(r.video.total, 3);
    assert_eq!((r.audio.total, r.pdf.total), (2, 1));
    // ![[local.png]] only; the fenced ref is code; ![[gone.png]] has no file.
    assert_eq!(r.unmanaged.total, 1);
    assert_eq!(r.missing.total, 1);
    assert_eq!(r.missing.by_folder["Clippings"], 1);
    // _MD5 files outside _custom: A, B, C, D.
    assert_eq!(r.managed_images, 4);
    let c = r.coverage.unwrap();
    assert_eq!(c.by_kind["vision"], 1);
    assert_eq!(c.by_kind["failed"], 2);
    // B is the reported exception; C (no provenance) and D (no record) are undescribed.
    assert_eq!(c.exception.len(), 1);
    assert_eq!(c.exception[0].0, B);
    assert_eq!(c.undescribed.total, 2);
    assert_eq!(c.undescribed.by_folder["att"], 1);
    assert_eq!(c.undescribed.by_folder["Clippings"], 1);
}

#[test]
fn strict_exit_codes_and_mode_gated_audio_pdf() {
    let f = fixture();
    assert_eq!(run(&args(&f, &[])), 0);
    assert_eq!(run(&args(&f, &["--strict"])), 1);
    // A clean vault passes strict; audio/pdf count only when their mode forbids them.
    let dir = tempfile::tempdir().unwrap();
    let v = dir.path().join("v");
    write(
        &v,
        "a.md",
        b"![](https://x.test/s.mp3)\n![](https://x.test/d.pdf)\n![[gone.png]]\n",
    );
    write(&v, "att/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa_MD5.png", b"x");
    let db = dir.path().join("s.db");
    let mut st = StateDb::open(&db, 1, QueueConfig::default()).unwrap();
    st.put_image_description(&rec(A, "vision", Some("p")))
        .unwrap();
    drop(st);
    let st = StateReader::open(&db).unwrap();
    let r = audit(&v, Some(&st), LINK).unwrap();
    assert!(r.acceptance_ok());
    let r = audit(
        &v,
        Some(&st),
        Modes {
            audio_forbidden: true,
            pdf_forbidden: false,
        },
    )
    .unwrap();
    assert!(!r.acceptance_ok());
    // Coverage unavailable cannot be proved: strict fails.
    let r = audit(&v, None, LINK).unwrap();
    assert!(r.coverage.is_none());
    assert!(!r.acceptance_ok());
    assert!(render(&r, false).contains("unavailable"));
}

#[test]
fn output_carries_no_note_text_or_paths_and_verbose_lists_md5() {
    let f = fixture();
    let st = StateReader::open(&f.state_db).unwrap();
    let r = audit(&f.dir.path().join("vault"), Some(&st), LINK).unwrap();
    let plain = render(&r, false);
    let verbose = render(&r, true);
    for text in [&plain, &verbose] {
        for banned in [
            "SECRET-TITLE",
            "n.md",
            "x.test",
            "local.png",
            "gone.png",
            "Clippings/",
            ".md",
        ] {
            assert!(!text.contains(banned), "{banned} leaked:\n{text}");
        }
    }
    assert!(!plain.contains(B));
    assert!(verbose.contains(&format!("{B}: decode failed")));
    assert!(plain.contains("reported exception (describe failed permanently): 1"));
}

#[test]
fn failure_text_is_truncated_to_80_chars() {
    let dir = tempfile::tempdir().unwrap();
    let v = dir.path().join("v");
    write(&v, &format!("att/{A}_MD5.png"), b"x");
    let db = dir.path().join("s.db");
    let mut st = StateDb::open(&db, 1, QueueConfig::default()).unwrap();
    let mut d = rec(A, "failed", Some("p"));
    d.raw = serde_json::json!({"providerId": "p", "failure": "x".repeat(200)}).to_string();
    st.put_image_description(&d).unwrap();
    drop(st);
    let st = StateReader::open(&db).unwrap();
    let r = audit(&v, Some(&st), LINK).unwrap();
    assert_eq!(r.coverage.unwrap().exception[0].1.chars().count(), 80);
}

#[test]
fn help_and_bad_args() {
    assert_eq!(run(&["--help".to_owned()]), 0);
    assert_eq!(run(&["audit".to_owned(), "--help".to_owned()]), 0);
    assert_eq!(run(&["audit".to_owned(), "--bogus".to_owned()]), 2);
    assert_eq!(run(&["nope".to_owned()]), 2);
}

#[test]
fn absent_state_db_is_reported_not_an_error() {
    let f = fixture();
    let mut a = args(&f, &[]);
    let at = a.iter().position(|x| x == "--state-db").unwrap() + 1;
    a[at] = f
        .dir
        .path()
        .join("missing.db")
        .to_string_lossy()
        .into_owned();
    assert_eq!(run(&a), 0);
}

/// The example `vault.toml` (and so migration 0023's block, byte-identical to it) parses and
/// states the defaults.
#[test]
fn example_vault_toml_parses_to_the_defaults() {
    let src = include_str!("../../../config/examples/vault.toml");
    let c = VaultConfig::from_table(&src.parse::<toml::Table>().unwrap()).unwrap();
    assert_eq!(
        c.remote_media,
        cv_actions::vault::RemoteMediaConfig::default()
    );
}

/// A legacy `length` failure (even with `providerId`) is retried by the describe step, so it is
/// undescribed, not the reported exception.
#[test]
fn legacy_length_failure_is_undescribed_not_exception() {
    let dir = tempfile::tempdir().unwrap();
    let v = dir.path().join("v");
    write(&v, &format!("att/{A}_MD5.png"), b"x");
    let db = dir.path().join("s.db");
    let mut st = StateDb::open(&db, 1, QueueConfig::default()).unwrap();
    let mut d = rec(A, "failed", Some("p"));
    d.raw = serde_json::json!({
        "providerId": "p",
        "failure": "extraction: finish reason \"length\"; output discarded"
    })
    .to_string();
    st.put_image_description(&d).unwrap();
    drop(st);
    let st = StateReader::open(&db).unwrap();
    let c = audit(&v, Some(&st), LINK).unwrap().coverage.unwrap();
    assert_eq!((c.exception.len(), c.undescribed.total), (0, 1));
}
