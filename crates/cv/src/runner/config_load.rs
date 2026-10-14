//! Reads `CV_CONFIG_DIR` into a [`ConfigSet`] (hot reload is driven by WP-8's poller).

use std::collections::BTreeMap;
use std::path::Path;

use cv_actions::{CONFIG_FILES, ConfigSet};

/// Reloads every known file of `dir` into `set` (missing = empty; unparseable = last valid
/// version kept + error), validating steps with [`crate::steps::validate`].
pub fn load_config_dir(dir: &Path, set: &mut ConfigSet) {
    let mut files = BTreeMap::new();
    for name in CONFIG_FILES {
        let p = dir.join(name);
        match std::fs::read(&p) {
            Ok(bytes) => {
                files.insert(
                    name.to_owned(),
                    Some(String::from_utf8_lossy(&bytes).into_owned()),
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                files.insert(name.to_owned(), None);
            }
            Err(e) => {
                // Unreadable: an empty-on-error file would silently disarm everything; a
                // syntactically invalid stand-in keeps the last valid version and reports it.
                tracing::warn!(file = name, error = %e, "reading config file");
                files.insert(name.to_owned(), Some(format!("= \"unreadable: {e}\"")));
            }
        }
    }
    set.reload(&files, &crate::steps::validate);
}
