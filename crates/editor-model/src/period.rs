//! Day/month period date math for nav (WP-4).
//!
//! Replaces the Note Toolbar `{{js:}}` day/month bars (inventory #2, D10 "Nav bars"). The bar
//! items are taken from the Note Toolbar settings (`folderMappings`: `daily/day` →
//! Day nav, `daily/month` → Month nav). Pure: "today" is always injected.

use chrono::{Datelike, Days, Months, NaiveDate};

/// Folder of day notes when `captures.toml` configures no `[periods.day].folder`. The editor reads
/// that same key itself ([`PeriodFolders::from_config`]); this fallback is the editor's one copy of
/// `cv_actions::vault::DEFAULT_DAY_FOLDER` (review S5: the editor has no `cv-actions` edge and the
/// daemon sends no day folder, and the schema leaves the key required with no default).
pub const DAY_FOLDER: &str = "daily/day";
/// Folder of month notes.
pub const MONTH_FOLDER: &str = "daily/month";
/// The default nav link's page (plan public-release WP-2, C#12; review S5): the per-publication
/// works views, on this page under the configured app folder (`vault.toml` `[structure] app_folder`).
pub const PUBLICATIONS_PAGE: &str = "Publications.md";

/// The body the default nav link creates its missing page with (a nav link to a missing note creates
/// it): the `works` views — per publication (Active / Complete), its entries and the unread ones.
pub const PUBLICATIONS_SEED: &str = "# Publications\n\n## Active\n\n```crucible-view\nworks#Active\n```\n\n## Complete\n\n```crucible-view\nworks#Complete\n```\n\n## Entries\n\n```crucible-view\nworks#Entries\n```\n\n## Unread\n\n```crucible-view\nworks#Unread\n```\n";

/// The day / month note folders navigation recognizes (plan `editor-settings-tab` WP-4, audit
/// E2): the editor fills them from `captures.toml` `[periods.day].folder` /
/// `[periods.month].folder`; [`Default`] is [`DAY_FOLDER`] / [`MONTH_FOLDER`]. Only the folder
/// is configurable here — note basenames must stay ISO (`YYYY-MM-DD` / `YYYY-MM`); a non-ISO
/// `format` is not honoured by navigation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodFolders {
    pub day: String,
    pub month: String,
}

impl Default for PeriodFolders {
    fn default() -> Self {
        Self {
            day: DAY_FOLDER.to_owned(),
            month: MONTH_FOLDER.to_owned(),
        }
    }
}

impl PeriodFolders {
    /// Folders from optional configured values; a missing or blank one falls back to its
    /// constant. Surrounding `/` are trimmed.
    pub fn from_config(day: Option<&str>, month: Option<&str>) -> Self {
        let pick = |v: Option<&str>, d: &str| {
            v.map(|s| s.trim().trim_matches('/'))
                .filter(|s| !s.is_empty())
                .unwrap_or(d)
                .to_owned()
        };
        Self {
            day: pick(day, DAY_FOLDER),
            month: pick(month, MONTH_FOLDER),
        }
    }
}

/// The period a `period.open` call targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    Day,
    Month,
}

impl Period {
    /// The `period` param value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Month => "month",
        }
    }
}

/// Where a nav item leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavTarget {
    /// `run_action period.open {period, date}` (month: first of month).
    Period(Period, NaiveDate),
    /// Opens a fixed vault path directly; a missing note is created with `seed` (review E7: the
    /// seed belongs to the link).
    Path { path: String, seed: String },
}

impl NavTarget {
    /// A path target with no seed (a configured link).
    pub fn path(path: impl Into<String>) -> Self {
        Self::Path {
            path: path.into(),
            seed: String::new(),
        }
    }
}

/// One configured nav link (editor.toml `nav_links`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavLink {
    pub label: String,
    pub path: String,
    /// The body a missing [`Self::path`] is created with (empty for a configured link).
    pub seed: String,
}

/// The default links under `app_folder` (the configured `[structure] app_folder`): one
/// "Publications" page ([`PUBLICATIONS_PAGE`], seeded with [`PUBLICATIONS_SEED`]; a configured
/// `nav_links` overrides it).
pub fn default_nav_links(app_folder: &str) -> Vec<NavLink> {
    vec![NavLink {
        label: "Publications".into(),
        path: format!("{app_folder}/{PUBLICATIONS_PAGE}"),
        seed: PUBLICATIONS_SEED.into(),
    }]
}

