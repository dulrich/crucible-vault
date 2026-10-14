//! Step module `capture`: Capture (URL/clipboard → note). **Stub** — the body belongs to WP-10 of plan `v0-core-daemon`.
//!
//! Contract (see `steps/mod.rs`): `validate` parses this module's params from the step's opaque
//! table and may consult [`ConfigSet::table`]; `execute` runs one step on a blocking thread.

use cv_actions::{ConfigSet, Step};

use super::{StepCtx, StepError, StepOutcome};

/// Validates one step's params. Stub: accepts everything until WP-10 lands.
pub fn validate(_step: &Step, _config: &ConfigSet) -> Result<(), String> {
    Ok(())
}

/// Executes one step. Stub: always fails with "not implemented (WP-10)".
pub fn execute(step: &Step, _ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    Err(StepError::not_implemented(step.kind(), "WP-10"))
}
