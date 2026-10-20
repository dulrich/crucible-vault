//! plan editor-settings-tab WP-3 tests: the schema-driven form model, the document edits it
//! emits, the Views carve-out, the shared bookmarks file and the Actions page build time.
//! Synthetic inline TOML + `FakeDaemon` only. Shared helpers live here; the tests are grouped by
//! behaviour in the child modules.

mod editor_page;
mod fields;
mod network_pickers;
mod records;
mod views_bookmarks;

use std::sync::Arc;

use cv_schema::{ConfigDoc, Path};
use cv_testkit::{SyntheticVault, VaultSpec};

use super::form::{self, Ctx, FormState, Kind, Row};
use super::*;
use crate::app::App;
use crate::bookmarks::{BOOKMARKS_PATH, BookmarkMsg};
use crate::daemon::ContentHash;
use crate::daemon::FakeDaemon;
use crate::message::Message;
use crate::message::{PanesMsg, TabsMsg};
use cv_schema::Node;

const ACTIONS: &str = "_custom/crucible-vault/actions.toml";
const KINDS: &str = "_custom/crucible-vault/kinds.toml";

fn fake() -> (tempfile::TempDir, Arc<FakeDaemon>) {
    let tmp = tempfile::tempdir().expect("tmp");
    let vault = tmp.path().join("vault");
    SyntheticVault::build(&vault, &VaultSpec::default()).expect("vault");
    let d = FakeDaemon::open(&vault, &tmp.path().join("data")).expect("fake");
    (tmp, Arc::new(d))
}

fn app_over(d: &Arc<FakeDaemon>) -> App {
    let dc: Arc<dyn DaemonClient> = Arc::clone(d) as Arc<dyn DaemonClient>;
    let mut app = App::new(Some(dc), None);
    app.queue_rpcs();
    app
}

fn s(m: SettingsMsg) -> Message {
    Message::Settings(m)
}

fn p(s: &str) -> Path {
    Path::parse(s).expect("path")
}

/// A settings model with `file` loaded from `text` (no daemon).
fn loaded(file: &str, text: &str) -> Settings {
    let mut st = Settings::default();
    let f = st.files.entry(file.to_owned()).or_insert_with(|| {
        ConfigFile::new(
            file,
            cv_schema::schema_for(file.rsplit('/').next().unwrap_or(file)),
        )
    });
    f.apply_read(Ok((text.as_bytes().to_vec(), ContentHash::from_hex("00"))));
    st
}

fn rows_of(st: &Settings, file: &str) -> Vec<Row> {
    let f = st.file(file).expect("file");
    pages::file_rows(st, f)
}

fn doc_text(st: &Settings, file: &str) -> String {
    st.file(file)
        .and_then(|f| f.doc())
        .map(ToString::to_string)
        .unwrap_or_default()
}

const ONE_ACTION: &str = r#"# my actions
[[action]]
id = "tidy"
title = "Tidy"
mystery = 7 # unknown, kept

[[action.steps]]
type = "move"
path = "{file}"
to = "archive"
kind = "note"
"#;

// ---- plan round-5 WP-6: network-backed pickers ---------------------------------------------

const PIPES: &str = "_custom/crucible-vault/pipelines.toml";
const PROVIDERS: &str = "_custom/crucible-vault/providers.toml";
const AGENTS: &str = "_custom/crucible-vault/agents.toml";
const SOURCES: &str = "_custom/crucible-vault/sources.toml";

/// Loads `file` from `text` into `st` (no daemon read).
fn put(st: &mut Settings, file: &str, text: &str) {
    let f = st.files.entry(file.to_owned()).or_insert_with(|| {
        ConfigFile::new(
            file,
            cv_schema::schema_for(file.rsplit('/').next().unwrap_or(file)),
        )
    });
    f.apply_read(Ok((text.as_bytes().to_vec(), ContentHash::from_hex("00"))));
}

/// The picker the form gives the field at `at` of `file`: walks the file's schema to the field
/// and asks `pickers::for_field` with its node and reference hint.
fn picker_of(file: &str, at: &Path) -> Option<form::Picker> {
    let name = file.rsplit('/').next().unwrap_or(file);
    let mut node = cv_schema::schema_for(name).expect("schema");
    let mut hint = None;
    for seg in &at.0 {
        match (seg, node) {
            (cv_schema::Seg::Key(k), Node::Record(r)) => {
                let f = r.field(k).expect("field");
                node = &f.node;
                hint = f.hint;
            }
            (cv_schema::Seg::Key(_), Node::Map(inner))
            | (cv_schema::Seg::Index(_), Node::List(inner)) => {
                node = inner;
                hint = None;
            }
            _ => panic!("{at}: not walkable"),
        }
    }
    pickers::for_field(node, hint)
}

/// Types `text` into the picker field `(file, at)` through the app.
fn type_in(app: &mut App, file: &str, at: &str, text: &str) {
    let path = p(at);
    let picker = picker_of(file, &path);
    assert!(picker.is_some(), "{at}: no picker");
    let _ = app.send(s(SettingsMsg::Form(
        file.to_owned(),
        FormMsg::Text {
            path,
            text: text.to_owned(),
            required: false,
            picker,
        },
    )));
}
