//! The `localize` step's remote-media rewrites (plan remote-media-sprint WP-3, Decisions 1–3):
//! image embeds are localized or, on a permanent failure, de-embedded to a link; video embeds
//! become a linked local thumbnail or a link card; posts, other iframes, audio and pdf become
//! link cards or (audio/pdf in `localize` mode) local embeds. HTML media elements go through the
//! same rules. Every output form is a plain link, a local `_MD5` embed, or a link wrapping one,
//! so a second pass never re-matches it (plan audit E2).

use cv_providers::web::{FetchRequest, Limited};
use cv_text::attachments::{
    AttachmentMatch, Media, Syntax, classify_media, format_embed, format_ref, is_embed_page,
    md_ref_raw_destination,
};
use cv_text::remote_media::{
    HtmlMedia, HtmlMediaTag, RemoteKind, VideoProvider, classify_remote, html_attr,
};

use super::super::{StepCtx, StepError};
use super::note::{DownloadError, Fetched, Run, SERVICE, classify_fetch_error};
use super::params::VideoMode;
use cv_actions::MediaMode;

/// One remote media source to rewrite.
struct Src<'a> {
    /// The URL fetched/classified.
    url: &'a str,
    /// The card destination (original raw destination, `<…>`-wrapped when needed).
    dest: String,
    /// The sanitized alt text, if any.
    alt: Option<String>,
    /// A remote `<video poster>`.
    poster: Option<&'a str>,
    /// `Some(tag)` for an HTML element.
    tag: Option<HtmlMediaTag>,
}

/// A thumbnail lookup.
enum Thumb {
    Got(String),
    None,
    Transient,
}

