//! RSS 2.0 / Atom item parsing over `quick-xml` into a plain [`FeedItem`].
//!
//! Field mapping follows Crucible's blog fetcher (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/utils/
//! blogs.ts`: title, link, guid/id, published, authors, categories, `content:encoded`/Atom
//! `content` body, `description`/`summary`, audio enclosure at `:495-511`). Crucible parsed with
//! the browser's `DOMParser` (an Obsidian/Electron-provided facility); here a streaming parser
//! does the same job without a DOM. Policy (body detection, canon, automatable) is WP-8's.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// One feed entry. Empty strings mean "absent".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedItem {
    /// Entry title (text; entities resolved).
    pub title: String,
    /// The entry's page URL (RSS `<link>`; Atom `<link rel="alternate">` or rel-less `href`).
    pub link: String,
    /// RSS `<guid>` / Atom `<id>`.
    pub guid: String,
    /// RSS `<pubDate>` / `dc:date`; Atom `<published>`, else `<updated>`. Raw string.
    pub published: String,
    /// RSS `<author>`/`dc:creator`; Atom `<author><name>`.
    pub authors: Vec<String>,
    /// RSS `<category>` text; Atom `<category term>`.
    pub categories: Vec<String>,
    /// Full-body HTML: RSS `content:encoded`, Atom `<content>`.
    pub content_html: String,
    /// Summary HTML: RSS `<description>`, Atom `<summary>`.
    pub summary_html: String,
    /// First `audio/*` enclosure URL (RSS `<enclosure>`, Atom `<link rel="enclosure">`).
    pub enclosure_audio_url: String,
}

/// A feed that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedError(pub String);

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "feed parse error: {}", self.0)
    }
}

impl std::error::Error for FeedError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Link,
    Guid,
    Published,
    Updated,
    Author,
    Category,
    Content,
    Summary,
}

fn field_for(qname: &[u8]) -> Option<Field> {
    Some(match qname {
        b"title" => Field::Title,
        b"link" => Field::Link,
        b"guid" | b"id" => Field::Guid,
        b"pubDate" | b"published" | b"dc:date" => Field::Published,
        b"updated" => Field::Updated,
        b"dc:creator" | b"author" => Field::Author,
        b"category" => Field::Category,
        b"content:encoded" | b"content" => Field::Content,
        b"description" | b"summary" => Field::Summary,
        _ => return None,
    })
}

fn attr(e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    e.try_get_attribute(name).ok().flatten().and_then(|a| {
        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .ok()
            .map(|v| v.trim().to_owned())
    })
}

fn predefined(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => return None,
    })
}

/// Handles attribute-only elements (Atom links, categories, RSS enclosures).
fn on_attrs(item: &mut FeedItem, qname: &[u8], e: &BytesStart<'_>) {
    match qname {
        b"link" => {
            let rel = attr(e, b"rel").unwrap_or_default();
            let href = attr(e, b"href").unwrap_or_default();
            let ty = attr(e, b"type").unwrap_or_default().to_ascii_lowercase();
            if href.is_empty() {
                return;
            }
            if rel == "enclosure" {
                if ty.starts_with("audio/") && item.enclosure_audio_url.is_empty() {
                    item.enclosure_audio_url = href;
                }
            } else if (rel.is_empty() || rel == "alternate") && item.link.is_empty() {
                item.link = href;
            }
        }
        b"enclosure" => {
            let ty = attr(e, b"type").unwrap_or_default().to_ascii_lowercase();
            if let Some(url) = attr(e, b"url").filter(|u| !u.is_empty())
                && ty.starts_with("audio/")
                && item.enclosure_audio_url.is_empty()
            {
                item.enclosure_audio_url = url;
            }
        }
        b"category" => {
            if let Some(term) = attr(e, b"term").filter(|t| !t.is_empty()) {
                item.categories.push(term);
            }
        }
        _ => {}
    }
}

