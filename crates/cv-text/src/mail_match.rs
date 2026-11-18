//! The mail matchers a publication record is resolved by: the `List-Id` normal form, the
//! sender/list match, and the `<pub>.substack.com` host of a message.

/// `"Name" <id.example>` / `<id.example>` / `id.example` → `id.example` (lowercase).
#[must_use]
pub fn normalize_list_id(s: &str) -> String {
    let inner = match (s.rfind('<'), s.rfind('>')) {
        (Some(a), Some(b)) if a < b => &s[a + 1..b],
        _ => s,
    };
    inner.trim().to_lowercase()
}

/// Whether a mail from `from_addr` with the normalized `List-Id` `list` matches: some `from`
/// entry is the exact address or an `@domain` suffix of it (case-insensitive), or some
/// `list_ids` entry normalizes to `list`. Blank matchers never match.
#[must_use]
pub fn mail_matches(
    from_addr: &str,
    list: Option<&str>,
    from: &[String],
    list_ids: &[String],
) -> bool {
    let addr = from_addr.trim().to_lowercase();
    let from_hit = !addr.is_empty()
        && from.iter().any(|f| {
            let f = f.trim().to_lowercase();
            if f.is_empty() {
                false
            } else if f.starts_with('@') {
                addr.ends_with(&f)
            } else {
                addr == f
            }
        });
    let list_hit = list.is_some_and(|got| {
        list_ids.iter().any(|l| {
            let want = normalize_list_id(l);
            !want.is_empty() && want == got
        })
    });
    from_hit || list_hit
}

/// The `<pub>.substack.com` host a message comes from: the `List-Id` when it is exactly
/// `<pub>.substack.com` (one non-empty label), else `<local>.substack.com` for a
/// `<local>@substack.com` sender, else `None`.
#[must_use]
pub fn substack_host(from_addr: &str, list_id: Option<&str>) -> Option<String> {
    const SUFFIX: &str = ".substack.com";
    let single_label = |h: &str| {
        h.strip_suffix(SUFFIX)
            .is_some_and(|l| !l.is_empty() && !l.contains('.'))
    };
    if let Some(l) = list_id {
        let l = l.trim().to_ascii_lowercase();
        if single_label(&l) {
            return Some(l);
        }
    }
    let (local, domain) = from_addr.trim().rsplit_once('@')?;
    if domain.eq_ignore_ascii_case("substack.com") && !local.is_empty() {
        let h = format!("{}{SUFFIX}", local.to_ascii_lowercase());
        return single_label(&h).then_some(h);
    }
    None
}