/// One bar button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavItem {
    pub label: String,
    pub target: NavTarget,
}

/// `path` is exactly `<day folder>/YYYY-MM-DD.md` → its date. Reading entries under
/// `<day folder>/<date>/…` are not day notes.
pub fn day_note_date(path: &str, folders: &PeriodFolders) -> Option<NaiveDate> {
    let stem = path
        .strip_prefix(folders.day.as_str())?
        .strip_prefix('/')?
        .strip_suffix(".md")?;
    if stem.len() != 10 || !shape(stem, &[4, 2, 2]) {
        return None;
    }
    NaiveDate::parse_from_str(stem, "%Y-%m-%d").ok()
}

/// `path` is exactly `<month folder>/YYYY-MM.md` → the first of that month.
pub fn month_note_date(path: &str, folders: &PeriodFolders) -> Option<NaiveDate> {
    let stem = path
        .strip_prefix(folders.month.as_str())?
        .strip_prefix('/')?
        .strip_suffix(".md")?;
    if stem.len() != 7 || !shape(stem, &[4, 2]) {
        return None;
    }
    let (y, m) = stem.split_once('-')?;
    NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, 1)
}

/// `s` is digit groups of the given lengths joined by `-`.
fn shape(s: &str, groups: &[usize]) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == groups.len()
        && parts
            .iter()
            .zip(groups)
            .all(|(p, &n)| p.len() == n && p.bytes().all(|b| b.is_ascii_digit()))
}

/// The day before `d` (saturating at the calendar minimum).
pub fn yesterday(d: NaiveDate) -> NaiveDate {
    d.checked_sub_days(Days::new(1)).unwrap_or(d)
}

/// The day after `d`.
pub fn tomorrow(d: NaiveDate) -> NaiveDate {
    d.checked_add_days(Days::new(1)).unwrap_or(d)
}

/// First of `d`'s month.
pub fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap_or(d)
}

/// First of the previous month.
pub fn prev_month(d: NaiveDate) -> NaiveDate {
    let s = month_start(d);
    s.checked_sub_months(Months::new(1)).unwrap_or(s)
}

/// First of the next month.
pub fn next_month(d: NaiveDate) -> NaiveDate {
    let s = month_start(d);
    s.checked_add_months(Months::new(1)).unwrap_or(s)
}

/// "October 2026".
pub fn month_label(d: NaiveDate) -> String {
    d.format("%B %Y").to_string()
}

