//! Day/month period date math for nav (WP-4).
//!
//! Replaces the Note Toolbar `{{js:}}` day/month bars (inventory #2, D10 "Nav bars"). The bar
//! items are transcribed from the live Note Toolbar settings (`folderMappings`: `daily/day` →
//! Day nav, `daily/month` → Month nav). Pure: "today" is always injected.

use chrono::{Datelike, Days, Months, NaiveDate};

/// Folder of day notes.
pub const DAY_FOLDER: &str = "daily/day";
/// Folder of month notes.
pub const MONTH_FOLDER: &str = "daily/month";
/// Fixed Day-bar link.
pub const CONTROL_PLANE: &str = "dashboard/Sample Charts.md";
/// Fixed Day-bar link.
pub const WORKS: &str = "core/Works.md";

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
    /// Opens a fixed vault path directly.
    Path(&'static str),
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
pub fn bar_for(path: &str, today: NaiveDate, folders: &PeriodFolders) -> Option<Vec<NavItem>> {
    let item = |label: String, target| NavItem { label, target };
    if let Some(d) = day_note_date(path, folders) {
        return Some(vec![
            item(
                "← yesterday".into(),
                NavTarget::Period(Period::Day, yesterday(d)),
            ),
            item(
                month_label(d),
                NavTarget::Period(Period::Month, month_start(d)),
            ),
            item("Today".into(), NavTarget::Period(Period::Day, today)),
            item("Sample Charts".into(), NavTarget::Path(CONTROL_PLANE)),
            item("Works".into(), NavTarget::Path(WORKS)),
            item(
                "tomorrow →".into(),
                NavTarget::Period(Period::Day, tomorrow(d)),
            ),
        ]);
    }
    if let Some(m) = month_note_date(path, folders) {
        return Some(vec![
            item(
                "← Last Month".into(),
                NavTarget::Period(Period::Month, prev_month(m)),
            ),
            item("Today".into(), NavTarget::Period(Period::Day, today)),
            item(
                "Next Month →".into(),
                NavTarget::Period(Period::Month, next_month(m)),
            ),
        ]);
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
        let day = bar_for("daily/day/2026-12-31.md", today, &F).expect("day bar");
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
        assert_eq!(
            day[1].target,
            NavTarget::Period(Period::Month, d(2026, 12, 1))
        );
        assert_eq!(day[2].target, NavTarget::Period(Period::Day, today));
        let month = bar_for("daily/month/2026-01.md", today, &F).expect("month bar");
        assert_eq!(month.len(), 3);
        assert_eq!(
            month[0].target,
            NavTarget::Period(Period::Month, d(2025, 12, 1))
        );
        assert_eq!(month[1].target, NavTarget::Period(Period::Day, today));
        assert_eq!(
            month[2].target,
            NavTarget::Period(Period::Month, d(2026, 2, 1))
        );
        assert!(bar_for("daily/day/2026-10-13/entry.md", today, &F).is_none());
        assert!(bar_for("notes/a.md", today, &F).is_none());
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
        assert!(bar_for("journal/days/2026-10-13.md", d(2026, 10, 14), &f).is_some());
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
