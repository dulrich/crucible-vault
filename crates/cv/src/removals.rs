//! `cv removals export` (plan intake-removals-sources-rescan WP-4): every `removal_labels` row of
//! `state.db` as one JSON object per line on stdout (fields = the columns minus the row id),
//! oldest first. Daemon-free and read-only (`StateReader`, the configured `CV_STATE_DB`).

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

use crate::config::Config;

/// Usage line(s) for `cv --help`.
pub const USAGE: &str = "  cv removals export               print the removal labels (state.db) as JSON lines on stdout\n";

/// `cv removals …`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    if args.len() != 1 || args[0] != "export" {
        eprint!("usage:\n{USAGE}");
        return 2;
    }
    let r = Config::from_env().and_then(|c| {
        let mut out = std::io::stdout().lock();
        export(&c.state_db, &mut out)
    });
    match r {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("cv: {e:#}");
            1
        }
    }
}

/// Writes every label of the state database at `state_db` to `out` as JSON lines.
pub fn export(state_db: &Path, out: &mut impl Write) -> Result<()> {
    let reader = cv_state::StateReader::open(state_db)
        .with_context(|| format!("opening {}", state_db.display()))?;
    for row in reader.removal_labels()? {
        serde_json::to_writer(&mut *out, &row)?;
        out.write_all(b"\n")?;
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use cv_state::{QueueConfig, RemovalLabel, StateDb};

    #[test]
    fn exports_one_json_line_per_row_and_nothing_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.db");
        let mut db = StateDb::open(&p, 1, QueueConfig::default()).unwrap();
        let mut out = Vec::new();
        super::export(&p, &mut out).unwrap();
        assert!(out.is_empty());
        let row = |verdict: &str, origin: &str, line: &str| RemovalLabel {
            note_id: 4,
            publication: "Synth Pub".into(),
            kind: "cta".into(),
            verdict: verdict.into(),
            origin: origin.into(),
            line: line.into(),
            line_key: 7,
            at_ms: 11,
        };
        db.insert_removal_labels(&[
            row("delete", "fold", "subscribe now"),
            row("keep", "selection", "real text"),
        ])
        .unwrap();
        super::export(&p, &mut out).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "{\"note_id\":4,\"publication\":\"Synth Pub\",\"kind\":\"cta\",\"verdict\":\"delete\",\"origin\":\"fold\",\"line\":\"subscribe now\",\"line_key\":7,\"at_ms\":11}\n\
             {\"note_id\":4,\"publication\":\"Synth Pub\",\"kind\":\"cta\",\"verdict\":\"keep\",\"origin\":\"selection\",\"line\":\"real text\",\"line_key\":7,\"at_ms\":11}\n"
        );
    }
}
