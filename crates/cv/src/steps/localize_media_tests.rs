//! `localize` media-mode tests (plan remote-media-sprint WP-3): synthetic notes and a
//! `MockHttp` on 127.0.0.1 standing in for every host, incl. the YouTube thumbnail and Vimeo
//! oEmbed bases (never the network).

use std::collections::HashMap;
use std::sync::Arc;

use cv_testkit::{MockHttp, Reply};
use serde_json::Value;

use super::{Env, StepOutcome, env, ignored, png, rt, var};
use crate::steps::localize::TEST_MEDIA_BASE;
use crate::steps::{DeferCause, StepError, localize};

type Routes = HashMap<String, (String, Vec<u8>)>;

/// `/s<NNN>/…` answers HTTP NNN; a route answers 200 with its type; anything else 404.
fn serve(routes: Routes) -> MockHttp {
    MockHttp::start(move |req| {
        if let Some(code) = req
            .target
            .strip_prefix("/s")
            .and_then(|r| r.get(..3))
            .and_then(|c| c.parse::<u16>().ok())
        {
            return Reply::new(code, Vec::new());
        }
        match routes.get(&req.target) {
            Some((ct, body)) if ct.contains("json") => {
                // `{host}` in a JSON body = this server (the oEmbed thumbnail URL).
                let host = req.header("host").unwrap_or("");
                let body = String::from_utf8_lossy(body).replace("{host}", host);
                Reply::new(200, body.into_bytes()).header("Content-Type", ct)
            }
            Some((ct, body)) => Reply::new(200, body.clone()).header("Content-Type", ct),
            None => Reply::new(404, Vec::new()),
        }
    })
}

/// Runs `f(base, server)` on a blocking thread with the media host bases pointed at `base`.
fn with_media<T: Send + 'static>(
    routes: Vec<(&str, &str, Vec<u8>)>,
    f: impl FnOnce(String, Arc<MockHttp>) -> T + Send + 'static,
) -> T {
    let routes: Routes = routes
        .into_iter()
        .map(|(p, ct, b)| (p.to_owned(), (ct.to_owned(), b)))
        .collect();
    let rt = rt();
    let s = Arc::new(serve(routes));
    let base = format!("http://127.0.0.1:{}", s.addr().port());
    rt.block_on(async move {
        tokio::task::spawn_blocking(move || {
            TEST_MEDIA_BASE.with(|b| *b.borrow_mut() = Some(base.clone()));
            f(base, s)
        })
        .await
        .unwrap()
    })
}

/// One note-scope run of `step` over a note `body` (`{b}` = the server base).
fn run_note(
    routes: Vec<(&str, &str, Vec<u8>)>,
    step: &'static str,
    body: &'static str,
) -> (Env, Result<StepOutcome, StepError>, String, usize) {
    with_media(routes, move |base, s| {
        let note = body.replace("{b}", &base);
        let env = env(&[("n.md", note.as_bytes())]);
        let st = env.step(step);
        let res = env.run(&st, "n.md");
        (env, res, base, s.hits())
    })
}

fn md5(b: &[u8]) -> String {
    crate::steps::util::md5_hex(b)
}

const YT: &str = "dQw4w9WgXcQ";

#[test]
fn image_401_is_delinked_with_its_alt() {
    let (env, res, b, _) = run_note(
        vec![],
        "",
        "x ![My [big] pic](<{b}/s401/a_(1).png> \"t\")\n",
    );
    let out = res.unwrap();
    assert_eq!(var(&out, "delinked"), 1);
    assert_eq!(
        env.text("n.md"),
        format!("x [🖼 My big pic](<{b}/s401/a_(1).png>)\n")
    );
}

#[test]
fn image_delink_without_alt_uses_the_host() {
    let (env, res, b, _) = run_note(
        vec![],
        "",
        "![](http://127.0.0.1:1/x.png)\n![]({b}/s410/a.png)\n",
    );
    assert!(res.is_ok());
    // Port 1: connection refused → transient, unchanged; 410 → permanent.
    assert_eq!(
        env.text("n.md"),
        format!("![](http://127.0.0.1:1/x.png)\n[🖼 127.0.0.1]({b}/s410/a.png)\n")
    );
}

#[test]
fn image_503_is_left_and_defers() {
    let (env, res, b, _) = run_note(vec![], "", "![]({b}/s503/a.png)\n");
    assert_eq!(
        res,
        Err(StepError::Deferred {
            retry_after_ms: crate::steps::TRANSIENT_BASE_MS,
            service: Some(localize::SERVICE.into()),
            cause: DeferCause::Transient(cv_state::ServiceFailureKind::ServerError),
        })
    );
    assert_eq!(env.text("n.md"), format!("![]({b}/s503/a.png)\n"));
}