/// A card destination: wrapped in `<…>` when it holds whitespace, parens or angle brackets.
fn card_dest(d: &str) -> String {
    if d.chars()
        .any(|c| c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>'))
    {
        format!("<{d}>")
    } else {
        d.to_owned()
    }
}

/// The URL host minus a leading `www.` (`link` when there is none).
fn host(url: &str) -> String {
    let h = url::Url::parse(url.trim())
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_else(|| "link".into());
    h.strip_prefix("www.").map(str::to_owned).unwrap_or(h)
}

/// Link text: no `[`, `]` or line breaks (the card must re-parse as one link).
fn clean_text(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '[' | ']' | '\n' | '\r'))
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The alt of a markdown embed's source text (`![alt](…)`; one nesting level of `[…]`).
fn md_alt(original: &str) -> Option<String> {
    let body = original.strip_prefix("![")?;
    let mut depth = 0_usize;
    for (i, c) in body.char_indices() {
        match c {
            '[' => depth += 1,
            ']' if depth == 0 => return Some(body[..i].to_owned()),
            ']' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn card(icon_text: &str, dest: &str) -> String {
    format!("[{icon_text}]({dest})")
}

impl Src<'_> {
    fn video_card(&self) -> String {
        card(&format!("▶ video · {}", host(self.url)), &self.dest)
    }
    fn delink(&self) -> String {
        let text = self
            .alt
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| host(self.url));
        card(&format!("🖼 {text}"), &self.dest)
    }
}

/// `[![](<thumb>)](<href>)`: a normal local `_MD5` embed inside a link.
fn thumb_ref(thumb: &str, href: &str) -> String {
    format!("[{}]({})", format_embed(Syntax::Md, thumb), card_dest(href))
}

impl Run<'_> {
    /// A remote markdown ref (Crucible `processRemote`, extended by the media modes).
    /// `Ok(None)` = left as-is.
    pub(super) fn process_remote(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        m: &AttachmentMatch,
    ) -> Result<Option<String>, StepError> {
        let p = self.p;
        if !m.is_embed {
            // A remote `_MD5`-named link (E2 case a): downloaded as before, never carded or
            // de-embedded — a failure leaves it as it is.
            return Ok(
                match self.fetch(ctx, note, &m.link, false, &|e| p.eligible(e))? {
                    Fetched::Local(t) => Some(format_ref(m, &t)),
                    _ => None,
                },
            );
        }
        let raw = md_ref_raw_destination(&m.original).unwrap_or_else(|| m.link.clone());
        let src = Src {
            url: &m.link,
            dest: card_dest(&raw),
            alt: md_alt(&m.original).map(|a| clean_text(&a)),
            poster: None,
            tag: None,
        };
        self.rewrite(ctx, note, &src, &|t| format_ref(m, t))
    }

    /// A page-relative ref joined to the note's `source` (plan round-15 WP-1): an image
    /// download only — never de-embedded (its text is not the URL).
    pub(super) fn process_relative(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        m: &AttachmentMatch,
    ) -> Result<Option<String>, StepError> {
        let p = self.p;
        if is_embed_page(&m.link) {
            self.counts.ineligible += 1;
            return Ok(None);
        }
        Ok(
            match self.fetch(ctx, note, &m.link, true, &|e| p.eligible(e))? {
                Fetched::Local(t) => Some(format_ref(m, &t)),
                _ => None,
            },
        )
    }

    /// An HTML media element (`text` is its source text): its replacement, if any.
    pub(super) fn process_html(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        h: &HtmlMedia,
        text: &str,
    ) -> Result<Option<String>, StepError> {
        let alt = (h.tag == HtmlMediaTag::Img)
            .then(|| html_attr(text, "alt"))
            .flatten()
            .map(|a| clean_text(&a));
        let src = Src {
            url: &h.src,
            dest: card_dest(&h.src),
            alt,
            poster: h.poster.as_deref(),
            tag: Some(h.tag),
        };
        self.rewrite(ctx, note, &src, &|t| format_embed(Syntax::Md, t))
    }

    /// The kind × mode dispatch shared by markdown and HTML embeds.
    fn rewrite(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        s: &Src<'_>,
        local: &dyn Fn(&str) -> String,
    ) -> Result<Option<String>, StepError> {
        let p = self.p;
        let mut kind = classify_remote(s.url);
        match (s.tag, &kind) {
            (_, RemoteKind::VideoPage { .. } | RemoteKind::Post) => {}
            (Some(HtmlMediaTag::Iframe), _) => kind = RemoteKind::Embed,
            (Some(HtmlMediaTag::Video), _) => kind = RemoteKind::VideoFile,
            (Some(HtmlMediaTag::Audio), _) => kind = RemoteKind::AudioFile,
            (Some(HtmlMediaTag::Img), _) => kind = RemoteKind::Image,
            (None, _) if is_embed_page(s.url) => {
                // A YouTube page without a usable id: no thumbnail to be had.
                self.counts.media_links += 1;
                return Ok(Some(s.video_card()));
            }
            (None, _) => {}
        }
        let out = match kind {
            RemoteKind::VideoPage { provider, id } => {
                if p.video == VideoMode::Link {
                    Some(s.video_card())
                } else {
                    let (canonical, thumb) = match provider {
                        VideoProvider::YouTube => (
                            format!("https://www.youtube.com/watch?v={id}"),
                            self.youtube_thumb(ctx, note, &id)?,
                        ),
                        VideoProvider::Vimeo => {
                            let c = format!("https://vimeo.com/{id}");
                            let t = self.vimeo_thumb(ctx, note, &c)?;
                            (c, t)
                        }
                    };
                    match thumb {
                        Thumb::Got(t) => Some(thumb_ref(&t, &canonical)),
                        Thumb::None => Some(s.video_card()),
                        Thumb::Transient => return Ok(None),
                    }
                }
            }
            RemoteKind::Post => Some(card("𝕏 post", &s.dest)),
            RemoteKind::Embed => Some(card(&format!("⧉ embed · {}", host(s.url)), &s.dest)),
            RemoteKind::VideoFile => match p.video {
                VideoMode::Localize => {
                    match self.fetch(ctx, note, s.url, false, &|e| p.eligible(e))? {
                        Fetched::Local(t) => return Ok(Some(local(&t))),
                        Fetched::Permanent | Fetched::Ineligible => Some(s.video_card()),
                        Fetched::Transient | Fetched::Failed => return Ok(None),
                    }
                }
                VideoMode::Thumbnail => match s.poster {
                    Some(poster) => match self.thumbnail(ctx, note, &[poster.to_owned()])? {
                        Thumb::Got(t) => Some(thumb_ref(&t, s.url)),
                        Thumb::None => Some(s.video_card()),
                        Thumb::Transient => return Ok(None),
                    },
                    None => Some(s.video_card()),
                },
                VideoMode::Link => Some(s.video_card()),
            },
            RemoteKind::AudioFile | RemoteKind::Pdf => {
                let (mode, text) = if kind == RemoteKind::Pdf {
                    (p.pdf, format!("⧉ pdf · {}", host(s.url)))
                } else {
                    (p.audio, format!("♪ audio · {}", host(s.url)))
                };
                let fallback = Some(card(&text, &s.dest));
                match mode {
                    MediaMode::Link => fallback,
                    MediaMode::Localize => {
                        match self.fetch(ctx, note, s.url, false, &|e| p.eligible(e))? {
                            Fetched::Local(t) => return Ok(Some(local(&t))),
                            Fetched::Permanent | Fetched::Ineligible => fallback,
                            Fetched::Transient | Fetched::Failed => return Ok(None),
                        }
                    }
                }
            }
            RemoteKind::Image | RemoteKind::Unknown => {
                return Ok(
                    match self.fetch(ctx, note, s.url, true, &|e| p.eligible(e))? {
                        Fetched::Local(t) => Some(local(&t)),
                        Fetched::Permanent => {
                            self.counts.delinked += 1;
                            Some(s.delink())
                        }
                        Fetched::Transient | Fetched::Ineligible | Fetched::Failed => None,
                    },
                );
            }
        };
        if let Some(o) = &out {
            if o.starts_with("[![") {
                self.counts.thumbnails += 1;
            } else {
                self.counts.media_links += 1;
            }
        }
        Ok(out)
    }

    /// The first thumbnail of `urls` that downloads; a transient failure stops the walk.
    fn thumbnail(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        urls: &[String],
    ) -> Result<Thumb, StepError> {
        let is_image = |e: &str| classify_media(e) == Some(Media::Images);
        for u in urls {
            match self.fetch(ctx, note, u, true, &is_image)? {
                Fetched::Local(t) => return Ok(Thumb::Got(t)),
                Fetched::Transient => return Ok(Thumb::Transient),
                Fetched::Permanent | Fetched::Ineligible | Fetched::Failed => {}
            }
        }
        Ok(Thumb::None)
    }

    /// YouTube: `hqdefault.jpg`, then `mqdefault.jpg`.
    fn youtube_thumb(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        id: &str,
    ) -> Result<Thumb, StepError> {
        let base = self.p.ytimg_base.trim_end_matches('/').to_owned();
        let urls = ["hqdefault.jpg", "mqdefault.jpg"].map(|f| format!("{base}/vi/{id}/{f}"));
        self.thumbnail(ctx, note, &urls)
    }

    /// Vimeo: the oEmbed `thumbnail_url`.
    fn vimeo_thumb(
        &mut self,
        ctx: &mut StepCtx<'_>,
        note: &str,
        canonical: &str,
    ) -> Result<Thumb, StepError> {
        let (Some(client), Some(handle)) = (self.client, self.handle.as_ref()) else {
            return Err(StepError::failed("no async runtime"));
        };
        let enc: String = url::form_urlencoded::byte_serialize(canonical.as_bytes()).collect();
        let api = format!(
            "{}/api/oembed.json?url={enc}",
            self.p.vimeo_base.trim_end_matches('/')
        );
        let mut req = FetchRequest::get(SERVICE, &api);
        req.timeout = Some(self.p.timeout);
        self.counts.remote_attempts += 1;
        let body = match handle.block_on(client.fetch_limited(req, 1 << 20, |_| Some(()))) {
            Ok(Limited::Body(b, ())) => b,
            Ok(Limited::Rejected) => return Ok(Thumb::None),
            Err(e) => {
                self.counts.failed += 1;
                return Ok(match classify_fetch_error(e) {
                    DownloadError::Unreachable => {
                        self.counts.unreachable += 1;
                        self.counts.transient += 1;
                        Thumb::Transient
                    }
                    DownloadError::Throttled(f) => {
                        self.counts.throttled += 1;
                        self.counts.transient += 1;
                        self.counts.first_throttle.get_or_insert(f);
                        Thumb::Transient
                    }
                    _ => Thumb::None,
                });
            }
        };
        let thumb = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("thumbnail_url")?.as_str().map(str::to_owned))
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"));
        match thumb {
            Some(u) => self.thumbnail(ctx, note, &[u]),
            None => Ok(Thumb::None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_parts() {
        assert_eq!(host("https://www.Example.com/a"), "example.com");
        assert_eq!(card_dest("https://h/a b"), "<https://h/a b>");
        assert_eq!(card_dest("https://h/a_(b)"), "<https://h/a_(b)>");
        assert_eq!(card_dest("https://h/a"), "https://h/a");
        assert_eq!(clean_text(" a [b]\nc "), "a bc");
        assert_eq!(md_alt("![a [b] c](u)").as_deref(), Some("a [b] c"));
        assert_eq!(md_alt("![](u)").as_deref(), Some(""));
        assert_eq!(
            thumb_ref("_a/x y_MD5.jpg", "https://v/1"),
            "[![](_a/x%20y_MD5.jpg)](https://v/1)"
        );
    }
}