fn assign(item: &mut FeedItem, field: Field, text: String, updated: &mut String) {
    let raw_html = matches!(field, Field::Content | Field::Summary);
    let v = if raw_html {
        text
    } else {
        text.trim().to_owned()
    };
    if v.is_empty() {
        return;
    }
    match field {
        Field::Title => item.title = v,
        Field::Link => {
            if item.link.is_empty() {
                item.link = v;
            }
        }
        Field::Guid => item.guid = v,
        Field::Published => item.published = v,
        Field::Updated => *updated = v,
        Field::Author => item.authors.push(v),
        Field::Category => item.categories.push(v),
        Field::Content => item.content_html = v,
        Field::Summary => item.summary_html = v,
    }
}

struct Capture {
    field: Field,
    depth: usize,
    buf: String,
}

/// Parses every `<item>` (RSS) / `<entry>` (Atom) of `xml`, in document order.
pub fn parse_feed(xml: &str) -> Result<Vec<FeedItem>, FeedError> {
    let mut reader = Reader::from_str(xml);
    let mut items = Vec::new();
    let mut cur: Option<(FeedItem, usize, String)> = None; // (item, depth, atom <updated>)
    let mut depth = 0usize;
    let mut cap: Option<Capture> = None;
    loop {
        let ev = reader
            .read_event()
            .map_err(|e| FeedError(format!("at byte {}: {e}", reader.buffer_position())))?;
        match ev {
            Event::Eof => break,
            Event::Start(e) => {
                depth += 1;
                if let Some(c) = cap.as_mut() {
                    // Atom `<author><name>`: the author capture restarts on its `<name>` child
                    // (RSS `<author>` is plain text and keeps the outer capture).
                    if c.field == Field::Author
                        && c.depth + 1 == depth
                        && e.name().as_ref() == b"name"
                    {
                        *c = Capture {
                            field: Field::Author,
                            depth,
                            buf: String::new(),
                        };
                        continue;
                    }
                    c.buf.push('<');
                    c.buf.push_str(&String::from_utf8_lossy(&e));
                    c.buf.push('>');
                    continue;
                }
                let qname = e.name().as_ref().to_vec();
                match cur.as_mut() {
                    None if qname == b"item" || qname == b"entry" => {
                        cur = Some((FeedItem::default(), depth, String::new()));
                    }
                    None => {}
                    Some((item, _, _)) => {
                        on_attrs(item, &qname, &e);
                        if let Some(field) = field_for(&qname) {
                            cap = Some(Capture {
                                field,
                                depth,
                                buf: String::new(),
                            });
                        }
                    }
                }
            }
            Event::Empty(e) => {
                if let Some(c) = cap.as_mut() {
                    c.buf.push('<');
                    c.buf.push_str(&String::from_utf8_lossy(&e));
                    c.buf.push_str("/>");
                } else if let Some((item, _, _)) = cur.as_mut() {
                    on_attrs(item, e.name().as_ref(), &e);
                }
            }
            Event::End(e) => {
                if let Some(c) = cap.as_mut()
                    && c.depth != depth
                {
                    c.buf.push_str("</");
                    c.buf.push_str(&String::from_utf8_lossy(e.name().as_ref()));
                    c.buf.push('>');
                } else if let Some(c) = cap.take()
                    && let Some((item, _, updated)) = cur.as_mut()
                {
                    assign(item, c.field, c.buf, updated);
                }
                if cur.as_ref().is_some_and(|(_, d, _)| *d == depth)
                    && let Some((mut item, _, updated)) = cur.take()
                {
                    if item.published.is_empty() {
                        item.published = updated;
                    }
                    items.push(item);
                }
                depth = depth.saturating_sub(1);
            }
            Event::Text(t) => {
                if let Some(c) = cap.as_mut() {
                    let s = t.decode().map_err(|e| FeedError(e.to_string()))?;
                    c.buf.push_str(&s);
                }
            }
            Event::CData(t) => {
                if let Some(c) = cap.as_mut() {
                    c.buf.push_str(&String::from_utf8_lossy(&t));
                }
            }
            Event::GeneralRef(r) => {
                if let Some(c) = cap.as_mut() {
                    if let Ok(Some(ch)) = r.resolve_char_ref() {
                        c.buf.push(ch);
                    } else {
                        let name = String::from_utf8_lossy(&r).into_owned();
                        match predefined(&name) {
                            Some(ch) => c.buf.push(ch),
                            None => {
                                c.buf.push('&');
                                c.buf.push_str(&name);
                                c.buf.push(';');
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/" xmlns:dc="http://purl.org/dc/elements/1.1/">
<channel><title>Blog</title><link>https://blog.example/</link>
<item>
  <title>Fish &amp; Chips</title>
  <link>https://blog.example/p/1</link>
  <guid isPermaLink="false">post-1</guid>
  <pubDate>Mon, 12 Oct 2026 08:00:00 GMT</pubDate>
  <dc:creator>Ann</dc:creator>
  <category>food</category><category>uk</category>
  <description>Short &lt;b&gt;teaser&lt;/b&gt;</description>
  <content:encoded><![CDATA[<p>Full <em>body</em></p>]]></content:encoded>
  <enclosure url="https://cdn.example/a.mp3" type="audio/mpeg" length="1"/>
</item>
<item><title>Second</title><link>https://blog.example/p/2</link></item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>A</title><link href="https://a.example/"/>
<entry>
  <title type="html">It&#8217;s here</title>
  <link rel="alternate" type="text/html" href="https://a.example/x"/>
  <link rel="enclosure" type="audio/ogg" href="https://a.example/x.ogg"/>
  <id>tag:a.example,2026:x</id>
  <updated>2026-10-11T00:00:00Z</updated>
  <author><name>Bo</name></author><author><name>Cy</name></author>
  <category term="rust"/>
  <summary>Sum</summary>
  <content type="xhtml"><div xmlns="http://www.w3.org/1999/xhtml"><p>Hi<br/>there</p></div></content>
</entry>
</feed>"#;

    #[test]
    fn rss_items_parse() {
        let items = parse_feed(RSS).unwrap();
        assert_eq!(items.len(), 2);
        let a = &items[0];
        assert_eq!(a.title, "Fish & Chips");
        assert_eq!(a.link, "https://blog.example/p/1");
        assert_eq!(a.guid, "post-1");
        assert_eq!(a.published, "Mon, 12 Oct 2026 08:00:00 GMT");
        assert_eq!(a.authors, vec!["Ann"]);
        assert_eq!(a.categories, vec!["food", "uk"]);
        assert_eq!(a.summary_html, "Short <b>teaser</b>");
        assert_eq!(a.content_html, "<p>Full <em>body</em></p>");
        assert_eq!(a.enclosure_audio_url, "https://cdn.example/a.mp3");
        assert_eq!(items[1].title, "Second");
        assert!(items[1].content_html.is_empty());
    }

    #[test]
    fn atom_entries_parse() {
        let items = parse_feed(ATOM).unwrap();
        assert_eq!(items.len(), 1);
        let e = &items[0];
        assert_eq!(e.title, "It\u{2019}s here");
        assert_eq!(e.link, "https://a.example/x");
        assert_eq!(e.enclosure_audio_url, "https://a.example/x.ogg");
        assert_eq!(e.guid, "tag:a.example,2026:x");
        assert_eq!(
            e.published, "2026-10-11T00:00:00Z",
            "falls back to <updated>"
        );
        assert_eq!(e.authors, vec!["Bo", "Cy"]);
        assert_eq!(e.categories, vec!["rust"]);
        assert_eq!(e.summary_html, "Sum");
        assert!(
            e.content_html.contains("<p>Hi<br/>there</p>"),
            "{}",
            e.content_html
        );
    }

    #[test]
    fn malformed_feed_errors() {
        assert!(parse_feed("<rss><channel><item><title>x</item></rss>").is_err());
    }
}