#[test]
fn a_non_image_response_to_an_image_embed_is_permanent() {
    let (env, res, b, _) = run_note(
        vec![("/a.dat", "application/json", b"{}".to_vec())],
        "",
        "![]({b}/a.dat)\n",
    );
    assert_eq!(var(&res.unwrap(), "delinked"), 1);
    assert_eq!(env.text("n.md"), format!("[🖼 127.0.0.1]({b}/a.dat)\n"));
}

#[test]
fn youtube_thumbnail_hq() {
    let img = png(20);
    let m = md5(&img);
    let hq = format!("/vi/{YT}/hqdefault.jpg");
    let (env, res, _, hits) = run_note(
        vec![(&hq, "image/png", img.clone())],
        "",
        "![](https://youtu.be/dQw4w9WgXcQ)\n",
    );
    let out = res.unwrap();
    assert_eq!(hits, 1);
    let t = format!("_attachments/n/{m}_MD5.png");
    assert_eq!(
        env.text("n.md"),
        format!("[![]({t})](https://www.youtube.com/watch?v={YT})\n")
    );
    assert_eq!(env.file(&t), Some(img));
    assert_eq!((var(&out, "thumbnails"), var(&out, "images")), (1, 1));
}

#[test]
fn youtube_thumbnail_falls_back_to_mq() {
    let img = png(21);
    let mq = format!("/vi/{YT}/mqdefault.jpg");
    let (env, res, _, hits) = run_note(
        vec![(&mq, "image/png", img.clone())],
        "",
        "![](https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=3)\n",
    );
    assert_eq!(var(&res.unwrap(), "thumbnails"), 1);
    assert_eq!(hits, 2);
    assert_eq!(
        env.text("n.md"),
        format!(
            "[![](_attachments/n/{}_MD5.png)](https://www.youtube.com/watch?v={YT})\n",
            md5(&img)
        )
    );
}

#[test]
fn youtube_without_thumbnail_becomes_a_video_card() {
    let (env, res, _, hits) = run_note(
        vec![],
        "",
        "![](https://www.youtube.com/watch?v=dQw4w9WgXcQ)\n",
    );
    assert_eq!(var(&res.unwrap(), "media_links"), 1);
    assert_eq!(hits, 2);
    assert_eq!(
        env.text("n.md"),
        "[▶ video · youtube.com](https://www.youtube.com/watch?v=dQw4w9WgXcQ)\n"
    );
}

#[test]
fn youtube_iframe_gets_the_canonical_watch_url() {
    let img = png(22);
    let hq = format!("/vi/{YT}/hqdefault.jpg");
    let (env, _, _, _) = run_note(
        vec![(&hq, "image/png", img.clone())],
        "",
        "a\n<iframe width=\"560\" src=\"https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?si=x\" allowfullscreen></iframe>\nb\n",
    );
    assert_eq!(
        env.text("n.md"),
        format!(
            "a\n[![](_attachments/n/{}_MD5.png)](https://www.youtube.com/watch?v={YT})\nb\n",
            md5(&img)
        )
    );
}

#[test]
fn vimeo_thumbnail_via_oembed() {
    let img = png(23);
    let (env, res, _, _) = run_note(
        vec![
            (
                "/api/oembed.json?url=https%3A%2F%2Fvimeo.com%2F76979871",
                "application/json",
                br#"{"type":"video","thumbnail_url":"http://{host}/vt/1.jpg"}"#.to_vec(),
            ),
            ("/vt/1.jpg", "image/png", img.clone()),
        ],
        "",
        "![](https://player.vimeo.com/video/76979871)\n",
    );
    assert_eq!(var(&res.unwrap(), "thumbnails"), 1);
    assert_eq!(
        env.text("n.md"),
        format!(
            "[![](_attachments/n/{}_MD5.png)](https://vimeo.com/76979871)\n",
            md5(&img)
        )
    );
}

#[test]
fn video_poster_is_the_thumbnail() {
    let img = png(24);
    let (env, res, _, _) = run_note(
        vec![("/poster.jpg", "image/png", img.clone())],
        "",
        "<video controls poster=\"{b}/poster.jpg\" src=\"https://media.example/v.mp4\"></video>\n",
    );
    assert_eq!(var(&res.unwrap(), "thumbnails"), 1);
    assert_eq!(
        env.text("n.md"),
        format!(
            "[![](_attachments/n/{}_MD5.png)](https://media.example/v.mp4)\n",
            md5(&img)
        )
    );
}

