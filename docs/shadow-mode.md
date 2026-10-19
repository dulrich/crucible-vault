# Shadow mode (retired)

Shadow mode — the read-only soak daemon that logged would-be writes to an overlay under `CV_SCRATCH` (D11) — was **retired on 2026-10-19** by plan `tn-review-baseline-remediation` WP-R6. The daemon now has a single live mode; `CV_MODE`, `CV_SCRATCH`, the overlay/tombstones, the `shadow_write` notification and the Landlock guard are gone. The protected-root + cut-over-marker guard (I1) still refuses live mode against the protected vault without the `state.db` cut-over marker, which keeps tests and scratch runs off the live vault.

The former soak runbook is in git history (this file before WP-R6); `docs/cutover.md` records how the soak was used for the 2026-10-14 cut-over.
