//! `ignore_video` (plan `v0-automation-pipelines` WP-9): Ignore Shorts.
//!
//! WP-1 stub: params must be empty until WP-9 defines them; execution fails `not implemented`.

use cv_actions::{ConfigSet, Step};

use crate::steps::{StepCtx, StepError, StepOutcome};

/// Validates the step's params (none accepted yet; unknown keys disarm, I5).
pub fn validate(step: &Step, _config: &ConfigSet) -> Result<(), String> {
    crate::steps::reject_params(step)
}

/// Executes the step.
pub fn execute(step: &Step, _ctx: &mut StepCtx<'_>) -> Result<StepOutcome, StepError> {
    Err(StepError::not_implemented(step.kind(), "WP-9"))
}