#[test]
fn video_files_without_poster_become_cards() {
    let (env, res, _, hits) = run_note(
        vec![],
        "",
        "<video><source src=\"https://www.media.example/v.webm\" type=\"video/webm\"></video>\n![](https://cdn.example/a%20b.mp4)\n",
    );
    assert_eq!(var(&res.unwrap(), "media_links"), 2);
    assert_eq!(hits, 0);
    assert_eq!(
        env.text("n.md"),
        "[▶ video · media.example](https://www.media.example/v.webm)\n[▶ video · cdn.example](https://cdn.example/a%20b.mp4)\n"
    );
}

#[test]
fn video_link_mode_never_fetches_a_thumbnail() {
    let (env, _, _, hits) = run_note(
        vec![],
        "video = \"link\"",
        "![](https://youtu.be/dQw4w9WgXcQ)\n<video poster=\"{b}/p.jpg\" src=\"https://m.example/v.mp4\"></video>\n",
    );
    assert_eq!(hits, 0);
    assert_eq!(
        env.text("n.md"),
        "[▶ video · youtu.be](https://youtu.be/dQw4w9WgXcQ)\n[▶ video · m.example](https://m.example/v.mp4)\n"
    );
}

#[test]
fn video_bool_is_a_load_error() {
    let e = env(&[("n.md", b"x")]);
    let t: toml::Table = toml::from_str("video = true").unwrap();
    let mut full = t.clone();
    full.insert("type".into(), toml::Value::String("localize".into()));
    let step = cv_actions::Step::from_table(full).unwrap();
    let err = crate::steps::validate(&step, &e.config).unwrap_err();
    assert!(err.contains("video must be"), "{err}");
    assert!(err.contains("a bool is not accepted"), "{err}");
}

#[test]
fn posts_and_other_iframes_become_cards() {
    let (env, res, _, _) = run_note(
        vec![],
        "",
        "![](https://x.com/someone/status/123)\n<iframe src=\"https://twitter.com/i/status/9\"></iframe>\n<iframe src=\"https://www.datawrapper.example/chart/1\" />\n",
    );
    assert_eq!(var(&res.unwrap(), "media_links"), 3);
    assert_eq!(
        env.text("n.md"),
        "[𝕏 post](https://x.com/someone/status/123)\n[𝕏 post](https://twitter.com/i/status/9)\n[⧉ embed · datawrapper.example](https://www.datawrapper.example/chart/1)\n"
    );
}

#[test]
fn audio_link_mode_writes_cards() {
    let (env, _, _, hits) = run_note(
        vec![],
        "",
        "![](https://pod.example/ep.mp3)\n<audio controls src=\"https://pod.example/e (2).ogg\"></audio>\n",
    );
    assert_eq!(hits, 0);
    assert_eq!(
        env.text("n.md"),
        "[♪ audio · pod.example](https://pod.example/ep.mp3)\n[♪ audio · pod.example](<https://pod.example/e (2).ogg>)\n"
    );
}

#[test]
fn audio_localize_mode_downloads_and_falls_back_to_a_card() {
    let (env, _, b, _) = run_note(
        vec![("/ep.mp3", "audio/mpeg", b"ID3 fake".to_vec())],
        "audio = \"localize\"",
        "![]({b}/ep.mp3)\n![]({b}/gone.mp3)\n",
    );
    assert_eq!(
        env.text("n.md"),
        format!(
            "![](_attachments/n/{}_MD5.mp3)\n[♪ audio · 127.0.0.1]({b}/gone.mp3)\n",
            md5(b"ID3 fake")
        )
    );
}

#[test]
fn pdf_embed_card_link_untouched_and_localize() {
    let (env, _, _, hits) = run_note(
        vec![],
        "",
        "![](https://docs.example/a.pdf)\n[read](https://docs.example/a.pdf)\n",
    );
    assert_eq!(hits, 0);
    assert_eq!(
        env.text("n.md"),
        "[⧉ pdf · docs.example](https://docs.example/a.pdf)\n[read](https://docs.example/a.pdf)\n"
    );
    let (env, _, _, _) = run_note(
        vec![("/a.pdf", "application/pdf", b"%PDF-1.4 x".to_vec())],
        "pdf = \"localize\"",
        "![]({b}/a.pdf)\n",
    );
    assert_eq!(
        env.text("n.md"),
        format!("![](_attachments/n/{}_MD5.pdf)\n", md5(b"%PDF-1.4 x"))
    );
}

