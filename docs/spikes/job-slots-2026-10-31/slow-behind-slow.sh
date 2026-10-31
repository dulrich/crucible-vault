#!/usr/bin/env bash
# Plan job-slots-fast-lane: counts permit waits in the last <hours> (default 24) where a slow job
# waited while every listed permit holder (the blocker, N comma-separated) was slow too. "Slow" = an action with any settled attempt
# >= 1 s. Read-only; aggregates and action ids only (no note paths).
# Usage: slow-behind-slow.sh [hours] [state.db]
set -euo pipefail
. "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/../../../scripts/lib/build-id.sh"
build_flags "${BASH_SOURCE[0]}" "$@"
case "${1:-}" in -h|--help) sed -n 2,5p "$0" | sed 's/^# \{0,1\}//'; exit 0;; esac
H=${1:-24}; DB=${2:-${CV_STATE_DB:-$HOME/.local/state/crucible-vault/state.db}}
NOW=$(($(date +%s) * 1000)); SINCE=$((NOW - H * 3600000))
sqlite3 -readonly "$DB" "
with recursive slow(a) as (select j.job_type from job_attempt_times t join jobs j on j.id=t.job_id
  where t.settled_ms is not null group by j.job_type having max(t.settled_ms-t.claimed_ms) >= 1000),
split(job_id, seq, rest, item) as (select job_id, seq, blocker || ',', '' from job_wait_events
  where cause='permit' and at_ms > $SINCE and coalesce(blocker,'') <> ''
  union all select job_id, seq, substr(rest, instr(rest, ',')+1), substr(rest, 1, instr(rest, ',')-1) from split where rest <> ''),
ev as (select w.*, (select min(claimed_ms) from job_attempt_times t where t.job_id=w.job_id and t.claimed_ms>=w.at_ms) cl,
  (select min(w2.at_ms) from job_wait_events w2 where w2.job_id=w.job_id and w2.seq>w.seq) nxt
 from job_wait_events w join jobs j on j.id=w.job_id
 where w.cause='permit' and w.at_ms > $SINCE and j.job_type in (select a from slow) and coalesce(w.blocker,'') <> ''
  and not exists (select 1 from split s where s.job_id=w.job_id and s.seq=w.seq and s.item <> ''
    and s.item not in (select a from slow)))
select 'episodes=' || count(*) || ' jobs=' || count(distinct job_id)
  || ' total_s=' || round(coalesce(sum(min(coalesce(nxt,cl),cl)-at_ms),0)/1000.0,1)
  || ' max_s=' || round(coalesce(max(min(coalesce(nxt,cl),cl)-at_ms),0)/1000.0,1) from ev;"
