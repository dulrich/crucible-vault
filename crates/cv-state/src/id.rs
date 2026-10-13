//! Job ids: millisecond timestamp + per-process monotonic counter.
//!
//! Crucible `newJobId` is "millisecond+monotonic, and claim order IS the sorted order"
//! (`https://github.com/dulrich/obsidian-crucible/blob/master/src/orchestration/AGENTS.md:43`). The encoding here is fixed-width
//! decimal `"{ms:013}-{seq:06}"`, so byte-wise (lexical) order equals numeric order equals
//! creation order. If the clock steps backwards, ids keep climbing from the last-issued
//! millisecond, so order never inverts within a process. Nothing Obsidian-induced.

use std::sync::Mutex;

static LAST: Mutex<(i64, u32)> = Mutex::new((-1, 0));
const SEQ_MAX: u32 = 999_999;

/// Mint a new job id for creation time `now_ms`.
#[must_use]
pub fn new_job_id(now_ms: i64) -> String {
    let now_ms = now_ms.clamp(0, 9_999_999_999_999);
    let mut guard = match LAST.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let (last_ms, last_seq) = *guard;
    let next = if now_ms > last_ms {
        (now_ms, 0)
    } else if last_seq >= SEQ_MAX {
        (last_ms + 1, 0)
    } else {
        (last_ms, last_seq + 1)
    };
    *guard = next;
    format!("{:013}-{:06}", next.0, next.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_sort_in_creation_order_even_within_one_ms_and_across_clock_regression() {
        let mut ids = vec![new_job_id(5_000), new_job_id(5_000), new_job_id(5_000)];
        ids.push(new_job_id(4_000)); // clock stepped back
        ids.push(new_job_id(6_000));
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        let mut dedup = sorted.clone();
        dedup.dedup();
        assert_eq!(dedup.len(), ids.len());
    }
}
