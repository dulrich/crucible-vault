//! Test helpers shared by the `enrich` and `fold` test modules.

use super::EnrichOpts;

pub(crate) fn opts() -> EnrichOpts {
    EnrichOpts { headings: true }
}
