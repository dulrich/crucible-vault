//! The Captures & periods page: `captures.toml` with `[periods.*]` first under a "Period notes"
//! heading, then the `[[capture]]` presets (then any unknown keys, read-only).

use cv_schema::Path;
use iced::Element;

use super::PageCx;
use crate::message::Message;
use crate::settings::form::{self, Ctx, Kind, Row};
use crate::settings::{ConfigFile, Settings};

/// The page intro line.
pub const INTRO: &str =
    "Period notes (day / week / month folders, formats, templates), then named capture presets.";

/// The rows: periods, captures, unknown root keys.
pub fn rows(s: &Settings, f: &ConfigFile) -> Vec<Row> {
    let (Some(doc), Some(schema)) = (&f.doc, f.schema) else {
        return Vec::new();
    };
    let cx = Ctx {
        file: &f.path,
        doc,
        errors: &f.errors,
        form: &s.form,
    };
    let mut out = form::field_rows(&cx, schema, "periods");
    out.extend(form::field_rows(&cx, schema, "capture"));
    for k in doc.keys(&Path::root()) {
        if k == "periods" || k == "capture" {
            continue;
        }
        let p = Path::root().key(&k);
        out.push(Row {
            depth: 0,
            path: p.clone(),
            label: k,
            help: "not in the schema; kept as is",
            kind: Kind::ReadOnly {
                toml: doc.get(&p).map(form::toml_text).unwrap_or_default(),
            },
            errors: Vec::new(),
        });
    }
    out
}

/// The page.
pub fn view<'a>(cx: PageCx<'a>, f: &'a ConfigFile) -> Element<'a, Message> {
    super::frame(cx, f, INTRO, rows(cx.s, f))
}
