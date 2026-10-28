//! Mail completeness (plan round-16 WP-6): a pure structural check on raw `.eml` bytes, used by the
//! maildir watcher (ready trigger) and the `track-mail` scan (consumer gate) so a message still
//! being written is neither enqueued nor consumed. Not a validator: it only answers "could more
//! bytes still be coming?" — a complete-looking message that fails [`super::parse_eml`] stays the
//! caller's `invalid` case.

/// Maximum multipart nesting followed (a bound against hostile input, not a MIME limit).
const MAX_DEPTH: usize = 16;

/// Whether `bytes` look like a fully written message: when the data opens with a header line, the
/// header block must be terminated by a blank line, and every MIME multipart body (nested ones
/// included) must carry its closing `--boundary--` line. A message with no multipart body is
/// complete once its headers are.
#[must_use]
pub fn is_complete(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let Some((headers, body)) = split(&text) else {
        // No blank line: complete only when it never started a header block (not a message).
        return !text.lines().next().is_some_and(is_header_line);
    };
    closed(headers, body, 0)
}

fn is_header_line(l: &str) -> bool {
    l.split_once(':')
        .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_graphic() && b != b':'))
}

/// Splits at the first blank line into (headers, body).
fn split(text: &str) -> Option<(&str, &str)> {
    let mut pos = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            return Some((&text[..pos], &text[pos + line.len()..]));
        }
        pos += line.len();
    }
    None
}

/// The multipart boundary declared by the (unfolded) `Content-Type` header, if any.
fn boundary(headers: &str) -> Option<String> {
    let mut unfolded: Vec<String> = Vec::new();
    for l in headers.lines() {
        if l.starts_with([' ', '\t']) {
            if let Some(last) = unfolded.last_mut() {
                last.push(' ');
                last.push_str(l.trim());
            }
        } else {
            unfolded.push(l.to_owned());
        }
    }
    let ct = unfolded.iter().find_map(|l| {
        let (n, v) = l.split_once(':')?;
        n.trim().eq_ignore_ascii_case("content-type").then_some(v)
    })?;
    if !ct
        .trim_start()
        .to_ascii_lowercase()
        .starts_with("multipart/")
    {
        return None;
    }
    let lower = ct.to_ascii_lowercase();
    let at = lower.find("boundary=")? + "boundary=".len();
    let rest = &ct[at..];
    let b = if let Some(q) = rest.strip_prefix('"') {
        q.split('"').next()?
    } else {
        rest.split([';', ' ', '\t']).next()?
    };
    (!b.is_empty()).then(|| b.to_owned())
}

/// Whether the part (`headers`, `body`) and all parts nested in it are closed.
fn closed(headers: &str, body: &str, depth: usize) -> bool {
    let Some(b) = boundary(headers) else {
        return true;
    };
    if depth >= MAX_DEPTH {
        return true;
    }
    let open = format!("--{b}");
    let close = format!("--{b}--");
    let mut parts: Vec<String> = Vec::new();
    let mut cur: Option<String> = None;
    let mut done = false;
    for line in body.split_inclusive('\n') {
        let t = line.trim_end();
        if t == close {
            if let Some(p) = cur.take() {
                parts.push(p);
            }
            done = true;
            break;
        }
        if t == open {
            if let Some(p) = cur.take() {
                parts.push(p);
            }
            cur = Some(String::new());
        } else if let Some(p) = cur.as_mut() {
            p.push_str(line);
        }
    }
    if !done {
        return false;
    }
    parts.iter().all(|p| match split(p) {
        Some((h, bd)) => closed(h, bd, depth + 1),
        None => true,
    })
}

#[cfg(test)]
mod tests {
    use super::is_complete;

    fn crlf(s: &str) -> Vec<u8> {
        s.replace('\n', "\r\n").into_bytes()
    }

    const FLAT: &str = "From: a@x.example\nSubject: hi\n\nbody\n";
    const MULTI: &str = "From: a@x.example\nContent-Type: multipart/alternative; boundary=\"b1\"\n\n--b1\nContent-Type: text/plain\n\nplain\n--b1\nContent-Type: text/html\n\n<p>h</p>\n--b1--\n";
    const NESTED: &str = "From: a@x.example\nContent-Type: multipart/mixed; boundary=outer\n\n--outer\nContent-Type: multipart/alternative; boundary=inner\n\n--inner\nContent-Type: text/plain\n\nplain\n--inner--\n--outer\nContent-Type: text/plain\n\nattachment\n--outer--\n";

    #[test]
    fn non_multipart_is_complete() {
        assert!(is_complete(&crlf(FLAT)));
        assert!(is_complete(b"just some bytes without headers\n"));
        assert!(is_complete(b""));
    }

    #[test]
    fn truncated_headers_are_incomplete() {
        assert!(!is_complete(b"From: a@x.example\r\nSubject: h"));
        assert!(!is_complete(
            b"From: a@x.example\r\nContent-Type: multipart/mixed; bound"
        ));
    }

    #[test]
    fn multipart_needs_its_closing_boundary() {
        assert!(is_complete(&crlf(MULTI)));
        let cut = MULTI.find("--b1--").unwrap();
        assert!(!is_complete(&crlf(&MULTI[..cut])));
        assert!(!is_complete(&crlf(&MULTI[..cut - 8])));
    }

    #[test]
    fn nested_multipart_needs_every_level_closed() {
        assert!(is_complete(&crlf(NESTED)));
        // Outer closed, inner never closed.
        let broken = NESTED.replace("--inner--\n", "");
        assert!(!is_complete(&crlf(&broken)));
        // Inner closed, outer not.
        let cut = NESTED.rfind("--outer--").unwrap();
        assert!(!is_complete(&crlf(&NESTED[..cut])));
    }
}
