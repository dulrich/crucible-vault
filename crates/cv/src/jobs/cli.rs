//! Argument parsing of `cv jobs|intake|run`.

use cv_protocol::{IntakeListParams, JobsListParams};

/// A parsed `cv jobs|intake|run` command.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    JobsList(JobsListParams),
    JobRetry(String),
    JobCancel(String),
    JobsStats {
        job_type: Option<String>,
        since: Option<Since>,
    },
    JobShow(String),
    IntakeList(IntakeListParams),
    IntakeReconcile {
        dry_run: bool,
    },
    Run {
        action_id: String,
        note: Option<String>,
    },
}

/// A `--since` bound: a relative age or an absolute UTC instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// `<N>m|h|d`: this many ms before now.
    Ago(i64),
    /// `YYYY-MM-DD`: UTC midnight, in epoch ms.
    At(i64),
}

impl Since {
    /// The `since_ms` bound at clock `now_ms`.
    #[must_use]
    pub fn resolve(self, now_ms: i64) -> i64 {
        match self {
            Self::Ago(ms) => now_ms.saturating_sub(ms),
            Self::At(ms) => ms,
        }
    }
}

/// Parses `--since`: `<N>m|<N>h|<N>d` (N a non-negative integer) or a real `YYYY-MM-DD` date
/// (UTC midnight, 1970 or later). Anything else is `None` (a usage error), never coerced.
#[must_use]
pub fn parse_since(v: &str) -> Option<Since> {
    if !v.is_ascii() || v.is_empty() {
        return None;
    }
    let b = v.as_bytes();
    if b.len() == 10 && b[4] == b'-' && b[7] == b'-' {
        let field = |r: std::ops::Range<usize>| -> Option<i64> {
            let t = &v[r];
            if t.bytes().all(|c| c.is_ascii_digit()) {
                t.parse().ok()
            } else {
                None
            }
        };
        let (y, m, d) = (field(0..4)?, field(5..7)?, field(8..10)?);
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let dim = match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if leap => 29,
            2 => 28,
            _ => return None,
        };
        if y < 1970 || d < 1 || d > dim {
            return None;
        }
        return Some(Since::At(cv_text::days_from_civil(y, m, d) * 86_400_000));
    }
    let (num, unit) = v.split_at(v.len() - 1);
    if num.is_empty() || !num.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let n: i64 = num.parse().ok()?;
    let unit_ms: i64 = match unit {
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    n.checked_mul(unit_ms).map(Since::Ago)
}

fn num(v: &str) -> Option<u32> {
    v.parse().ok()
}

/// Parses `args` (from the top-level command word on: `["jobs", "list", …]`); `None` is a
/// usage error.
#[must_use]
pub fn parse(args: &[String]) -> Option<Command> {
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["jobs", "list", rest @ ..] => {
            let mut p = JobsListParams::default();
            for pair in pairs(rest)? {
                match pair {
                    ("--status", v) => p.status = Some(v.to_owned()),
                    ("--type", v) => p.job_type = Some(v.to_owned()),
                    ("--limit", v) => p.limit = Some(num(v)?),
                    ("--offset", v) => p.offset = Some(num(v)?),
                    _ => return None,
                }
            }
            Some(Command::JobsList(p))
        }
        ["jobs", "stats", rest @ ..] => {
            let (mut job_type, mut since) = (None, None);
            for pair in pairs(rest)? {
                match pair {
                    ("--type", v) => job_type = Some(v.to_owned()),
                    ("--since", v) => since = Some(parse_since(v)?),
                    _ => return None,
                }
            }
            Some(Command::JobsStats { job_type, since })
        }
        ["jobs", "show", id] if !id.starts_with('-') => Some(Command::JobShow((*id).to_owned())),
        ["jobs", "retry", id] if !id.starts_with('-') => Some(Command::JobRetry((*id).to_owned())),
        ["jobs", "cancel", id] if !id.starts_with('-') => {
            Some(Command::JobCancel((*id).to_owned()))
        }
        ["intake", "list", rest @ ..] => {
            let mut p = IntakeListParams::default();
            for pair in pairs(rest)? {
                match pair {
                    ("--source-kind", v) => p.source_kind = Some(v.to_owned()),
                    ("--source", v) => p.source_key = Some(v.to_owned()),
                    ("--day", v) => p.day = Some(v.to_owned()),
                    ("--outcome", v) => p.outcome = Some(v.to_owned()),
                    ("--limit", v) => p.limit = Some(num(v)?),
                    ("--offset", v) => p.offset = Some(num(v)?),
                    _ => return None,
                }
            }
            Some(Command::IntakeList(p))
        }
        ["intake", "reconcile"] => Some(Command::IntakeReconcile { dry_run: false }),
        ["intake", "reconcile", "--dry-run"] => Some(Command::IntakeReconcile { dry_run: true }),
        ["run", action, rest @ ..] if !action.starts_with('-') => {
            let note = match rest {
                [] => None,
                ["--note", n] => Some((*n).to_owned()),
                _ => return None,
            };
            Some(Command::Run {
                action_id: (*action).to_owned(),
                note,
            })
        }
        _ => None,
    }
}

/// Splits `--flag value` pairs; an odd count or a non-flag key is `None`.
fn pairs<'a>(rest: &[&'a str]) -> Option<Vec<(&'a str, &'a str)>> {
    if !rest.len().is_multiple_of(2) {
        return None;
    }
    rest.chunks(2)
        .map(|c| c[0].starts_with("--").then_some((c[0], c[1])))
        .collect()
}
