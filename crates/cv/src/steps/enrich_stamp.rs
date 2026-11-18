//! The enrich stamp: `enriched` (date) and `enrich-model` (provider:model) mark a note the enrich
//! pass has written; `enrich-miss` counts the passes that wrote nothing. [`set`] stamps a note,
//! [`clear`] makes it eligible again.

use cv_note::{FmError, FrontmatterDoc, JsValue};

/// Stamps the note as enriched on `date` by `model_ref` and drops its miss count.
pub(in crate::steps) fn set(
    doc: &mut FrontmatterDoc,
    date: String,
    model_ref: String,
) -> Result<(), FmError> {
    doc.set("enriched", &JsValue::Str(date))?;
    doc.set("enrich-model", &JsValue::Str(model_ref))?;
    doc.remove("enrich-miss")?;
    Ok(())
}

/// Removes the stamp and the miss count, so the note re-enters the enrich backfill.
pub(crate) fn clear(doc: &mut FrontmatterDoc) -> Result<(), FmError> {
    for k in ["enriched", "enrich-model", "enrich-miss"] {
        doc.remove(k)?;
    }
    Ok(())
}
