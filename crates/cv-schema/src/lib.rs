//! `cv-schema`: declarative schemas of every vault config file and step kind, a
//! comment-preserving TOML document model ([`ConfigDoc`]) and form-level [`validate`].
//!
//! Pure crate (only `toml_edit`): the editor's settings tab renders forms from [`Node`]s, edits a
//! [`ConfigDoc`] and blocks a save on [`validate`] errors; the daemon's reload errors are the
//! second layer. The drift test in `crates/cv/tests/schema_drift.rs` holds the schemas to the
//! real parsers.

pub mod doc;
pub mod node;
pub mod schemas;
pub mod validate;

pub use doc::{ConfigDoc, DocError, Path, Seg};
pub use node::{Default, Field, Node, Record, RefKind, Rule, Summary, Union, Variant};
pub use schemas::{CONFIG_DIR, FILES, editor_schema, schema_for, step_schema, view_schema};
pub use toml_edit;
pub use validate::{FieldError, covered_paths, schema_paths, validate, validate_item};

#[cfg(test)]
mod tests;