#[test]
fn html_img_is_localized_or_delinked_with_its_alt() {
    let img = png(25);
    let (env, res, b, _) = run_note(
        vec![("/ok.png", "image/png", img.clone())],
        "",
        "<p><img src=\"{b}/ok.png\" alt=\"x\"></p>\n<img alt=\"A [b]\nc\" src='{b}/s403/n.png' />\n",
    );
    let out = res.unwrap();
    assert_eq!((var(&out, "delinked"), var(&out, "localized")), (1, 2));
    assert_eq!(
        env.text("n.md"),
        format!(
            "<p>![](_attachments/n/{}_MD5.png)</p>\n[🖼 A bc]({b}/s403/n.png)\n",
            md5(&img)
        )
    );
}

#[test]
fn fenced_media_is_untouched() {
    let body = "```\n![](https://youtu.be/dQw4w9WgXcQ)\n<iframe src=\"https://x.example/e\"></iframe>\n![]({b}/s404/a.png)\n```\n";
    let (env, res, b, hits) = run_note(vec![], "", body);
    assert_eq!(res.unwrap().vars.get("changed"), Some(&Value::Bool(false)));
    assert_eq!(hits, 0);
    assert_eq!(env.text("n.md"), body.replace("{b}", &b));
    assert!(!cv_text::remote_media::has_remote_media_refs(
        &body.replace("{b}", &b)
    ));
}

#[test]
fn media_predicate_selects_every_convertible_form() {
    use cv_text::remote_media::has_remote_media_refs as has;
    assert!(has("![](https://youtu.be/dQw4w9WgXcQ)"));
    assert!(has("![](https://pod.example/a.mp3)"));
    assert!(has("<iframe src=\"https://e.example/x\"></iframe>"));
    assert!(has("<img src=\"https://e.example/x.png\">"));
    assert!(!has("[a](https://youtu.be/dQw4w9WgXcQ)"));
    assert!(!has("[▶ video · e.example](https://e.example/v.mp4)"));
    assert!(!has(
        "[![](_a/0123456789abcdef0123456789abcdef_MD5.png)](https://www.youtube.com/watch?v=dQw4w9WgXcQ)"
    ));
}

/// Pin 6 (audit E2): every output form is stable under a second pass, incl. a de-embedded
/// `_MD5`-named remote URL that fails permanently again.
#[test]
fn second_pass_writes_nothing() {
    let img = png(26);
    let hq = format!("/vi/{YT}/hqdefault.jpg");
    let routes = vec![
        (hq.as_str(), "image/png", img.clone()),
        ("/ok.png", "image/png", png(27)),
        ("/ep.mp3", "audio/mpeg", b"ID3 a".to_vec()),
        ("/a.pdf", "application/pdf", b"%PDF b".to_vec()),
    ];
    let (first, second, text1, text2) = with_media(routes, |b, _| {
        let note = format!(
            "![](https://youtu.be/{YT})\n\
             ![](https://vimeo.com/1)\n\
             ![]({b}/s404/gone.png)\n\
             ![md5]({b}/s404/0123456789abcdef0123456789abcdef_MD5.png)\n\
             ![]({b}/ok.png)\n\
             ![]({b}/ep.mp3)\n\
             ![](https://docs.example/a.pdf)\n\
             ![]({b}/a.pdf)\n\
             ![](https://x.com/a/status/1)\n\
             <iframe src=\"https://e.example/w\"></iframe>\n\
             <video src=\"https://m.example/v.mp4\"></video>\n\
             <audio src=\"https://m.example/a.mp3\"></audio>\n\
             <img src=\"{b}/s404/i.png\" alt=\"i\">\n"
        );
        let env = env(&[("n.md", note.as_bytes())]);
        let st = env.step("audio = \"link\"\npdf = \"link\"");
        let first = env.run(&st, "n.md").unwrap();
        let text1 = env.text("n.md");
        let second = env.run(&st, "n.md").unwrap();
        let text2 = env.text("n.md");
        (first, second, text1, text2)
    });
    assert_eq!(first.vars.get("changed"), Some(&Value::Bool(true)));
    assert!(text1.contains("[🖼 md5]("), "{text1}");
    assert!(text1.contains("_MD5.png)\n![](_attachments"), "{text1}");
    assert_eq!(
        second.vars.get("changed"),
        Some(&Value::Bool(false)),
        "{text2}"
    );
    assert_eq!(text1, text2);
    // The thumbnail and the localized image are managed images (+ the de-embedded remote
    // `_MD5` URL, which the text-level `count_managed_images` scan also matches).
    assert_eq!(var(&first, "images"), 3);
    assert_eq!(var(&first, "thumbnails"), 1);
}