/// `YYYY-MM-DD` (the `period.open` date param).
pub fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// The nav bar for a tab on `path`, or `None` when the path gets no bar.
///
/// Day note: ← yesterday, month, Today, the links, tomorrow →. Month note: ← Last Month, Today,
/// the links, Next Month →. Any other markdown note: Today plus the links. Else `None`.
pub fn bar_for(
    path: &str,
    today: NaiveDate,
    folders: &PeriodFolders,
    links: &[NavLink],
) -> Option<Vec<NavItem>> {
    let item = |label: String, target| NavItem { label, target };
    let link_items = || {
        links.iter().map(|l| NavItem {
            label: l.label.clone(),
            target: NavTarget::Path {
                path: l.path.clone(),
                seed: l.seed.clone(),
            },
        })
    };
    if let Some(d) = day_note_date(path, folders) {
        let mut bar = vec![
            item(
                "← yesterday".into(),
                NavTarget::Period(Period::Day, yesterday(d)),
            ),
            item(
                month_label(d),
                NavTarget::Period(Period::Month, month_start(d)),
            ),
            item("Today".into(), NavTarget::Period(Period::Day, today)),
        ];
        bar.extend(link_items());
        bar.push(item(
            "tomorrow →".into(),
            NavTarget::Period(Period::Day, tomorrow(d)),
        ));
        return Some(bar);
    }
    if let Some(m) = month_note_date(path, folders) {
        let mut bar = vec![
            item(
                "← Last Month".into(),
                NavTarget::Period(Period::Month, prev_month(m)),
            ),
            item("Today".into(), NavTarget::Period(Period::Day, today)),
        ];
        bar.extend(link_items());
        bar.push(item(
            "Next Month →".into(),
            NavTarget::Period(Period::Month, next_month(m)),
        ));
        return Some(bar);
    }
    if path.ends_with(".md") {
        let mut bar = vec![item("Today".into(), NavTarget::Period(Period::Day, today))];
        bar.extend(link_items());
        return Some(bar);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;

    static F: LazyLock<PeriodFolders> = LazyLock::new(PeriodFolders::default);

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).expect("date")
    }

    #[test]
    fn day_math_across_boundaries() {
        assert_eq!(yesterday(d(2026, 3, 1)), d(2026, 2, 28));
        assert_eq!(yesterday(d(2024, 3, 1)), d(2024, 2, 29));
        assert_eq!(tomorrow(d(2024, 2, 28)), d(2024, 2, 29));
        assert_eq!(tomorrow(d(2024, 2, 29)), d(2024, 3, 1));
        assert_eq!(tomorrow(d(2026, 10, 31)), d(2026, 11, 1));
        assert_eq!(tomorrow(d(2026, 12, 31)), d(2027, 1, 1));
        assert_eq!(yesterday(d(2027, 1, 1)), d(2026, 12, 31));
    }

    #[test]
    fn month_math_across_years() {
        assert_eq!(prev_month(d(2026, 1, 15)), d(2025, 12, 1));
        assert_eq!(next_month(d(2026, 12, 31)), d(2027, 1, 1));
        assert_eq!(next_month(d(2024, 1, 31)), d(2024, 2, 1));
        assert_eq!(month_label(d(2026, 10, 13)), "October 2026");
        assert_eq!(iso(d(2026, 2, 3)), "2026-02-03");
    }

    #[test]
    fn path_predicates() {
        assert_eq!(
            day_note_date("daily/day/2026-10-13.md", &F),
            Some(d(2026, 10, 13))
        );
        assert_eq!(day_note_date("daily/day/2026-10-13/reading.md", &F), None);
        assert_eq!(
            day_note_date("daily/day/2026-10-13/2026-10-13.md", &F),
            None
        );
        assert_eq!(day_note_date("daily/day/2026-02-30.md", &F), None);
        assert_eq!(day_note_date("daily/day/2026-1-13.md", &F), None);
        assert_eq!(day_note_date("notes/2026-10-13.md", &F), None);
        assert_eq!(day_note_date("x/daily/day/2026-10-13.md", &F), None);
        assert_eq!(
            month_note_date("daily/month/2026-10.md", &F),
            Some(d(2026, 10, 1))
        );
        assert_eq!(month_note_date("daily/month/2026-13.md", &F), None);
        assert_eq!(month_note_date("daily/day/2026-10.md", &F), None);
        assert_eq!(month_note_date("notes/2026-10.md", &F), None);
    }

    #[test]
    fn bars() {
        let today = d(2026, 10, 13);
        let links = owner_links();
        let day = bar_for("daily/day/2026-12-31.md", today, &F, &links).expect("day bar");
        let labels: Vec<&str> = day.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "← yesterday",
                "December 2026",
                "Today",
                "Sample Charts",
                "Works",
                "tomorrow →"
            ]
        );
        assert_eq!(day[5].target, NavTarget::Period(Period::Day, d(2027, 1, 1)));
        assert_eq!(day[3].target, NavTarget::path(CONTROL_PLANE));
        assert_eq!(day[4].target, NavTarget::path(WORKS));
        assert_eq!(
            day[1].target,
            NavTarget::Period(Period::Month, d(2026, 12, 1))
        );
        assert_eq!(day[2].target, NavTarget::Period(Period::Day, today));
        let month = bar_for("daily/month/2026-01.md", today, &F, &links).expect("month bar");
        let labels: Vec<&str> = month.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "← Last Month",
                "Today",
                "Sample Charts",
                "Works",
                "Next Month →"
            ]
        );
        assert_eq!(
            month[0].target,
            NavTarget::Period(Period::Month, d(2025, 12, 1))
        );
        assert_eq!(month[1].target, NavTarget::Period(Period::Day, today));
        assert_eq!(
            month[4].target,
            NavTarget::Period(Period::Month, d(2026, 2, 1))
        );
        // A reading entry under the day folder is an ordinary markdown note.
        let entry = bar_for("daily/day/2026-10-13/entry.md", today, &F, &links).expect("bar");
        assert_eq!(entry[0].label, "Today");
        assert_eq!(entry.len(), 3);
    }

    /// Plan public-release WP-2 (C#12): the default is one Publications link, seeded with the
    /// works views when created; the owner's configured links reproduce the old bar.
    #[test]
    fn ordinary_note_gets_today_plus_links() {
        let today = d(2026, 10, 13);
        let bar = bar_for("notes/a.md", today, &F, &default_nav_links("_crucible")).expect("bar");
        let labels: Vec<&str> = bar.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Today", "Publications"]);
        assert_eq!(bar[0].target, NavTarget::Period(Period::Day, today));
        assert_eq!(
            bar[1].target,
            NavTarget::Path {
                path: "_crucible/Publications.md".into(),
                seed: PUBLICATIONS_SEED.into(),
            }
        );
        assert!(PUBLICATIONS_SEED.contains("works#Active"));
        // The page follows the configured app folder (review S5).
        assert_eq!(default_nav_links("app")[0].path, "app/Publications.md");
        let bar = bar_for("notes/a.md", today, &F, &owner_links()).expect("bar");
        let labels: Vec<&str> = bar.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Today", "Sample Charts", "Works"]);
        assert_eq!(bar[2].target, NavTarget::path(WORKS));
    }

    /// The owner's `editor.toml` `nav_links` (written by WP-6): the pre-WP-2 built-in default.
    const CONTROL_PLANE: &str = "dashboard/Sample Charts.md";
    const WORKS: &str = "core/Works.md";
    fn owner_links() -> Vec<NavLink> {
        vec![
            NavLink {
                label: "Sample Charts".into(),
                path: CONTROL_PLANE.into(),
                seed: String::new(),
            },
            NavLink {
                label: "Works".into(),
                path: WORKS.into(),
                seed: String::new(),
            },
        ]
    }

    #[test]
    fn custom_links_and_non_markdown() {
        let today = d(2026, 10, 13);
        let links = [NavLink {
            label: "Inbox".into(),
            path: "core/Inbox.md".into(),
            seed: String::new(),
        }];
        let bar = bar_for("notes/a.md", today, &F, &links).expect("bar");
        assert_eq!(bar.len(), 2);
        assert_eq!(bar[1].label, "Inbox");
        assert_eq!(bar[1].target, NavTarget::path("core/Inbox.md"));
        let day = bar_for("daily/day/2026-10-13.md", today, &F, &links).expect("bar");
        assert_eq!(day.len(), 5);
        assert_eq!(day[3].label, "Inbox");
        // No links: just Today.
        assert_eq!(bar_for("notes/a.md", today, &F, &[]).expect("bar").len(), 1);
        assert!(bar_for("images/a.png", today, &F, &links).is_none());
    }

    #[test]
    fn configured_folders_replace_the_constants() {
        let f = PeriodFolders::from_config(Some("journal/days/"), Some(" /journal/months "));
        assert_eq!(f.day, "journal/days");
        assert_eq!(f.month, "journal/months");
        assert_eq!(
            day_note_date("journal/days/2026-10-13.md", &f),
            Some(d(2026, 10, 13))
        );
        assert_eq!(day_note_date("daily/day/2026-10-13.md", &f), None);
        assert_eq!(
            month_note_date("journal/months/2026-10.md", &f),
            Some(d(2026, 10, 1))
        );
        assert!(bar_for("journal/days/2026-10-13.md", d(2026, 10, 14), &f, &[]).is_some());
        // ISO basenames stay required (a non-ISO `format` is not honoured).
        assert_eq!(day_note_date("journal/days/13-10-2026.md", &f), None);
    }

    #[test]
    fn missing_or_blank_folders_fall_back() {
        assert_eq!(
            PeriodFolders::from_config(None, None),
            PeriodFolders::default()
        );
        let f = PeriodFolders::from_config(Some("  "), Some("m"));
        assert_eq!(f.day, DAY_FOLDER);
        assert_eq!(f.month, "m");
    }
}