/// Pin 6 for local audio/pdf output: a second localize-mode pass is a no-op.
#[test]
fn second_pass_over_localized_audio_and_pdf_writes_nothing() {
    let (changed, t1, t2) = with_media(
        vec![
            ("/ep.mp3", "audio/mpeg", b"ID3 c".to_vec()),
            ("/a.pdf", "application/pdf", b"%PDF d".to_vec()),
        ],
        |b, _| {
            let note = format!("![]({b}/ep.mp3)\n![]({b}/a.pdf)\n");
            let env = env(&[("n.md", note.as_bytes())]);
            let st = env.step("audio = \"localize\"\npdf = \"localize\"");
            env.run(&st, "n.md").unwrap();
            let t1 = env.text("n.md");
            let out = env.run(&st, "n.md").unwrap();
            (out.vars.get("changed").cloned(), t1, env.text("n.md"))
        },
    );
    assert_eq!(changed, Some(Value::Bool(false)));
    assert!(t1.contains("_MD5.mp3") && t1.contains("_MD5.pdf"), "{t1}");
    assert_eq!(t1, t2);
}

/// Pin 7 (audit B1): a mixed note — one 200, one unreachable, one 404 — is not settled; the
/// batch moves on and a later run re-selects it.
#[test]
fn folder_backfill_never_settles_a_note_with_a_transient_failure() {
    let ok = png(28);
    let down = MockHttp::unused_port();
    let (env, outs, b) = with_media(vec![("/ok.png", "image/png", ok.clone())], move |b, _| {
        let mixed =
            format!("![]({b}/ok.png)\n![](http://127.0.0.1:{down}/t.png)\n![]({b}/s404/g.png)\n");
        let next = format!("![]({b}/ok.png)\n");
        let env = env(&[
            ("daily/a.md", mixed.as_bytes()),
            ("daily/b.md", next.as_bytes()),
        ]);
        let st = env.step("scope = \"folder\"\nfolder = \"daily\"\nlimit = 10\ntimeout_secs = 5");
        let outs: Vec<StepOutcome> = (0..2).map(|_| env.run(&st, "").unwrap()).collect();
        (env, outs, b)
    });
    let v = |i: usize, k: &str| var(&outs[i], k);
    // Run 1: a retried (not recorded), b localized — the batch moved on.
    assert_eq!(
        (
            v(0, "retrying"),
            v(0, "localized"),
            v(0, "failed"),
            v(0, "unattempted_remaining")
        ),
        (1, 1, 0, 0)
    );
    assert!(ignored(&env).is_empty(), "never settled");
    assert_eq!(
        env.text("daily/a.md"),
        format!(
            "![](daily/_attachments/a/{m}_MD5.png)\n![](http://127.0.0.1:{down}/t.png)\n[🖼 127.0.0.1]({b}/s404/g.png)\n",
            m = md5(&ok)
        )
    );
    // Run 2: a is re-selected (only the unreachable ref left: the all-unreachable break).
    assert_eq!(v(1, "unattempted_remaining"), 1);
    assert!(ignored(&env).is_empty());
}

/// Pin 5: HTML replacements are matched on the current text inside the one `modify`, so an
/// edit made during the download is kept.
#[test]
fn html_rewrite_keeps_a_concurrent_edit() {
    let img = png(29);
    let (text, out) = with_media(vec![("/ok.png", "image/png", img.clone())], |b, _| {
        let note = format!("<img src=\"{b}/ok.png\">\n");
        let env = env(&[("n.md", note.as_bytes())]);
        let st = env.step("");
        // Edit the note between the first read and the write (a fresh index entry is fine).
        super::super::race::inject(&env.vault(), "n.md", 1, |t| format!("edited\n{t}"));
        let out = env.run(&st, "n.md").unwrap();
        (env.text("n.md"), out)
    });
    assert_eq!(out.vars.get("changed"), Some(&Value::Bool(true)));
    assert_eq!(
        text,
        format!("edited\n![](_attachments/n/{}_MD5.png)\n", md5(&img))
    );
}
